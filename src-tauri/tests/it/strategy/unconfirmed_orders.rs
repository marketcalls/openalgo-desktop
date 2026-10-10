//! LOG-08: a placement with no definite answer is neither accepted nor
//! refused. Its claim holds, nothing is placed again, and only the order
//! reconciler settles it from the broker's order book.

use super::*;
use openalgo_desktop_lib::brokers::mock::{AfterSend, MockCall};
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::services::order_reconciler::{BookSnapshot, ABSENT_READS, ABSENT_SPAN_SECS};
use openalgo_desktop_lib::strategy::dispatch::{BookOrder, RunMode};
use openalgo_desktop_lib::strategy::recovery::recover_run;

fn book(rows: Vec<Value>, now: chrono::DateTime<chrono::Utc>) -> BookSnapshot {
    BookSnapshot::new(rows.iter().filter_map(BookOrder::from_row).collect(), now)
}

/// A broker order-book row, as the gateway reads it.
fn broker_row(id: &str, action: &str, qty: i64, status: &str, price: f64) -> Value {
    json!({
        "orderid": id, "symbol": ATM_CE, "exchange": "NFO", "action": action,
        "quantity": qty, "product": "NRML", "order_status": status,
        "filled_quantity": if status == "complete" { qty } else { 0 },
        "average_price": price, "timestamp": "",
    })
}

fn exits(t: &T, run: i64) -> Vec<openalgo_desktop_lib::strategy::store::OrderRow> {
    t.orders(run).into_iter().filter(|o| o.kind != "entry").collect()
}

#[tokio::test]
async fn an_uncertain_exit_is_never_sent_again_while_the_stop_keeps_breaching() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.uncertain_next(None);
    // The stop breaches; the exit's answer is lost.
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    // Ten seconds of breaching ticks, the pending-stop retry included.
    for i in 0..20 {
        t.clock.advance(chrono::Duration::milliseconds(500));
        t.m.process_tick(ATM_CE, "NFO", 121.0 + i as f64).await;
    }
    t.m.reconcile_pending_stops().await;
    let r = t.m.close_leg(run, 1, USER).await;
    assert!(!r.ok, "a manual close while the exit is unconfirmed is refused");
    assert_eq!(
        t.gw.actions(),
        vec!["SELL", "BUY"],
        "exactly one exit while its outcome is unknown"
    );
    let x = exits(&t, run);
    assert_eq!(x.len(), 1);
    assert_eq!(x[0].status, "unconfirmed", "never recorded as rejected");
    assert!(t.leg(run, 1).exit_order_id.is_some(), "the claim holds");
    assert!(t.event_kinds(sid).contains(&"order_unconfirmed".to_string()));
}

#[tokio::test]
async fn the_book_settles_an_uncertain_exit_once_and_the_leg_closes() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.uncertain_next(None);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    // The broker did take it, and it filled; an older identical order that
    // is somebody else's is ignored by its time.
    let now = t.clock.now();
    let mut old = broker_row("OLD-1", "BUY", 65, "complete", 90.0);
    old["timestamp"] = json!("2026-10-07 09:15:00");
    let snap = book(
        vec![old, broker_row("BRK-9", "BUY", 65, "complete", 121.0)],
        now,
    );
    assert_eq!(t.m.reconcile_book(RunMode::Sandbox, &snap).await, 0);
    let x = exits(&t, run);
    assert_eq!(x[0].broker_order_id.as_deref(), Some("BRK-9"));
    assert_eq!(x[0].status, "complete");
    assert!(t.run(run).stopped_at.is_some(), "flat: the run is finished");
    // A second read of the same fact books nothing more.
    assert_eq!(t.m.reconcile_book(RunMode::Sandbox, &snap).await, 0);
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
    assert!(t
        .event_kinds(sid)
        .contains(&"order_unconfirmed_resolved".to_string()));
}

#[tokio::test]
async fn an_uncertain_exit_absent_from_repeated_reads_frees_the_leg_to_exit_again() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.uncertain_next(None);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    let start = t.clock.now();
    let step = ABSENT_SPAN_SECS / i64::from(ABSENT_READS);
    for i in 0..ABSENT_READS {
        let at = start + chrono::Duration::seconds(step * i64::from(i));
        t.m.reconcile_book(RunMode::Sandbox, &book(vec![], at)).await;
        assert!(
            t.leg(run, 1).exit_order_id.is_some(),
            "one empty read is not proof"
        );
    }
    let at = start + chrono::Duration::seconds(ABSENT_SPAN_SECS + 1);
    assert_eq!(t.m.reconcile_book(RunMode::Sandbox, &book(vec![], at)).await, 0);
    let leg = t.leg(run, 1);
    assert!(leg.exit_order_id.is_none() && leg.exit_claim_token.is_none());
    assert_eq!(exits(&t, run)[0].status, "rejected");
    // Managed again: the stop sends a new exit.
    t.m.process_tick(ATM_CE, "NFO", 122.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
}

#[tokio::test]
async fn an_uncertain_exit_keeps_its_claim_across_a_restart() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start(sid).await.run_id.unwrap();
    // The entry's fill is durable (it came through the order-update path).
    let id = t.orders(run)[0].broker_order_id.clone().unwrap();
    t.frame(&id, "complete", 65, 100.0).await;
    t.gw.uncertain_next(None);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    // The process dies; the run is rebuilt from the database.
    t.m.state.clear(run);
    let r = recover_run(&t.m, run).await;
    assert!(r.ok, "{:?}", r);
    assert!(t.leg(run, 1).exit_order_id.is_some(), "the claim survives");
    t.m.process_tick(ATM_CE, "NFO", 125.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

#[tokio::test]
async fn an_uncertain_entry_keeps_the_run_open_until_the_book_settles_it() {
    let t = t();
    let sid = t.default_strategy();
    t.gw.uncertain_next(None);
    let r = t.start(sid).await;
    assert!(r.ok, "an unconfirmed entry may be a position: {:?}", r);
    let run = r.run_id.unwrap();
    assert!(t.run(run).stopped_at.is_none(), "never finalised as flat");
    assert_eq!(t.orders(run)[0].status, "unconfirmed");
    // A stop cannot finish while the entry may exist.
    let s = t.m.stop_run(run, USER, "manual").await;
    assert!(s.stop_pending, "{:?}", s);
    // The book shows the entry filled: it becomes a managed position and
    // the pending stop exits it.
    let snap = book(
        vec![broker_row("BRK-1", "SELL", 65, "complete", 100.0)],
        t.clock.now(),
    );
    t.m.reconcile_book(RunMode::Sandbox, &snap).await;
    assert_eq!(t.orders(run)[0].broker_order_id.as_deref(), Some("BRK-1"));
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"]);
}

// ------------------------------------------------------------ full app

fn live_places(a: &App) -> usize {
    a.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
        .count()
}

async fn live_run_with_lost_exit(kind: AfterSend) {
    let a = app();
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
    // The broker takes the exit and fills it, but the HTTP exchange fails
    // after the request was sent.
    *a.mock.after_send_price.lock() = 121.0;
    a.mock.after_send.lock().push_back((kind, "complete"));
    a.ctx.strategy.process_tick(ATM_CE, "NFO", 121.0).await;
    for p in 122..130 {
        a.ctx.strategy.process_tick(ATM_CE, "NFO", p as f64).await;
    }
    a.ctx.strategy.reconcile_pending_stops().await;
    assert_eq!(live_places(&a), 2, "entry and one exit, never a second exit");
    // The reconciler reads the book and settles it (its own task may
    // already have).
    a.ctx.reconciler.pass().await;
    let orders = a.ctx.strategy.store.list_orders(run).unwrap();
    let exit = orders.iter().find(|o| o.kind != "entry").unwrap();
    assert_eq!(exit.status, "complete", "{:?}", exit);
    assert!(exit.broker_order_id.is_some());
    assert!(
        a.ctx.strategy.store.get_run(run).unwrap().unwrap().stopped_at.is_some(),
        "the fill closed the leg and the run"
    );
    assert_eq!(live_places(&a), 2);
}

#[tokio::test]
async fn a_dropped_connection_after_an_exit_is_sent_is_reconciled_not_retried() {
    live_run_with_lost_exit(AfterSend::Dropped).await;
}

#[tokio::test]
async fn a_timeout_after_an_exit_is_sent_is_reconciled_not_retried() {
    live_run_with_lost_exit(AfterSend::TimedOut).await;
}
