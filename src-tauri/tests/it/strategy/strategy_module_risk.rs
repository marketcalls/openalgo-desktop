//! web: test/test_strategy_module_risk.py

use openalgo_desktop_lib::strategy::risk_adapter as ra;
use openalgo_desktop_lib::strategy::state::{new_leg_state, LegSpec, LegState, RunState};
use serde_json::json;

fn leg(position: &str) -> LegState {
    let mut l = new_leg_state(&LegSpec {
        leg_id: 1,
        position: "B".into(),
        symbol: "X".into(),
        exchange: "NFO".into(),
        lots: 1,
        quantity: 10,
        risk_unit: "points".into(),
        ..Default::default()
    })
    .unwrap();
    l.position = position.into();
    l.status = "open".into();
    l.entry_status = "complete".into();
    l
}

fn priced(position: &str, entry: f64, qty: i64, ltp: Option<f64>) -> LegState {
    let mut l = leg(position);
    l.entry_avg = entry;
    l.qty = qty;
    l.ltp = ltp;
    l
}

fn state(legs: Vec<LegState>) -> RunState {
    let mut legs = legs;
    for (i, l) in legs.iter_mut().enumerate() {
        l.leg_id = i as i64 + 1;
    }
    RunState::new(1, 1, legs)
}

#[test]
fn a_leg_without_a_usable_side_is_refused_not_defaulted() {
    // PORTED DEFECT. The original never writes `position` on signal legs
    // and reads anything not "B" as a short, so the stop fired on a
    // favourable move. Defaulting is the bug; refusing is the fix.
    for bad in ["", "LONG", "buy", "x"] {
        assert!(ra::leg_to_position_risk(&leg(bad)).is_err(), "{:?}", bad);
        assert!(new_leg_state(&LegSpec {
            position: bad.into(),
            ..Default::default()
        })
        .is_err());
    }
}

#[test]
fn a_short_leg_is_evaluated_as_a_short() {
    let mut l = priced("S", 100.0, 10, None);
    l.sl_pts = Some(20.0);
    l.target_pts = Some(30.0);
    let r = ra::leg_to_position_risk(&l).unwrap();
    assert_eq!(r.stop_price, Some(120.0));
    assert_eq!(r.target_price, Some(70.0));
    let d = ra::evaluate_leg(&mut l, 121.0).unwrap();
    assert!(d.breached && d.reason.unwrap().as_str() == "sl" && d.pnl < 0.0);
}

#[test]
fn a_long_leg_is_evaluated_as_a_long() {
    let mut l = priced("B", 100.0, 10, None);
    l.sl_pts = Some(20.0);
    l.target_pts = Some(30.0);
    let r = ra::leg_to_position_risk(&l).unwrap();
    assert_eq!(r.stop_price, Some(80.0));
    assert_eq!(r.target_price, Some(130.0));
    assert_eq!(
        ra::evaluate_leg(&mut l.clone(), 79.0)
            .unwrap()
            .reason
            .unwrap()
            .as_str(),
        "sl"
    );
    let d = ra::evaluate_leg(&mut l, 131.0).unwrap();
    assert_eq!(d.reason.unwrap().as_str(), "target");
    assert!(d.pnl > 0.0);
}

#[test]
fn a_fixed_distance_trail_uses_x_as_its_gap() {
    let mut l = priced("B", 100.0, 10, None);
    l.sl_pts = Some(20.0);
    l.trail_x = 5.0;
    let d = ra::evaluate_leg(&mut l, 110.0).unwrap();
    assert!(d.trail_armed);
    assert_eq!(d.stop_price, Some(105.0));
}

#[test]
fn a_percent_leg_converts_against_its_own_entry() {
    let mut l = priced("S", 2500.0, 10, None);
    l.risk_unit = "percent".into();
    l.sl_pts = Some(2.0);
    l.target_pts = Some(4.0);
    let r = ra::leg_to_position_risk(&l).unwrap();
    assert_eq!(r.stop_price, Some(2550.0));
    assert_eq!(r.target_price, Some(2400.0));
}

#[test]
fn a_percent_leg_with_no_confirmed_fill_gets_no_levels() {
    let mut l = priced("B", 0.0, 10, None);
    l.risk_unit = "percent".into();
    l.sl_pts = Some(2.0);
    let r = ra::leg_to_position_risk(&l).unwrap();
    assert_eq!(r.stop_price, None);
    assert_eq!(r.target_price, None);
}

#[test]
fn run_pnl_marks_from_entry_rather_than_a_stale_leg_field() {
    // PORTED DEFECT. The original summed a per-leg `mtm` written on an
    // earlier pass, so a stale field poisoned every strategy-level rule.
    let mut l = priced("B", 100.0, 10, Some(110.0));
    l.mtm = -99999.0;
    let (realized, unrealized) = ra::run_pnl(&state(vec![l])).unwrap();
    assert_eq!(realized, 0.0);
    assert!((unrealized - 100.0).abs() < 1e-9);
}

#[test]
fn a_closed_leg_contributes_its_realized_figure() {
    let open = priced("B", 100.0, 10, Some(105.0));
    let mut closed = priced("B", 100.0, 10, None);
    closed.status = "closed".into();
    closed.realized_pnl = 250.0;
    let (r, u) = ra::run_pnl(&state(vec![open, closed])).unwrap();
    assert!((r - 250.0).abs() < 1e-9);
    assert!((u - 50.0).abs() < 1e-9);
}

#[test]
fn a_reentered_signal_leg_keeps_its_earlier_round_trip() {
    let mut l = priced("B", 100.0, 10, Some(101.0));
    l.realized_pnl = -500.0;
    let (r, _) = ra::run_pnl(&state(vec![l])).unwrap();
    assert!((r + 500.0).abs() < 1e-9);
}

#[test]
fn peak_and_trough_are_written_on_every_pass_not_only_on_a_breach() {
    // PORTED DEFECT. The original persisted peak and trough on one of
    // several stop paths only.
    let mut s = state(vec![priced("B", 100.0, 10, Some(120.0))]);
    let strategy = json!({"overall_sl_mtm": null, "overall_target_mtm": null});
    ra::evaluate_run(&mut s, &strategy).unwrap();
    assert!((s.pnl_peak - 200.0).abs() < 1e-9);
    s.legs.get_mut("1").unwrap().ltp = Some(90.0);
    ra::evaluate_run(&mut s, &strategy).unwrap();
    assert!((s.pnl_peak - 200.0).abs() < 1e-9);
    assert!((s.pnl_trough + 100.0).abs() < 1e-9);
    assert!((s.pnl_total + 100.0).abs() < 1e-9);
}

#[test]
fn the_overall_stop_is_entered_positive_and_applied_negative() {
    let mut s = state(vec![priced("B", 100.0, 10, Some(60.0))]);
    let d = ra::evaluate_run(&mut s, &json!({"overall_sl_mtm": 300})).unwrap();
    assert_eq!(d.reason.unwrap().as_str(), "combined_sl");
}

#[test]
fn the_overall_target_fires_on_the_total() {
    let mut s = state(vec![priced("B", 100.0, 10, Some(160.0))]);
    let d = ra::evaluate_run(&mut s, &json!({"overall_target_mtm": 500})).unwrap();
    assert_eq!(d.reason.unwrap().as_str(), "combined_target");
}

#[test]
fn a_plain_lock_does_not_trail_even_with_a_step_configured() {
    let mut s = state(vec![priced("B", 100.0, 10, Some(160.0))]);
    let d = ra::evaluate_run(
        &mut s,
        &json!({"lock_profit": {"mode": "lock", "if_profit_reaches": 500,
            "lock_profit": 200, "trail_step": 50}}),
    )
    .unwrap();
    assert!(d.lock_armed);
    assert_eq!(d.lock_floor, Some(200.0));
    let d = ra::evaluate_run(
        &mut s,
        &json!({"lock_profit": {"mode": "lock_and_trail", "if_profit_reaches": 500,
            "lock_profit": 200, "trail_step": 50}}),
    )
    .unwrap();
    assert_eq!(d.lock_floor, Some(550.0));
}

#[test]
fn trail_to_entry_moves_the_other_open_legs() {
    let mut a = priced("B", 100.0, 10, Some(110.0));
    a.sl_pts = Some(20.0);
    let mut b = priced("B", 50.0, 10, Some(60.0));
    b.sl_pts = Some(10.0);
    let mut s = state(vec![a, b]);
    let moved = ra::trail_open_legs_to_entry(&mut s, 1);
    assert_eq!(moved, vec!["2".to_string()]);
    assert_eq!(s.legs["2"].effective_sl, Some(50.0));
    assert!(s.trail_to_entry_active);
}
