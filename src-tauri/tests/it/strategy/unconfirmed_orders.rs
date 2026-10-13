//! LOG-08: a placement with no definite answer is neither accepted nor
//! refused. Its claim holds, nothing is placed again, and only the order
//! reconciler settles it from the broker's order book.

use super::*;
use openalgo_desktop_lib::brokers::mock::{AfterSend, MockCall};
use openalgo_desktop_lib::clock::Clock;
use openalgo_desktop_lib::services::order_reconciler::{
    BookSnapshot, ABSENT_READS, ABSENT_SPAN_SECS,
};
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
    t.orders(run)
        .into_iter()
        .filter(|o| o.kind != "entry")
        .collect()
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
    assert!(
        !r.ok,
        "a manual close while the exit is unconfirmed is refused"
    );
    assert_eq!(
        t.gw.actions(),
        vec!["SELL", "BUY"],
        "exactly one exit while its outcome is unknown"
    );
    let x = exits(&t, run);
    assert_eq!(x.len(), 1);
    assert_eq!(x[0].status, "unconfirmed", "never recorded as rejected");
    assert!(t.leg(run, 1).exit_order_id.is_some(), "the claim holds");
    assert!(t
        .event_kinds(sid)
        .contains(&"order_unconfirmed".to_string()));
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

/// Sandbox keeps the absence rule: the sandbox book is complete, so an
/// absence confirmed over several reads frees the leg. (Live never frees on
/// absence alone; see the tests below.)
#[tokio::test]
async fn a_sandbox_uncertain_exit_absent_from_repeated_reads_frees_the_leg_to_exit_again() {
    let t = t();
    let sid = t.default_strategy();
    let run = t.start_filled(sid, 100.0).await;
    t.gw.uncertain_next(None);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    let start = t.clock.now();
    let step = ABSENT_SPAN_SECS / i64::from(ABSENT_READS);
    for i in 0..ABSENT_READS {
        let at = start + chrono::Duration::seconds(step * i64::from(i));
        t.m.reconcile_book(RunMode::Sandbox, &book(vec![], at))
            .await;
        assert!(
            t.leg(run, 1).exit_order_id.is_some(),
            "one empty read is not proof"
        );
    }
    let at = start + chrono::Duration::seconds(ABSENT_SPAN_SECS + 1);
    assert_eq!(
        t.m.reconcile_book(RunMode::Sandbox, &book(vec![], at))
            .await,
        0
    );
    let leg = t.leg(run, 1);
    assert!(leg.exit_order_id.is_none() && leg.exit_claim_token.is_none());
    assert_eq!(exits(&t, run)[0].status, "rejected");
    // Managed again: the stop sends a new exit.
    t.m.process_tick(ATM_CE, "NFO", 122.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
}

/// A Live run with its leg filled at 100, then an exit whose answer is
/// lost.
async fn live_run_with_unconfirmed_exit(t: &T) -> i64 {
    let sid = t.default_strategy();
    t.m.store.set_live_enabled(sid, USER, true).unwrap();
    let r = t.m.start_run(sid, USER, "live", "manual", None).await;
    assert!(r.ok, "{:?}", r);
    let run = r.run_id.unwrap();
    t.m.apply_fill(run, 1, Some(100.0), true, FillOpts::default())
        .await;
    t.gw.uncertain_next(None);
    t.m.process_tick(ATM_CE, "NFO", 121.0).await;
    assert_eq!(exits(t, run)[0].status, "unconfirmed");
    run
}

/// P0-3 (replaces the Live reading of the absence rule): a Live exit with
/// no definite answer that the broker's order book does not show is never
/// released on absence alone. The claim holds over many reads, a critical
/// event is raised once, nothing is sent again, and the leg is freed only
/// when the trader marks the order not placed.
#[tokio::test]
async fn a_live_uncertain_exit_absent_from_the_book_is_held_until_the_trader_marks_it_not_placed() {
    let t = t();
    let run = live_run_with_unconfirmed_exit(&t).await;
    let start = t.clock.now();
    for i in 0..(ABSENT_READS * 4) {
        let at = start + chrono::Duration::seconds(i64::from(i) * ABSENT_SPAN_SECS);
        assert_eq!(
            t.m.reconcile_book(RunMode::Live, &book(vec![], at)).await,
            1,
            "still unresolved"
        );
    }
    assert!(t.leg(run, 1).exit_order_id.is_some(), "the claim holds");
    assert_eq!(exits(&t, run)[0].status, "unconfirmed");
    let sid = t.run(run).strategy_id;
    let held_events = t
        .event_kinds(sid)
        .into_iter()
        .filter(|k| k == "order_unconfirmed_held")
        .count();
    assert_eq!(held_events, 1, "alerted once");
    let held = t.m.held_orders();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, exits(&t, run)[0].id.to_string());
    t.m.process_tick(ATM_CE, "NFO", 125.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY"], "never sent again");
    // The trader checked the broker's books: not placed.
    assert_eq!(t.m.confirm_not_placed(&held[0].id).await, Ok(true));
    assert!(t.m.held_orders().is_empty());
    let leg = t.leg(run, 1);
    assert!(leg.exit_order_id.is_none() && leg.exit_claim_token.is_none());
    assert_eq!(exits(&t, run)[0].status, "rejected");
    t.m.process_tick(ATM_CE, "NFO", 126.0).await;
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
}

/// P0-3: Live releases without the trader only with corroboration: absent
/// for LIVE_RELEASE_SECS, no matching trade in the trade book, and the net
/// position unchanged since the absence was confirmed.
#[tokio::test]
async fn a_live_absence_is_released_only_with_corroboration() {
    use openalgo_desktop_lib::services::order_reconciler::{Corroboration, LIVE_RELEASE_SECS};
    let t = t();
    let run = live_run_with_unconfirmed_exit(&t).await;
    let start = t.clock.now();
    let short = json!({"status": "success", "data": [
        {"symbol": ATM_CE, "exchange": "NFO", "product": "NRML", "quantity": -65}
    ]});
    let none = json!({"status": "success", "data": []});
    let fill = json!({"status": "success", "data": [
        {"symbol": ATM_CE, "exchange": "NFO", "action": "BUY", "quantity": 65, "timestamp": ""}
    ]});
    let read = |secs: i64, trades: &Value, positions: &Value| {
        book(vec![], start + chrono::Duration::seconds(secs))
            .with_corroboration(Corroboration::from_books(trades, positions))
    };
    // The absence is confirmed: held (the position is the baseline).
    for i in 0..=ABSENT_READS {
        let secs = i64::from(i) * ABSENT_SPAN_SECS;
        assert_eq!(
            t.m.reconcile_book(RunMode::Live, &read(secs, &none, &short))
                .await,
            1
        );
    }
    // Not long enough yet, though nothing contradicts it.
    let early = LIVE_RELEASE_SECS - 60;
    assert_eq!(
        t.m.reconcile_book(RunMode::Live, &read(early, &none, &short))
            .await,
        1
    );
    // Long enough, but a matching trade (its time unreadable) is there.
    let late = LIVE_RELEASE_SECS + 30;
    assert_eq!(
        t.m.reconcile_book(RunMode::Live, &read(late, &fill, &short))
            .await,
        1
    );
    // Long enough, no trade, but the position changed.
    assert_eq!(
        t.m.reconcile_book(RunMode::Live, &read(late, &none, &none))
            .await,
        1
    );
    assert_eq!(exits(&t, run)[0].status, "unconfirmed");
    // Long enough, no trade, position unchanged: released.
    assert_eq!(
        t.m.reconcile_book(RunMode::Live, &read(late + 30, &none, &short))
            .await,
        0
    );
    assert_eq!(exits(&t, run)[0].status, "rejected");
    assert!(t.leg(run, 1).exit_order_id.is_none());
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
    assert_eq!(
        live_places(&a),
        2,
        "entry and one exit, never a second exit"
    );
    // The reconciler reads the book and settles it (its own task may
    // already have).
    a.ctx.reconciler.pass().await;
    let orders = a.ctx.strategy.store.list_orders(run).unwrap();
    let exit = orders.iter().find(|o| o.kind != "entry").unwrap();
    assert_eq!(exit.status, "complete", "{:?}", exit);
    assert!(exit.broker_order_id.is_some());
    assert!(
        a.ctx
            .strategy
            .store
            .get_run(run)
            .unwrap()
            .unwrap()
            .stopped_at
            .is_some(),
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

/// P0-3, end to end: a Live exit whose answer was lost and that the
/// broker's order book never shows is held, listed on the Health page with
/// a critical alert, and released only by the trader's "not placed" action
/// there. The alert resolves once nothing is held.
#[tokio::test]
async fn a_held_live_exit_is_listed_on_the_health_page_and_released_by_the_trader() {
    use axum::http::StatusCode;
    let a = app();
    // Passes run by hand here, against the hand-driven clock.
    a.ctx.reconciler.stop();
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
    // The exit reaches the broker, its answer is lost, and the broker's
    // book never shows it.
    a.mock
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "open"));
    a.ctx.strategy.process_tick(ATM_CE, "NFO", 121.0).await;
    *a.mock.order_book.lock() = Some(Ok(Vec::new()));
    for _ in 0..=ABSENT_READS {
        a.clock.advance(chrono::Duration::seconds(ABSENT_SPAN_SECS));
        a.ctx.reconciler.pass().await;
    }
    for p in 122..126 {
        a.ctx.strategy.process_tick(ATM_CE, "NFO", p as f64).await;
    }
    assert_eq!(live_places(&a), 2, "entry and one exit, never a second");
    let (s, body) = a.get("/health/api/held-orders").await;
    assert_eq!(s, StatusCode::OK);
    let rows = body["data"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "{}", body);
    assert_eq!(rows[0]["owner"], "strategy");
    assert_eq!(rows[0]["action"], "BUY");
    let (_, alerts) = a.get("/health/api/alerts").await;
    assert!(
        alerts.to_string().contains("order_unconfirmed_held"),
        "{}",
        alerts
    );
    // A pass whose book read fails settles nothing: the alert stays
    // (review P2-1).
    *a.mock.order_book.lock() = Some(Err("The broker is busy".into()));
    a.ctx.reconciler.pass().await;
    let (_, alerts) = a.get("/health/api/alerts").await;
    assert!(
        alerts.to_string().contains("order_unconfirmed_held"),
        "a failed read is not a resolution: {}",
        alerts
    );
    *a.mock.order_book.lock() = Some(Ok(Vec::new()));
    let confirm = json!({"owner": "strategy", "id": rows[0]["id"]});
    let (s, b) = a
        .post("/health/api/held-orders/not-placed", confirm.clone())
        .await;
    assert_eq!(s, StatusCode::OK, "{}", b);
    let (_, body) = a.get("/health/api/held-orders").await;
    assert!(body["data"].as_array().unwrap().is_empty());
    let exit = a
        .ctx
        .strategy
        .store
        .list_orders(run)
        .unwrap()
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap();
    assert_eq!(exit.status, "rejected");
    a.ctx.reconciler.pass().await;
    let (_, alerts) = a.get("/health/api/alerts").await;
    assert!(
        !alerts.to_string().contains("order_unconfirmed_held"),
        "{}",
        alerts
    );
    // Nothing is held any more.
    let (s, _) = a.post("/health/api/held-orders/not-placed", confirm).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Review P1-3: the trader's "not placed" is checked against the broker's
/// books once more. An order that now shows in the order book, a matching
/// trade, or a book that cannot be read keeps it held.
#[tokio::test]
async fn not_placed_is_refused_while_the_books_show_the_order() {
    use axum::http::StatusCode;
    use openalgo_desktop_lib::brokers::types::{Order, Trade};
    let a = app();
    a.ctx.reconciler.stop();
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
    a.mock
        .after_send
        .lock()
        .push_back((AfterSend::Dropped, "open"));
    a.ctx.strategy.process_tick(ATM_CE, "NFO", 121.0).await;
    *a.mock.order_book.lock() = Some(Ok(Vec::new()));
    for _ in 0..=ABSENT_READS {
        a.clock.advance(chrono::Duration::seconds(ABSENT_SPAN_SECS));
        a.ctx.reconciler.pass().await;
    }
    let (_, body) = a.get("/health/api/held-orders").await;
    let id = body["data"][0]["id"].clone();
    assert!(id.is_string(), "{}", body);
    let confirm = json!({"owner": "strategy", "id": id});
    let path = "/health/api/held-orders/not-placed";

    // The order now shows in the book.
    *a.mock.order_book.lock() = Some(Ok(vec![Order {
        order_id: "BRK-9".into(),
        exchange_order_id: None,
        symbol: ATM_CE.into(),
        exchange: "NFO".into(),
        side: "BUY".into(),
        quantity: 65,
        filled_quantity: 0,
        pending_quantity: 65,
        price: 0.0,
        trigger_price: 0.0,
        average_price: 0.0,
        order_type: "MARKET".into(),
        product: "NRML".into(),
        status: "open".into(),
        validity: "DAY".into(),
        order_timestamp: "2026-10-07 10:00:05".into(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }]));
    let (s, v) = a.post(path, confirm.clone()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert!(
        v["message"].as_str().unwrap().contains("order book"),
        "{}",
        v
    );

    // The book cannot be read.
    *a.mock.order_book.lock() = Some(Err("The broker is busy".into()));
    let (s, v) = a.post(path, confirm.clone()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert!(
        v["message"].as_str().unwrap().contains("could not read"),
        "{}",
        v
    );

    // No order, but a matching trade.
    *a.mock.order_book.lock() = Some(Ok(Vec::new()));
    *a.mock.trade_book.lock() = Some(Ok(vec![Trade {
        order_id: "BRK-9".into(),
        trade_id: "T1".into(),
        symbol: ATM_CE.into(),
        exchange: "NFO".into(),
        product: "NRML".into(),
        side: "BUY".into(),
        quantity: 65,
        average_price: 121.0,
        trade_value: 7865.0,
        timestamp: String::new(),
        order_tag: None,
    }]));
    let (s, v) = a.post(path, confirm.clone()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert!(
        v["message"].as_str().unwrap().contains("trade book"),
        "{}",
        v
    );

    // Nothing at the broker: accepted.
    *a.mock.trade_book.lock() = Some(Ok(Vec::new()));
    let (s, v) = a.post(path, confirm.clone()).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let exit = a
        .ctx
        .strategy
        .store
        .list_orders(run)
        .unwrap()
        .into_iter()
        .find(|o| o.kind != "entry")
        .unwrap();
    assert_eq!(exit.status, "rejected");
}
