//! Bridge between a run's state and the shared risk core (web
//! `services/strategy_module/risk_adapter.py`). It owns translation only:
//!
//! ```text
//! leg state -> PositionRisk  -> evaluate_position  -> PositionDecision  -> leg state
//! run state -> AggregateRisk -> evaluate_aggregate -> AggregateDecision -> run state
//! ```
//!
//! The rules are in `crate::risk`. Pure: callers hold the run lock across a
//! call and nothing here touches I/O.

use super::state::{LegState, RunState};
use crate::risk::{
    aggregate_pnl, evaluate_aggregate, evaluate_position, stop_from_points, target_from_points,
    trail_stops_to_entry, AggregateDecision, AggregateRisk, PositionDecision, PositionPnL,
    PositionRisk, Side, TrailMode,
};
use serde_json::Value;
use std::collections::HashMap;

/// A leg's `B`/`S` as the core's side. Strict: an unusable value is an error,
/// never a default (the original read anything not `B` as a short).
pub fn side_of(position: &str) -> Result<Side, String> {
    match position.trim().to_ascii_uppercase().as_str() {
        "B" => Ok(Side::Buy),
        "S" => Ok(Side::Sell),
        _ => Err(format!("Unusable leg position: {:?}", position)),
    }
}

/// What one configured unit is worth in points for this leg: 1 for a points
/// leg, 1% of entry for a percent leg, and 0 for a percent leg with no
/// confirmed entry (a percentage of nothing is a stop that cannot exist yet).
fn points_per_unit(leg: &LegState, entry: f64) -> f64 {
    if !leg.risk_unit.eq_ignore_ascii_case("percent") {
        return 1.0;
    }
    if entry > 0.0 {
        entry / 100.0
    } else {
        0.0
    }
}

fn in_points(value: Option<f64>, scale: f64) -> Option<f64> {
    value.map(|v| v * scale).filter(|v| *v > 0.0)
}

/// One leg's state as the core's input. The configured stop is always the
/// stepped trail's anchor; the live effective levels win when present.
pub fn leg_to_position_risk(leg: &LegState) -> Result<PositionRisk, String> {
    let side = side_of(&leg.position)?;
    let entry = leg.entry_avg;
    let scale = points_per_unit(leg, entry);
    let sl_pts = in_points(leg.sl_pts, scale);
    let target_pts = in_points(leg.target_pts, scale);
    let initial_stop = sl_pts.and_then(|p| stop_from_points(side, entry, p));
    let configured_target = target_pts.and_then(|p| target_from_points(side, entry, p));
    let trail_x = in_points(Some(leg.trail_x), scale).unwrap_or(0.0);
    let trail_y = in_points(Some(leg.trail_y), scale).unwrap_or(0.0);
    Ok(PositionRisk {
        identifier: leg.leg_id.to_string(),
        side,
        entry_price: entry,
        quantity: leg.qty as f64,
        stop_price: leg.effective_sl.or(initial_stop),
        initial_stop_price: initial_stop,
        target_price: leg.effective_target.or(configured_target),
        trailing_enabled: trail_x > 0.0,
        trail_trigger: trail_x,
        // X alone is a fixed-distance trail: the gap is X. X with Y arms at X
        // and advances in Y-point steps.
        trail_step: if trail_y > 0.0 { trail_y } else { trail_x },
        trail_mode: if trail_y > 0.0 {
            TrailMode::Stepped
        } else {
            TrailMode::Continuous
        },
        highest_price: leg.highest_price,
        lowest_price: leg.lowest_price,
    })
}

/// Write an evaluation back onto the leg. Applied on every tick: the
/// extremes and the trailed stop are ratchets.
pub fn apply_leg_decision(leg: &mut LegState, d: &PositionDecision) {
    leg.effective_sl = d.stop_price;
    leg.effective_target = d.target_price;
    leg.highest_price = d.highest_price;
    leg.lowest_price = d.lowest_price;
    leg.mtm = d.pnl;
    if d.trail_armed {
        leg.trail_active = true;
    }
}

/// Evaluate one leg against a tick and write the outcome back.
pub fn evaluate_leg(leg: &mut LegState, last_price: f64) -> Result<PositionDecision, String> {
    let d = evaluate_position(&leg_to_position_risk(leg)?, Some(last_price));
    if d.evaluated {
        leg.ltp = Some(last_price);
    }
    apply_leg_decision(leg, &d);
    Ok(d)
}

/// A run's `(realized, unrealized)`, marked from each leg's own entry,
/// quantity and last price, never from a per-leg `mtm` written earlier.
/// Anything not open contributes its realized figure (a signal leg returns
/// to `configured` after an exit and must keep its round trip's result).
pub fn run_pnl(state: &RunState) -> Result<(f64, f64), String> {
    let mut positions = Vec::new();
    for leg in state.legs.values() {
        if leg.status != "open" && leg.realized_pnl == 0.0 {
            continue;
        }
        positions.push(PositionPnL {
            identifier: leg.leg_id.to_string(),
            side: side_of(&leg.position)?,
            entry_price: leg.entry_avg,
            quantity: leg.qty as f64,
            last_price: leg.ltp,
            closed: leg.status != "open",
            realized_pnl: leg.realized_pnl,
        });
    }
    let s = aggregate_pnl(&positions);
    Ok((s.realized, s.unrealized))
}

/// A run's aggregate limits and ratchets. `overall_sl_mtm` passes through
/// positive; the core applies it as a negative threshold.
pub fn run_to_aggregate_risk(state: &RunState, strategy: &Value) -> AggregateRisk {
    let lock = strategy.get("lock_profit").filter(|v| v.is_object());
    let f = |v: Option<&Value>| crate::risk::models::optional_float(v);
    let mode = lock.and_then(|l| l.get("mode")).and_then(Value::as_str);
    AggregateRisk {
        combined_stoploss: f(strategy.get("overall_sl_mtm")),
        combined_target: f(strategy.get("overall_target_mtm")),
        lock_profit_at: f(lock.and_then(|l| l.get("if_profit_reaches"))),
        lock_profit_floor: f(lock.and_then(|l| l.get("lock_profit"))),
        // A trail step means something only in the trailing variant.
        lock_trail_step: if mode == Some("lock_and_trail") {
            f(lock.and_then(|l| l.get("trail_step")))
        } else {
            None
        },
        lock_armed: state.lock_armed,
        lock_floor: state.lock_floor,
        peak_pnl: state.pnl_peak,
        trough_pnl: state.pnl_trough,
        stop_bypassed: state.trail_to_entry_active,
    }
}

/// Write an aggregate evaluation back. Peak and trough are written on EVERY
/// pass, breach or not (the original wrote them on one stop path only).
pub fn apply_run_decision(state: &mut RunState, d: &AggregateDecision) {
    state.pnl_realized = d.realized_pnl;
    state.pnl_unrealized = d.unrealized_pnl;
    state.pnl_total = d.total_pnl;
    state.pnl_peak = d.peak_pnl;
    state.pnl_trough = d.trough_pnl;
    state.lock_armed = d.lock_armed;
    state.lock_floor = d.lock_floor;
}

/// Evaluate a run's combined limits and write the outcome back.
pub fn evaluate_run(state: &mut RunState, strategy: &Value) -> Result<AggregateDecision, String> {
    let (realized, unrealized) = run_pnl(state)?;
    let d = evaluate_aggregate(
        &run_to_aggregate_risk(state, strategy),
        realized,
        unrealized,
    );
    apply_run_decision(state, &d);
    Ok(d)
}

/// Move every other open leg's stop to its own entry, and say which moved.
/// Only a stop-driven exit calls this; a manual close is an override.
pub fn trail_open_legs_to_entry(state: &mut RunState, triggering_leg_id: i64) -> Vec<String> {
    let open: Vec<LegState> = state.open_legs().into_iter().cloned().collect();
    let risks: Vec<PositionRisk> = open
        .iter()
        .filter_map(|l| leg_to_position_risk(l).ok())
        .collect();
    let prices: HashMap<String, Option<f64>> =
        open.iter().map(|l| (l.leg_id.to_string(), l.ltp)).collect();
    let d = trail_stops_to_entry(&risks, &[triggering_leg_id.to_string()], &prices);
    let mut moved = Vec::new();
    for m in d.moves {
        if let Ok(id) = m.identifier.parse::<i64>() {
            if let Some(leg) = state.leg_mut(id) {
                leg.effective_sl = Some(m.new_stop);
                moved.push(m.identifier.clone());
            }
        }
    }
    if !moved.is_empty() {
        state.trail_to_entry_active = true;
    }
    moved
}
