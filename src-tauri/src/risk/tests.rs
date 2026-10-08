//! Port of the web's `test/risk/test_risk_core.py`: the golden vectors
//! through both entry points, targeted unit tests, and property checks over
//! price walks.

use super::*;
use serde_json::{json, Value};
use std::collections::HashMap;

const VECTORS: &str = include_str!("../../../tests/fixtures/risk/vectors.json");

fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("vectors.json parses")
}

fn cases() -> Vec<Value> {
    vectors()["cases"].as_array().cloned().unwrap_or_default()
}

fn close(actual: Option<f64>, expected: &Value, tol: f64) -> bool {
    match (actual, expected) {
        (None, Value::Null) => true,
        (Some(a), e) => e.as_f64().map(|e| (a - e).abs() <= tol).unwrap_or(false),
        _ => false,
    }
}

/// One vector against the decision API. `Err` names the first mismatch.
pub(crate) fn check_vector(case: &Value, tol: f64) -> Result<(), String> {
    let name = case["name"].as_str().unwrap_or("?");
    let decision = evaluate_position(
        &PositionRisk::from_state(&case["state"]),
        value_to_f64(&case["ltp"]),
    );
    let expected = case["expected"].as_object().ok_or("expected missing")?;
    for (key, want) in expected {
        let ok = match key.as_str() {
            "reason" => decision.reason.map(|r| r.as_str()) == want.as_str(),
            "evaluated" => Some(decision.evaluated) == want.as_bool(),
            "breached" => Some(decision.breached) == want.as_bool(),
            "stop_moved" => Some(decision.stop_moved) == want.as_bool(),
            "trail_armed" => Some(decision.trail_armed) == want.as_bool(),
            "current_sl" => close(decision.stop_price, want, tol),
            "highest_price" => close(decision.highest_price, want, tol),
            "lowest_price" => close(decision.lowest_price, want, tol),
            "pnl" => close(Some(decision.pnl), want, tol),
            other => return Err(format!("{}: unknown expected key {}", name, other)),
        };
        if !ok {
            return Err(format!("{}: {} was {:?}", name, key, decision));
        }
    }
    Ok(())
}

/// One vector through the legacy dict adapter.
pub(crate) fn check_vector_legacy(case: &Value, tol: f64) -> Result<(), String> {
    let name = case["name"].as_str().unwrap_or("?");
    let result = evaluate_trail(&case["state"], &case["ltp"]);
    let keys: Vec<&String> = result
        .as_object()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    if keys.len() != 5 {
        return Err(format!("{}: adapter keys {:?}", name, keys));
    }
    let expected = &case["expected"];
    if result["breached"] != expected["breached"] {
        return Err(format!("{}: breached", name));
    }
    if result["reason"] != expected["reason"] {
        return Err(format!("{}: reason", name));
    }
    for key in ["current_sl", "highest_price", "lowest_price"] {
        if let Some(want) = expected.get(key) {
            let got = value_to_f64(&result[key]);
            if !close(got, want, tol) {
                return Err(format!("{}: {} was {}", name, key, result[key]));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ vectors

#[test]
fn every_golden_vector_passes_through_the_decision_api() {
    let v = vectors();
    let tol = v["tolerance"].as_f64().unwrap();
    let all = cases();
    assert_eq!(all.len(), 35, "vectors.json case count changed");
    for case in &all {
        check_vector(case, tol).unwrap();
    }
}

#[test]
fn every_golden_vector_passes_through_the_legacy_adapter() {
    let tol = vectors()["tolerance"].as_f64().unwrap();
    for case in &cases() {
        check_vector_legacy(case, tol).unwrap();
    }
}

#[test]
fn case_names_are_unique_and_complete() {
    let all = cases();
    let mut names: Vec<&str> = all.iter().map(|c| c["name"].as_str().unwrap()).collect();
    let n = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), n);
    for c in &all {
        assert!(c["description"]
            .as_str()
            .map(|d| !d.is_empty())
            .unwrap_or(false));
        assert!(c["state"].is_object());
        assert!(c.get("ltp").is_some());
        assert!(c["expected"].is_object());
    }
}

#[test]
fn reasons_are_wire_values() {
    for c in &cases() {
        let r = &c["expected"]["reason"];
        assert!(r.is_null() || r == "sl" || r == "target", "{}", c["name"]);
    }
}

#[test]
fn both_sides_and_both_trail_modes_are_covered() {
    let all = cases();
    let sides: std::collections::HashSet<String> = all
        .iter()
        .map(|c| {
            c["state"]["side"]
                .as_str()
                .unwrap_or("BUY")
                .to_ascii_uppercase()
        })
        .collect();
    assert!(sides.contains("BUY") && sides.contains("SELL"));
    let modes: std::collections::HashSet<String> = all
        .iter()
        .map(|c| {
            c["state"]["trail_mode"]
                .as_str()
                .unwrap_or("continuous")
                .to_string()
        })
        .collect();
    assert!(modes.contains("continuous") && modes.contains("stepped"));
}

#[test]
fn the_defect_cases_are_labelled() {
    let labelled = cases().iter().filter(|c| c.get("fixes").is_some()).count();
    assert!(labelled >= 6);
}

#[test]
fn every_breach_explains_itself() {
    for c in &cases() {
        let d = evaluate_position(
            &PositionRisk::from_state(&c["state"]),
            value_to_f64(&c["ltp"]),
        );
        if d.breached {
            assert!(!d.detail.is_empty());
            assert!(!d.detail.split_whitespace().any(|w| w == "sl"));
        }
    }
}

// ------------------------------------------------------------------ purity

#[test]
fn input_is_never_mutated() {
    let state = json!({
        "side": "BUY", "entry_price": 100.0, "quantity": 50, "current_sl": 90.0,
        "initial_sl": 90.0, "trailing_enabled": true, "trailing_step": 3.0,
        "highest_price": 100.0, "lowest_price": 100.0
    });
    let before = state.clone();
    evaluate_trail(&state, &json!(120.0));
    assert_eq!(state, before);
}

#[test]
fn the_package_imports_nothing_that_does_io() {
    // Asserted on the source rather than trusted to review: the core must be
    // callable from any thread, any handler and any test with nothing running.
    let sources = [
        ("mod.rs", include_str!("mod.rs")),
        ("models.rs", include_str!("models.rs")),
        ("position.rs", include_str!("position.rs")),
        ("aggregate.rs", include_str!("aggregate.rs")),
        ("adapters.rs", include_str!("adapters.rs")),
    ];
    let forbidden = [
        "rusqlite",
        "reqwest",
        "tokio",
        "tracing",
        "std::fs",
        "std::net",
        "std::time",
        "chrono",
        "crate::db",
        "crate::brokers",
        "crate::state",
        "crate::services",
        "socketioxide",
        "SystemTime",
        "Utc::now",
        "println",
    ];
    for (name, src) in sources {
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for token in forbidden {
            assert!(!code.contains(token), "{} uses {}", name, token);
        }
    }
}

// ------------------------------------------------------------------ conversions

#[test]
fn points_convert_to_prices_on_the_right_side() {
    assert_eq!(stop_from_points(Side::Buy, 100.0, 10.0), Some(90.0));
    assert_eq!(stop_from_points(Side::Sell, 100.0, 10.0), Some(110.0));
    assert_eq!(target_from_points(Side::Buy, 100.0, 20.0), Some(120.0));
    assert_eq!(target_from_points(Side::Sell, 100.0, 20.0), Some(80.0));
}

#[test]
fn a_stop_that_would_land_at_or_below_zero_is_no_stop() {
    assert_eq!(stop_from_points(Side::Buy, 100.0, 120.0), None);
    assert_eq!(target_from_points(Side::Sell, 100.0, 100.0), None);
}

#[test]
fn side_comes_from_the_sign_of_a_net_quantity() {
    assert_eq!(side_from_quantity(-75.0), Side::Sell);
    assert_eq!(side_from_quantity(75.0), Side::Buy);
    assert_eq!(side_from_quantity(0.0), Side::Buy);
}

#[test]
fn points_configuration_loads_through_from_state() {
    let r = PositionRisk::from_state(
        &json!({"side": "SELL", "entry_price": 100.0, "sl_points": 10.0, "target_points": 20.0}),
    );
    assert_eq!(r.effective_stop(), Some(110.0));
    assert_eq!(r.target_price, Some(80.0));
}

// ------------------------------------------------------------------ validation

fn risk(side: Side, entry: f64) -> PositionRisk {
    PositionRisk {
        side,
        entry_price: entry,
        ..Default::default()
    }
}

#[test]
fn a_stop_on_the_wrong_side_is_refused() {
    let p = validate_position(
        &PositionRisk {
            stop_price: Some(105.0),
            ..risk(Side::Buy, 100.0)
        },
        Some(100.0),
    );
    assert!(p.iter().any(|t| t.contains("exits immediately")));
}

#[test]
fn a_short_target_above_the_market_is_refused() {
    let p = validate_position(
        &PositionRisk {
            target_price: Some(110.0),
            ..risk(Side::Sell, 100.0)
        },
        Some(100.0),
    );
    assert!(p.iter().any(|t| t.contains("target")));
}

#[test]
fn trailing_without_a_step_is_refused() {
    let p = validate_position(
        &PositionRisk {
            stop_price: Some(90.0),
            trailing_enabled: true,
            ..risk(Side::Buy, 100.0)
        },
        Some(100.0),
    );
    assert!(p.iter().any(|t| t.contains("trail step")));
}

#[test]
fn a_stepped_trail_that_overruns_its_trigger_is_refused() {
    let p = validate_position(
        &PositionRisk {
            stop_price: Some(90.0),
            trailing_enabled: true,
            trail_mode: TrailMode::Stepped,
            trail_step: 10.0,
            trail_trigger: 1.0,
            ..risk(Side::Buy, 100.0)
        },
        Some(100.0),
    );
    assert!(p.iter().any(|t| t.contains("larger than its trigger")));
}

#[test]
fn a_sane_configuration_has_no_problems() {
    let p = validate_position(
        &PositionRisk {
            stop_price: Some(90.0),
            target_price: Some(120.0),
            trailing_enabled: true,
            trail_step: 3.0,
            ..risk(Side::Buy, 100.0)
        },
        Some(100.0),
    );
    assert!(p.is_empty(), "{:?}", p);
}

// ------------------------------------------------------------------ aggregate

fn open(id: &str, side: Side, entry: f64, qty: f64, ltp: f64) -> PositionPnL {
    PositionPnL {
        identifier: id.into(),
        side,
        entry_price: entry,
        quantity: qty,
        last_price: Some(ltp),
        ..Default::default()
    }
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn open_and_closed_positions_are_summed_separately() {
    let s = aggregate_pnl(&[
        open("a", Side::Buy, 100.0, 50.0, 110.0),
        open("b", Side::Sell, 200.0, 25.0, 190.0),
        PositionPnL {
            identifier: "c".into(),
            closed: true,
            realized_pnl: -125.0,
            ..Default::default()
        },
    ]);
    assert!(approx(s.unrealized, 750.0));
    assert!(approx(s.realized, -125.0));
    assert!(approx(s.total, 625.0));
    assert_eq!(s.priced, 2);
}

#[test]
fn an_unpriced_open_position_is_counted_not_guessed() {
    let s = aggregate_pnl(&[
        open("a", Side::Buy, 100.0, 50.0, 110.0),
        PositionPnL {
            identifier: "b".into(),
            entry_price: 100.0,
            quantity: 50.0,
            ..Default::default()
        },
    ]);
    assert_eq!(s.unpriced, 1);
    assert!(approx(s.total, 500.0));
}

#[test]
fn a_position_book_row_infers_its_side_from_a_signed_quantity() {
    let s = aggregate_pnl(&[PositionPnL::from_state(
        &json!({"symbol": "X", "quantity": -75, "average_price": 100.0, "ltp": 90.0}),
    )]);
    assert!(approx(s.total, 750.0));
}

#[test]
fn the_aggregate_is_derived_not_read_back() {
    let s = aggregate_pnl(&[PositionPnL::from_state(&json!({
        "symbol": "X", "side": "BUY", "quantity": 50, "entry_price": 100.0,
        "ltp": 110.0, "mtm": 999999.0
    }))]);
    assert!(approx(s.total, 500.0));
}

#[test]
fn position_pnl_matches_the_per_position_decision() {
    let r = PositionRisk {
        quantity: 75.0,
        stop_price: Some(110.0),
        ..risk(Side::Sell, 100.0)
    };
    let d = evaluate_position(&r, Some(90.0));
    assert!(approx(
        d.pnl,
        position_pnl(Side::Sell, 100.0, 75.0, Some(90.0))
    ));
}

fn agg(f: impl FnOnce(&mut AggregateRisk)) -> AggregateRisk {
    let mut r = AggregateRisk::default();
    f(&mut r);
    r
}

#[test]
fn combined_stop_fires_at_the_limit() {
    let r = agg(|r| r.combined_stoploss = Some(5000.0));
    assert!(!evaluate_aggregate(&r, 0.0, -4999.0).breached);
    let d = evaluate_aggregate(&r, 0.0, -5000.0);
    assert!(d.breached && d.reason == Some(BreachReason::CombinedStop));
    assert!(!d.detail.is_empty());
}

#[test]
fn combined_stop_is_read_as_a_magnitude() {
    let p = evaluate_aggregate(&agg(|r| r.combined_stoploss = Some(5000.0)), 0.0, -6000.0);
    let n = evaluate_aggregate(&agg(|r| r.combined_stoploss = Some(-5000.0)), 0.0, -6000.0);
    assert_eq!(p.reason, Some(BreachReason::CombinedStop));
    assert_eq!(n.reason, Some(BreachReason::CombinedStop));
}

#[test]
fn combined_target_fires_at_the_limit() {
    let r = agg(|r| r.combined_target = Some(10000.0));
    assert!(!evaluate_aggregate(&r, 0.0, 9999.0).breached);
    assert_eq!(
        evaluate_aggregate(&r, 4000.0, 6000.0).reason,
        Some(BreachReason::CombinedTarget)
    );
}

#[test]
fn peak_and_trough_ratchet_in_both_directions() {
    let d = evaluate_aggregate(
        &agg(|r| {
            r.peak_pnl = 8000.0;
            r.trough_pnl = -200.0
        }),
        0.0,
        500.0,
    );
    assert!(approx(d.peak_pnl, 8000.0));
    assert!(approx(d.trough_pnl, -200.0));
}

#[test]
fn trail_to_entry_bypasses_the_stop_but_not_the_target() {
    let r = agg(|r| {
        r.combined_stoploss = Some(5000.0);
        r.combined_target = Some(10000.0);
        r.stop_bypassed = true;
    });
    assert!(!evaluate_aggregate(&r, 0.0, -9000.0).breached);
    assert_eq!(
        evaluate_aggregate(&r, 0.0, 10000.0).reason,
        Some(BreachReason::CombinedTarget)
    );
}

#[test]
fn lock_profit_does_not_arm_below_the_threshold() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
    });
    let d = evaluate_aggregate(&r, 0.0, 4999.0);
    assert!(!d.lock_armed && !d.breached);
}

#[test]
fn lock_profit_arms_at_the_threshold_and_sets_the_floor() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
    });
    let d = evaluate_aggregate(&r, 0.0, 5000.0);
    assert!(d.lock_armed && d.lock_armed_now);
    assert_eq!(d.lock_floor, Some(3000.0));
    assert!(!d.breached);
}

#[test]
fn lock_profit_fires_when_profit_falls_back_to_the_floor() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
        r.lock_armed = true;
        r.lock_floor = Some(3000.0);
    });
    assert!(!evaluate_aggregate(&r, 0.0, 3001.0).breached);
    assert_eq!(
        evaluate_aggregate(&r, 0.0, 3000.0).reason,
        Some(BreachReason::LockProfit)
    );
}

#[test]
fn the_trailing_floor_rises_with_the_peak() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
        r.lock_trail_step = Some(1000.0);
        r.lock_armed = true;
        r.lock_floor = Some(3000.0);
        r.peak_pnl = 5000.0;
    });
    let d = evaluate_aggregate(&r, 0.0, 8000.0);
    assert_eq!(d.lock_floor, Some(7000.0));
    assert!(d.lock_floor_raised);
}

#[test]
fn the_floor_never_falls_back() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
        r.lock_trail_step = Some(1000.0);
        r.lock_armed = true;
        r.lock_floor = Some(7000.0);
        r.peak_pnl = 8000.0;
    });
    let d = evaluate_aggregate(&r, 0.0, 7100.0);
    assert_eq!(d.lock_floor, Some(7000.0));
    assert!(!d.lock_floor_raised && !d.breached);
}

#[test]
fn a_floor_above_its_activation_threshold_says_so() {
    let r = agg(|r| {
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(6000.0);
    });
    let d = evaluate_aggregate(&r, 0.0, 5000.0);
    assert_eq!(d.reason, Some(BreachReason::LockProfit));
    assert!(d.detail.contains("above its activation threshold"));
}

#[test]
fn a_removed_configuration_does_not_keep_firing() {
    let r = agg(|r| {
        r.lock_armed = true;
        r.lock_floor = Some(3000.0);
    });
    assert!(!evaluate_aggregate(&r, 0.0, 100.0).breached);
}

#[test]
fn lock_profit_outranks_the_combined_stop() {
    let r = agg(|r| {
        r.combined_stoploss = Some(5000.0);
        r.lock_profit_at = Some(5000.0);
        r.lock_profit_floor = Some(3000.0);
        r.lock_armed = true;
        r.lock_floor = Some(3000.0);
    });
    assert_eq!(
        evaluate_aggregate(&r, 0.0, 2000.0).reason,
        Some(BreachReason::LockProfit)
    );
}

// ------------------------------------------------------------------ trail to entry

fn pr(id: &str, side: Side, entry: f64, stop: f64) -> PositionRisk {
    PositionRisk {
        identifier: id.into(),
        side,
        entry_price: entry,
        stop_price: Some(stop),
        ..Default::default()
    }
}

#[test]
fn trail_to_entry_moves_the_others_to_their_own_entry() {
    let set = vec![
        pr("win", Side::Buy, 100.0, 90.0),
        pr("short", Side::Sell, 200.0, 210.0),
        pr("already", Side::Buy, 50.0, 55.0),
        pr("trigger", Side::Buy, 10.0, 9.0),
    ];
    let d = trail_stops_to_entry(&set, &["trigger".into()], &HashMap::new());
    let moved: HashMap<String, f64> = d
        .moves
        .iter()
        .map(|m| (m.identifier.clone(), m.new_stop))
        .collect();
    assert_eq!(moved.len(), 2);
    assert_eq!(moved["win"], 100.0);
    assert_eq!(moved["short"], 200.0);
    assert_eq!(d.skipped_not_improving, vec!["already".to_string()]);
}

#[test]
fn trail_to_entry_never_loosens_a_stop_already_past_entry() {
    let d = trail_stops_to_entry(&[pr("a", Side::Buy, 100.0, 107.0)], &[], &HashMap::new());
    assert_eq!(d.moved(), 0);
    assert_eq!(d.skipped_not_improving, vec!["a".to_string()]);
}

#[test]
fn trail_to_entry_refuses_to_turn_a_loser_into_an_immediate_market_exit() {
    let prices = HashMap::from([("a".to_string(), Some(95.0))]);
    let d = trail_stops_to_entry(&[pr("a", Side::Buy, 100.0, 90.0)], &[], &prices);
    assert_eq!(d.moved(), 0);
    assert_eq!(d.skipped_through_price, vec!["a".to_string()]);
}

#[test]
fn trail_to_entry_moves_a_winner_when_a_price_is_supplied() {
    let prices = HashMap::from([("a".to_string(), Some(108.0))]);
    let d = trail_stops_to_entry(&[pr("a", Side::Buy, 100.0, 90.0)], &[], &prices);
    assert_eq!(d.moved(), 1);
}

#[test]
fn a_position_with_no_entry_is_reported_not_skipped_silently() {
    let d = trail_stops_to_entry(
        &[PositionRisk {
            identifier: "a".into(),
            ..Default::default()
        }],
        &[],
        &HashMap::new(),
    );
    assert_eq!(d.skipped_no_entry, vec!["a".to_string()]);
}

#[test]
fn the_moved_stops_hold_on_the_next_tick() {
    let r = pr("a", Side::Buy, 100.0, 90.0);
    let mv = trail_stops_to_entry(std::slice::from_ref(&r), &[], &HashMap::new()).moves[0].clone();
    let moved = PositionRisk {
        stop_price: Some(mv.new_stop),
        ..r
    };
    assert_eq!(
        evaluate_position(&moved, Some(100.0)).reason,
        Some(BreachReason::Stop)
    );
    assert!(!evaluate_position(&moved, Some(100.5)).breached);
}

// ------------------------------------------------------------------ properties

/// Deterministic price walk (xorshift), standing in for Python's seeded
/// `random`: the invariants, not the exact series, are what is asserted.
fn walk(seed: u64, start: f64, steps: usize) -> Vec<f64> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut price = start;
    let mut out = Vec::with_capacity(steps);
    for _ in 0..steps {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let u = (x >> 11) as f64 / (1u64 << 53) as f64; // [0, 1)
        let change = -0.04 + u * 0.085;
        price = ((price * (1.0 + change)) * 100.0).round() / 100.0;
        if price < 0.05 {
            price = 0.05;
        }
        out.push(price);
    }
    out
}

#[test]
fn a_long_stop_never_decreases() {
    for seed in [1u64, 7, 42, 99, 2024] {
        for mode in [TrailMode::Continuous, TrailMode::Stepped] {
            let (mut stop, mut highest) = (90.0, 100.0);
            for ltp in walk(seed, 100.0, 200) {
                let r = PositionRisk {
                    side: Side::Buy,
                    entry_price: 100.0,
                    quantity: 50.0,
                    stop_price: Some(stop),
                    initial_stop_price: Some(90.0),
                    highest_price: Some(highest),
                    trailing_enabled: true,
                    trail_step: 3.0,
                    trail_trigger: 5.0,
                    trail_mode: mode,
                    ..Default::default()
                };
                let d = evaluate_position(&r, Some(ltp));
                let s = d.stop_price.unwrap();
                let h = d.highest_price.unwrap();
                assert!(s >= stop - 1e-9, "stop fell from {} to {}", stop, s);
                assert!(h >= highest - 1e-9);
                assert!(s <= h + 1e-9);
                stop = s;
                highest = h;
                if d.breached {
                    break;
                }
            }
        }
    }
}

#[test]
fn a_short_stop_never_increases() {
    for seed in [1u64, 7, 42, 99, 2024] {
        for mode in [TrailMode::Continuous, TrailMode::Stepped] {
            let (mut stop, mut lowest) = (110.0, 100.0);
            for ltp in walk(seed, 100.0, 200) {
                let r = PositionRisk {
                    side: Side::Sell,
                    entry_price: 100.0,
                    quantity: 75.0,
                    stop_price: Some(stop),
                    initial_stop_price: Some(110.0),
                    lowest_price: Some(lowest),
                    trailing_enabled: true,
                    trail_step: 3.0,
                    trail_trigger: 5.0,
                    trail_mode: mode,
                    ..Default::default()
                };
                let d = evaluate_position(&r, Some(ltp));
                let s = d.stop_price.unwrap();
                let l = d.lowest_price.unwrap();
                assert!(s <= stop + 1e-9, "stop rose from {} to {}", stop, s);
                assert!(l <= lowest + 1e-9);
                assert!(s >= l - 1e-9);
                stop = s;
                lowest = l;
                if d.breached {
                    break;
                }
            }
        }
    }
}

#[test]
fn the_lock_profit_floor_never_falls_back() {
    for seed in [3u64, 11, 77] {
        let walk = walk(seed, 1000.0, 300);
        let mut r = agg(|r| {
            r.lock_profit_at = Some(5000.0);
            r.lock_profit_floor = Some(3000.0);
            r.lock_trail_step = Some(1500.0);
        });
        let mut total = 0.0;
        let mut floor: Option<f64> = None;
        for p in walk {
            total += (p - 1000.0) * 0.9;
            let d = evaluate_aggregate(&r, 0.0, total);
            if let (Some(new), Some(old)) = (d.lock_floor, floor) {
                assert!(new >= old - 1e-9);
            }
            r.lock_armed = d.lock_armed;
            r.lock_floor = d.lock_floor;
            r.peak_pnl = d.peak_pnl;
            r.trough_pnl = d.trough_pnl;
            floor = d.lock_floor;
            if d.breached {
                break;
            }
        }
    }
}

#[test]
fn an_unusable_tick_never_changes_anything() {
    let mut state = json!({
        "side": "BUY", "entry_price": 100.0, "quantity": 50, "initial_sl": 90.0,
        "current_sl": 90.0, "trailing_enabled": true, "trailing_step": 3.0,
        "highest_price": 100.0, "lowest_price": 100.0
    });
    for ltp in walk(5, 110.0, 50) {
        let good = evaluate_trail(&state, &json!(ltp));
        for k in ["highest_price", "lowest_price", "current_sl"] {
            state[k] = good[k].clone();
        }
        let snapshot = state.clone();
        for dead in [
            json!(0.0),
            json!(-1.0),
            Value::Null,
            json!("nan"),
            json!("inf"),
        ] {
            let r = evaluate_trail(&state, &dead);
            assert_eq!(r["breached"], json!(false));
            assert_eq!(r["current_sl"], snapshot["current_sl"]);
            assert_eq!(r["highest_price"], snapshot["highest_price"]);
        }
        if good["breached"] == json!(true) {
            break;
        }
    }
}

// ------------------------------------------------------------------ scalping parity

fn long(over: Value) -> Value {
    let mut s = json!({
        "symbol": "NIFTY25JUN2623600CE", "exchange": "NFO", "product": "NRML",
        "side": "BUY", "entry_price": 100.0, "current_sl": 90.0, "initial_sl": 90.0,
        "target": 0.0, "trailing_enabled": false, "trailing_step": 0.0,
        "highest_price": 100.0, "lowest_price": 100.0
    });
    for (k, v) in over.as_object().unwrap() {
        s[k] = v.clone();
    }
    s
}

fn short(over: Value) -> Value {
    let mut base = json!({"side": "SELL", "current_sl": 110.0, "initial_sl": 110.0});
    for (k, v) in over.as_object().unwrap() {
        base[k] = v.clone();
    }
    long(base)
}

fn trail(state: &Value, ltp: f64) -> Value {
    evaluate_trail(state, &json!(ltp))
}

#[test]
fn scalping_stop_boundaries() {
    assert_eq!(
        trail(&long(json!({"current_sl": 95.0})), 95.0)["reason"],
        "sl"
    );
    assert_eq!(
        trail(&long(json!({"current_sl": 95.0})), 94.9)["breached"],
        true
    );
    assert_eq!(
        trail(&long(json!({"current_sl": 95.0})), 95.1)["breached"],
        false
    );
    assert_eq!(
        trail(&short(json!({"current_sl": 105.0})), 105.0)["reason"],
        "sl"
    );
    assert_eq!(
        trail(&short(json!({"current_sl": 105.0})), 105.1)["breached"],
        true
    );
    assert_eq!(
        trail(&short(json!({"current_sl": 105.0})), 104.9)["breached"],
        false
    );
}

#[test]
fn scalping_target_boundaries_and_stop_priority() {
    assert_eq!(
        trail(&long(json!({"target": 120.0})), 120.0)["reason"],
        "target"
    );
    assert_eq!(
        trail(&long(json!({"target": 120.0})), 119.9)["breached"],
        false
    );
    assert_eq!(
        trail(&short(json!({"target": 80.0})), 80.0)["reason"],
        "target"
    );
    assert_eq!(
        trail(&short(json!({"target": 80.0})), 80.1)["breached"],
        false
    );
    assert_eq!(
        trail(&long(json!({"current_sl": 95.0, "target": 120.0})), 90.0)["reason"],
        "sl"
    );
}

#[test]
fn scalping_trails() {
    let s = long(json!({"trailing_enabled": true, "trailing_step": 3.0, "current_sl": 90.0}));
    let r = trail(&s, 110.0);
    assert_eq!(r["breached"], false);
    assert_eq!(r["current_sl"], json!(107.0));
    assert_eq!(r["highest_price"], json!(110.0));
    let held = trail(
        &long(
            json!({"trailing_enabled": true, "trailing_step": 3.0, "current_sl": 107.0, "highest_price": 110.0}),
        ),
        108.0,
    );
    assert_eq!(held["current_sl"], json!(107.0));
    assert_eq!(trail(&s, 100.5)["current_sl"], json!(90.0));
    let mut next = s.clone();
    for (k, v) in r.as_object().unwrap() {
        next[k] = v.clone();
    }
    let second = trail(&next, 106.9);
    assert_eq!(second["reason"], "sl");

    let ss = short(json!({"trailing_enabled": true, "trailing_step": 3.0, "current_sl": 110.0}));
    let r = trail(&ss, 90.0);
    assert_eq!(r["current_sl"], json!(93.0));
    assert_eq!(r["lowest_price"], json!(90.0));
    let mut next = ss.clone();
    for (k, v) in r.as_object().unwrap() {
        next[k] = v.clone();
    }
    assert_eq!(trail(&next, 93.1)["reason"], "sl");
}
