//! web: test/test_strategy_module_signals.py

use super::*;
use openalgo_desktop_lib::strategy::store::StrategyRow;

fn strategy(t: &T, legs: Value, overrides: Value) -> StrategyRow {
    let sid = t.make(signal_config(legs, overrides));
    t.m.store.get_strategy(sid, USER).unwrap().unwrap()
}

async fn signal(
    t: &T,
    s: &StrategyRow,
    action: &str,
    leg: i64,
) -> openalgo_desktop_lib::strategy::signals::SignalResult {
    let fresh = t.m.store.get_strategy(s.id, USER).unwrap().unwrap();
    t.m.handle_signal(&fresh, action, Some(&json!(leg)), None, None)
        .await
}

fn current_run(t: &T, s: &StrategyRow) -> i64 {
    t.m.store
        .get_strategy(s.id, USER)
        .unwrap()
        .unwrap()
        .current_run_id
        .unwrap()
}

#[tokio::test]
async fn a_leg_is_held_on_the_side_the_signal_opened_not_the_one_configured() {
    // PORTED DEFECT. A signal leg's configuration says which signals it
    // accepts, not which way it is held; the original never recorded a
    // side, so the risk core evaluated every signal leg as a short.
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    let r = signal(&t, &s, "short_entry", 1).await;
    assert!(r.acted(), "{:?}", r);
    let run = current_run(&t, &s);
    assert_eq!(t.leg(run, 1).position, "S");
    assert_eq!(t.gw.actions(), vec!["SELL"]);
}

#[tokio::test]
async fn a_long_signal_opens_a_long() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    assert_eq!(t.leg(run, 1).position, "B");
    assert_eq!(t.gw.actions(), vec!["BUY"]);
}

#[tokio::test]
async fn a_signal_entry_persists_the_live_position_reference() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    let entry = &t.orders(run)[0];
    assert_eq!(entry.position_ref.as_ref().map(|r| r.len()), Some(32));
    assert_eq!(t.leg(run, 1).position_ref, entry.position_ref);
}

#[tokio::test]
async fn an_exit_covers_the_side_actually_held() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "short_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    let r = signal(&t, &s, "short_exit", 1).await;
    assert!(r.acted());
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn a_repeated_entry_is_a_noop_not_a_failure() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    let r = signal(&t, &s, "long_entry", 1).await;
    assert!(r.ok);
    assert_eq!(r.note.as_deref(), Some("already_long"));
    assert_eq!(t.gw.placed().len(), 1);
}

#[tokio::test]
async fn an_exit_for_a_position_not_held_is_a_noop() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    let r = signal(&t, &s, "long_exit", 1).await;
    assert!(r.ok);
    assert_eq!(r.note.as_deref(), Some("no_matching_position"));
    assert!(t.gw.placed().is_empty());
}

#[tokio::test]
async fn a_repeated_exit_alert_does_not_reverse_the_position() {
    // PORTED DEFECT 8. A leg stays open until its exit fill arrives, so
    // the second alert found the position still held and sent a second
    // closing order.
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    assert!(signal(&t, &s, "long_exit", 1).await.acted());
    let again = signal(&t, &s, "long_exit", 1).await;
    assert_eq!(again.note.as_deref(), Some("no_matching_position"));
    assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
}

#[tokio::test]
async fn a_direction_refuses_the_other_side() {
    let t = t();
    let s = strategy(
        &t,
        json!([signal_leg(1, "RELIANCE", "long")]),
        json!({"direction": "long_only"}),
    );
    let r = signal(&t, &s, "short_entry", 1).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("long_only"));
}

#[tokio::test]
async fn a_leg_side_refuses_signals_it_does_not_accept() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "long")]), json!({}));
    let r = signal(&t, &s, "short_entry", 1).await;
    assert_eq!(r.error.as_deref(), Some("Leg 1 only accepts long signals"));
}

#[tokio::test]
async fn an_unknown_leg_is_refused() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    let r = signal(&t, &s, "long_entry", 9).await;
    assert_eq!(r.error.as_deref(), Some("No leg matches this signal"));
}

#[tokio::test]
async fn a_leg_can_be_found_by_symbol() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    let r =
        t.m.handle_signal(&s, "long_entry", None, Some("reliance"), Some("NSE"))
            .await;
    assert!(r.acted());
}

#[tokio::test]
async fn signals_outside_the_trading_window_are_noops() {
    let t = t(); // 10:00 IST
    let s = strategy(
        &t,
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({"strategy_type": "intraday", "entry_time": "10:30", "exit_time": "15:00"}),
    );
    let r = signal(&t, &s, "long_entry", 1).await;
    assert_eq!(r.note.as_deref(), Some("outside_entry_window"));
    t.clock.set(ist(2026, 10, 7, 15, 5));
    let r = signal(&t, &s, "long_exit", 1).await;
    assert_eq!(r.note.as_deref(), Some("outside_trading_window"));
    assert!(t.gw.placed().is_empty());
}

#[tokio::test]
async fn an_opposite_entry_squares_first_then_opens() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    let r = signal(&t, &s, "short_entry", 1).await;
    assert!(r.ok && r.flipped, "{:?}", r);
    assert_eq!(t.gw.actions(), vec!["BUY", "SELL", "SELL"]);
    let leg = t.leg(run, 1);
    assert_eq!(leg.position, "S");
    assert_eq!(leg.superseded.as_ref().unwrap().position, "B");
}

#[tokio::test]
async fn an_uncarryable_short_is_refused_before_anything_is_squared() {
    let t = t();
    let s = strategy(
        &t,
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({"product": "CNC"}),
    );
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    let r = signal(&t, &s, "short_entry", 1).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("cannot be held short overnight"));
    assert_eq!(t.gw.actions(), vec!["BUY"], "the long was not liquidated");
}

#[tokio::test]
async fn a_misspelt_contract_is_refused() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELAINCE", "both")]), json!({}));
    let r = signal(&t, &s, "long_entry", 1).await;
    assert!(r
        .error
        .unwrap()
        .contains("RELAINCE is not a contract on NSE"));
    assert!(t.gw.placed().is_empty());
}

#[tokio::test]
async fn lots_mode_multiplies_by_the_master_lot_size() {
    let t = t();
    let leg = json!({"id": 1, "symbol": "RELIANCE27OCT26FUT", "exchange": "NFO", "side": "both",
                     "qty": 2, "qty_mode": "lots", "segment": "futures", "expiry": "current"});
    let s = strategy(&t, json!([leg]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    assert_eq!(t.gw.placed()[0].quantity, 1000);
}

#[tokio::test]
async fn a_signal_exit_on_a_leg_does_not_end_the_session_run() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let run = current_run(&t, &s);
    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    signal(&t, &s, "long_exit", 1).await;
    t.fill_last_exit(run, 1010.0).await;
    assert!(t.run(run).stopped_at.is_none());
    let leg = t.leg(run, 1);
    assert_eq!(leg.status, "closed");
    assert!((leg.realized_pnl - 100.0).abs() < 1e-9);
    // The same session can re-enter; realized accumulates on the leg.
    signal(&t, &s, "long_entry", 1).await;
    assert_eq!(current_run(&t, &s), run);
    assert!((t.leg(run, 1).realized_pnl - 100.0).abs() < 1e-9);
}

#[tokio::test]
async fn a_stale_run_from_an_earlier_session_is_rolled_on_the_next_signal() {
    let t = t();
    let s = strategy(&t, json!([signal_leg(1, "RELIANCE", "both")]), json!({}));
    signal(&t, &s, "long_entry", 1).await;
    let first = current_run(&t, &s);
    let id = t.orders(first)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "rejected", 0, 0.0).await;
    t.clock.set(ist(2026, 10, 8, 10, 0));
    signal(&t, &s, "long_entry", 1).await;
    let second = current_run(&t, &s);
    assert_ne!(first, second);
    assert_eq!(t.run(first).stop_reason.as_deref(), Some("eod"));
}
