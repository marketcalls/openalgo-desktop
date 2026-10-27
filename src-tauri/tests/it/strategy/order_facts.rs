//! ARCH-01, EV-01, EV-03: an order fact lost on the way (a dropped critical
//! delivery, a relay that fell behind) is repaired from the broker's order
//! book by the order reconciler, applied once; while it is not, health says
//! so and new entries wait, and exits still run.

use super::*;
use axum::http::StatusCode;
use openalgo_desktop_lib::brokers::mock::MockCall;
use openalgo_desktop_lib::brokers::types::Order;
use openalgo_desktop_lib::services::order_reconciler::ENTRIES_PAUSED;
use openalgo_desktop_lib::strategy::dispatch::RunMode;
use std::time::Duration;

fn broker_order(id: &str, action: &str, qty: i32, status: &str, price: f64) -> Order {
    Order {
        order_id: id.into(),
        exchange_order_id: None,
        symbol: ATM_CE.into(),
        exchange: "NFO".into(),
        side: action.into(),
        quantity: qty,
        filled_quantity: if status == "complete" { qty } else { 0 },
        pending_quantity: 0,
        price: 0.0,
        trigger_price: 0.0,
        average_price: price,
        order_type: "MARKET".into(),
        product: "NRML".into(),
        status: status.into(),
        validity: "DAY".into(),
        order_timestamp: String::new(),
        exchange_timestamp: None,
        rejection_reason: None,
        order_tag: None,
    }
}

fn places(a: &App) -> usize {
    a.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
        .count()
}

async fn live_strategy(a: &App, name: &str) -> i64 {
    let cfg = config(name, json!([short_call_leg()]), json!({}));
    let (row, _) = a.ctx.strategy.store.create_strategy(USER, &cfg).unwrap();
    a.ctx
        .strategy
        .store
        .set_live_enabled(row.id, USER, true)
        .unwrap();
    row.id
}

fn order_fact_alerts(a: &App) -> usize {
    let c = a.ctx.logs.conn().unwrap();
    openalgo_desktop_lib::db::sqlite::monitor::active_alerts(&c)
        .unwrap()
        .iter()
        .filter(|r| r.alert_type == "order_facts_gap")
        .count()
}

#[tokio::test]
async fn a_lost_fill_is_applied_once_and_health_shows_the_gap_until_then() {
    // Health judges the whole process (its threads, memory and
    // descriptors): alone, so other tests' contexts do not decide it.
    crate::isolated!(a_lost_fill_is_applied_once_and_health_shows_the_gap_until_then);
    let a = app();
    // Passes run by hand: the reconciler's own task would also open a gap
    // when the market feed connects for the new runs, at a time the test
    // does not control.
    a.ctx.reconciler.stop();
    a.ctx.sqlite.set_analyze_mode(false).unwrap();
    // A live run holding a filled leg (it can exit), and a second one whose
    // entry is working at the broker.
    let held = live_strategy(&a, "Held").await;
    let r = a
        .ctx
        .strategy
        .start_run(held, USER, "live", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    let held_run = r.run_id.unwrap();
    a.ctx
        .strategy
        .apply_fill(held_run, 1, Some(100.0), true, FillOpts::default())
        .await;
    let sid = live_strategy(&a, "Working").await;
    let r = a
        .ctx
        .strategy
        .start_run(sid, USER, "live", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    let run = r.run_id.unwrap();
    let bid = a.ctx.strategy.store.list_orders(run).unwrap()[0]
        .broker_order_id
        .clone()
        .unwrap();

    // The entry fills at the broker, and its order update is lost: a
    // critical delivery was dropped. The book cannot be read yet.
    *a.mock.order_book.lock() = Some(Err("The broker is busy".into()));
    a.ctx.reconciler.report_fact_loss(false).await;
    assert!(a.ctx.reconciler.gap_open(RunMode::Live));
    a.ctx.reconciler.pass().await;
    assert!(
        a.ctx.reconciler.gap_open(RunMode::Live),
        "an unread book repairs nothing"
    );

    // Health shows the gap while it is open.
    let (s, v) = a.get("/health/status").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
    assert_eq!(v["status"], "fail");
    let (s, v) = a.get("/health/api/current").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["overall_status"], "fail");
    assert_eq!(v["order_facts"]["status"], "fail");
    assert_eq!(v["order_facts"]["gaps"][0]["mode"], "live");
    assert_eq!(v["order_facts"]["gaps"][0]["cause"], "event_loss");
    assert_eq!(order_fact_alerts(&a), 1, "the alert is raised");

    // New entries wait ...
    let other = live_strategy(&a, "New").await;
    let r = a
        .ctx
        .strategy
        .start_run(other, USER, "live", "manual", None)
        .await;
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some(ENTRIES_PAUSED));
    // ... and a stop still exits.
    let before = places(&a);
    a.ctx.strategy.process_tick(ATM_CE, "NFO", 121.0).await;
    assert_eq!(places(&a), before + 1, "risk-reducing exits always run");

    // The book answers: the lost fill is applied, once.
    *a.mock.order_book.lock() = Some(Ok(vec![broker_order(&bid, "SELL", 65, "complete", 100.0)]));
    a.ctx.reconciler.pass().await;
    assert!(!a.ctx.reconciler.gap_open(RunMode::Live));
    let leg = a
        .ctx
        .strategy
        .state
        .snapshot(run)
        .unwrap()
        .leg(1)
        .unwrap()
        .clone();
    assert_eq!(leg.entry_status, "complete");
    assert_eq!(leg.status, "open");
    assert_eq!(leg.qty, 65);
    assert_eq!(leg.entry_avg, 100.0);
    assert_eq!(leg.sl_pts, Some(20.0), "the stop is in place");
    // A second read of the same fact books nothing more.
    a.ctx.reconciler.pass().await;
    let leg2 = a
        .ctx
        .strategy
        .state
        .snapshot(run)
        .unwrap()
        .leg(1)
        .unwrap()
        .clone();
    assert_eq!(leg2.qty, 65);
    assert_eq!(
        a.ctx.strategy.store.list_orders(run).unwrap()[0].filled_qty,
        Some(65)
    );

    // Health is back, the alert resolved.
    let (s, v) = a.get("/health/status").await;
    assert_eq!(
        s,
        StatusCode::OK,
        "{} reconciler {} orders {:?}",
        v,
        a.ctx.reconciler.health(),
        a.ctx
            .strategy
            .store
            .list_orders(held_run)
            .unwrap()
            .iter()
            .map(|o| (o.kind.clone(), o.status.clone()))
            .collect::<Vec<_>>()
    );
    assert_eq!(order_fact_alerts(&a), 0);
}

#[tokio::test]
async fn a_relay_that_fell_behind_opens_a_live_gap() {
    let a = app();
    a.ctx.sqlite.set_analyze_mode(false).unwrap();
    let sid = live_strategy(&a, "Working").await;
    let r = a
        .ctx
        .strategy
        .start_run(sid, USER, "live", "manual", None)
        .await;
    assert!(r.ok, "{:?}", r);
    // The market feed connects for the new run, and the reconciler repairs
    // the gap that opens (a readable book) before the scenario starts.
    let mut feed = a.ctx.websocket.watch_status();
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        feed.wait_for(|s| {
            matches!(
                s,
                openalgo_desktop_lib::websocket::FeedStatus::Connected { .. }
            )
        }),
    )
    .await;
    a.until(|| !a.ctx.reconciler.gap_open(RunMode::Live)).await;
    *a.mock.order_book.lock() = Some(Err("The broker is busy".into()));
    // The broker relay skipped updates; the reconciler's own task notices.
    a.ctx.bus.report_relay_lag(3);
    assert!(
        a.until(|| a.ctx.reconciler.gap_open(RunMode::Live)).await,
        "the relay's lag opens a live gap: reconciler {} bus gaps {} orders {:?}",
        a.ctx.reconciler.health(),
        a.ctx.bus.gap_count(),
        a.ctx
            .strategy
            .store
            .list_orders(r.run_id.unwrap())
            .unwrap()
            .iter()
            .map(|o| (o.kind.clone(), o.status.clone(), o.broker_order_id.clone()))
            .collect::<Vec<_>>()
    );
    assert!(!a.ctx.reconciler.gap_open(RunMode::Sandbox));
    let (_, v) = a.get("/health/api/current").await;
    assert_eq!(v["order_facts"]["gaps"][0]["cause"], "relay_lagged");
    assert_eq!(v["order_facts"]["bus"]["relay_lagged"], 3);
    *a.mock.order_book.lock() = Some(Ok(vec![]));
    a.ctx.reconciler.pass().await;
    assert!(!a.ctx.reconciler.gap_open(RunMode::Live));
}

/// A leg of a run, as the run's live state holds it.
fn leg_of(a: &App, run: i64) -> openalgo_desktop_lib::strategy::state::LegState {
    a.ctx
        .strategy
        .state
        .snapshot(run)
        .unwrap()
        .leg(1)
        .unwrap()
        .clone()
}

/// PROD-REL-02 residual: a reconcile pass folds the strategies' working
/// orders and settles their unconfirmed ones without the database on the
/// runtime that judges the ticks. It looked every working order up and
/// folded the book's fact there, and wrote what it settled there too, so
/// with the pool busy each pass held the runtime for the wait. Counted,
/// not timed: the pass takes no pooled connection on the runtime thread,
/// and the fill and the entry found in the book are each applied once.
#[test]
fn a_reconcile_pass_takes_no_database_connection_on_the_runtime() {
    crate::on_watched_runtime(|runtime| async move {
        use openalgo_desktop_lib::brokers::mock::AfterSend;
        use openalgo_desktop_lib::db::sqlite::checkouts;
        let a = app();
        a.ctx.reconciler.stop();
        a.ctx.sqlite.set_analyze_mode(false).unwrap();
        // An entry working at the broker.
        let working = live_strategy(&a, "Working").await;
        let r = a
            .ctx
            .strategy
            .start_run(working, USER, "live", "manual", None)
            .await;
        assert!(r.ok, "{:?}", r);
        let run = r.run_id.unwrap();
        let bid = a.ctx.strategy.store.list_orders(run).unwrap()[0]
            .broker_order_id
            .clone()
            .unwrap();
        // An entry the broker took and filled, whose answer was lost.
        *a.mock.after_send_time.lock() = "2026-10-07 10:00:05".into();
        a.mock
            .after_send
            .lock()
            .push_back((AfterSend::Dropped, "complete"));
        let lost = live_strategy(&a, "Lost").await;
        let r = a
            .ctx
            .strategy
            .start_run(lost, USER, "live", "manual", None)
            .await;
        assert!(r.ok, "an unconfirmed entry keeps its run: {:?}", r);
        let lost_run = r.run_id.unwrap();
        assert_eq!(
            a.ctx.strategy.store.list_orders(lost_run).unwrap()[0].status,
            "unconfirmed"
        );
        // The book shows both: the working entry has filled.
        {
            let mut book = a.mock.order_book.lock();
            let mut rows = match book.take() {
                Some(Ok(rows)) => rows,
                _ => Vec::new(),
            };
            rows.push(broker_order(&bid, "SELL", 65, "complete", 100.0));
            *book = Some(Ok(rows));
        }
        runtime.until_idle().await;
        let before = checkouts();
        a.ctx.reconciler.pass().await;
        let taken = checkouts().0 - before.0;
        assert_eq!(
            taken, 0,
            "pooled connections taken on the runtime during the pass"
        );
        let leg = leg_of(&a, run);
        assert_eq!(
            (leg.entry_status.as_str(), leg.qty, leg.entry_avg),
            ("complete", 65, 100.0),
            "the fill the book shows is applied"
        );
        let found = a.ctx.strategy.store.list_orders(lost_run).unwrap()[0].clone();
        assert!(found.broker_order_id.is_some(), "{:?}", found);
        assert_eq!(found.status, "complete", "{:?}", found);
        let lost_leg = leg_of(&a, lost_run);
        assert_eq!(
            (lost_leg.entry_status.as_str(), lost_leg.qty),
            ("complete", 65),
            "the entry found in the book is managed"
        );
        // A second pass over the same book folds and settles nothing again,
        // and still takes no connection on the runtime.
        let before = checkouts();
        a.ctx.reconciler.pass().await;
        assert_eq!(checkouts().0 - before.0, 0);
        assert_eq!(leg_of(&a, run).qty, 65);
        assert_eq!(leg_of(&a, lost_run).qty, 65);
        assert_eq!(places(&a), 2, "nothing is sent again");
    })
}
