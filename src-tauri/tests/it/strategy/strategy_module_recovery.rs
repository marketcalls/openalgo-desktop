//! web: test/test_strategy_module_recovery.py

use super::*;
use openalgo_desktop_lib::strategy::checkpoint::write_once;
use openalgo_desktop_lib::strategy::recovery::{
    normalise_order_status, order_is_dead, order_is_filled, order_is_working, recover_all,
    recover_run,
};

#[test]
fn a_working_broker_status_reads_the_same_way_everywhere() {
    // PORTED DEFECT. Two normalisers disagreed on exactly these: one read
    // them as live orders, the other as unknown and therefore dead.
    for s in [
        "submitted",
        "trigger_pending",
        "TRIGGER PENDING",
        "modified",
    ] {
        assert!(order_is_working(s), "{}", s);
        assert!(!order_is_filled(s) && !order_is_dead(s));
    }
}

#[test]
fn an_unrecognised_status_is_read_as_working_rather_than_dead() {
    assert_eq!(normalise_order_status("some-new-broker-word"), "open");
    assert!(!order_is_dead("some-new-broker-word"));
    assert_eq!(normalise_order_status("Filled"), "complete");
    assert_eq!(normalise_order_status("canceled"), "cancelled");
}

/// Simulate a restart: drop the in-memory state, keep the database.
fn crash(t: &T, run: i64) {
    t.m.state.clear(run);
}

#[tokio::test]
async fn a_held_run_is_rebuilt_from_its_orders_and_checkpoint() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 95.0).await;
    assert_eq!(write_once(&t.m, Some(false)), 1);
    crash(&t, run);
    let r = recover_run(&t.m, run).await;
    assert!(r.ok, "{:?}", r);
    assert_eq!(r.symbols, vec![(ATM_CE.to_string(), "NFO".to_string())]);
    let leg = t.leg(run, 1);
    assert_eq!(leg.position, "S", "the side comes from the entry action");
    assert_eq!(leg.entry_avg, 100.0);
    assert_eq!(leg.qty, 65);
    assert_eq!(leg.sl_pts, Some(20.0));
    assert_eq!(
        leg.lowest_price,
        Some(95.0),
        "volatile state from the checkpoint"
    );
    // And it is managed: its stop still fires.
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn a_run_that_died_flat_is_finished_at_startup() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "rejected", 0, 0.0).await;
    crash(&t, run);
    let resumed = recover_all(&t.m).await.unwrap();
    assert!(resumed.is_empty());
    assert!(t.run(run).stopped_at.is_some());
    assert_eq!(
        t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
        "stopped"
    );
}

#[tokio::test]
async fn a_dead_order_is_never_upgraded_by_a_checkpoint() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await; // checkpoint will say complete
    write_once(&t.m, Some(false));
    t.m.store
        .execute_raw(&format!(
            "UPDATE sm_strategy_order SET status = 'rejected', filled_qty = 0 WHERE run_id = {}",
            run
        ))
        .unwrap();
    crash(&t, run);
    let r = recover_run(&t.m, run).await;
    assert!(r.finalised, "{:?}", r);
}

#[tokio::test]
async fn a_working_exit_without_a_confirmed_owner_stays_reserved() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    // An exit row that is working while the entry never filled.
    t.m.store
        .execute_raw(&format!(
            "INSERT INTO sm_strategy_order (run_id, leg_id, kind, position_ref, broker_order_id, symbol, exchange, action, qty, pricetype, status, placed_at) \
             SELECT run_id, leg_id, 'exit_sl', position_ref, 'X-1', symbol, exchange, 'BUY', qty, 'MARKET', 'open', placed_at FROM sm_strategy_order WHERE run_id = {}",
            run
        ))
        .unwrap();
    crash(&t, run);
    let r = recover_run(&t.m, run).await;
    assert!(!r.ok && !r.finalised, "{:?}", r);
    assert!(
        t.run(run).stopped_at.is_none(),
        "not finalised over possible exposure"
    );
    assert!(t.event_kinds(sid).contains(&"recovery_failed".to_string()));
}

#[tokio::test]
async fn recovery_releases_an_empty_claim_when_a_process_dies_before_run_linkage() {
    let t = t();
    let sid = t.default_strategy();
    assert_eq!(
        t.claim(sid),
        openalgo_desktop_lib::strategy::store::ClaimOutcome::Claimed
    );
    let run =
        t.m.store
            .create_run(sid, "sandbox", "sandbox", "manual", None, None)
            .unwrap();
    let r = recover_run(&t.m, run).await;
    assert!(r.finalised);
    assert!(t.run(run).stopped_at.is_some());
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    assert_eq!((s.status.as_str(), s.current_run_id), ("stopped", None));
}

#[tokio::test]
async fn recovery_is_idempotent_over_live_state() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    let before = t.m.state.snapshot(run).unwrap();
    assert!(recover_run(&t.m, run).await.ok);
    assert_eq!(t.m.state.snapshot(run).unwrap(), before);
}

#[tokio::test]
async fn a_durable_pending_stop_is_recovered_with_its_reason() {
    let t = t();
    let sid = t.default_strategy();
    // Filled through the broker-frame path so the fill is durable.
    let run = t.start(sid).await.run_id.unwrap();
    let entry = t.orders(run)[0].clone();
    t.frame(
        entry.broker_order_id.as_deref().unwrap(),
        "complete",
        entry.qty,
        100.0,
    )
    .await;
    t.gw.reject_next("Rate limited");
    t.m.stop_run(run, USER, "overall_sl").await;
    crash(&t, run);
    assert!(recover_run(&t.m, run).await.ok);
    assert!(t.m.state.snapshot(run).unwrap().stopping);
    let r = t.m.reconcile_pending_stop(run).await.unwrap();
    assert!(r.ok, "{:?}", r);
    assert!(
        r.stop_pending && r.exits.iter().any(|e| e["ok"] == json!(true)),
        "the retry placed the exit: {:?}",
        r
    );
    t.fill_last_exit(run, 100.0).await;
    assert_eq!(t.run(run).stop_reason.as_deref(), Some("overall_sl"));
}
