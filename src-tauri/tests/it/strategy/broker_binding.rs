//! web: test/test_strategy_module_broker_binding.py (marketcalls/openalgo#2337,
//! eec700046). A Live run's orders go to the broker session it started on,
//! or nowhere (LOG-07 already held that for a run that recorded its
//! broker); these cover what the web added: a run opened before any login
//! is bound by its first order, a run that sent orders before it recorded
//! its broker is bound to the session that took them or to none, a stop
//! and a breach are said once however often they are retried, and a
//! sign-in to another broker is refused while a Live run trades elsewhere.

use super::*;
use chrono::Duration as Span;
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::services::broker_switch;
use openalgo_desktop_lib::services::live_owner::LiveOwner;
use openalgo_desktop_lib::services::obligations::open_live_obligations;
use openalgo_desktop_lib::strategy::store::StrategyRow;

fn signal_strategy(t: &T) -> StrategyRow {
    let sid = t.make(signal_config(
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({}),
    ));
    t.m.store.set_live_enabled(sid, USER, true).unwrap();
    t.row(sid)
}

async fn signal(
    t: &T,
    s: &StrategyRow,
    action: &str,
) -> openalgo_desktop_lib::strategy::signals::SignalResult {
    let fresh = t.row(s.id);
    t.m.handle_signal(&fresh, action, Some(&json!(1)), None, None)
        .await
}

/// A run holding a short ATM call (entry filled at 100), Live on Zerodha
/// account AB1234.
async fn live_on_zerodha(t: &T) -> (i64, i64) {
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_run SET mode = 'live', broker = 'zerodha', \
             broker_account = 'AB1234' WHERE id = {}",
            run
        ))
        .unwrap();
    // The run's tick inputs were read when its prices were subscribed.
    t.m.tick_contexts.forget(run);
    t.gw.sign_in("zerodha", "AB1234", t.clock.now());
    (sid, run)
}

fn count(t: &T, sid: i64, kind: &str) -> usize {
    t.event_kinds(sid).iter().filter(|k| *k == kind).count()
}

/// Web `test_a_run_opened_before_any_login_is_bound_by_its_first_live_order`:
/// a signal run opened while no broker was logged in records `live`, and
/// its first Live order binds it to the session placing it, once; a later
/// session (another broker) then reaches none of its orders.
#[tokio::test]
async fn a_run_opened_before_any_login_is_bound_by_its_first_live_order() {
    let t = t();
    let s = signal_strategy(&t);
    t.gw.sign_out();
    let r = signal(&t, &s, "long_entry").await;
    assert!(!r.acted(), "no session, nothing sent: {:?}", r);
    let run = t.row(s.id).current_run_id.expect("the signal run is open");
    assert_eq!(t.run(run).broker, "live");
    assert!(t.gw.placed().is_empty());

    t.gw.sign_in("zerodha", "AB1234", t.clock.now());
    let r = signal(&t, &s, "long_entry").await;
    assert!(r.acted(), "{:?}", r);
    let row = t.run(run);
    assert_eq!(
        (row.broker.as_str(), row.broker_account.as_str()),
        ("zerodha", "AB1234")
    );
    // Bound once: neither a racing order nor a later session rebinds it.
    let other = LiveOwner::new("fyers", "FY1");
    assert!(!t.m.store.bind_run_owner(run, &other).unwrap());

    t.m.apply_fill(run, 1, Some(1000.0), true, FillOpts::default())
        .await;
    t.gw.sign_in("fyers", "FY1", t.clock.now());
    let r = signal(&t, &s, "long_exit").await;
    assert!(!r.acted(), "{:?}", r);
    assert!(
        r.error
            .as_deref()
            .is_some_and(|e| e.contains("opened on Zerodha (AB1234)")),
        "{:?}",
        r
    );
    assert_eq!(t.gw.actions(), vec!["BUY"], "the exit never reached Fyers");
    assert_eq!(t.run(run).broker, "zerodha");
}

/// Web `test_a_check_never_binds_a_run_that_has_sent_nothing`: only an order
/// binds a run to a session; a stop of a run with nothing at any broker
/// neither binds nor refuses it.
#[tokio::test]
async fn a_check_never_binds_a_run_that_has_sent_nothing() {
    let t = t();
    let s = signal_strategy(&t);
    t.gw.sign_out();
    signal(&t, &s, "long_entry").await;
    let run = t.row(s.id).current_run_id.unwrap();
    t.gw.sign_in("zerodha", "AB1234", t.clock.now());
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(r.ok, "{:?}", r);
    assert_eq!(t.run(run).broker, "live");
    assert!(t.gw.placed().is_empty());
}

/// A run open across the upgrade that added the binding: an order went
/// out (the entry) while it recorded `live`. Web
/// `test_a_run_that_sent_orders_before_recording_its_broker_is_bound_to_the_broker_that_took_them`:
/// the desktop keeps no history of broker sign-ins, so the broker that took
/// the order is the one connected now only when it was signed in before
/// that order (one broker at a time: it was in place then).
#[tokio::test]
async fn a_run_that_sent_orders_before_recording_its_broker_is_bound_to_the_session_that_took_them()
{
    let t = t();
    let (sid, run) = live_on_zerodha(&t).await;
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_run SET broker = 'live', broker_account = '' WHERE id = {}",
            run
        ))
        .unwrap();
    t.gw.sign_in("zerodha", "AB1234", t.clock.now() - Span::hours(1));
    let r = t.m.close_leg(run, 1, USER).await;
    assert!(r.ok, "{:?}", r);
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    let row = t.run(run);
    assert_eq!(
        (row.broker.as_str(), row.broker_account.as_str()),
        ("zerodha", "AB1234")
    );
    assert_eq!(count(&t, sid, "leg_exit_rejected"), 0);
}

/// Web `test_a_run_whose_earlier_broker_is_not_on_record_stays_unbound_and_sends_nothing`:
/// signed in after the run's first order, the session connected now cannot
/// be shown to have taken it, so nothing is sent and the run stays unbound.
#[tokio::test]
async fn a_run_whose_earlier_broker_is_not_known_stays_unbound_and_sends_nothing() {
    let t = t();
    let (_sid, run) = live_on_zerodha(&t).await;
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_run SET broker = 'live', broker_account = '' WHERE id = {}",
            run
        ))
        .unwrap();
    t.gw.sign_in("fyers", "FY1", t.clock.now() + Span::hours(1));
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(!r.ok && r.stop_pending, "{:?}", r);
    assert!(
        r.error.as_deref().is_some_and(
            |e| e.contains("could not confirm which broker this strategy's run trades on")
        ),
        "{:?}",
        r
    );
    assert_eq!(t.gw.actions(), vec!["SELL"], "nothing reached Fyers");
    assert_eq!(t.run(run).broker, "live");
}

/// Web `test_a_pending_stop_says_it_was_requested_once_however_often_it_is_retried`:
/// a stop refused because another broker is connected stays pending and is
/// retried every few seconds; "Stop requested" is written once, not on every
/// retry, and the refusal once too. Back on its own session it completes.
#[tokio::test]
async fn a_pending_stop_says_it_was_requested_once_however_often_it_is_retried() {
    let t = t();
    let (sid, run) = live_on_zerodha(&t).await;
    t.gw.sign_in("fyers", "FY1", t.clock.now());
    let r = t.m.stop_run(run, USER, "manual").await;
    assert!(!r.ok && r.stop_pending, "{:?}", r);
    for _ in 0..5 {
        t.m.reconcile_pending_stop(run).await;
    }
    assert_eq!(count(&t, sid, "run_stop_requested"), 1);
    assert_eq!(count(&t, sid, "run_stop_failed"), 1);
    assert_eq!(t.run(run).stop_requested_reason.as_deref(), Some("manual"));
    assert_eq!(t.gw.actions(), vec!["SELL"]);

    t.gw.sign_in("zerodha", "AB1234", t.clock.now());
    t.m.reconcile_pending_stop(run).await;
    assert_eq!(count(&t, sid, "run_stop_requested"), 1);
    assert_eq!(
        t.gw.actions(),
        vec!["SELL", "BUY"],
        "the cover went to Zerodha"
    );
}

/// Web `test_a_stop_hit_while_logged_in_elsewhere_is_reported_once_and_sends_nothing`:
/// prices keep arriving from the session connected now, so the stop fires
/// on every tick; the breach and the refusal are each written once, and
/// nothing is sent until the run's own session is back.
#[tokio::test]
async fn a_stop_hit_while_logged_in_elsewhere_is_reported_once_and_sends_nothing() {
    let t = t();
    let (sid, _run) = live_on_zerodha(&t).await;
    t.gw.sign_in("fyers", "FY1", t.clock.now());
    for _ in 0..4 {
        t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    }
    assert_eq!(t.gw.actions(), vec!["SELL"], "nothing reached Fyers");
    let rejected: Vec<Value> = t
        .events(sid)
        .into_iter()
        .filter(|e| e["kind"] == "leg_exit_rejected")
        .collect();
    assert_eq!(rejected.len(), 1, "{:?}", rejected);
    assert_eq!(rejected[0]["severity"], "critical");
    assert_eq!(count(&t, sid, "leg_sl_hit"), 1, "the breach is said once");

    t.gw.sign_in("zerodha", "AB1234", t.clock.now());
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    assert!(t
        .event_kinds(sid)
        .contains(&"recovery_succeeded".to_string()));
}

// ------------------------------------------------- the broker switch guard

/// The App harness's Live run (Zerodha, AB1234), entry filled.
async fn app_live_run(a: &App) -> i64 {
    let cfg = config("Live", json!([short_call_leg()]), json!({}));
    let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
    a.ctx
        .strategy
        .store
        .set_live_enabled(row.id, USER, true)
        .unwrap();
    a.ctx.sqlite.set_analyze_mode(false).unwrap();
    let r = a
        .ctx
        .strategy
        .start_run(row.id, USER, "live", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    let run = r.run_id.unwrap();
    a.ctx
        .strategy
        .apply_fill(run, 1, Some(100.0), true, FillOpts::default())
        .await;
    run
}

/// Web `test_a_login_to_another_broker_is_refused_while_a_live_run_trades_elsewhere`
/// (AUDIT R02): after the broker session ended (a sign-out, the daily
/// boundary) no session is there to ask, and a sign-in to another broker
/// went ahead under a Live run. The run records its broker, and that holds
/// the sign-in now; signing back in to that broker is allowed.
#[tokio::test]
async fn a_sign_in_to_another_broker_is_refused_while_a_live_run_trades_elsewhere() {
    let a = app();
    a.ctx.reconciler.stop();
    let _run = app_live_run(&a).await;
    a.ctx.set_broker_session(None);
    let e = broker_switch::check_sign_in(&a.ctx, "fyers").unwrap_err();
    assert!(
        e.client_message()
            .starts_with("Zerodha still has 1 running Live strategy."),
        "{}",
        e.client_message()
    );
    assert!(broker_switch::check_sign_in(&a.ctx, "zerodha").is_ok());
}

/// Web `test_a_run_not_yet_bound_holds_no_login_back`: a run that records
/// `live` has sent nothing anywhere, so it holds no sign-in back; with a
/// session connected it counts as that session's (its next order binds it
/// there), where before the `live` it records read as another broker.
#[tokio::test]
async fn a_run_not_yet_bound_holds_no_login_back_and_counts_for_the_session() {
    let a = app();
    a.ctx.reconciler.stop();
    let run = app_live_run(&a).await;
    let conn = a.ctx.sqlite.conn().unwrap();
    conn.execute(
        "UPDATE sm_strategy_run SET broker = 'live', broker_account = '' WHERE id = ?1",
        [run],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        open_live_obligations(&a.ctx, "zerodha")
            .unwrap()
            .strategy_runs,
        1
    );
    a.ctx.set_broker_session(None);
    assert!(broker_switch::check_sign_in(&a.ctx, "fyers").is_ok());
    assert!(broker_switch::check_sign_in(&a.ctx, "zerodha").is_ok());
}
