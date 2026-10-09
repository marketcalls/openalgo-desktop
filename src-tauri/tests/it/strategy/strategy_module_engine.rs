//! web: test/test_strategy_module_engine.py

use super::*;

#[tokio::test]
async fn a_leg_that_cannot_be_resolved_stops_the_start_before_anything_is_claimed() {
    let t = t();
    let mut leg = short_call_leg();
    leg["atm_offset"] = json!("OTM5");
    leg["option_type"] = json!("XX");
    let sid = t.make(config("Bad leg", json!([leg]), json!({})));
    let r = t.start(sid).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().starts_with("Leg 1:"));
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    assert_eq!(s.status, "stopped");
    assert!(t.m.store.list_runs(sid, 10).unwrap().is_empty());
    assert!(t.gw.placed().is_empty());
}

#[tokio::test]
async fn a_second_start_is_refused_by_the_atomic_claim() {
    let t = t();
    let sid = t.default_strategy();
    assert!(t.start(sid).await.ok);
    let r = t.start(sid).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("already running"));
    assert_eq!(t.gw.placed().len(), 1);
}

#[tokio::test]
async fn live_is_refused_unless_the_strategy_opted_in() {
    let t = t();
    let sid = t.default_strategy();
    let r = t.m.start_run(sid, USER, "live", "manual", None).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("not enabled for live trading"));
    assert!(t.gw.placed().is_empty());
}

#[tokio::test]
async fn an_unknown_mode_is_refused() {
    let t = t();
    let sid = t.default_strategy();
    let r = t.m.start_run(sid, USER, "paper", "manual", None).await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("Unknown run mode"));
}

#[tokio::test]
async fn a_signal_strategy_has_no_start() {
    let t = t();
    let sid = t.make(signal_config(
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({}),
    ));
    let r = t.start(sid).await;
    assert!(!r.ok && r.error.unwrap().contains("signal strategy has no start"));
}

#[tokio::test]
async fn entries_are_placed_longs_first() {
    let t = t();
    let mut buy = short_call_leg();
    buy["id"] = json!(2);
    buy["position"] = json!("B");
    buy["atm_offset"] = json!("OTM2");
    let sid = t.make(config("Spread", json!([short_call_leg(), buy]), json!({})));
    assert!(t.start(sid).await.ok);
    assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
}

#[tokio::test]
async fn every_leg_of_one_spread_is_priced_off_the_same_quote() {
    let t = t();
    let mut b = short_call_leg();
    b["id"] = json!(2);
    let sid = t.make(config("Straddle", json!([short_call_leg(), b]), json!({})));
    let r = t.start(sid).await;
    assert!(r.ok);
    let symbols: Vec<String> = t.gw.placed().into_iter().map(|o| o.symbol).collect();
    assert_eq!(symbols, vec![ATM_CE, ATM_CE]);
}

#[tokio::test]
async fn every_entry_rejected_finalises_the_run_rather_than_leaving_it_running() {
    let t = t();
    let sid = t.default_strategy();
    t.gw.reject_next("Insufficient funds");
    let r = t.start(sid).await;
    assert!(!r.ok);
    assert_eq!(
        r.error.as_deref(),
        Some("Every entry order was rejected: Insufficient funds")
    );
    let run = t.run(r.run_id.unwrap());
    assert!(run.stopped_at.is_some());
    assert_eq!(run.stop_reason.as_deref(), Some("error"));
    assert_eq!(
        t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
        "stopped"
    );
    assert!(t.m.state.is_empty());
}

#[tokio::test]
async fn a_started_run_records_its_orders_and_live_state() {
    let t = t();
    let sid = t.default_strategy();
    let r = t.start(sid).await;
    let run = r.run_id.unwrap();
    let orders = t.orders(run);
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].kind, "entry");
    assert_eq!(orders[0].status, "open");
    assert_eq!(orders[0].broker_order_id.as_deref(), Some("SB-1"));
    assert_eq!(orders[0].position_ref.as_ref().map(|p| p.len()), Some(32));
    let leg = t.leg(run, 1);
    assert_eq!(leg.position, "S");
    assert_eq!(leg.symbol, ATM_CE);
    assert_eq!(leg.qty, 65);
    assert_eq!(leg.entry_order_id, Some(orders[0].id));
    assert_eq!(leg.position_ref, orders[0].position_ref);
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    assert_eq!(
        (s.status.as_str(), s.current_run_id),
        ("running", Some(run))
    );
}

#[tokio::test]
async fn an_entry_fill_sets_the_price_risk_is_measured_from() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    let leg = t.leg(run, 1);
    assert_eq!(leg.entry_avg, 100.0);
    assert_eq!(leg.entry_status, "complete");
    assert_eq!(leg.status, "open");
}

#[tokio::test]
async fn an_exit_fill_locks_in_realized_pnl_with_the_right_sign() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 90.0).await;
    let r = t.run(run);
    assert!(r.stopped_at.is_some());
    // A short from 100 covered at 90 made 10 x 65.
    assert!((r.pnl_realized - 650.0).abs() < 1e-6);
}

#[tokio::test]
async fn a_long_exit_fill_carries_the_opposite_sign() {
    let t = t();
    let mut leg = short_call_leg();
    leg["position"] = json!("B");
    let sid = t.make(config("Long", json!([leg]), json!({})));
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 90.0).await;
    assert!((t.run(run).pnl_realized + 650.0).abs() < 1e-6);
}

#[tokio::test]
async fn an_exit_uses_the_symbol_the_run_holds_not_a_re_resolved_one() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    // The underlying moves 400 points: a re-resolved ATM would differ.
    t.gw.ltps
        .lock()
        .insert(("NIFTY".into(), "NSE_INDEX".into()), 24910.0);
    t.m.stop_run(run, USER, "manual").await;
    let placed = t.gw.placed();
    assert_eq!(placed.last().unwrap().symbol, ATM_CE);
}

#[tokio::test]
async fn an_exit_covers_a_short_rather_than_adding_to_it() {
    // PORTED DEFECT. The original derived the exit action from the
    // configured side, which defaulted to "B", so an exit on a short
    // placed another SELL and doubled the position.
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn a_leg_already_exiting_is_not_sent_a_second_exit() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    // A stop-loss tick, then the operator's close: one exit.
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    let r = t.m.close_leg(run, 1, USER).await;
    assert!(!r.ok);
    let exits = t
        .orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .count();
    assert_eq!(exits, 1);
    assert_eq!(t.gw.placed().len(), 2);
}

#[tokio::test]
async fn a_rejected_exit_can_be_retried_rather_than_looking_like_a_duplicate() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.reject_next("RMS: margin exceeded");
    let r = t.m.close_leg(run, 1, USER).await;
    assert!(!r.ok);
    let leg = t.leg(run, 1);
    assert!(
        leg.exit_kind.is_none() && leg.exit_claim_token.is_none() && leg.exit_order_id.is_none()
    );
    let r = t.m.close_leg(run, 1, USER).await;
    assert!(r.ok, "{:?}", r);
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
}

#[tokio::test]
async fn a_manual_close_does_not_trail_the_other_legs_to_entry() {
    let t = t();
    let mut b = short_call_leg();
    b["id"] = json!(2);
    b["atm_offset"] = json!("OTM1");
    let sid = t.make(config(
        "Two",
        json!([short_call_leg(), b]),
        json!({"trail_sl_to_entry": true}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    t.m.close_leg(run, 1, USER).await;
    let s = t.m.state.snapshot(run).unwrap();
    assert!(!s.trail_to_entry_active);
    assert!(s.leg(2).unwrap().effective_sl.is_none());
}

#[tokio::test]
async fn the_run_finalises_when_the_last_exit_fills_not_when_it_is_placed() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(r.ok && r.stop_pending);
    assert!(t.run(run).stopped_at.is_none());
    assert_eq!(t.leg(run, 1).status, "open");
    t.fill_last_exit(run, 95.0).await;
    assert!(t.run(run).stopped_at.is_some());
}

#[tokio::test]
async fn a_stop_loss_tick_exits_that_leg() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 120.0).await;
    let exit = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap();
    assert_eq!(
        (exit.kind.as_str(), exit.action.as_str()),
        ("exit_sl", "BUY")
    );
    assert!(t.event_kinds(sid).contains(&"leg_sl_hit".to_string()));
}

#[tokio::test]
async fn a_quiet_tick_places_nothing() {
    let t = t();
    let sid = t.default_strategy();
    t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 105.0).await;
    assert_eq!(t.gw.placed().len(), 1);
}

#[tokio::test]
async fn an_overall_stop_closes_the_whole_run() {
    let t = t();
    let sid = t.make(config(
        "SL",
        json!([short_call_leg()]),
        json!({"overall_sl_mtm": 500}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650
    let run_row = t.run(run);
    assert_eq!(run_row.stop_requested_reason.as_deref(), Some("overall_sl"));
    let exit = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap();
    assert_eq!(exit.kind, "exit_overall_sl");
}

#[tokio::test]
async fn a_tick_for_an_instrument_no_run_holds_is_ignored() {
    let t = t();
    let sid = t.default_strategy();
    t.start_filled(sid, 100.0).await;
    t.m.process_tick("SOMETHING-ELSE", "NFO", 1.0).await;
    assert_eq!(t.gw.placed().len(), 1);
}

#[tokio::test]
async fn peak_and_trough_reach_the_run_row_on_a_rule_driven_stop() {
    // PORTED DEFECT. The original passed peak and trough on only one of
    // its stop paths, so a rule-driven stop recorded both as zero.
    let t = t();
    let sid = t.make(config(
        "PT",
        json!([short_call_leg()]),
        json!({"overall_sl_mtm": 500}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 95.0).await; // +325
    t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650, breaches
    let pending = t.run(run);
    assert_eq!(pending.stop_requested_reason.as_deref(), Some("overall_sl"));
    assert!(pending.stopped_at.is_none());
    t.fill_last_exit(run, 110.0).await;
    let done = t.run(run);
    assert!((done.pnl_peak - 325.0).abs() < 1e-6, "{}", done.pnl_peak);
    assert!(
        (done.pnl_trough + 650.0).abs() < 1e-6,
        "{}",
        done.pnl_trough
    );
    assert_eq!(done.stop_reason.as_deref(), Some("overall_sl"));
}

#[tokio::test]
async fn a_finished_run_leaves_no_live_state_behind() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 100.0).await;
    assert!(t.m.state.snapshot(run).is_none());
    assert!(t.m.state.is_empty());
    assert_eq!(t.m.feed.runs_tracked(), 0);
}

#[tokio::test]
async fn a_flat_run_can_finalize_without_a_broker_session() {
    let t = t();
    let sid = t.default_strategy();
    let r = t.start(sid).await;
    let run = r.run_id.unwrap();
    // The entry dies at the broker, so nothing is held.
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "rejected", 0, 0.0).await;
    t.gw.unauthorised
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let out = t.m.stop_run(run, USER, "manual").await;
    assert!(out.ok && !out.stop_pending);
    assert!(t.run(run).stopped_at.is_some());
}

#[tokio::test]
async fn a_stop_whose_exits_were_refused_leaves_the_run_open_and_managed() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.reject_next("Exchange closed");
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(!r.ok && r.stop_pending);
    assert!(r
        .error
        .unwrap()
        .contains("1 of 1 exit order(s) were refused"));
    let row = t.run(run);
    assert!(row.stopped_at.is_none());
    assert!(
        t.m.state.snapshot(run).is_some(),
        "the run is still managed"
    );
    assert_eq!(
        t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
        "running"
    );
    assert!(t.event_kinds(sid).contains(&"run_stop_failed".to_string()));
    // The stop can be retried and succeeds.
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(r.ok && r.stop_pending);
    t.fill_last_exit(run, 99.0).await;
    assert!(t.run(run).stopped_at.is_some());
}

#[tokio::test]
async fn a_stop_of_an_unfilled_entry_is_refused_and_retried_once_it_fills() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let r = t.m.stop_run(run, USER, "manual").await;
    // The cancel was sent; nothing was exited against an unfilled entry.
    assert_eq!(t.gw.cancels.lock().len(), 1);
    assert!(!r.ok && r.stop_pending, "{:?}", r);
    assert_eq!(t.gw.actions(), vec!["SELL"]);
    // The entry fills after all: the durable stop exits it.
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 100.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn a_keyless_stop_with_possible_exposure_is_durable_pending_and_retryable() {
    let t = t();
    let sid = t.default_strategy();
    let run =
        t.m.start_run(sid, USER, "sandbox", "manual", None)
            .await
            .run_id
            .unwrap();
    t.m.apply_fill(run, 1, Some(100.0), true, FillOpts::default())
        .await;
    // Mark the run live after the fact so the broker session matters.
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_run SET mode = 'live' WHERE id = {}",
            run
        ))
        .unwrap();
    t.gw.unauthorised
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(!r.ok && r.stop_pending);
    assert_eq!(t.run(run).stop_requested_reason.as_deref(), Some("manual"));
    t.gw.unauthorised
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let r = t.m.reconcile_pending_stop(run).await.unwrap();
    assert!(r.ok && r.stop_pending);
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn risk_that_cannot_be_acted_on_reaches_the_audit_trail_once_per_episode() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_run SET mode = 'live' WHERE id = {}",
            run
        ))
        .unwrap();
    t.gw.unauthorised
        .store(true, std::sync::atomic::Ordering::SeqCst);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    t.m.process_tick(ATM_CE, "NFO", 122.0).await;
    let critical: Vec<Value> = t
        .events(sid)
        .into_iter()
        .filter(|e| e["kind"] == "leg_exit_rejected" && e["severity"] == "critical")
        .collect();
    assert_eq!(critical.len(), 1);
    assert_eq!(t.gw.placed().len(), 1, "nothing was pretended");
    // The session returning is recorded too.
    t.gw.unauthorised
        .store(false, std::sync::atomic::Ordering::SeqCst);
    t.m.process_tick(ATM_CE, "NFO", 123.0).await;
    assert!(t
        .event_kinds(sid)
        .contains(&"recovery_succeeded".to_string()));
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn a_daily_loss_limit_stops_the_run_on_the_session_total() {
    let t = t();
    let sid = t.make(config(
        "DLL",
        json!([short_call_leg()]),
        json!({"daily_loss_limit_inr": 1000}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 110.0).await; // -650: inside
    assert!(t.run(run).stop_requested_reason.is_none());
    t.m.process_tick(ATM_CE, "NFO", 116.0).await; // -1040
    assert_eq!(
        t.run(run).stop_requested_reason.as_deref(),
        Some("daily_loss_limit")
    );
}

#[tokio::test]
async fn trail_to_entry_fires_on_a_stop_driven_exit() {
    let t = t();
    let mut b = short_call_leg();
    b["id"] = json!(2);
    b["atm_offset"] = json!("OTM2");
    b["sl_pts"] = json!(50);
    let sid = t.make(config(
        "TTE",
        json!([short_call_leg(), b]),
        json!({"trail_sl_to_entry": true}),
    ));
    let run = t.start_filled(sid, 100.0).await;
    // Leg 2 in profit first, then leg 1 stops out.
    t.m.process_tick("NIFTY13OCT2624600CE", "NFO", 90.0).await;
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    let s = t.m.state.snapshot(run).unwrap();
    assert!(s.trail_to_entry_active);
    assert_eq!(s.leg(2).unwrap().effective_sl, Some(100.0));
}

#[tokio::test]
async fn a_stepped_trail_ratchets_and_then_fires() {
    let t = t();
    let mut l = short_call_leg();
    l["position"] = json!("B");
    l["trail"] = json!({"x": 10, "y": 5});
    let sid = t.make(config("Trail", json!([l]), json!({})));
    let run = t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 125.0).await; // two steps: 80 + 10
    assert_eq!(t.leg(run, 1).effective_sl, Some(90.0));
    t.m.process_tick(ATM_CE, "NFO", 115.0).await; // ratchet holds
    assert_eq!(t.leg(run, 1).effective_sl, Some(90.0));
    t.m.process_tick(ATM_CE, "NFO", 89.0).await;
    assert_eq!(t.gw.actions(), vec!["BUY", "SELL"]);
}
