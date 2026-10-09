//! web: test/test_strategy_module_order_events.py

use super::*;

#[tokio::test]
async fn a_fill_is_applied_exactly_once() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 100.0).await;
    t.frame(&id, "complete", 65, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    let exit = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap();
    let eid = exit.broker_order_id.unwrap();
    t.frame(&eid, "complete", 65, 90.0).await;
    t.frame(&eid, "complete", 65, 90.0).await;
    assert!((t.run(run).pnl_realized - 650.0).abs() < 1e-6);
}

#[tokio::test]
async fn a_rejection_is_final() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "rejected", 0, 0.0).await;
    t.frame(&id, "open", 0, 0.0).await;
    assert_eq!(t.orders(run)[0].status, "rejected");
    assert_eq!(t.leg(run, 1).status, "rejected");
}

#[tokio::test]
async fn a_zero_price_is_not_a_fill_price() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 0.0).await;
    let leg = t.leg(run, 1);
    assert_eq!(leg.entry_avg, 0.0);
    assert_eq!(leg.status, "open", "the quantity is still managed");
    assert!(t.event_kinds(sid).contains(&"leg_entry_placed".to_string()));
}

#[tokio::test]
async fn the_leg_is_resized_to_what_actually_filled() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "cancelled", 30, 100.0).await;
    assert_eq!(t.leg(run, 1).qty, 30);
    t.m.stop_run(run, USER, "manual").await;
    assert_eq!(t.gw.placed().last().unwrap().quantity, 30);
}

#[tokio::test]
async fn a_partial_fill_then_the_rest_prices_only_the_delta() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 100.0).await;
    t.m.stop_run(run, USER, "manual").await;
    let eid = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap()
        .broker_order_id
        .unwrap();
    t.frame(&eid, "open", 25, 90.0).await;
    assert_eq!(t.leg(run, 1).qty, 40);
    // Cumulative average 94 over 65 means the last 40 traded at 96.5.
    t.frame(&eid, "complete", 65, 94.0).await;
    // realized = (100-90)*25 + (100-96.5)*40 = 250 + 140 = 390
    assert!(
        (t.run(run).pnl_realized - 390.0).abs() < 1e-6,
        "{}",
        t.run(run).pnl_realized
    );
}

#[tokio::test]
async fn a_rejected_exit_on_the_stream_releases_its_claim() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.m.close_leg(run, 1, USER).await;
    let eid = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap()
        .broker_order_id
        .unwrap();
    t.frame(&eid, "rejected", 0, 0.0).await;
    let leg = t.leg(run, 1);
    assert!(leg.exit_kind.is_none() && leg.exit_order_id.is_none());
    assert!(t.m.close_leg(run, 1, USER).await.ok);
}

#[tokio::test]
async fn a_fill_that_beats_its_row_is_held_and_replayed() {
    let t = t();
    let sid = t.default_strategy();
    // The update arrives for an id no row carries yet.
    t.frame("SB-1", "complete", 65, 100.0).await;
    assert_eq!(t.m.order_events.len(), 1);
    let run = t.start(sid).await.run_id.unwrap();
    assert_eq!(t.leg(run, 1).entry_status, "complete");
    assert_eq!(t.leg(run, 1).entry_avg, 100.0);
    assert!(t.m.order_events.is_empty());
}

#[tokio::test]
async fn the_hold_buffer_is_bounded() {
    let t = t();
    for i in 0..2000 {
        t.frame(&format!("OTHER-{}", i), "complete", 1, 1.0).await;
    }
    assert!(t.m.order_events.len() <= 512);
}

#[tokio::test]
async fn a_signal_flips_exit_fill_never_closes_the_new_position() {
    // The defect: a flip squares the long and opens the short at once;
    // applying the long's exit fill by leg alone closed the short, which
    // then vanished from every stop and square-off.
    let t = t();
    let sid = t.make(signal_config(
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({}),
    ));
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    t.m.handle_signal(&s, "long_entry", Some(&json!(1)), None, None)
        .await;
    let run =
        t.m.store
            .get_strategy(sid, USER)
            .unwrap()
            .unwrap()
            .current_run_id
            .unwrap();
    let entry = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&entry, "complete", 10, 1000.0).await;
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    assert!(
        t.m.handle_signal(&s, "short_entry", Some(&json!(1)), None, None)
            .await
            .flipped
    );
    let rows = t.orders(run);
    let long_exit = rows.iter().find(|o| o.kind == "exit_signal").unwrap();
    let short_entry = rows.iter().rev().find(|o| o.kind == "entry").unwrap();
    // The new short fills, then the OLD long's exit fills.
    t.frame(
        short_entry.broker_order_id.as_deref().unwrap(),
        "complete",
        10,
        990.0,
    )
    .await;
    t.frame(
        long_exit.broker_order_id.as_deref().unwrap(),
        "complete",
        10,
        990.0,
    )
    .await;
    let leg = t.leg(run, 1);
    assert_eq!(leg.position, "S");
    assert_eq!(leg.status, "open", "the new short is still managed");
    assert_eq!(leg.qty, 10);
    assert!(leg.superseded.is_none(), "the outgoing long settled");
    assert!((leg.realized_pnl + 100.0).abs() < 1e-9);
    // And it is still exitable by its stop.
    t.m.process_tick("RELIANCE", "NSE", 1011.0).await;
    assert_eq!(t.gw.actions(), vec!["BUY", "SELL", "SELL", "BUY"]);
}

#[tokio::test]
async fn a_flips_refused_outgoing_exit_leaves_the_old_side_closable() {
    let t = t();
    let sid = t.make(signal_config(
        json!([signal_leg(1, "RELIANCE", "both")]),
        json!({}),
    ));
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    t.m.handle_signal(&s, "long_entry", Some(&json!(1)), None, None)
        .await;
    let run =
        t.m.store
            .get_strategy(sid, USER)
            .unwrap()
            .unwrap()
            .current_run_id
            .unwrap();
    let entry = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&entry, "complete", 10, 1000.0).await;
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    t.m.handle_signal(&s, "short_entry", Some(&json!(1)), None, None)
        .await;
    let long_exit = t
        .orders(run)
        .into_iter()
        .find(|o| o.kind == "exit_signal")
        .unwrap();
    t.frame(
        long_exit.broker_order_id.as_deref().unwrap(),
        "rejected",
        0,
        0.0,
    )
    .await;
    let sup = t.leg(run, 1).superseded.unwrap();
    assert!(sup.exit_order_id.is_none() && sup.exit_claim_token.is_none());
    assert!(t
        .event_kinds(sid)
        .contains(&"flip_outgoing_exit_rejected".to_string()));
    // A long_exit now closes the outgoing long rather than reading flat.
    let s = t.m.store.get_strategy(sid, USER).unwrap().unwrap();
    let r =
        t.m.handle_signal(&s, "long_exit", Some(&json!(1)), None, None)
            .await;
    assert!(r.acted(), "{:?}", r);
    assert_eq!(t.gw.actions().last().unwrap(), "SELL");
}
