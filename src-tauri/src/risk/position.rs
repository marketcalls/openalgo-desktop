//! Per-position risk: stop loss, target and trailing stop (web
//! `services/risk/position.py`). One tick in, one decision out.
//!
//! The web module's reconciliation notes apply unchanged; the short form:
//!
//! 1. Two trail modes. CONTINUOUS keeps a fixed gap behind the best price
//!    seen (what is live today); STEPPED advances the configured stop by one
//!    step per completed trigger of favourable movement.
//! 2. The trail arms off the favourable PEAK, not the current price, so a
//!    state restored after a restart keeps the trail it had earned.
//! 3. No implicit stop at entry: `None` means no stop.
//! 4. Zero is not a price: a zero stop, target, entry or tick is absent.
//! 5. An unusable tick (missing, zero, negative, not finite) returns
//!    `evaluated = false` with the input carried through.
//! 6. A non-positive entry disables trailing and P&L; absolute stops and
//!    targets still work.
//! 7. A trail never places the stop beyond the best price actually traded.
//! 8. When the stop and the target are both hit on one tick, the stop wins.
//!
//! The trail runs before the stop test, so a stop that just ratcheted is the
//! one tested.

use super::models::{
    as_price, format_price, is_price, BreachReason, PositionDecision, PositionRisk, TrailMode,
};
use serde_json::Value;

/// Judge one position against one last price.
pub fn evaluate_position(risk: &PositionRisk, last_price: Option<f64>) -> PositionDecision {
    let Some(ltp) = last_price.filter(|p| is_price(Some(*p))) else {
        return PositionDecision {
            identifier: risk.identifier.clone(),
            evaluated: false,
            stop_price: risk.effective_stop(),
            target_price: as_price(risk.target_price),
            highest_price: risk.highest_price,
            lowest_price: risk.lowest_price,
            pnl: 0.0,
            breached: false,
            reason: None,
            detail: "tick ignored: last price is missing, zero, negative or not finite".into(),
            stop_moved: false,
            trail_armed: false,
        };
    };

    let long_side = risk.is_long();
    let entry = is_price(Some(risk.entry_price)).then_some(risk.entry_price);

    // Seed a missing extreme from entry, or from this tick when even entry is
    // unusable, so the extreme is never seeded at zero.
    let seed = entry.unwrap_or(ltp);
    let mut highest = risk.highest_price;
    let mut lowest = risk.lowest_price;
    if long_side {
        highest = Some(py_max(highest.unwrap_or(seed), ltp));
    } else {
        lowest = Some(py_min(lowest.unwrap_or(seed), ltp));
    }

    // Favourable excursion: how far the best price seen has run our way.
    let mut favourable = 0.0;
    if let Some(entry) = entry {
        let best = if long_side { highest } else { lowest };
        if let Some(best) = best {
            favourable = py_max(
                0.0,
                if long_side {
                    best - entry
                } else {
                    entry - best
                },
            );
        }
    }

    let mut stop = risk.effective_stop();
    let original_stop = stop;
    let mut trail_armed = false;

    let trigger = py_max(0.0, risk.trail_trigger);
    let can_trail = risk.trailing_enabled
        && risk.trail_step > 0.0
        && risk.trail_step.is_finite()
        && entry.is_some();
    // A trigger of zero means "as soon as it is in profit", so a strictly
    // positive excursion is always required.
    if can_trail && favourable > 0.0 && favourable >= trigger {
        trail_armed = true;
        if let Some(candidate) = trail_candidate(risk, favourable, highest, lowest, trigger) {
            let candidate = clamp_to_peak(candidate, long_side, highest, lowest);
            // Ratchet: a trail may only ever tighten.
            let tighter = match stop {
                None => true,
                Some(s) => {
                    if long_side {
                        candidate > s
                    } else {
                        candidate < s
                    }
                }
            };
            if tighter {
                stop = Some(candidate);
            }
        }
    }

    let stop_moved = stop != original_stop;
    let target = as_price(risk.target_price);

    let stop_hit = match stop {
        Some(s) => {
            if long_side {
                ltp <= s
            } else {
                ltp >= s
            }
        }
        None => false,
    };
    let target_hit = match target {
        Some(t) => {
            if long_side {
                ltp >= t
            } else {
                ltp <= t
            }
        }
        None => false,
    };

    let side_word = if long_side { "long" } else { "short" };
    let mut reason = None;
    let mut detail = String::new();
    if stop_hit {
        reason = Some(BreachReason::Stop);
        detail = format!(
            "stop loss hit: last price {} is at or {} the stop {} on a {} position",
            format_price(ltp),
            if long_side { "below" } else { "above" },
            format_price(stop.unwrap_or_default()),
            side_word
        );
    } else if target_hit {
        reason = Some(BreachReason::Target);
        detail = format!(
            "target hit: last price {} is at or {} the target {} on a {} position",
            format_price(ltp),
            if long_side { "above" } else { "below" },
            format_price(target.unwrap_or_default()),
            side_word
        );
    } else if stop_moved {
        if let Some(s) = stop {
            let previous = original_stop
                .map(format_price)
                .unwrap_or_else(|| "none".to_string());
            detail = format!(
                "trailing stop moved from {} to {} after {} of favourable movement",
                previous,
                format_price(s),
                format_price(favourable)
            );
        }
    }

    let mut pnl = 0.0;
    if let Some(entry) = entry {
        if risk.quantity != 0.0 {
            pnl = if long_side {
                (ltp - entry) * risk.quantity
            } else {
                (entry - ltp) * risk.quantity
            };
        }
    }

    PositionDecision {
        identifier: risk.identifier.clone(),
        evaluated: true,
        stop_price: stop,
        target_price: target,
        highest_price: highest,
        lowest_price: lowest,
        pnl,
        breached: reason.is_some(),
        reason,
        detail,
        stop_moved,
        trail_armed,
    }
}

/// Convenience wrapper for callers holding a loose dict and a loose price.
pub fn evaluate_position_state(state: &Value, last_price: &Value) -> PositionDecision {
    evaluate_position(
        &PositionRisk::from_state(state),
        super::models::value_to_f64(last_price),
    )
}

/// Plain-text reasons a configuration is unusable; empty when it is fine.
pub fn validate_position(risk: &PositionRisk, last_price: Option<f64>) -> Vec<String> {
    let mut problems = Vec::new();
    let long_side = risk.is_long();
    let entry = is_price(Some(risk.entry_price)).then_some(risk.entry_price);
    let reference = as_price(last_price).or(entry);

    if entry.is_none() {
        problems.push("entry price is missing or not a positive number".to_string());
    }

    if let (Some(stop), Some(reference)) = (risk.effective_stop(), reference) {
        if long_side && stop >= reference {
            problems.push(format!(
                "stop {} is at or above {} on a long position, which exits immediately",
                format_price(stop),
                format_price(reference)
            ));
        }
        if !long_side && stop <= reference {
            problems.push(format!(
                "stop {} is at or below {} on a short position, which exits immediately",
                format_price(stop),
                format_price(reference)
            ));
        }
    }

    if let (Some(target), Some(reference)) = (as_price(risk.target_price), reference) {
        if long_side && target <= reference {
            problems.push(format!(
                "target {} is at or below {} on a long position, which exits immediately",
                format_price(target),
                format_price(reference)
            ));
        }
        if !long_side && target >= reference {
            problems.push(format!(
                "target {} is at or above {} on a short position, which exits immediately",
                format_price(target),
                format_price(reference)
            ));
        }
    }

    if risk.trailing_enabled
        && risk.trail_step.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
    {
        problems.push("trailing is enabled but the trail step is not a positive number".into());
    }
    let stepped = risk.trailing_enabled && risk.trail_mode == TrailMode::Stepped;
    if stepped && risk.trail_trigger.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        problems.push("a stepped trail needs a positive trail trigger".into());
    }
    if stepped && risk.trail_step > risk.trail_trigger && risk.trail_trigger > 0.0 {
        problems.push(
            "a stepped trail with a step larger than its trigger gives back more than \
             it locks in; reduce the step or raise the trigger"
                .into(),
        );
    }
    problems
}

/// The level the trail wants, before the peak clamp and the ratchet.
fn trail_candidate(
    risk: &PositionRisk,
    favourable: f64,
    highest: Option<f64>,
    lowest: Option<f64>,
    trigger: f64,
) -> Option<f64> {
    if risk.trail_mode == TrailMode::Stepped {
        if trigger <= 0.0 {
            // Undefined: a stepped trail with no trigger has no step boundary.
            return None;
        }
        let anchor = as_price(risk.initial_stop_price).or_else(|| risk.effective_stop())?;
        let steps = (favourable / trigger).floor();
        if steps <= 0.0 {
            return None;
        }
        let advance = steps * risk.trail_step;
        return Some(if risk.is_long() {
            anchor + advance
        } else {
            anchor - advance
        });
    }
    if risk.is_long() {
        highest.map(|h| h - risk.trail_step)
    } else {
        lowest.map(|l| l + risk.trail_step)
    }
}

/// Never place a stop beyond the best price the position has actually seen.
fn clamp_to_peak(
    candidate: f64,
    long_side: bool,
    highest: Option<f64>,
    lowest: Option<f64>,
) -> f64 {
    match (long_side, highest, lowest) {
        (true, Some(h), _) => py_min(candidate, h),
        (false, _, Some(l)) => py_max(candidate, l),
        _ => candidate,
    }
}

/// Python `max(a, b)` (first argument wins a tie).
fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// Python `min(a, b)`.
fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}
