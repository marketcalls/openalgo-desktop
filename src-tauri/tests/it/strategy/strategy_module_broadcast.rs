//! web: test/test_strategy_module_broadcast.py

use super::*;
use openalgo_desktop_lib::strategy::broadcast::room_for;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn frames_go_to_the_strategy_room_with_the_envelope() {
    let t = t();
    t.rooms.watching.store(true, Ordering::SeqCst);
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    t.fill_last_exit(run, 90.0).await;
    let frames = t.rooms.frames.lock().clone();
    assert!(frames.iter().all(|f| f.0 == room_for(sid)));
    let kinds: std::collections::HashSet<String> = frames.iter().map(|f| f.1.clone()).collect();
    for k in [
        "strategy_event",
        "strategy_delta",
        "strategy_order_update",
        "strategy_run_update",
        "strategy_terminal",
    ] {
        assert!(kinds.contains(k), "missing {}", k);
    }
    let terminal = frames.iter().find(|f| f.1 == "strategy_terminal").unwrap();
    assert_eq!(terminal.2["type"], "terminal");
    assert_eq!(terminal.2["strategy_id"], sid);
    assert_eq!(terminal.2["run_id"], run);
    assert_eq!(terminal.2["stop_reason"], "manual");
    assert_eq!(terminal.2["pnl_realized"], 650.0);
    assert!(terminal.2["ts_ms"].as_i64().unwrap() > 0);
    assert!(terminal.2["ts"].as_str().unwrap().ends_with("+05:30"));
    assert_eq!(
        t.m.broadcast.tracked(),
        0,
        "terminal drops the throttle entry"
    );
}

#[tokio::test]
async fn an_unwatched_run_sends_nothing() {
    let t = t();
    let sid = t.default_strategy();
    t.start_filled(sid, 100.0).await;
    t.m.process_tick(ATM_CE, "NFO", 101.0).await;
    assert!(t.rooms.frames.lock().is_empty());
}

#[tokio::test]
async fn deltas_are_throttled_and_one_offs_are_not() {
    let t = t();
    t.rooms.watching.store(true, Ordering::SeqCst);
    let sid = t.default_strategy();
    t.start_filled(sid, 100.0).await;
    t.rooms.frames.lock().clear();
    for p in 0..20 {
        t.m.process_tick(ATM_CE, "NFO", 101.0 + p as f64 * 0.05)
            .await;
    }
    let deltas = t
        .rooms
        .events()
        .iter()
        .filter(|e| *e == "strategy_delta")
        .count();
    assert!((1..20).contains(&deltas), "{} deltas", deltas);
}

#[tokio::test]
async fn the_snapshot_carries_every_leg_in_id_order() {
    let t = t();
    let mut b = short_call_leg();
    b["id"] = json!(2);
    let sid = t.make(config("Snap", json!([b, short_call_leg()]), json!({})));
    let run = t.start(sid).await.run_id.unwrap();
    let p =
        t.m.broadcast
            .snapshot_payload(&t.m.state.snapshot(run).unwrap());
    assert_eq!(p["type"], "snapshot");
    let ids: Vec<i64> = p["legs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["leg_id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2]);
    for k in [
        "mtm_realized",
        "mtm_unrealized",
        "mtm_total",
        "peak",
        "trough",
        "lock_armed",
        "lock_floor",
        "trail_to_entry_active",
        "tick_source_degraded",
    ] {
        assert!(p.get(k).is_some(), "{}", k);
    }
    for k in [
        "symbol",
        "position",
        "qty",
        "status",
        "entry_status",
        "ltp",
        "entry_avg",
        "mtm",
        "effective_sl",
        "favorable_points",
        "tick_source",
    ] {
        assert!(p["legs"][0].get(k).is_some(), "{}", k);
    }
}
