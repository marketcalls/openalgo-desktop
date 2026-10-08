//! Rebuild runs that were open when the process stopped (web
//! `services/strategy_module/recovery.py`).
//!
//! Identity, disposition and ownership come from the ORDER ROWS grouped by
//! exact `position_ref`; volatile risk state (last price, effective stop and
//! target, trail flags, favourable extremes) from the newest checkpoint. A
//! leg's side is derived from its entry action. A dead order is never
//! upgraded by a checkpoint; a working one may be.
//!
//! Order status is normalised in exactly one place, and an unrecognised
//! status reads as WORKING: reading an unknown exit as dead would let a
//! second exit be placed, and a second exit opens the opposite position.
//!
//! When durable rows prove ambiguous exposure (more than two held owners on
//! one leg, a working exit with no confirmed owner) the run is not installed
//! and not finalised: it stays open and reserved with a critical
//! `recovery_failed` event. Malformed state with no proven exposure is
//! finalised as `recovery_failed` so it cannot wedge startup.

use super::state::{LegState, RunState, Superseded};
use super::store::{OrderRow, Store};
use super::tick_feed::Key;
use super::StrategyModule;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const FILLED: &[&str] = &[
    "complete",
    "completed",
    "filled",
    "fill",
    "executed",
    "traded",
    "trade",
    "success",
    "successful",
];
const CANCELLED: &[&str] = &["cancelled", "canceled", "cancel", "expired", "lapsed"];
const REJECTED: &[&str] = &[
    "rejected", "reject", "failed", "failure", "error", "invalid",
];
const PENDING: &[&str] = &["pending", "queued", "validation_pending", "transit"];

/// One broker or store status as one of `pending`, `open`, `complete`,
/// `cancelled`, `rejected`. Unknown reads as working (`open`).
pub fn normalise_order_status(raw: &str) -> &'static str {
    let t = raw.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if FILLED.contains(&t.as_str()) {
        "complete"
    } else if CANCELLED.contains(&t.as_str()) {
        "cancelled"
    } else if REJECTED.contains(&t.as_str()) {
        "rejected"
    } else if PENDING.contains(&t.as_str()) {
        "pending"
    } else {
        "open"
    }
}

pub fn order_is_filled(raw: &str) -> bool {
    normalise_order_status(raw) == "complete"
}

pub fn order_is_dead(raw: &str) -> bool {
    matches!(normalise_order_status(raw), "cancelled" | "rejected")
}

pub fn order_is_working(raw: &str) -> bool {
    matches!(normalise_order_status(raw), "pending" | "open")
}

/// What one run's recovery produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecoveredRun {
    pub run_id: i64,
    pub strategy_id: Option<i64>,
    pub ok: bool,
    pub finalised: bool,
    pub symbols: Vec<Key>,
    pub legs: usize,
    pub open_legs: usize,
    pub error: Option<String>,
}

enum RecoveryError {
    /// Persisted exposure exists but cannot be represented safely.
    Managed(String),
    Other(String),
}

/// Rebuild every open run, once, at startup (before any price arrives).
pub async fn recover_all(module: &StrategyModule) -> crate::error::Result<HashMap<i64, Vec<Key>>> {
    let mut resumed = HashMap::new();
    for run in module.store.list_open_runs()? {
        let r = recover_run(module, run.id).await;
        if r.ok {
            resumed.insert(run.id, r.symbols);
        }
    }
    Ok(resumed)
}

/// Rebuild one run, or finalise it if it cannot be rebuilt. Never fails.
pub async fn recover_run(module: &StrategyModule, run_id: i64) -> RecoveredRun {
    if let Some(existing) = module.state.snapshot(run_id) {
        // Idempotent: never overwrite a run already live here.
        return RecoveredRun {
            run_id,
            strategy_id: Some(existing.strategy_id),
            ok: true,
            symbols: existing.subscribed_symbols(),
            legs: existing.legs.len(),
            open_legs: existing.open_legs().len(),
            ..Default::default()
        };
    }
    let Ok(Some(run)) = module.store.get_run(run_id) else {
        return RecoveredRun {
            run_id,
            error: Some("Run not found".into()),
            ..Default::default()
        };
    };
    if run.stopped_at.is_some() {
        return RecoveredRun {
            run_id,
            strategy_id: Some(run.strategy_id),
            error: Some("Run is already stopped".into()),
            ..Default::default()
        };
    }
    let strategy = module
        .store
        .get_strategy_unscoped(run.strategy_id)
        .ok()
        .flatten();
    let user_id = strategy
        .as_ref()
        .map(|s| s.user_id.clone())
        .unwrap_or_default();
    match recover_inner(module, &run, strategy.as_ref()).await {
        Ok(r) => r,
        Err(RecoveryError::Managed(msg)) => {
            module
                .emit(
                    run.strategy_id,
                    &user_id,
                    "recovery_failed",
                    &format!(
                        "Run {} remains open for manual reconciliation: {}",
                        run_id, msg
                    ),
                    super::store::EventFields {
                        run_id: Some(run_id),
                        severity: Some("critical"),
                        ..Default::default()
                    },
                )
                .await;
            RecoveredRun {
                run_id,
                strategy_id: Some(run.strategy_id),
                error: Some(msg),
                ..Default::default()
            }
        }
        Err(RecoveryError::Other(msg)) => {
            let finalised = module
                .finalise(
                    run_id,
                    run.strategy_id,
                    &user_id,
                    "recovery_failed",
                    &format!(
                        "Run {} could not be recovered and was closed: {}",
                        run_id, msg
                    ),
                )
                .await;
            RecoveredRun {
                run_id,
                strategy_id: Some(run.strategy_id),
                finalised,
                error: Some(msg),
                ..Default::default()
            }
        }
    }
}

async fn recover_inner(
    module: &StrategyModule,
    run: &super::store::RunRow,
    strategy: Option<&super::store::StrategyRow>,
) -> Result<RecoveredRun, RecoveryError> {
    let run_id = run.id;
    let user_id = strategy.map(|s| s.user_id.clone()).unwrap_or_default();
    if module.reconcile_acks(run_id, false).await > 0 {
        return Err(RecoveryError::Managed(
            "broker acknowledgements could not be linked to their order rows; the possible position stays reserved".into(),
        ));
    }
    let orders = module
        .store
        .list_orders(run_id)
        .map_err(|e| RecoveryError::Other(e.to_string()))?;

    // The crash window between creating a run and linking it: no order can
    // have been dispatched, so the empty claim is safe to release.
    if orders.is_empty() {
        if let Some(s) = strategy {
            if s.status == "running" && s.current_run_id.is_none() {
                let finalised = module
                    .store
                    .finish_unlinked_run_and_release_claim(run_id, s.id, "error")
                    .unwrap_or(false);
                return Ok(RecoveredRun {
                    run_id,
                    strategy_id: Some(s.id),
                    finalised,
                    ..Default::default()
                });
            }
        }
    }

    let checkpoint = module.store.latest_checkpoint(run_id).ok().flatten();
    let config_legs = strategy.map(|s| s.legs.clone()).unwrap_or(Value::Null);
    let rebuilt = rebuild_state(
        run_id,
        run.strategy_id,
        &orders,
        checkpoint.as_ref(),
        &config_legs,
        run.stop_requested_reason.is_some(),
    )?;
    let symbols = rebuilt.subscribed_symbols();
    let legs = rebuilt.legs.len();
    let open = rebuilt.open_legs().len();
    if !rebuilt.requires_management() {
        // Every leg closed or refused: the process died between the last
        // exit fill and the finalise it would have triggered.
        let pending = run.stop_requested_reason.is_some();
        let reason = run
            .stop_requested_reason
            .clone()
            .unwrap_or_else(|| "manual".to_string());
        module.state.install(rebuilt);
        let finalised = module
            .finalise(
                run_id,
                run.strategy_id,
                &user_id,
                &reason,
                &if pending {
                    format!("Run stopped ({}); recovery confirmed it was flat", reason)
                } else {
                    "Recovered flat: every leg had already closed, so the run was finished".into()
                },
            )
            .await;
        if !finalised {
            module.state.clear(run_id);
        }
        return Ok(RecoveredRun {
            run_id,
            strategy_id: Some(run.strategy_id),
            finalised,
            legs,
            ..Default::default()
        });
    }
    if !module.state.hydrate_if_absent(rebuilt) {
        return Ok(RecoveredRun {
            run_id,
            strategy_id: Some(run.strategy_id),
            ok: true,
            symbols,
            legs,
            open_legs: open,
            ..Default::default()
        });
    }
    module
        .emit(
            run.strategy_id,
            &user_id,
            "recovery_succeeded",
            &format!(
                "Run {} recovered: {} open of {} legs, {} instruments resubscribed{}",
                run_id,
                open,
                legs,
                symbols.len(),
                if checkpoint.is_some() {
                    ""
                } else {
                    " (no checkpoint; risk levels re-derive on the next tick)"
                }
            ),
            super::store::EventFields {
                run_id: Some(run_id),
                ..Default::default()
            },
        )
        .await;
    Ok(RecoveredRun {
        run_id,
        strategy_id: Some(run.strategy_id),
        ok: true,
        symbols,
        legs,
        open_legs: open,
        ..Default::default()
    })
}

fn position_from_action(action: &str) -> Result<String, RecoveryError> {
    match action.trim().to_ascii_uppercase().as_str() {
        "BUY" => Ok("B".into()),
        "SELL" => Ok("S".into()),
        other => Err(RecoveryError::Other(format!(
            "entry action {:?} names no side",
            other
        ))),
    }
}

fn config_leg(config: &Value, leg_id: i64) -> Value {
    config
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|l| l.get("id").or(l.get("leg_id")).and_then(Value::as_i64) == Some(leg_id))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// The checkpoint fields for one exact incarnation (live or superseded).
fn checkpoint_for(cp_leg: &Value, position_ref: Option<&str>) -> Value {
    if cp_leg["position_ref"].as_str() == position_ref {
        return cp_leg.clone();
    }
    if cp_leg["superseded"]["position_ref"].as_str() == position_ref && position_ref.is_some() {
        return cp_leg["superseded"].clone();
    }
    Value::Null
}

struct Recovered {
    rank: i64,
    leg: LegState,
    priced: bool,
}

#[allow(clippy::too_many_arguments)]
fn rebuild_position(
    leg_id: i64,
    position_ref: Option<String>,
    group: &[&OrderRow],
    cp: &Value,
    cfg: &Value,
) -> Result<Recovered, RecoveryError> {
    let entries: Vec<&&OrderRow> = group.iter().filter(|o| o.kind == "entry").collect();
    if entries.is_empty() {
        return Err(RecoveryError::Managed(format!(
            "leg {} has exits for a position without its entry; ownership is ambiguous",
            leg_id
        )));
    }
    // The decisive entry: the latest one (rows are in placement order).
    let entry = entries[entries.len() - 1];
    let exits: Vec<&&OrderRow> = group.iter().filter(|o| o.kind != "entry").collect();
    let working: Vec<&&&OrderRow> = exits
        .iter()
        .filter(|o| order_is_working(&o.status))
        .collect();
    if working.len() > 1 {
        return Err(RecoveryError::Managed(format!(
            "leg {} has more than one working exit; ownership is ambiguous",
            leg_id
        )));
    }
    let position = position_from_action(&entry.action)?;
    let reported = entry.filled_qty.filter(|q| *q > 0);
    let cp_filled = cp["entry_status"].as_str() == Some("complete");
    let entry_filled = reported.is_some()
        || order_is_filled(&entry.status)
        || (!order_is_dead(&entry.status) && cp_filled);
    let mut entry_qty = entry.qty.max(0);
    if let Some(r) = reported {
        entry_qty = if entry_qty > 0 { entry_qty.min(r) } else { r };
    }
    let entry_avg = if entry_filled {
        entry
            .avg_fill_price
            .filter(|p| *p > 0.0)
            .or_else(|| cp["entry_avg"].as_f64().filter(|p| *p > 0.0))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let mut remaining = if entry_filled { entry_qty } else { 0 };
    let mut realized = 0.0;
    let mut priced = true;
    let mut last_exit_avg = None;
    for x in &exits {
        let applied = match x.filled_qty.filter(|q| *q > 0) {
            Some(r) => remaining.min(r),
            None if order_is_filled(&x.status) => {
                remaining.min(if x.qty > 0 { x.qty } else { remaining })
            }
            None => 0,
        };
        if applied <= 0 {
            continue;
        }
        remaining -= applied;
        let price = x.avg_fill_price.filter(|p| *p > 0.0);
        if price.is_some() {
            last_exit_avg = price;
        }
        match price {
            Some(p) if entry_avg > 0.0 => {
                let sign = if position == "B" { 1.0 } else { -1.0 };
                realized += (p - entry_avg) * applied as f64 * sign;
            }
            _ => priced = false,
        }
    }
    if !working.is_empty() && (!entry_filled || remaining <= 0) {
        return Err(RecoveryError::Managed(format!(
            "leg {} has a working exit without a confirmed position behind it",
            leg_id
        )));
    }
    let entry_status_norm = normalise_order_status(&entry.status);
    let (status, entry_status, qty) = if entry_filled && remaining > 0 {
        ("open", "complete".to_string(), remaining)
    } else if entry_filled {
        ("closed", "complete".to_string(), 0)
    } else if order_is_dead(&entry.status) {
        ("rejected", entry_status_norm.to_string(), entry.qty)
    } else {
        ("configured", entry_status_norm.to_string(), entry.qty)
    };
    let active = working.last().map(|o| (o.id, o.kind.clone()));
    let num = |k: &str| cp[k].as_f64().or_else(|| cfg[k].as_f64());
    let trail = |k: &str| {
        cp[if k == "x" { "trail_x" } else { "trail_y" }]
            .as_f64()
            .or_else(|| cfg["trail"][k].as_f64())
            .unwrap_or(0.0)
    };
    let leg = LegState {
        leg_id,
        position,
        symbol: entry.symbol.clone(),
        exchange: entry.exchange.clone(),
        lots: cp["lots"]
            .as_i64()
            .or_else(|| cfg["lots"].as_i64())
            .unwrap_or(1),
        qty,
        position_ref,
        entry_order_id: Some(entry.id),
        entry_status,
        entry_filled_qty: reported.unwrap_or(if entry_filled { entry_qty } else { 0 }),
        entry_avg,
        exit_order_id: active.as_ref().map(|a| a.0),
        exit_claim_token: None,
        exit_kind: active.map(|a| a.1),
        exit_avg: last_exit_avg,
        ltp: cp["ltp"].as_f64(),
        mtm: if status == "closed" {
            0.0
        } else {
            cp["mtm"].as_f64().unwrap_or(0.0)
        },
        realized_pnl: realized,
        status: status.into(),
        tick_source: "ws".into(),
        risk_unit: cp["risk_unit"]
            .as_str()
            .or_else(|| cfg["risk_unit"].as_str())
            .unwrap_or("points")
            .into(),
        sl_pts: num("sl_pts"),
        target_pts: num("target_pts"),
        trail_x: trail("x"),
        trail_y: trail("y"),
        effective_sl: cp["effective_sl"].as_f64(),
        effective_target: cp["effective_target"].as_f64(),
        trail_active: cp["trail_active"].as_bool().unwrap_or(false),
        highest_price: cp["highest_price"].as_f64(),
        lowest_price: cp["lowest_price"].as_f64(),
        superseded: None,
    };
    Ok(Recovered {
        rank: entry.id,
        leg,
        priced,
    })
}

fn rebuild_state(
    run_id: i64,
    strategy_id: i64,
    orders: &[OrderRow],
    checkpoint: Option<&Value>,
    config_legs: &Value,
    stopping: bool,
) -> Result<RunState, RecoveryError> {
    let cp_legs = checkpoint
        .map(|c| c["leg_state"].clone())
        .unwrap_or(Value::Null);
    let mut by_leg: BTreeMap<i64, BTreeMap<Option<String>, Vec<&OrderRow>>> = BTreeMap::new();
    for o in orders {
        by_leg
            .entry(o.leg_id)
            .or_default()
            .entry(o.position_ref.clone())
            .or_default()
            .push(o);
    }
    let mut legs = Vec::new();
    for (leg_id, groups) in by_leg {
        let cp_leg = cp_legs[leg_id.to_string()].clone();
        let cfg = config_leg(config_legs, leg_id);
        let mut positions = Vec::new();
        for (pref, group) in groups {
            let cp = checkpoint_for(&cp_leg, pref.as_deref());
            positions.push(rebuild_position(leg_id, pref, &group, &cp, &cfg)?);
        }
        positions.sort_by_key(|p| p.rank);
        let priced = positions.iter().all(|p| p.priced);
        let durable_realized: f64 = positions.iter().map(|p| p.leg.realized_pnl).sum();
        let managed: Vec<usize> = positions
            .iter()
            .enumerate()
            .filter(|(_, p)| p.leg.requires_management())
            .map(|(i, _)| i)
            .collect();
        if managed.len() > 2 {
            return Err(RecoveryError::Managed(format!(
                "leg {} has more than two held positions; the run cannot be represented safely",
                leg_id
            )));
        }
        let mut live = match managed.last() {
            Some(i) => positions[*i].leg.clone(),
            None => positions
                .last()
                .map(|p| p.leg.clone())
                .ok_or_else(|| RecoveryError::Other(format!("leg {} has no position", leg_id)))?,
        };
        if managed.len() == 2 {
            let outgoing = &positions[managed[0]].leg;
            if outgoing.status != "open" {
                return Err(RecoveryError::Managed(format!(
                    "leg {} has an older working entry that cannot be represented as an outgoing position",
                    leg_id
                )));
            }
            if (outgoing.symbol.as_str(), outgoing.exchange.as_str())
                != (live.symbol.as_str(), live.exchange.as_str())
            {
                return Err(RecoveryError::Managed(format!(
                    "leg {} holds positions on different instruments; one leg cannot manage both",
                    leg_id
                )));
            }
            live.superseded = Some(Superseded {
                exit_order_id: outgoing.exit_order_id,
                exit_claim_token: None,
                exit_kind: outgoing.exit_kind.clone(),
                entry_order_id: outgoing.entry_order_id,
                position_ref: outgoing.position_ref.clone(),
                position: outgoing.position.clone(),
                entry_avg: outgoing.entry_avg,
                qty: outgoing.qty,
            });
        }
        live.realized_pnl = if priced {
            durable_realized
        } else {
            cp_leg["realized_pnl"].as_f64().unwrap_or(durable_realized)
        };
        legs.push(live);
    }
    let cp = checkpoint.cloned().unwrap_or(Value::Null);
    let mut state = RunState::new(run_id, strategy_id, legs);
    state.pnl_realized = state.legs.values().map(|l| l.realized_pnl).sum();
    state.pnl_unrealized = cp["pnl_unrealized"].as_f64().unwrap_or(0.0);
    state.pnl_total = state.pnl_realized + state.pnl_unrealized;
    state.pnl_peak = cp["pnl_peak"].as_f64().unwrap_or(0.0);
    state.pnl_trough = cp["pnl_trough"].as_f64().unwrap_or(0.0);
    state.lock_floor = cp["lock_floor"].as_f64();
    state.lock_armed = state.lock_floor.is_some();
    state.trail_to_entry_active = cp["trail_to_entry_active"].as_bool().unwrap_or(false);
    state.stopping = stopping;
    Ok(state)
}

/// Reopen a run finished on a zero-fill fact after a later, larger entry
/// fill: the stop it finished with becomes a pending stop again, and the
/// strategy is re-linked when nobody newer owns it.
pub fn reopen_run_for_late_entry_fill(store: &Store, run_id: i64) -> crate::error::Result<bool> {
    let Some(run) = store.get_run(run_id)? else {
        return Ok(false);
    };
    let Some(stopped) = run.stopped_at.clone() else {
        return Ok(run.stop_requested_reason.is_some());
    };
    let reason = run.stop_reason.clone().unwrap_or_else(|| "manual".into());
    let now = super::store::ts(store.now());
    let esc = |s: &str| s.replace('\'', "''");
    store.execute_raw(&format!(
        "UPDATE sm_strategy_run SET stopped_at = NULL, stop_reason = NULL, \
         stop_requested_at = '{}', stop_requested_reason = '{}' \
         WHERE id = {} AND stopped_at = '{}'",
        esc(&now),
        esc(&reason),
        run_id,
        esc(&stopped)
    ))?;
    store.execute_raw(&format!(
        "UPDATE sm_strategy SET status = 'running', current_run_id = {} \
         WHERE id = {} AND current_run_id IS NULL AND status = 'stopped'",
        run_id, run.strategy_id
    ))?;
    Ok(store
        .get_run(run_id)?
        .map(|r| r.stopped_at.is_none() && r.stop_requested_reason.is_some())
        .unwrap_or(false))
}
