//! The strategy engine: run lifecycle and the tick decision path (web
//! `services/strategy_module/engine.py`).
//!
//! ```text
//! start_run    resolve every leg, claim the strategy, place entries
//! stop_run     request exits and finalise after confirmed fills
//! close_leg    exit one leg; the run continues with the rest
//! process_tick evaluate risk against a price and dispatch what it decides
//! apply_fill   record a fill against the exact position it belongs to
//! ```
//!
//! Load-bearing orderings, kept from the web:
//!
//! * Locks are released before orders are placed: evaluate under the lock,
//!   collect, release, then dispatch.
//! * Entries go BUY before SELL (a short leg alone can be refused for margin).
//! * Every leg is resolved before anything is claimed.
//! * An exit uses the symbol the run holds, never a re-resolved one.
//! * The order row is written before the broker is called. An entry that
//!   cannot be recorded is not placed; an exit that cannot be recorded is
//!   placed anyway (getting flat wins).
//! * A leg whose entry was accepted but not filled is never exited.
//! * A rejected exit releases its claim; a stop whose exits were refused
//!   leaves the run open and managed.
//! * A leg is closed by its fill arriving, not by its exit being placed.
//! * Trail-to-entry fires only on a stop-driven exit, never a manual close.

use super::dispatch::{build_order, entry_action, exit_action, RunMode, EXIT_PRICETYPE};
use super::resolver::{resolve_leg, Failure};
use super::risk_adapter;
use super::state::{new_leg_state, new_position_ref, ClaimId, LegSpec, RunState};
use super::store::{EventFields, NewOrder, StrategyRow, ORDER_KINDS};
use super::StrategyModule;
use crate::risk::BreachReason;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};

/// What a start attempt produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StartResult {
    pub ok: bool,
    pub run_id: Option<i64>,
    pub error: Option<String>,
    /// Per-leg outcome, so a caller can say which leg failed and why.
    pub legs: Vec<Value>,
}

impl StartResult {
    fn fail(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }
}

/// A stop or close answer, in the web's dict shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StopOutcome {
    pub ok: bool,
    pub stop_pending: bool,
    pub error: Option<String>,
    pub exits: Vec<Value>,
    pub run_stopped: Option<bool>,
}

impl StopOutcome {
    fn err(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Default::default()
        }
    }

    fn pending(error: impl Into<String>, exits: Vec<Value>) -> Self {
        Self {
            ok: false,
            stop_pending: true,
            error: Some(error.into()),
            exits,
            run_stopped: None,
        }
    }

    pub fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("ok".into(), json!(self.ok));
        m.insert("stop_pending".into(), json!(self.stop_pending));
        m.insert("exits".into(), json!(self.exits));
        if let Some(e) = &self.error {
            m.insert("error".into(), json!(e));
        }
        if let Some(r) = self.run_stopped {
            m.insert("run_stopped".into(), json!(r));
        }
        Value::Object(m)
    }
}

/// How a fill names the order and position it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct FillOpts {
    /// The quantity this fill adds (`None`: the whole owner).
    pub filled_qty: Option<i64>,
    pub order_row_id: Option<i64>,
    pub position_ref: Option<String>,
    pub cumulative_filled_qty: Option<i64>,
    pub order_terminal: bool,
    pub allow_prior_order_correction: bool,
}

impl Default for FillOpts {
    fn default() -> Self {
        Self {
            filled_qty: None,
            order_row_id: None,
            position_ref: None,
            cumulative_filled_qty: None,
            order_terminal: true,
            allow_prior_order_correction: false,
        }
    }
}

const UNFILLED_EXIT: &str = "The entry for this leg has been accepted but not filled, so there is no confirmed quantity to exit. Retry once it fills.";
const STATE_UNAVAILABLE: &str = "The run remains open because its live state is unavailable";

/// Which order kind a risk breach records.
fn exit_kind_for(reason: BreachReason) -> Option<&'static str> {
    match reason {
        BreachReason::Stop => Some("exit_sl"),
        BreachReason::Target => Some("exit_target"),
        BreachReason::CombinedStop => Some("exit_overall_sl"),
        BreachReason::CombinedTarget => Some("exit_overall_target"),
        BreachReason::LockProfit => Some("exit_lock_profit"),
    }
}

fn stop_reason_for(reason: BreachReason) -> Option<&'static str> {
    match reason {
        BreachReason::CombinedStop => Some("overall_sl"),
        BreachReason::CombinedTarget => Some("overall_target"),
        BreachReason::LockProfit => Some("lock_profit"),
        _ => None,
    }
}

fn ev(run_id: Option<i64>, leg_id: Option<i64>, severity: &'static str) -> EventFields {
    EventFields {
        run_id,
        leg_id,
        severity: Some(severity),
        payload: None,
    }
}

/// `(applied, remaining)` whole quantities for one owner's exit fill.
fn exit_fill_quantities(filled_qty: Option<i64>, held: i64) -> (i64, i64) {
    let held = held.max(0);
    let applied = match filled_qty {
        None => held,
        Some(q) => q.max(0).min(held),
    };
    (applied, held - applied)
}

fn rejection_summary(placed: &[Value]) -> String {
    let mut reasons: Vec<(String, Vec<String>)> = Vec::new();
    for leg in placed {
        let reason = leg["error"].as_str().unwrap_or("").trim().to_string();
        if reason.is_empty() {
            continue;
        }
        let id = leg["leg_id"].to_string();
        match reasons.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, ids)) => ids.push(id),
            None => reasons.push((reason, vec![id])),
        }
    }
    match reasons.len() {
        0 => "Every entry order was rejected".into(),
        1 => format!("Every entry order was rejected: {}", reasons[0].0),
        _ => format!(
            "Every entry order was rejected. {}",
            reasons
                .iter()
                .map(|(r, ids)| format!("leg {}: {}", ids.join(", "), r))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn leg_id_of(leg: &Value, index: usize) -> i64 {
    leg.get("id")
        .or_else(|| leg.get("leg_id"))
        .and_then(Value::as_i64)
        .unwrap_or(index as i64 + 1)
}

impl StrategyModule {
    fn mode_of(&self, mode: &str) -> RunMode {
        RunMode::parse(mode).unwrap_or(RunMode::Sandbox)
    }

    // ------------------------------------------------------------ start

    /// Start a batch strategy, or explain why it did not start.
    pub async fn start_run(
        &self,
        strategy_id: i64,
        user_id: &str,
        mode: &str,
        trigger_source: &str,
        webhook_event_id: Option<i64>,
    ) -> StartResult {
        let strategy = match self.store.get_strategy(strategy_id, user_id) {
            Ok(Some(s)) => s,
            Ok(None) => return StartResult::fail("Strategy not found"),
            Err(e) => {
                tracing::error!("Could not read strategy {}: {}", strategy_id, e);
                return StartResult::fail("Strategy not found");
            }
        };
        let Some(run_mode) = RunMode::parse(mode) else {
            return StartResult::fail(format!("Unknown run mode: '{}'", mode));
        };
        if strategy.strategy_kind == "signal" {
            return StartResult::fail(
                "A signal strategy has no start. Its run opens on the first long_entry or short_entry signal after the session boundary.",
            );
        }
        // Live is opt-in per strategy, checked here as the last point before
        // real orders as well as at every caller.
        if run_mode == RunMode::Live && !strategy.live_enabled {
            return StartResult::fail(
                "This strategy is not enabled for live trading. Enable it first.",
            );
        }
        if let Err(e) = self.gateway.authorised(run_mode) {
            return StartResult::fail(e);
        }

        // Resolve everything before claiming anything.
        let (resolved, failures) = self.resolve_all_legs(&strategy).await;
        if !failures.is_empty() {
            let error = failures[0]["error"].as_str().unwrap_or("").to_string();
            return StartResult {
                ok: false,
                run_id: None,
                error: Some(error),
                legs: failures,
            };
        }

        // One conditional UPDATE, not a read then a write.
        match self.store.claim_strategy_for_run(strategy_id) {
            Ok(true) => {}
            Ok(false) => return StartResult::fail("This strategy is already running"),
            Err(e) => {
                tracing::error!("Could not claim strategy {}: {}", strategy_id, e);
                return StartResult::fail("This strategy is already running");
            }
        }

        let expiries: serde_json::Map<String, Value> = resolved
            .iter()
            .filter_map(|l| {
                l.expiry
                    .as_ref()
                    .map(|e| (l.leg_id.to_string(), Value::String(e.clone())))
            })
            .collect();
        let run_id = match self.store.create_run(
            strategy_id,
            mode,
            &self.gateway.broker_name(run_mode),
            trigger_source,
            webhook_event_id,
            Some(&Value::Object(expiries)),
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Could not open a run for strategy {}: {}", strategy_id, e);
                let _ = self.store.release_strategy(strategy_id);
                return StartResult::fail("Could not open a run");
            }
        };

        if !matches!(
            self.store
                .set_strategy_status(strategy_id, "running", Some(run_id)),
            Ok(true)
        ) {
            let cleaned = self
                .store
                .finish_unlinked_run_and_release_claim(run_id, strategy_id, "error")
                .unwrap_or(false);
            if !cleaned {
                tracing::error!(
                    "Run {} could not be linked to strategy {} and its empty claim could not be released",
                    run_id,
                    strategy_id
                );
            }
            return StartResult {
                ok: false,
                run_id: if cleaned { None } else { Some(run_id) },
                error: Some(
                    "Could not link the new run to its strategy; no order was placed".into(),
                ),
                legs: vec![],
            };
        }

        let mut legs = Vec::with_capacity(resolved.len());
        for spec in &resolved {
            match new_leg_state(spec) {
                Ok(l) => legs.push(l),
                Err(e) => {
                    tracing::error!("Run {} leg refused: {}", run_id, e);
                    self.finalise(
                        run_id,
                        strategy_id,
                        user_id,
                        "error",
                        "Start failed before any entry dispatch",
                    )
                    .await;
                    return StartResult {
                        ok: false,
                        run_id: Some(run_id),
                        error: Some("Could not start the strategy".into()),
                        legs: vec![],
                    };
                }
            }
        }
        self.state.install(RunState::new(run_id, strategy_id, legs));
        // Prices before entries: a fill can be reported within milliseconds.
        let symbols: Vec<(String, String)> = resolved
            .iter()
            .map(|l| (l.symbol.clone(), l.exchange.clone()))
            .collect();
        self.feed.add_run(run_id, &symbols).await;
        self.emit(
            strategy_id,
            user_id,
            "run_started",
            &format!("Run started in {} mode ({})", mode, trigger_source),
            ev(Some(run_id), None, "info"),
        )
        .await;
        if let Ok(Some(run)) = self.store.get_run(run_id) {
            self.broadcast
                .push_run_update(strategy_id, run.to_dict())
                .await;
        }

        let placed = self
            .place_entries(run_id, &strategy, &resolved, run_mode, user_id)
            .await;

        if !placed.iter().any(|l| l["ok"] == json!(true)) {
            let finalised = self
                .finalise(
                    run_id,
                    strategy_id,
                    user_id,
                    "error",
                    "No entry order was accepted",
                )
                .await;
            let error = if finalised {
                rejection_summary(&placed)
            } else {
                "Every entry order was rejected, but the flat run could not be finalised; retry stop"
                    .into()
            };
            return StartResult {
                ok: false,
                run_id: Some(run_id),
                error: Some(error),
                legs: placed,
            };
        }
        StartResult {
            ok: true,
            run_id: Some(run_id),
            error: None,
            legs: placed,
        }
    }

    /// Resolve every leg to a contract, pricing the underlying once.
    async fn resolve_all_legs(&self, strategy: &StrategyRow) -> (Vec<LegSpec>, Vec<Value>) {
        let legs = strategy.legs.as_array().cloned().unwrap_or_default();
        if legs.is_empty() {
            return (
                vec![],
                vec![json!({"leg_id": null, "ok": false, "error": "The strategy has no legs"})],
            );
        }
        let g = self.symbols.snapshot();
        let today = self
            .clock
            .now()
            .with_timezone(&chrono_tz::Asia::Kolkata)
            .date_naive();
        let mut shared_ltp: Option<f64> = None;
        let mut resolved = Vec::new();
        let mut failures = Vec::new();
        for (index, leg) in legs.iter().enumerate() {
            let leg_id = leg_id_of(leg, index);
            let position = leg["position"].as_str().unwrap_or("").to_ascii_uppercase();
            match resolve_leg(
                &g,
                self.gateway.as_ref(),
                leg,
                &strategy.underlying,
                &strategy.underlying_exchange,
                today,
                shared_ltp,
            )
            .await
            {
                Err(Failure { code, error }) => failures.push(json!({
                    "leg_id": leg_id,
                    "ok": false,
                    "error": format!("Leg {}: {}", leg_id, error),
                    "code": code,
                })),
                Ok(r) => {
                    if shared_ltp.is_none() {
                        shared_ltp = r.underlying_ltp;
                    }
                    let trail = &leg["trail"];
                    resolved.push(LegSpec {
                        leg_id,
                        position,
                        symbol: r.symbol,
                        exchange: r.exchange,
                        lots: r.lots,
                        quantity: r.quantity,
                        position_ref: Some(new_position_ref()),
                        sl_pts: leg["sl_pts"].as_f64(),
                        target_pts: leg["target_pts"].as_f64(),
                        trail_x: trail["x"].as_f64().unwrap_or(0.0),
                        trail_y: trail["y"].as_f64().unwrap_or(0.0),
                        risk_unit: leg["risk_unit"].as_str().unwrap_or("points").to_string(),
                        expiry: r.expiry,
                        expiry_fallback: r.expiry_fallback,
                        expiry_rank: r.expiry_rank,
                    });
                }
            }
        }
        (resolved, failures)
    }

    /// Place every leg's entry, longs first.
    async fn place_entries(
        &self,
        run_id: i64,
        strategy: &StrategyRow,
        resolved: &[LegSpec],
        mode: RunMode,
        user_id: &str,
    ) -> Vec<Value> {
        let mut ordered: Vec<&LegSpec> = resolved.iter().collect();
        ordered.sort_by_key(|l| if l.position == "B" { 0 } else { 1 });
        let mut outcomes = Vec::new();
        for leg in ordered {
            if leg.expiry_fallback {
                self.emit(
                    strategy.id,
                    user_id,
                    "leg_expiry_fallback",
                    &format!(
                        "Leg {} asked for the {} expiry; the chain lists only {}, which was used",
                        leg.leg_id,
                        leg.expiry_rank.clone().unwrap_or_default(),
                        leg.expiry.clone().unwrap_or_default()
                    ),
                    ev(Some(run_id), Some(leg.leg_id), "warn"),
                )
                .await;
            }
            let action = entry_action(&leg.position);
            let order = build_order(
                &leg.symbol,
                &leg.exchange,
                action,
                leg.quantity,
                &strategy.product,
                &strategy.name,
                &strategy.pricetype,
            );
            // Durable BEFORE the broker is called.
            let row = self.store.record_order(
                run_id,
                leg.leg_id,
                "entry",
                &NewOrder {
                    symbol: leg.symbol.clone(),
                    exchange: leg.exchange.clone(),
                    action: action.into(),
                    qty: leg.quantity,
                    product: Some(order.product.clone()),
                    pricetype: strategy.pricetype.clone(),
                    status: "pending".into(),
                    position_ref: leg.position_ref.clone(),
                    broker_order_id: None,
                },
            );
            let row_id = match row {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(
                        "Entry row for run {} leg {} not written: {}",
                        run_id,
                        leg.leg_id,
                        e
                    );
                    // An entry that cannot be recorded is not placed.
                    self.state
                        .reject_entry_intent(run_id, leg.leg_id, leg.position_ref.as_deref());
                    self.emit(
                        strategy.id,
                        user_id,
                        "leg_entry_rejected",
                        &format!(
                            "Entry for leg {} not placed: its order row could not be written",
                            leg.leg_id
                        ),
                        ev(Some(run_id), Some(leg.leg_id), "critical"),
                    )
                    .await;
                    outcomes.push(json!({
                        "leg_id": leg.leg_id, "ok": false, "symbol": leg.symbol,
                        "broker_order_id": null,
                        "error": "Could not record the order before placing it",
                    }));
                    continue;
                }
            };

            let result = self.gateway.place(mode, &order).await;
            let acknowledged = self
                .record_acknowledgement(row_id, &result, strategy.id, user_id, run_id, leg.leg_id)
                .await;

            self.state.with_run(run_id, |run| {
                if let Some(l) = run.leg_mut(leg.leg_id) {
                    if l.position_ref == leg.position_ref && l.entry_status == "pending" {
                        l.entry_order_id = Some(row_id);
                        let s = if result.ok { "open" } else { "rejected" };
                        l.entry_status = s.into();
                        l.status = s.into();
                    }
                }
            });

            if result.ok {
                // After the leg's bookkeeping: a sandbox fill published inside
                // the dispatch was held and is applied now.
                self.replay_for(result.broker_order_id.as_deref()).await;
            }

            self.emit(
                strategy.id,
                user_id,
                if result.ok {
                    "leg_entry_placed"
                } else {
                    "leg_entry_rejected"
                },
                &if result.ok {
                    format!("Entry {} {} {} placed", action, leg.quantity, leg.symbol)
                } else {
                    format!(
                        "Entry rejected on leg {}: {}",
                        leg.leg_id,
                        result.error.clone().unwrap_or_default()
                    )
                },
                ev(
                    Some(run_id),
                    Some(leg.leg_id),
                    if result.ok { "info" } else { "warn" },
                ),
            )
            .await;

            outcomes.push(json!({
                "leg_id": leg.leg_id,
                "ok": result.ok,
                "symbol": leg.symbol,
                "broker_order_id": result.broker_order_id,
                "error": result.error,
                "acknowledged": acknowledged,
            }));
        }
        outcomes
    }

    /// Write what the broker answered onto the order row, and say whether it
    /// stuck. Retried once; a failure leaves an `order_ack_unrecorded`
    /// critical event carrying the exact row, run, leg and broker facts, from
    /// which the row is repaired.
    pub(crate) async fn record_acknowledgement(
        &self,
        row_id: i64,
        result: &super::dispatch::DispatchResult,
        strategy_id: i64,
        user_id: &str,
        run_id: i64,
        leg_id: i64,
    ) -> bool {
        let status = if result.ok { "open" } else { "rejected" };
        let reason = if result.ok {
            None
        } else {
            result.error.as_deref()
        };
        let write = || {
            self.store
                .update_order(
                    row_id,
                    Some(status),
                    result.broker_order_id.as_deref(),
                    reason,
                )
                .unwrap_or(false)
        };
        if write() || write() {
            return true;
        }
        tracing::error!(
            "Could not record the broker acknowledgement for strategy order row {}",
            row_id
        );
        let message = if result.ok {
            format!(
                "Broker order {} was accepted for leg {}, but order row {} remains pending. The order details are kept for automatic reconciliation; the position remains managed until the broker confirms it.",
                result.broker_order_id.clone().unwrap_or_else(|| "(no broker id)".into()),
                leg_id,
                row_id
            )
        } else {
            format!(
                "The broker rejected the order for leg {}, but the rejection could not be written to order row {}. The details are kept for automatic reconciliation; the rejected order created no position.",
                leg_id, row_id
            )
        };
        self.emit(
            strategy_id,
            user_id,
            "order_ack_unrecorded",
            &message,
            EventFields {
                run_id: Some(run_id),
                leg_id: Some(leg_id),
                severity: Some("critical"),
                payload: Some(json!({
                    "version": 1,
                    "order_id": row_id,
                    "run_id": run_id,
                    "leg_id": leg_id,
                    "broker_order_id": result.broker_order_id,
                    "accepted": result.ok,
                    "status": status,
                    "reject_reason": reason,
                })),
            },
        )
        .await;
        let _ = self.reconcile_acks(run_id, false).await;
        false
    }

    /// Repair every pending row named by an `order_ack_unrecorded` witness.
    /// Returns how many accepted acknowledgements still cannot be linked.
    /// Boxed: a replayed fill can reach recovery, which reconciles again.
    pub fn reconcile_acks(&self, run_id: i64, replay: bool) -> BoxFuture<'_, usize> {
        Box::pin(self.reconcile_acks_inner(run_id, replay))
    }

    async fn reconcile_acks_inner(&self, run_id: i64, replay: bool) -> usize {
        let events = match self.store.list_order_ack_events(run_id) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(
                    "Could not read acknowledgement witnesses for run {}: {}",
                    run_id,
                    e
                );
                return 1;
            }
        };
        let mut unresolved = 0;
        for event in events {
            let p = &event.payload;
            let (Some(order_id), Some(leg_id)) = (p["order_id"].as_i64(), p["leg_id"].as_i64())
            else {
                continue;
            };
            let accepted = p["accepted"].as_bool().unwrap_or(false);
            let outcome = self
                .store
                .bind_order_acknowledgement(
                    order_id,
                    run_id,
                    leg_id,
                    p["broker_order_id"].as_str(),
                    p["status"].as_str().unwrap_or(""),
                    p["reject_reason"].as_str(),
                )
                .unwrap_or("conflict");
            match outcome {
                "repaired" | "already_bound" => {
                    if replay && accepted {
                        self.replay_for(p["broker_order_id"].as_str()).await;
                    }
                }
                _ if accepted => unresolved += 1,
                _ => {}
            }
        }
        unresolved
    }

    // ------------------------------------------------------------ fills

    /// Record a fill against the exact position it belongs to. Returns
    /// whether the run went flat.
    pub fn apply_fill<'a>(
        &'a self,
        run_id: i64,
        leg_id: i64,
        avg_price: Option<f64>,
        is_entry: bool,
        opts: FillOpts,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.apply_fill_inner(run_id, leg_id, avg_price, is_entry, opts)
                .await
        })
    }

    async fn apply_fill_inner(
        &self,
        run_id: i64,
        leg_id: i64,
        avg_price: Option<f64>,
        is_entry: bool,
        opts: FillOpts,
    ) -> bool {
        let mut warnings: Vec<String> = Vec::new();
        enum Outcome {
            NoState,
            Ignored,
            Applied {
                went_flat: bool,
                entry_applied: bool,
                strategy_id: i64,
            },
        }
        let price = avg_price.filter(|p| p.is_finite() && *p > 0.0);
        let outcome = self
            .state
            .with_run(run_id, |run| {
                let strategy_id = run.strategy_id;
                let Some(leg) = run.leg_mut(leg_id) else {
                    return Outcome::Ignored;
                };
                let mut entry_applied = false;
                let settles_superseded = !is_entry
                    && leg.superseded.as_ref().is_some_and(|sup| {
                        (opts.position_ref.is_some() && sup.position_ref == opts.position_ref)
                            || (opts.position_ref.is_none()
                                && opts.order_row_id.is_some()
                                && sup.exit_order_id == opts.order_row_id)
                            || (opts.position_ref.is_none()
                                && opts.order_row_id.is_none()
                                && leg.exit_order_id.is_none())
                    });
                if settles_superseded {
                    // Settle the outgoing position from its own entry and
                    // size; the live position is untouched.
                    let mut realized_add = 0.0;
                    let mut clear = false;
                    if let Some(sup) = leg.superseded.as_mut() {
                        let entry = sup.entry_avg;
                        let (applied, remaining) = exit_fill_quantities(opts.filled_qty, sup.qty);
                        let sign = if sup.position == "B" { 1.0 } else { -1.0 };
                        if entry > 0.0 {
                            if let Some(p) = price {
                                realized_add = (p - entry) * applied as f64 * sign;
                            }
                        }
                        let owns =
                            opts.order_row_id.is_none() || sup.exit_order_id == opts.order_row_id;
                        let release = opts.order_terminal && owns;
                        if remaining > 0 || !release {
                            sup.qty = remaining;
                            if release {
                                sup.exit_order_id = None;
                                sup.exit_claim_token = None;
                                sup.exit_kind = None;
                            }
                        } else {
                            clear = true;
                        }
                    }
                    if clear {
                        leg.superseded = None;
                    }
                    leg.realized_pnl += realized_add;
                } else {
                    if opts.position_ref.is_some() && leg.position_ref != opts.position_ref {
                        warnings.push(format!(
                            "Ignoring a fill for position {:?} on leg {}: the live position is {:?}",
                            opts.position_ref, leg_id, leg.position_ref
                        ));
                        return Outcome::Ignored;
                    }
                    // A fill naming an order this leg is not waiting on
                    // belongs to an incarnation already replaced.
                    if let Some(row) = opts.order_row_id {
                        let expected = if is_entry {
                            leg.entry_order_id
                        } else {
                            leg.exit_order_id
                        };
                        if let Some(exp) = expected {
                            if exp != row && !opts.allow_prior_order_correction {
                                warnings.push(format!(
                                    "Ignoring a fill for order {} on leg {}: the leg is waiting on {}",
                                    row, leg_id, exp
                                ));
                                return Outcome::Ignored;
                            }
                        }
                        if !is_entry
                            && leg.exit_kind.is_none()
                            && leg.exit_order_id.is_none()
                            && !opts.allow_prior_order_correction
                        {
                            warnings.push(format!(
                                "Ignoring exit fill for order {} on leg {}: it has no exit in flight",
                                row, leg_id
                            ));
                            return Outcome::Ignored;
                        }
                    }
                    if is_entry {
                        match price {
                            Some(p) => leg.entry_avg = p,
                            None => warnings.push(format!(
                                "Leg {} on run {} filled without a usable average price; managing its quantity with valuation unavailable",
                                leg_id, run_id
                            )),
                        }
                        let managed = opts.cumulative_filled_qty.or(opts.filled_qty);
                        if let Some(q) = managed {
                            if q != leg.qty {
                                warnings.push(format!(
                                    "Leg {} on run {} filled {} of {}; managing the filled size",
                                    leg_id, run_id, q, leg.qty
                                ));
                                leg.qty = q;
                            }
                        }
                        if let Some(c) = opts.cumulative_filled_qty {
                            leg.entry_filled_qty = c;
                        }
                        leg.entry_status = if opts.order_terminal {
                            "complete".into()
                        } else {
                            "open".into()
                        };
                        leg.status = "open".into();
                        entry_applied = opts.order_terminal;
                    } else {
                        if let Some(p) = price {
                            leg.exit_avg = Some(p);
                        }
                        let entry = leg.entry_avg;
                        let (applied, remaining) = exit_fill_quantities(opts.filled_qty, leg.qty);
                        let sign = if leg.position == "B" { 1.0 } else { -1.0 };
                        if applied > 0 {
                            match price {
                                Some(p) if entry > 0.0 => {
                                    leg.realized_pnl += (p - entry) * applied as f64 * sign;
                                }
                                _ => warnings.push(format!(
                                    "Leg {} on run {} exited without complete fill pricing; booking no realized P&L for that quantity",
                                    leg_id, run_id
                                )),
                            }
                        }
                        let owns = opts.order_row_id.is_none() || leg.exit_order_id == opts.order_row_id;
                        let release = opts.order_terminal && owns;
                        if remaining > 0 {
                            leg.qty = remaining;
                            leg.status = "open".into();
                        } else {
                            leg.qty = 0;
                            leg.status = "closed".into();
                            leg.mtm = 0.0;
                        }
                        if release {
                            leg.exit_order_id = None;
                            leg.exit_claim_token = None;
                            leg.exit_kind = None;
                        }
                    }
                }
                // Recompute the run totals while the lock is held.
                if let Ok((realized, unrealized)) = risk_adapter::run_pnl(run) {
                    run.pnl_realized = realized;
                    run.pnl_unrealized = unrealized;
                    run.pnl_total = realized + unrealized;
                    if run.pnl_total > run.pnl_peak {
                        run.pnl_peak = run.pnl_total;
                    }
                    if run.pnl_total < run.pnl_trough {
                        run.pnl_trough = run.pnl_total;
                    }
                }
                Outcome::Applied {
                    went_flat: !run.requires_management(),
                    entry_applied,
                    strategy_id,
                }
            })
            .unwrap_or(Outcome::NoState);
        for w in warnings {
            tracing::warn!("{}", w);
        }

        let (went_flat, entry_applied, strategy_id) = match outcome {
            Outcome::NoState => {
                // A late terminal update after another worker finalised:
                // reconcile from the durable rows instead.
                if !is_entry {
                    self.reconcile_run_pnl(run_id);
                }
                return false;
            }
            Outcome::Ignored => return false,
            Outcome::Applied {
                went_flat,
                entry_applied,
                strategy_id,
            } => (went_flat, entry_applied, strategy_id),
        };

        if entry_applied {
            // The entry may have filled after a stop reported it unfilled.
            self.reconcile_pending_stop(run_id).await;
            return false;
        }
        if !went_flat {
            return false;
        }
        let Ok(Some(run_row)) = self.store.get_run(run_id) else {
            return true;
        };
        if run_row.stopped_at.is_some() {
            return true;
        }
        let strategy = self.store.get_strategy_unscoped(strategy_id).ok().flatten();
        let user_id = strategy
            .as_ref()
            .map(|s| s.user_id.clone())
            .unwrap_or_default();
        let kind = strategy.as_ref().map(|s| s.strategy_kind.clone());
        // A signal run is a trading day: a leg exiting does not end it.
        if run_row.stop_requested_reason.is_none() && kind.as_deref() == Some("signal") {
            return true;
        }
        let (reason, message) = match &run_row.stop_requested_reason {
            Some(r) => (r.clone(), format!("Run stopped ({})", r)),
            None => ("manual".to_string(), "All legs closed".to_string()),
        };
        self.finalise(run_id, strategy_id, &user_id, &reason, &message)
            .await;
        true
    }

    /// Recompute provable realized P&L from durable owner facts (FIFO per
    /// exact position reference). Leaves the row alone when ownership is
    /// ambiguous or no priced round trip exists.
    pub fn reconcile_run_pnl(&self, run_id: i64) -> Option<f64> {
        let orders = self.store.list_orders(run_id).ok()?;
        let mut owners: std::collections::BTreeMap<
            (i64, Option<String>),
            Vec<&super::store::OrderRow>,
        > = Default::default();
        for o in &orders {
            owners
                .entry((o.leg_id, o.position_ref.clone()))
                .or_default()
                .push(o);
        }
        let mut realized = 0.0;
        let mut settled = 0;
        for facts in owners.values() {
            let mut lots: Vec<(String, f64, i64)> = Vec::new();
            for o in facts {
                let status = o.status.to_ascii_lowercase();
                let qty = match status.as_str() {
                    "complete" => o.filled_qty.filter(|q| *q > 0).unwrap_or(o.qty),
                    "cancelled" | "rejected" => o.filled_qty.unwrap_or(0),
                    _ => 0,
                };
                if qty <= 0 {
                    continue;
                }
                let Some(price) = o.avg_fill_price.filter(|p| *p > 0.0) else {
                    lots.clear();
                    break;
                };
                if o.kind == "entry" {
                    lots.push((o.action.to_ascii_uppercase(), price, qty));
                    continue;
                }
                let mut left = qty;
                let mut matched = false;
                for lot in lots.iter_mut().filter(|l| l.2 > 0) {
                    let expected = if lot.0 == "BUY" { "SELL" } else { "BUY" };
                    if !o.action.eq_ignore_ascii_case(expected) {
                        return None;
                    }
                    let applied = left.min(lot.2);
                    let sign = if lot.0 == "BUY" { 1.0 } else { -1.0 };
                    realized += (price - lot.1) * applied as f64 * sign;
                    lot.2 -= applied;
                    left -= applied;
                    matched = true;
                    if left <= 0 {
                        break;
                    }
                }
                if matched {
                    settled += 1;
                }
            }
        }
        if settled == 0 {
            return None;
        }
        let realized = (realized * 100.0).round() / 100.0;
        let _ = self.store.execute_raw(&format!(
            "UPDATE sm_strategy_run SET pnl_realized = {} WHERE id = {}",
            realized, run_id
        ));
        Some(realized)
    }

    // ------------------------------------------------------------ exits

    /// Exit the named legs at market, from the symbol each leg HOLDS.
    pub(crate) async fn exit_legs(
        &self,
        run_id: i64,
        strategy: &StrategyRow,
        leg_ids: &[i64],
        kind: &str,
        mode: RunMode,
        user_id: &str,
    ) -> Vec<Value> {
        // Claimed and classified in ONE hold of the run lock.
        let (live, unfilled) = self.state.claim_legs_for_exit(run_id, leg_ids, kind);
        #[allow(clippy::large_enum_variant)] // short-lived, one per leg
        enum Target {
            Live(super::state::ExitClaim),
            Superseded(super::state::SupersededClaim),
        }
        let mut targets: Vec<Target> = live.into_iter().map(Target::Live).collect();
        for leg_id in leg_ids {
            let position = self
                .state
                .with_run(run_id, |r| {
                    r.leg(*leg_id)
                        .and_then(|l| l.superseded.as_ref().map(|s| s.position.clone()))
                })
                .flatten();
            if let Some(p) = position {
                if let Some(c) = self.state.claim_superseded_exit(run_id, *leg_id, &p) {
                    targets.push(Target::Superseded(c));
                }
            }
        }

        let mut outcomes = Vec::new();
        for target in targets {
            let (lid, position, symbol, exchange, quantity, position_ref, token, owner) =
                match &target {
                    Target::Live(c) => (
                        c.leg.leg_id,
                        c.leg.position.clone(),
                        c.leg.symbol.clone(),
                        c.leg.exchange.clone(),
                        c.leg.qty,
                        c.leg.position_ref.clone(),
                        c.claim_token.clone(),
                        "live",
                    ),
                    Target::Superseded(c) => (
                        c.leg_id,
                        c.position.clone(),
                        c.symbol.clone(),
                        c.exchange.clone(),
                        c.quantity,
                        c.position_ref.clone(),
                        c.claim_token.clone(),
                        "superseded",
                    ),
                };
            let action = match exit_action(&position) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Run {} leg {}: {}", run_id, lid, e);
                    outcomes.push(json!({"leg_id": lid, "ok": false, "error": e,
                        "position_ref": position_ref, "exit_owner": owner}));
                    continue;
                }
            };
            let order = build_order(
                &symbol,
                &exchange,
                action,
                quantity,
                &strategy.product,
                &strategy.name,
                EXIT_PRICETYPE,
            );
            let row = self.store.record_order(
                run_id,
                lid,
                kind,
                &NewOrder {
                    symbol: symbol.clone(),
                    exchange: exchange.clone(),
                    action: action.into(),
                    qty: quantity,
                    product: Some(order.product.clone()),
                    pricetype: EXIT_PRICETYPE.into(),
                    status: "pending".into(),
                    position_ref: position_ref.clone(),
                    broker_order_id: None,
                },
            );
            let row_id = match row {
                Ok(id) => Some(id),
                Err(e) => {
                    // Placed anyway: an exit that cannot be recorded still
                    // goes out, because getting flat wins.
                    tracing::error!("Exit row for run {} leg {} not written: {}", run_id, lid, e);
                    self.emit(
                        strategy.id,
                        user_id,
                        "leg_exit_placed",
                        &format!(
                            "Exit for leg {} is being placed without an order row: it could not be written",
                            lid
                        ),
                        ev(Some(run_id), Some(lid), "critical"),
                    )
                    .await;
                    None
                }
            };
            let mut claim = ClaimId::Token(token.clone());
            if let Some(row_id) = row_id {
                let bound = match &target {
                    Target::Superseded(_) => {
                        self.state.bind_superseded_exit(run_id, lid, &token, row_id)
                    }
                    Target::Live(_) => self.state.bind_live_exit(
                        run_id,
                        lid,
                        &token,
                        row_id,
                        position_ref.as_deref(),
                    ),
                };
                if !bound {
                    let reason = if owner == "superseded" {
                        "Outgoing position exit claim changed before dispatch"
                    } else {
                        "Live position exit claim changed before dispatch"
                    };
                    let _ = self
                        .store
                        .update_order(row_id, Some("rejected"), None, Some(reason));
                    if owner == "superseded" {
                        self.state.release_superseded_exit(run_id, lid, &claim);
                    } else {
                        self.state.release_leg_exit(run_id, lid, &claim);
                    }
                    self.emit(
                        strategy.id,
                        user_id,
                        "leg_exit_rejected",
                        &format!(
                            "{} exit claim changed on leg {} before dispatch",
                            if owner == "superseded" {
                                "Superseded"
                            } else {
                                "Live"
                            },
                            lid
                        ),
                        ev(Some(run_id), Some(lid), "critical"),
                    )
                    .await;
                    outcomes.push(json!({"leg_id": lid, "ok": false,
                        "position_ref": position_ref, "exit_owner": owner,
                        "error": format!("The {} position changed before its exit could be placed", owner)}));
                    continue;
                }
                claim = ClaimId::Row(row_id);
            }

            let result = self.gateway.place(mode, &order).await;
            if let Some(row_id) = row_id {
                self.record_acknowledgement(row_id, &result, strategy.id, user_id, run_id, lid)
                    .await;
            }
            if !result.ok {
                // Release the claim so a later attempt is not mistaken for a
                // duplicate and the leg skipped for the rest of the session.
                if owner == "superseded" {
                    if self.state.release_superseded_exit(run_id, lid, &claim) {
                        self.report_flip_outgoing_exit_rejected(
                            run_id,
                            lid,
                            "refused",
                            result.broker_order_id.as_deref(),
                        )
                        .await;
                    }
                } else {
                    self.state.release_leg_exit(run_id, lid, &claim);
                }
            }
            self.emit(
                strategy.id,
                user_id,
                if result.ok {
                    "leg_exit_placed"
                } else {
                    "leg_exit_rejected"
                },
                &if result.ok {
                    format!("Exit {} {} {} placed ({})", action, quantity, symbol, kind)
                } else {
                    format!(
                        "Exit rejected on leg {}: {}",
                        lid,
                        result.error.clone().unwrap_or_default()
                    )
                },
                ev(
                    Some(run_id),
                    Some(lid),
                    if result.ok { "info" } else { "critical" },
                ),
            )
            .await;
            if result.ok && row_id.is_some() {
                self.replay_for(result.broker_order_id.as_deref()).await;
            }
            outcomes.push(json!({
                "leg_id": lid,
                "ok": result.ok,
                "error": result.error,
                "position_ref": position_ref,
                "exit_owner": owner,
            }));
        }
        for leg in unfilled {
            outcomes.push(json!({
                "leg_id": leg.leg_id,
                "ok": false,
                "symbol": leg.symbol,
                "broker_order_id": null,
                "position_ref": leg.position_ref,
                "exit_owner": "live",
                "error": UNFILLED_EXIT,
            }));
        }
        outcomes
    }

    pub(crate) async fn report_flip_outgoing_exit_rejected(
        &self,
        run_id: i64,
        leg_id: i64,
        ended: &str,
        broker_order_id: Option<&str>,
    ) {
        let Ok(Some(run)) = self.store.get_run(run_id) else {
            return;
        };
        if run.stopped_at.is_some() {
            return;
        }
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
            return;
        };
        let order = broker_order_id
            .map(|b| format!(" order {}", b))
            .unwrap_or_default();
        self.emit(
            run.strategy_id,
            &strategy.user_id,
            "flip_outgoing_exit_rejected",
            &format!(
                "Outgoing exit{} for leg {} was {}. The old side is still held, remains managed, and can be closed again.",
                order, leg_id, ended
            ),
            ev(Some(run_id), Some(leg_id), "critical"),
        )
        .await;
    }

    /// Cancel each accepted working entry through the run's pipe, then fold
    /// one broker status fact. Cancellation is intent, never proof.
    async fn cancel_and_reconcile_working_entries(&self, run_id: i64, mode: RunMode) {
        let working = [
            "pending",
            "open",
            "working",
            "trigger pending",
            "trigger_pending",
        ];
        let rows = self.store.list_orders(run_id).unwrap_or_default();
        for row in rows.iter().filter(|r| {
            r.kind == "entry" && working.contains(&r.status.trim().to_ascii_lowercase().as_str())
        }) {
            let Some(bid) = row.broker_order_id.clone().filter(|b| !b.is_empty()) else {
                continue;
            };
            let c = self.gateway.cancel(mode, &bid).await;
            if !c.ok {
                tracing::warn!(
                    "Working entry {} on run {} could not be cancelled: {}",
                    bid,
                    run_id,
                    c.error.unwrap_or_default()
                );
            }
            let status = self.gateway.order_status(mode, &bid).await;
            if status.ok {
                self.apply_order_snapshot(&bid, &status.order).await;
            }
        }
    }

    /// Fold one broker fact for each accepted exit still awaiting a frame.
    async fn reconcile_working_exits(&self, run_id: i64, mode: RunMode) {
        let working = [
            "pending",
            "open",
            "working",
            "trigger pending",
            "trigger_pending",
        ];
        let rows = self.store.list_orders(run_id).unwrap_or_default();
        for row in rows.iter().filter(|r| {
            r.kind != "entry" && working.contains(&r.status.trim().to_ascii_lowercase().as_str())
        }) {
            let Some(bid) = row.broker_order_id.clone().filter(|b| !b.is_empty()) else {
                continue;
            };
            let status = self.gateway.order_status(mode, &bid).await;
            if status.ok {
                self.apply_order_snapshot(&bid, &status.order).await;
            }
        }
    }

    // ------------------------------------------------------------ stop

    /// Request a stop, exit every owned position, and finalise only once
    /// flat. Boxed: a synchronous fill inside it can reach it again.
    pub fn stop_run<'a>(
        &'a self,
        run_id: i64,
        user_id: &'a str,
        reason: &'a str,
    ) -> BoxFuture<'a, StopOutcome> {
        Box::pin(async move { self.stop_run_inner(run_id, user_id, reason).await })
    }

    async fn stop_run_inner(&self, run_id: i64, user_id: &str, reason: &str) -> StopOutcome {
        let Ok(Some(run_row)) = self.store.get_run(run_id) else {
            return StopOutcome::err("Run is not active");
        };
        if run_row.stopped_at.is_some() {
            return StopOutcome::err("Run is not active");
        }
        let strategy_id = run_row.strategy_id;
        let mode = self.mode_of(&run_row.mode);
        let Ok(Some(strategy)) = self.store.get_strategy(strategy_id, user_id) else {
            return StopOutcome::err("Strategy not found");
        };

        // Durable before every broker call.
        if !self.store.request_run_stop(run_id, reason).unwrap_or(false) {
            return StopOutcome {
                ok: false,
                stop_pending: false,
                error: Some(
                    "Could not save the stop request, so no exit order was placed. Try again."
                        .into(),
                ),
                exits: vec![],
                run_stopped: None,
            };
        }
        let reason = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.stop_requested_reason)
            .unwrap_or_else(|| reason.to_string());
        self.state.mark_stopping(run_id);
        self.emit(
            strategy_id,
            user_id,
            "run_stop_requested",
            &format!(
                "Stop requested ({}); exit orders are being attempted",
                reason
            ),
            ev(Some(run_id), None, "info"),
        )
        .await;

        if self.reconcile_acks(run_id, true).await > 0 {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                "Stop remains pending because one or more broker acknowledgements could not be linked to their order rows. Nothing was closed on a guess; reconciliation will retry.",
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(
                "Some broker acknowledgements could not be linked to their orders; the run remains managed",
                vec![],
            );
        }

        let Some(snapshot) = self.state.snapshot(run_id) else {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                "Stop remains pending because the run's live state is unavailable; nothing was assumed closed",
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(STATE_UNAVAILABLE, vec![]);
        };
        let still_held = snapshot.requires_management();

        if let Err(e) = self.gateway.authorised(mode) {
            if !still_held {
                return self
                    .finish_stop(run_id, strategy_id, user_id, &reason, vec![])
                    .await;
            }
            if self.claim_unactionable(run_id) {
                self.emit(
                    strategy_id,
                    user_id,
                    "run_stop_failed",
                    "Stop remains pending because there is no broker session. Positions remain open and managed; log in to your broker and the stop is retried.",
                    ev(Some(run_id), None, "critical"),
                )
                .await;
            }
            return StopOutcome::pending(format!("{} The run remains managed.", e), vec![]);
        }
        self.note_actionable_again(strategy_id, user_id, run_id)
            .await;

        // A working entry is possible future exposure, not a position to
        // reverse: cancel, then fold a broker fact.
        self.reconcile_working_exits(run_id, mode).await;
        self.cancel_and_reconcile_working_entries(run_id, mode)
            .await;

        if let Ok(Some(r)) = self.store.get_run(run_id) {
            if r.stopped_at.is_some() {
                return StopOutcome {
                    ok: true,
                    ..Default::default()
                };
            }
        }
        let Some(snapshot) = self.state.snapshot(run_id) else {
            return StopOutcome::pending(STATE_UNAVAILABLE, vec![]);
        };
        let managed = snapshot.managed_leg_ids();
        let kind = if reason == "manual" {
            "exit_close_all".to_string()
        } else {
            let k = format!("exit_{}", reason);
            if ORDER_KINDS.contains(&k.as_str()) {
                k
            } else {
                "exit_close_all".to_string()
            }
        };
        let exits = self
            .exit_legs(run_id, &strategy, &managed, &kind, mode, user_id)
            .await;

        // A synchronous sandbox fill may already have finished the run.
        if let Ok(Some(r)) = self.store.get_run(run_id) {
            if r.stopped_at.is_some() {
                return StopOutcome {
                    ok: true,
                    exits,
                    ..Default::default()
                };
            }
        }
        let Some(snapshot) = self.state.snapshot(run_id) else {
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                "Stop remains pending because the run's live state is unavailable; nothing was assumed closed",
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(STATE_UNAVAILABLE, exits);
        };
        let still_held = snapshot.requires_management();
        let refused = exits.iter().filter(|e| e["ok"] != json!(true)).count();
        if refused > 0 && still_held {
            // The positions are still at the broker: finalising would release
            // the strategy, drop the state and unsubscribe the prices.
            self.emit(
                strategy_id,
                user_id,
                "run_stop_failed",
                &format!(
                    "Stop refused for {} position(s); the run remains open, managed, and can be stopped again",
                    refused
                ),
                ev(Some(run_id), None, "critical"),
            )
            .await;
            return StopOutcome::pending(
                format!(
                    "{} of {} exit order(s) were refused. The run is still open and still managed; retry the stop.",
                    refused,
                    exits.len()
                ),
                exits,
            );
        }
        if still_held {
            return StopOutcome {
                ok: true,
                stop_pending: true,
                exits,
                ..Default::default()
            };
        }
        self.finish_stop(run_id, strategy_id, user_id, &reason, exits)
            .await
    }

    async fn finish_stop(
        &self,
        run_id: i64,
        strategy_id: i64,
        user_id: &str,
        reason: &str,
        exits: Vec<Value>,
    ) -> StopOutcome {
        let persisted = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .and_then(|r| r.stop_requested_reason)
            .unwrap_or_else(|| reason.to_string());
        if self
            .finalise(
                run_id,
                strategy_id,
                user_id,
                &persisted,
                &format!("Run stopped ({})", persisted),
            )
            .await
        {
            StopOutcome {
                ok: true,
                exits,
                ..Default::default()
            }
        } else {
            StopOutcome::pending(
                "The run is flat but its final stop could not be saved; retry the stop",
                exits,
            )
        }
    }

    /// Continue a durable stop (after an entry fill, or on the scheduler's
    /// periodic retry). `None` when no stop is pending.
    pub async fn reconcile_pending_stop(&self, run_id: i64) -> Option<StopOutcome> {
        let run = self.store.get_run(run_id).ok().flatten()?;
        if run.stopped_at.is_some() {
            return None;
        }
        let reason = run.stop_requested_reason.clone()?;
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(run.strategy_id) else {
            return Some(StopOutcome::pending(
                "The strategy owning this pending stop is unavailable",
                vec![],
            ));
        };
        Some(self.stop_run(run_id, &strategy.user_id, &reason).await)
    }

    /// Exit one leg; the run continues with the rest. Never triggers
    /// trail-to-entry: an operator's close is an override, not a signal.
    pub async fn close_leg(&self, run_id: i64, leg_id: i64, user_id: &str) -> StopOutcome {
        let Ok(Some(run_row)) = self.store.get_run(run_id) else {
            return StopOutcome::err("Run is not active");
        };
        if run_row.stopped_at.is_some() {
            return StopOutcome::err("Run is not active");
        }
        let strategy_id = run_row.strategy_id;
        let mode = self.mode_of(&run_row.mode);
        let Ok(Some(strategy)) = self.store.get_strategy(strategy_id, user_id) else {
            return StopOutcome::err("Strategy not found");
        };
        if let Err(e) = self.gateway.authorised(mode) {
            return StopOutcome::err(e);
        }
        let exits = self
            .exit_legs(
                run_id,
                &strategy,
                &[leg_id],
                "exit_leg_manual",
                mode,
                user_id,
            )
            .await;
        if exits.is_empty() {
            return StopOutcome::err("That leg is not open");
        }
        // Non-empty is not success: a refused exit is still a held position.
        if exits.iter().any(|e| e["ok"] != json!(true)) {
            let errors = exits
                .iter()
                .filter(|e| e["ok"] != json!(true))
                .map(|e| e["error"].as_str().unwrap_or("refused").to_string())
                .collect::<Vec<_>>()
                .join("; ");
            return StopOutcome {
                ok: false,
                stop_pending: false,
                error: Some(format!("Exit refused: {}", errors)),
                exits,
                run_stopped: None,
            };
        }
        self.emit(
            strategy_id,
            user_id,
            "leg_close_manual",
            &format!("Operator requested closure of leg {}", leg_id),
            ev(Some(run_id), Some(leg_id), "info"),
        )
        .await;
        let still_open = self
            .state
            .with_run(run_id, |r| !r.open_legs().is_empty())
            .unwrap_or(false);
        let mut stopped = false;
        if !still_open {
            stopped = self
                .finalise(run_id, strategy_id, user_id, "manual", "All legs closed")
                .await;
        }
        let run_closed = self
            .store
            .get_run(run_id)
            .ok()
            .flatten()
            .map(|r| r.stopped_at.is_some())
            .unwrap_or(false);
        StopOutcome {
            ok: true,
            exits,
            run_stopped: Some(stopped || run_closed),
            ..Default::default()
        }
    }

    /// Close the run row, release the strategy and drop the live state, once,
    /// and only when nothing is managed any more. Peak and trough are written
    /// on every path.
    pub async fn finalise(
        &self,
        run_id: i64,
        strategy_id: i64,
        user_id: &str,
        reason: &str,
        message: &str,
    ) -> bool {
        let figures = match self.state.with_run(run_id, |live| {
            if live.requires_management() {
                None
            } else {
                Some((live.pnl_realized, live.pnl_peak, live.pnl_trough))
            }
        }) {
            Some(Some(f)) => f,
            Some(None) => return false,
            None => (0.0, 0.0, 0.0),
        };
        let mut finished = self
            .store
            .finish_run_and_release_strategy(
                run_id,
                strategy_id,
                reason,
                figures.0,
                figures.1,
                figures.2,
            )
            .unwrap_or(false);
        if !finished {
            finished = self
                .store
                .finish_detached_run(run_id, strategy_id, reason, figures.0, figures.1, figures.2)
                .unwrap_or(false);
        }
        if !finished {
            return false;
        }
        self.emit(
            strategy_id,
            user_id,
            "run_stopped",
            message,
            ev(Some(run_id), None, "info"),
        )
        .await;
        // The final figures, forced past the throttle.
        self.broadcast
            .push_delta(self.state.snapshot(run_id), true)
            .await;
        if let Ok(Some(run)) = self.store.get_run(run_id) {
            self.broadcast
                .push_run_update(strategy_id, run.to_dict())
                .await;
        }
        self.broadcast
            .push_terminal(strategy_id, run_id, reason, figures.0)
            .await;
        self.release_unactionable(run_id);
        self.feed.remove_run(run_id).await;
        self.state.clear(run_id);
        // Every stop arms the webhook cooling-off window.
        self.webhook
            .note_run_stopped(strategy_id, std::time::Instant::now());
        true
    }

    async fn note_actionable_again(&self, strategy_id: i64, user_id: &str, run_id: i64) {
        if !self.release_unactionable(run_id) {
            return;
        }
        self.emit(
            strategy_id,
            user_id,
            "recovery_succeeded",
            "Broker session restored; this run can act on its risk rules again.",
            ev(Some(run_id), None, "warn"),
        )
        .await;
    }

    // ------------------------------------------------------------ ticks

    /// Evaluate every run holding this instrument against one price.
    pub async fn process_tick(&self, symbol: &str, exchange: &str, ltp: f64) {
        for run_id in self.state.active_run_ids() {
            let holds = self
                .state
                .with_run(run_id, |r| {
                    r.legs
                        .values()
                        .any(|l| l.status == "open" && l.symbol == symbol && l.exchange == exchange)
                })
                .unwrap_or(false);
            if holds {
                self.process_tick_for_run(run_id, symbol, exchange, ltp)
                    .await;
            }
        }
    }

    async fn process_tick_for_run(&self, run_id: i64, symbol: &str, exchange: &str, ltp: f64) {
        let Ok(Some(run_row)) = self.store.get_run(run_id) else {
            return;
        };
        if run_row.stopped_at.is_some() {
            return;
        }
        let strategy_id = run_row.strategy_id;
        let mode = self.mode_of(&run_row.mode);
        let Ok(Some(strategy)) = self.store.get_strategy_unscoped(strategy_id) else {
            return;
        };
        let strategy_dict = strategy.to_dict(false);
        let user_id = strategy.user_id.clone();

        // Read before the lock, never inside it.
        let limit = strategy
            .daily_loss_limit_inr
            .map(f64::abs)
            .filter(|l| *l > 0.0);
        let banked = match limit {
            Some(_) => {
                let since = super::session::session_started_at(
                    self.clock.now(),
                    self.session.0,
                    self.session.1,
                );
                Some(
                    self.store
                        .realized_pnl_since(strategy_id, since, Some(run_id))
                        .unwrap_or(0.0),
                )
            }
            None => None,
        };

        let mut leg_exits: Vec<(i64, &'static str)> = Vec::new();
        let mut stop_reason: Option<&'static str> = None;
        let mut events: Vec<(&'static str, String, EventFields)> = Vec::new();

        // In-memory arithmetic only.
        let evaluated = self.state.with_run(run_id, |run| {
            let ids: Vec<i64> = run
                .legs
                .values()
                .filter(|l| l.status == "open" && l.symbol == symbol && l.exchange == exchange)
                .map(|l| l.leg_id)
                .collect();
            for id in ids {
                let Some(leg) = run.leg_mut(id) else { continue };
                let d = match risk_adapter::evaluate_leg(leg, ltp) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("Run {} leg {} not evaluated: {}", run_id, id, e);
                        continue;
                    }
                };
                if d.trail_armed && d.stop_moved {
                    events.push((
                        "leg_trail_advanced",
                        format!(
                            "Trailing stop on leg {} moved to {}",
                            id,
                            d.stop_price.map(crate::risk::format_price).unwrap_or_default()
                        ),
                        ev(Some(run_id), Some(id), "info"),
                    ));
                }
                if let Some(reason) = d.reason {
                    if let Some(kind) = exit_kind_for(reason) {
                        leg_exits.push((id, kind));
                        events.push((
                            if reason == BreachReason::Stop {
                                "leg_sl_hit"
                            } else {
                                "leg_target_hit"
                            },
                            d.detail.clone(),
                            ev(Some(run_id), Some(id), "warn"),
                        ));
                    }
                }
            }
            if strategy.trail_sl_to_entry {
                if let Some((id, _)) = leg_exits.iter().find(|(_, k)| *k == "exit_sl") {
                    let moved = risk_adapter::trail_open_legs_to_entry(run, *id);
                    if !moved.is_empty() {
                        events.push((
                            "trail_to_entry_activated",
                            format!("Stop on leg {} moved {} other legs to entry", id, moved.len()),
                            ev(Some(run_id), None, "warn"),
                        ));
                    }
                }
            }
            let aggregate = match risk_adapter::evaluate_run(run, &strategy_dict) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Run {} not evaluated: {}", run_id, e);
                    return;
                }
            };
            if aggregate.lock_armed_now {
                events.push((
                    "lock_profit_armed",
                    format!(
                        "Lock profit now active with a floor of {}",
                        aggregate.lock_floor.map(crate::risk::format_price).unwrap_or_default()
                    ),
                    ev(Some(run_id), None, "info"),
                ));
            } else if aggregate.lock_floor_raised {
                events.push((
                    "lock_profit_floor_advanced",
                    format!(
                        "Lock profit floor advanced to {}",
                        aggregate.lock_floor.map(crate::risk::format_price).unwrap_or_default()
                    ),
                    ev(Some(run_id), None, "info"),
                ));
            }
            // The daily loss limit is a limit on the session, not the run.
            if let (Some(limit), Some(banked)) = (limit, banked) {
                let day = banked + run.pnl_total;
                if day <= -limit {
                    stop_reason = Some("daily_loss_limit");
                    events.push((
                        "overall_sl_hit",
                        format!(
                            "Daily loss limit reached: the session is down {:.2} against a limit of {:.2}",
                            day.abs(),
                            limit
                        ),
                        ev(Some(run_id), None, "critical"),
                    ));
                    return;
                }
            }
            if let Some(reason) = aggregate.reason.filter(|_| aggregate.breached) {
                if let Some(sr) = stop_reason_for(reason) {
                    stop_reason = Some(sr);
                    let threshold = match sr {
                        "overall_sl" => -strategy.overall_sl_mtm.unwrap_or(0.0).abs(),
                        "overall_target" => strategy.overall_target_mtm.unwrap_or(0.0),
                        _ => aggregate.lock_floor.unwrap_or(0.0),
                    };
                    let legs: Vec<Value> = run
                        .legs
                        .values()
                        .filter(|l| l.status == "open" || l.realized_pnl != 0.0)
                        .map(|l| {
                            json!({
                                "symbol": l.symbol, "exchange": l.exchange, "ltp": l.ltp,
                                "mtm": (l.mtm * 100.0).round() / 100.0,
                                "tick_source": l.tick_source, "qty": l.qty, "position": l.position,
                            })
                        })
                        .collect();
                    let kind = match sr {
                        "overall_sl" => "overall_sl_hit",
                        "overall_target" => "overall_target_hit",
                        _ => "lock_profit_triggered",
                    };
                    events.push((
                        kind,
                        aggregate.detail.clone(),
                        EventFields {
                            run_id: Some(run_id),
                            leg_id: None,
                            severity: Some("warn"),
                            payload: Some(json!({
                                "trigger_total": (aggregate.total_pnl * 100.0).round() / 100.0,
                                "reason": sr,
                                "threshold": (threshold * 100.0).round() / 100.0,
                                "triggering_tick": {"symbol": symbol, "exchange": exchange, "ltp": ltp},
                                "legs": legs,
                            })),
                        },
                    ));
                }
            }
        });
        if evaluated.is_none() {
            return;
        }

        // Lock released: everything below reaches the database or broker.
        self.broadcast
            .push_delta(self.state.snapshot(run_id), false)
            .await;
        for (kind, message, fields) in events {
            self.emit(strategy_id, &user_id, kind, &message, fields)
                .await;
        }
        if let Some(reason) = stop_reason {
            // A strategy-level breach closes everything.
            let r = self.stop_run(run_id, &user_id, reason).await;
            if r.ok {
                self.note_actionable_again(strategy_id, &user_id, run_id)
                    .await;
            }
            return;
        }
        if leg_exits.is_empty() {
            return;
        }
        if self.gateway.authorised(mode).is_err() {
            // The 3 AM window: record once per episode, never pretend to exit.
            if self.claim_unactionable(run_id) {
                tracing::warn!(
                    "Run {} has risk to act on but no broker session; positions left open",
                    run_id
                );
                self.emit(
                    strategy_id,
                    &user_id,
                    "leg_exit_rejected",
                    "Risk triggered but there is no broker session, so nothing could be exited. Positions are still open. Log in to your broker to restore the session.",
                    ev(Some(run_id), None, "critical"),
                )
                .await;
            }
            return;
        }
        self.note_actionable_again(strategy_id, &user_id, run_id)
            .await;
        for (leg_id, kind) in leg_exits {
            self.exit_legs(run_id, &strategy, &[leg_id], kind, mode, &user_id)
                .await;
        }
    }
}
