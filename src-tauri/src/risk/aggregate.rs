//! Aggregate risk across a set of positions: combined stop, combined target,
//! lock profit and trail to entry (web `services/risk/aggregate.py`).
//!
//! Layer order on every evaluation: mark to market summed from the
//! positions; lock profit (its floor sits above the combined stop, so it is
//! the tighter rule once active); then the combined stop and the combined
//! target. Trail-to-entry bypasses only the combined stop, never the target.
//! Amounts are gross mark to market; costs are not modelled.

use super::models::{
    as_price, format_price, is_price, AggregateDecision, AggregateRisk, BreachReason, PnLSummary,
    PositionPnL, PositionRisk, Side, StopMove, TrailToEntryDecision,
};
use std::collections::{HashMap, HashSet};

/// Mark to market for one open position. Zero when it cannot be marked.
pub fn position_pnl(side: Side, entry_price: f64, quantity: f64, last_price: Option<f64>) -> f64 {
    let Some(ltp) = last_price.filter(|p| is_price(Some(*p))) else {
        return 0.0;
    };
    if !is_price(Some(entry_price)) || quantity == 0.0 {
        return 0.0;
    }
    let magnitude = quantity.abs();
    match side {
        Side::Buy => (ltp - entry_price) * magnitude,
        Side::Sell => (entry_price - ltp) * magnitude,
    }
}

/// Sum realized and unrealized mark to market across a set of positions.
///
/// Open positions are marked from their own entry, quantity and last price,
/// never from a per-position figure written on an earlier pass. Realized is
/// counted whether or not the position is currently open: a position
/// re-entered after a round trip carries that round trip's result.
pub fn aggregate_pnl(positions: &[PositionPnL]) -> PnLSummary {
    let mut realized = 0.0;
    let mut unrealized = 0.0;
    let mut priced = 0;
    let mut unpriced = 0;
    for p in positions {
        realized += p.realized_pnl;
        if p.closed {
            continue;
        }
        if !is_price(p.last_price) || !is_price(Some(p.entry_price)) {
            unpriced += 1;
            continue;
        }
        priced += 1;
        unrealized += position_pnl(p.side, p.entry_price, p.quantity, p.last_price);
    }
    PnLSummary {
        realized,
        unrealized,
        total: realized + unrealized,
        priced,
        unpriced,
    }
}

/// Judge the whole set against its combined limits.
pub fn evaluate_aggregate(
    risk: &AggregateRisk,
    realized_pnl: f64,
    unrealized_pnl: f64,
) -> AggregateDecision {
    let total = realized_pnl + unrealized_pnl;
    let peak = if total > risk.peak_pnl {
        total
    } else {
        risk.peak_pnl
    };
    let trough = if total < risk.trough_pnl {
        total
    } else {
        risk.trough_pnl
    };

    let mut lock_armed = risk.lock_armed;
    let mut lock_floor = risk.lock_floor;
    let mut armed_now = false;
    let mut floor_raised = false;

    if let Some(at) = risk.lock_profit_at {
        if !lock_armed && total >= at {
            lock_armed = true;
            armed_now = true;
        }
        if lock_armed {
            // The max of the configured floor, the floor already held, and
            // (for a trailing lock) the peak less the give-back: a ratchet.
            let mut new_floor = risk.lock_profit_floor.unwrap_or(0.0);
            if let Some(f) = lock_floor {
                if f > new_floor {
                    new_floor = f;
                }
            }
            if let Some(step) = risk.lock_trail_step.filter(|s| *s > 0.0) {
                let candidate = peak - step;
                if candidate > new_floor {
                    new_floor = candidate;
                }
            }
            floor_raised = !armed_now && lock_floor.map(|f| new_floor > f).unwrap_or(false);
            lock_floor = Some(new_floor);
        }
    }

    let decide = |reason: Option<BreachReason>, detail: String| AggregateDecision {
        total_pnl: total,
        realized_pnl,
        unrealized_pnl,
        peak_pnl: peak,
        trough_pnl: trough,
        lock_armed,
        lock_floor,
        lock_armed_now: armed_now,
        lock_floor_raised: floor_raised,
        breached: reason.is_some(),
        reason,
        detail,
    };

    // Gated on the configuration still being present: a persisted
    // `lock_armed` from a removed configuration must not keep closing.
    if let (Some(at), true, Some(floor)) = (risk.lock_profit_at, lock_armed, lock_floor) {
        if total <= floor {
            if armed_now && floor > at {
                return decide(
                    Some(BreachReason::LockProfit),
                    format!(
                        "lock profit floor {} is above its activation threshold {}, so it \
                         triggered on the tick it became active; the floor must be below the threshold",
                        format_price(floor),
                        format_price(at)
                    ),
                );
            }
            return decide(
                Some(BreachReason::LockProfit),
                format!(
                    "lock profit triggered: mark to market {} fell to or below the locked floor {}",
                    format_price(total),
                    format_price(floor)
                ),
            );
        }
    }

    if !risk.stop_bypassed {
        if let Some(sl) = risk.combined_stoploss {
            let limit = sl.abs();
            if total <= -limit {
                return decide(
                    Some(BreachReason::CombinedStop),
                    format!(
                        "combined stop loss hit: mark to market {} fell to or below the limit {}",
                        format_price(total),
                        format_price(-limit)
                    ),
                );
            }
        }
    }

    if let Some(target) = risk.combined_target {
        if total >= target {
            return decide(
                Some(BreachReason::CombinedTarget),
                format!(
                    "combined target hit: mark to market {} reached the target {}",
                    format_price(total),
                    format_price(target)
                ),
            );
        }
    }

    let mut detail = String::new();
    if armed_now {
        detail = format!(
            "lock profit active at {}, floor set to {}",
            format_price(total),
            lock_floor
                .map(format_price)
                .unwrap_or_else(|| "none".into())
        );
    } else if floor_raised {
        if let Some(f) = lock_floor {
            detail = format!(
                "lock profit floor raised to {} on a peak of {}",
                format_price(f),
                format_price(peak)
            );
        }
    }
    decide(None, detail)
}

/// Move every remaining position's stop to its own entry price.
///
/// Nothing is mutated; the caller applies `moves`. A move is skipped when it
/// would not tighten the stop, and, when a last price is supplied for that
/// position, when entry is already on the wrong side of the market (moving
/// it there would be an instant market exit at a loss).
pub fn trail_stops_to_entry(
    positions: &[PositionRisk],
    exclude: &[String],
    last_prices: &HashMap<String, Option<f64>>,
) -> TrailToEntryDecision {
    let excluded: HashSet<&str> = exclude.iter().map(String::as_str).collect();
    let mut out = TrailToEntryDecision::default();

    for risk in positions {
        if excluded.contains(risk.identifier.as_str()) {
            continue;
        }
        if !is_price(Some(risk.entry_price)) {
            out.skipped_no_entry.push(risk.identifier.clone());
            continue;
        }
        let entry = risk.entry_price;
        let current = risk.effective_stop();
        let long_side = risk.is_long();

        if let Some(c) = current {
            let not_improving = if long_side { entry <= c } else { entry >= c };
            if not_improving {
                out.skipped_not_improving.push(risk.identifier.clone());
                continue;
            }
        }

        let reference = last_prices
            .get(&risk.identifier)
            .copied()
            .flatten()
            .and_then(|p| as_price(Some(p)));
        if let Some(r) = reference {
            let through = if long_side { entry >= r } else { entry <= r };
            if through {
                out.skipped_through_price.push(risk.identifier.clone());
                continue;
            }
        }

        out.moves.push(StopMove {
            identifier: risk.identifier.clone(),
            previous_stop: current,
            new_stop: entry,
        });
    }

    if !out.moves.is_empty() {
        out.detail = format!("moved {} stop(s) to entry", out.moves.len());
        if !out.skipped_through_price.is_empty() {
            out.detail.push_str(&format!(
                "; left {} alone because entry is already through the market",
                out.skipped_through_price.len()
            ));
        }
    } else if !out.skipped_through_price.is_empty()
        || !out.skipped_not_improving.is_empty()
        || !out.skipped_no_entry.is_empty()
    {
        out.detail = "no stop moved to entry".into();
    }
    out
}
