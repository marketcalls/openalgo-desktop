//! Recording fills against the exact position they belong to, and the
//! run's realized P&L (web `engine.py` `apply_fill`).

use super::{exit_fill_quantities, FillOpts, StrategyModule};
use crate::strategy::risk_adapter;
use crate::strategy::state::{LegState, RunState};
use futures_util::future::BoxFuture;

/// Whether an exit fill settles the leg's flip-outgoing (superseded)
/// position: it names that position, or that position's exit order, or
/// nothing while the live position has no exit in flight.
fn settles_superseded(leg: &LegState, opts: &FillOpts, is_entry: bool) -> bool {
    !is_entry
        && leg.superseded.as_ref().is_some_and(|sup| {
            (opts.position_ref.is_some() && sup.position_ref == opts.position_ref)
                || (opts.position_ref.is_none()
                    && opts.order_row_id.is_some()
                    && sup.exit_order_id == opts.order_row_id)
                || (opts.position_ref.is_none()
                    && opts.order_row_id.is_none()
                    && leg.exit_order_id.is_none())
        })
}

/// Settles the outgoing position from its own entry and size; the live
/// position is untouched. The position is cleared once its terminal exit
/// fully filled.
fn settle_superseded(leg: &mut LegState, opts: &FillOpts, price: Option<f64>) {
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
        let owns = opts.order_row_id.is_none() || sup.exit_order_id == opts.order_row_id;
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
}

/// Why a fill does not belong to the live position: another position, an
/// order this leg is not waiting on (an incarnation already replaced), or
/// an exit fill with no exit in flight. `None` when it belongs.
fn stale_fill(leg: &LegState, leg_id: i64, opts: &FillOpts, is_entry: bool) -> Option<String> {
    if opts.position_ref.is_some() && leg.position_ref != opts.position_ref {
        return Some(format!(
            "Ignoring a fill for position {:?} on leg {}: the live position is {:?}",
            opts.position_ref, leg_id, leg.position_ref
        ));
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
                return Some(format!(
                    "Ignoring a fill for order {} on leg {}: the leg is waiting on {}",
                    row, leg_id, exp
                ));
            }
        }
        if !is_entry
            && leg.exit_kind.is_none()
            && leg.exit_order_id.is_none()
            && !opts.allow_prior_order_correction
        {
            return Some(format!(
                "Ignoring exit fill for order {} on leg {}: it has no exit in flight",
                row, leg_id
            ));
        }
    }
    None
}

/// An entry fill: the entry price, the filled size managed, and the
/// entry's status. True when the entry order is complete.
fn apply_entry_fill(
    leg: &mut LegState,
    opts: &FillOpts,
    price: Option<f64>,
    (leg_id, run_id): (i64, i64),
    warnings: &mut Vec<String>,
) -> bool {
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
    opts.order_terminal
}

/// An exit fill: the realized P&L on the quantity it closed, the quantity
/// left (closed at zero), and the exit claim released once its own order
/// is terminal.
fn apply_exit_fill(
    leg: &mut LegState,
    opts: &FillOpts,
    price: Option<f64>,
    (leg_id, run_id): (i64, i64),
    warnings: &mut Vec<String>,
) {
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

/// The run's realized, unrealized and total P&L, with its peak and trough.
fn refresh_run_pnl(run: &mut RunState) {
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
}

/// What a fill did to the run's live state.
pub(crate) enum FillOutcome {
    NoState,
    Ignored,
    Applied {
        went_flat: bool,
        entry_applied: bool,
        strategy_id: i64,
    },
}

impl StrategyModule {
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
            let outcome = self.fill_state(run_id, leg_id, avg_price, is_entry, &opts);
            self.after_fill(run_id, is_entry, outcome).await
        })
    }

    /// The fill applied to the run's live state, at once and in memory
    /// only: what [`Self::after_fill`] then acts on.
    pub(crate) fn fill_state(
        &self,
        run_id: i64,
        leg_id: i64,
        avg_price: Option<f64>,
        is_entry: bool,
        opts: &FillOpts,
    ) -> FillOutcome {
        let mut warnings: Vec<String> = Vec::new();
        let price = avg_price.filter(|p| p.is_finite() && *p > 0.0);
        let outcome = self
            .state
            .with_run(run_id, |run| {
                let strategy_id = run.strategy_id;
                let Some(leg) = run.leg_mut(leg_id) else {
                    return FillOutcome::Ignored;
                };
                let mut entry_applied = false;
                if settles_superseded(leg, opts, is_entry) {
                    // Settle the outgoing position from its own entry and
                    // size; the live position is untouched.
                    settle_superseded(leg, opts, price);
                } else {
                    if let Some(w) = stale_fill(leg, leg_id, opts, is_entry) {
                        warnings.push(w);
                        return FillOutcome::Ignored;
                    }
                    if is_entry {
                        entry_applied =
                            apply_entry_fill(leg, opts, price, (leg_id, run_id), &mut warnings);
                    } else {
                        apply_exit_fill(leg, opts, price, (leg_id, run_id), &mut warnings);
                    }
                }
                // Recompute the run totals while the lock is held.
                refresh_run_pnl(run);
                FillOutcome::Applied {
                    went_flat: !run.requires_management(),
                    entry_applied,
                    strategy_id,
                }
            })
            .unwrap_or(FillOutcome::NoState);
        for w in warnings {
            tracing::warn!("{}", w);
        }
        outcome
    }

    /// What follows a fill applied by [`Self::fill_state`]: a late exit
    /// after the run was finalised reconciles its P&L from the rows, an
    /// entry may complete a pending stop, and a run left flat is finished.
    /// True when the run went flat.
    pub(crate) async fn after_fill(
        &self,
        run_id: i64,
        is_entry: bool,
        outcome: FillOutcome,
    ) -> bool {
        let (went_flat, entry_applied, strategy_id) = match outcome {
            FillOutcome::NoState => {
                // A late terminal update after another worker finalised:
                // reconcile from the durable rows instead.
                if !is_entry {
                    self.reconcile_run_pnl(run_id);
                }
                return false;
            }
            FillOutcome::Ignored => return false,
            FillOutcome::Applied {
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
        self.finish_if_done(run_id, strategy_id).await
    }

    /// A run that went flat: finished, unless it already was or it is a
    /// signal run with no stop requested (a signal run is a trading day: a
    /// leg exiting does not end it). True when the run is done.
    async fn finish_if_done(&self, run_id: i64, strategy_id: i64) -> bool {
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
            Vec<&crate::strategy::store::OrderRow>,
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
}
