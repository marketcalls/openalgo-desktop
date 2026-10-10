//! Barrier-synchronised proofs of the order-path invariants (web
//! test/test_gthread_strategy_*.py).

use super::*;
use openalgo_desktop_lib::strategy::state::{
    new_leg_state, ClaimId, LegSpec, RunState, StateRegistry,
};
use openalgo_desktop_lib::strategy::store::{
    ClaimOutcome, CHANGED_WHILE_STARTING, LIVE_NOT_ENABLED, WEBHOOK_LOCKED,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Barrier};

#[test]
fn two_rules_racing_on_one_leg_claim_it_exactly_once() {
    for _ in 0..200 {
        let reg = Arc::new(StateRegistry::new());
        let mut leg = new_leg_state(&LegSpec {
            leg_id: 1,
            position: "B".into(),
            symbol: "X".into(),
            exchange: "NFO".into(),
            quantity: 1,
            ..Default::default()
        })
        .unwrap();
        leg.status = "open".into();
        leg.entry_status = "complete".into();
        reg.install(RunState::new(1, 1, vec![leg]));
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (reg, barrier) = (reg.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    if i % 2 == 0 {
                        reg.claim_leg_exit(1, 1, "exit_sl").is_some() as usize
                    } else {
                        reg.claim_legs_for_exit(1, &[1], "exit_close_all").0.len()
                    }
                })
            })
            .collect();
        let wins: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(wins, 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_loss_and_a_manual_close_send_exactly_one_exit() {
    for _ in 0..10 {
        let t = t();
        let sid = t.default_strategy();
        let run = t.start_filled(sid, 100.0).await;
        // Widen the window: the first dispatch is in flight for a while.
        t.gw.delay_ms.store(30, Ordering::SeqCst);
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let (m1, m2, m3) = (t.m.clone(), t.m.clone(), t.m.clone());
        let (b1, b2, b3) = (barrier.clone(), barrier.clone(), barrier.clone());
        let a = tokio::spawn(async move {
            b1.wait().await;
            m1.process_tick(ATM_CE, "NFO", 125.0).await;
        });
        let b = tokio::spawn(async move {
            b2.wait().await;
            m2.close_leg(run, 1, USER).await;
        });
        let c = tokio::spawn(async move {
            b3.wait().await;
            m3.stop_run(run, USER, "manual").await;
        });
        let _ = tokio::join!(a, b, c);
        let exits: Vec<_> = t
            .orders(run)
            .into_iter()
            .filter(|o| o.kind != "entry")
            .collect();
        let sent: Vec<_> = exits.iter().filter(|o| o.status != "rejected").collect();
        assert_eq!(sent.len(), 1, "{:?}", exits);
        assert_eq!(t.gw.actions().iter().filter(|a| *a == "BUY").count(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_dispatch_releases_the_claim() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.reject_next("Broker busy");
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    let leg = t.leg(run, 1);
    assert!(
        leg.exit_kind.is_none() && leg.exit_claim_token.is_none() && leg.exit_order_id.is_none()
    );
    // The very next tick through the stop is not mistaken for a duplicate.
    t.m.process_tick(ATM_CE, "NFO", 122.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
    // And a release by the wrong claim id is refused.
    assert!(!t.m.state.release_leg_exit(run, 1, &ClaimId::Row(-1)));
    assert!(t.leg(run, 1).exit_order_id.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starts_place_one_set_of_entries() {
    let t = t();
    let sid = t.default_strategy();
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut handles = vec![];
    for _ in 0..4 {
        let (m, b) = (t.m.clone(), barrier.clone());
        handles.push(tokio::spawn(async move {
            b.wait().await;
            m.start_run(sid, USER, "sandbox", "manual", None).await.ok
        }));
    }
    let mut oks = 0;
    for h in handles {
        if h.await.unwrap() {
            oks += 1;
        }
    }
    assert_eq!(oks, 1);
    assert_eq!(t.gw.placed().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_alerts_on_one_bar_join_one_signal_run() {
    let t = t();
    let sid = t.make(signal_config(
        json!([
            signal_leg(1, "RELIANCE", "both"),
            signal_leg(2, "SBIN", "both")
        ]),
        json!({}),
    ));
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = vec![];
    for leg in [1, 2] {
        let (m, b, s) = (t.m.clone(), barrier.clone(), s.clone());
        handles.push(tokio::spawn(async move {
            b.wait().await;
            m.handle_signal(&s, "long_entry", Some(&json!(leg)), None, None)
                .await
        }));
    }
    for h in handles {
        assert!(h.await.unwrap().acted());
    }
    assert_eq!(t.m.store.list_runs(sid, 10).unwrap().len(), 1);
    assert_eq!(t.gw.placed().len(), 2);
}

// ------------------------------------------------- management versus start (SM-01)

/// Start in the background and hold it on its leg price, where a real start
/// waits on the broker. The caller acts, then lets it finish.
async fn held_start(
    t: &T,
    sid: i64,
    mode: &'static str,
    trigger: &'static str,
) -> (
    Arc<Gate>,
    tokio::task::JoinHandle<openalgo_desktop_lib::strategy::StartResult>,
) {
    let gate = t.gw.gate_ltp();
    let m = t.m.clone();
    let start = tokio::spawn(async move { m.start_run(sid, USER, mode, trigger, None).await });
    gate.entered().await;
    (gate, start)
}

/// Nothing was placed and no run exists; the strategy is still stopped.
fn assert_never_started(t: &T, sid: i64) {
    assert!(t.gw.placed().is_empty(), "{:?}", t.gw.placed());
    assert!(t.m.store.list_runs(sid, 10).unwrap().is_empty());
    let row = t.row(sid);
    assert_eq!((row.status.as_str(), row.current_run_id), ("stopped", None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabling_live_while_a_live_start_waits_refuses_the_start() {
    let t = t();
    let sid = t.default_strategy();
    t.m.store.set_live_enabled(sid, USER, true).unwrap();
    let (gate, start) = held_start(&t, sid, "live", "scheduler").await;
    // The trader clicks "Disable live"; the app confirms it.
    t.m.store.set_live_enabled(sid, USER, false).unwrap();
    gate.release();
    let r = start.await.unwrap();
    assert!(!r.ok, "{:?}", r);
    assert_eq!(r.error.as_deref(), Some(LIVE_NOT_ENABLED));
    assert_never_started(&t, sid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_while_a_start_waits_refuses_the_start() {
    let t = t();
    let sid = t.default_strategy();
    let (gate, start) = held_start(&t, sid, "sandbox", "manual").await;
    // The edit lands; the start must not run the old legs.
    let mut leg = short_call_leg();
    leg["lots"] = json!(4);
    let mut ch = serde_json::Map::new();
    ch.insert("legs".into(), json!([leg]));
    t.m.store
        .update_strategy(sid, USER, t.row(sid).revision, &ch)
        .unwrap();
    gate.release();
    let r = start.await.unwrap();
    assert!(!r.ok, "{:?}", r);
    assert_eq!(r.error.as_deref(), Some(CHANGED_WHILE_STARTING));
    assert_never_started(&t, sid);
    // Started again, it runs the saved legs.
    *t.gw.ltp_gate.lock() = None;
    let r = t.start(sid).await;
    assert!(r.ok, "{:?}", r);
    assert_eq!(t.gw.placed()[0].quantity, 4 * 65);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kill_switch_while_a_webhook_start_waits_refuses_the_start() {
    let t = t();
    let sid = t.default_strategy();
    let (gate, start) = held_start(&t, sid, "sandbox", "webhook").await;
    // The kill switch locks the webhook and finds no run to flatten.
    t.m.store.set_webhook_locked(sid, USER, true).unwrap();
    assert!(t.row(sid).current_run_id.is_none());
    gate.release();
    let r = start.await.unwrap();
    assert!(!r.ok, "{:?}", r);
    assert_eq!(r.error.as_deref(), Some(WEBHOOK_LOCKED));
    assert_never_started(&t, sid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_while_a_start_waits_places_nothing() {
    let t = t();
    let sid = t.default_strategy();
    let (gate, start) = held_start(&t, sid, "sandbox", "manual").await;
    t.m.store.delete_strategy(sid, USER).unwrap();
    gate.release();
    let r = start.await.unwrap();
    assert!(!r.ok, "{:?}", r);
    assert!(t.gw.placed().is_empty());
    assert!(t.m.store.get_strategy(sid, USER).unwrap().is_none());
}

#[test]
fn a_claimed_strategy_refuses_live_changes_edits_and_deletes() {
    let t = t();
    let sid = t.default_strategy();
    assert_eq!(t.claim(sid), ClaimOutcome::Claimed);
    let rev = t.row(sid).revision;
    assert!(t.m.store.set_live_enabled(sid, USER, true).is_err());
    let mut ch = serde_json::Map::new();
    ch.insert("overall_sl_mtm".into(), json!(100));
    assert!(t.m.store.update_strategy(sid, USER, rev, &ch).is_err());
    assert!(t.m.store.delete_strategy(sid, USER).is_err());
    let row = t.row(sid);
    assert_eq!((row.live_enabled, row.revision), (false, rev));
}
