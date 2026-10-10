//! web: subscribers/strategy_book_subscriber.py, test_strategy_book_prune_lock.py

use super::*;
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::db::sqlite::SqliteDb;
use openalgo_desktop_lib::events::{Event, Mode, OrderMeta, OrderUpdate};
use openalgo_desktop_lib::strategy::book::StrategyBook;
use std::sync::Arc;

fn placed(orderid: &str, strategy: &str) -> Event {
    Event::OrderPlaced {
        meta: OrderMeta {
            mode: Mode::Live,
            api_type: "placeorder".into(),
            request_data: json!({}),
            response_data: json!({}),
        },
        strategy: strategy.into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        quantity: 10,
        pricetype: "MARKET".into(),
        product: "MIS".into(),
        orderid: orderid.into(),
    }
}

fn fill(orderid: &str, action: &str, qty: i64, price: f64, status: &str) -> Event {
    Event::OrderUpdate(OrderUpdate {
        orderid: orderid.into(),
        action: action.into(),
        order_status: status.into(),
        filled_quantity: qty,
        average_price: price,
        ..Default::default()
    })
}

async fn legs(a: &App) -> Vec<Value> {
    a.ctx
        .strategy
        .book
        .get_strategy_legs(None, Some("MyAlgo"))
        .unwrap()
}

#[tokio::test]
async fn tags_and_fills_book_a_position_per_strategy() {
    let a = app();
    a.ctx.bus.publish(placed("O1", "MyAlgo"));
    a.ctx.bus.publish(fill("O1", "BUY", 10, 500.0, "complete"));
    let book = a.ctx.strategy.book.clone();
    assert!(
        a.until(|| book.get_strategy_legs(None, Some("MyAlgo")).unwrap().len() == 1)
            .await
    );
    let l = &legs(&a).await[0];
    assert_eq!(
        (l["quantity"].as_f64(), l["average_price"].as_f64()),
        (Some(10.0), Some(500.0))
    );
    // A duplicate fill books nothing; the closing fill realizes.
    a.ctx.bus.publish(fill("O1", "BUY", 10, 500.0, "complete"));
    a.ctx.bus.publish(placed("O2", "MyAlgo"));
    a.ctx.bus.publish(fill("O2", "SELL", 10, 510.0, "complete"));
    assert!(
        a.until(|| book.get_strategy_legs(None, Some("MyAlgo")).unwrap()[0]["quantity"] == 0.0)
            .await
    );
    let l = &legs(&a).await[0];
    assert_eq!(l["realized_pnl"].as_f64(), Some(100.0));
}

#[tokio::test]
async fn a_fill_that_beats_its_tag_is_buffered_and_drained() {
    let a = app();
    let book = &a.ctx.strategy.book;
    assert!(book.apply_fill("O9", 5.0, 100.0, "BUY").unwrap().is_none());
    assert_eq!(book.pending_fill_count().unwrap(), 1);
    book.record_order_tag("O9", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    assert_eq!(book.pending_fill_count().unwrap(), 0);
    let l = book.get_strategy_legs(None, Some("MyAlgo")).unwrap();
    assert_eq!(l[0]["quantity"].as_f64(), Some(5.0));
}

#[tokio::test]
async fn partials_are_priced_from_the_change_in_notional() {
    let a = app();
    let book = &a.ctx.strategy.book;
    book.record_order_tag("P1", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    book.apply_fill("P1", 4.0, 100.0, "BUY").unwrap();
    book.apply_fill("P1", 10.0, 103.0, "BUY").unwrap(); // last 6 at 105
    let l = book.get_strategy_legs(None, Some("MyAlgo")).unwrap();
    assert!((l[0]["average_price"].as_f64().unwrap() - 103.0).abs() < 1e-9);
    assert_eq!(l[0]["quantity"].as_f64(), Some(10.0));
}

#[tokio::test]
async fn an_untagged_order_never_books() {
    let a = app();
    let book = &a.ctx.strategy.book;
    book.record_order_tag("T1", "", "", "SBIN", "NSE", "MIS")
        .unwrap();
    assert!(book.get_strategy_legs(None, None).unwrap().is_empty());
}

// ------------------------------------------------- crash atomicity (DB-02)

/// A book over its own database, so a test can install a fault trigger.
fn bare_book() -> (Arc<StrategyBook>, Arc<SqliteDb>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(SqliteDb::new(&dir.path().join("openalgo.db")).unwrap());
    let book = Arc::new(StrategyBook::new(
        db.clone(),
        ManualClock::new(ist(2026, 10, 7, 10, 0)),
        (3, 0),
    ));
    (book, db, dir)
}

/// Make the watermark write fail, as a crash right after the position
/// write would leave it.
fn fail_watermark(db: &SqliteDb, on: bool) {
    let sql = if on {
        "CREATE TRIGGER fail_watermark BEFORE UPDATE OF applied_quantity \
         ON strategy_order_tags BEGIN SELECT RAISE(ABORT, 'injected fault'); END;"
    } else {
        "DROP TRIGGER fail_watermark;"
    };
    db.conn().unwrap().execute_batch(sql).unwrap();
}

/// `(quantity, realized_pnl)` of the one MyAlgo leg, if any.
fn leg_of(book: &StrategyBook) -> Option<(f64, f64)> {
    book.get_strategy_legs(None, Some("MyAlgo"))
        .unwrap()
        .first()
        .map(|l| {
            (
                l["quantity"].as_f64().unwrap(),
                l["realized_pnl"].as_f64().unwrap(),
            )
        })
}

#[tokio::test]
async fn a_fill_replayed_after_a_failed_watermark_is_booked_once() {
    let (book, db, _dir) = bare_book();
    book.record_order_tag("F1", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    fail_watermark(&db, true);
    assert!(book.apply_fill("F1", 10.0, 100.0, "BUY").is_err());
    // The position write rolled back with the watermark.
    assert_eq!(leg_of(&book), None);
    fail_watermark(&db, false);
    // The same fill again (a replay after restart): booked exactly once.
    book.apply_fill("F1", 10.0, 100.0, "BUY").unwrap();
    assert_eq!(leg_of(&book), Some((10.0, 0.0)));

    // A partial close under the same fault: P&L is realized once too.
    book.record_order_tag("F2", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    fail_watermark(&db, true);
    assert!(book.apply_fill("F2", 4.0, 110.0, "SELL").is_err());
    assert_eq!(leg_of(&book), Some((10.0, 0.0)));
    fail_watermark(&db, false);
    book.apply_fill("F2", 4.0, 110.0, "SELL").unwrap();
    assert_eq!(leg_of(&book), Some((6.0, 40.0)));
}

#[tokio::test]
async fn a_buffered_fill_whose_drain_fails_is_booked_once_on_the_next_drain() {
    let (book, db, _dir) = bare_book();
    assert!(book.apply_fill("D1", 10.0, 100.0, "BUY").unwrap().is_none());
    fail_watermark(&db, true);
    assert!(book
        .record_order_tag("D1", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .is_err());
    // Nothing booked, and the buffered fill is still there to drain.
    assert_eq!(leg_of(&book), None);
    assert_eq!(book.pending_fill_count().unwrap(), 1);
    fail_watermark(&db, false);
    book.record_order_tag("D1", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    assert_eq!(book.pending_fill_count().unwrap(), 0);
    assert_eq!(leg_of(&book), Some((10.0, 0.0)));
}

#[tokio::test]
async fn a_failed_pending_row_delete_rolls_the_fill_back_with_it() {
    let (book, db, _dir) = bare_book();
    assert!(book.apply_fill("D2", 5.0, 100.0, "BUY").unwrap().is_none());
    db.conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_drain BEFORE DELETE ON strategy_pending_fills \
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();
    assert!(book
        .record_order_tag("D2", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .is_err());
    // The position, the watermark and the buffered row moved together.
    assert_eq!(leg_of(&book), None);
    assert_eq!(book.pending_fill_count().unwrap(), 1);
    db.conn()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_drain;")
        .unwrap();
    book.record_order_tag("D2", "", "MyAlgo", "SBIN", "NSE", "MIS")
        .unwrap();
    assert_eq!(leg_of(&book), Some((5.0, 0.0)));
    assert_eq!(book.pending_fill_count().unwrap(), 0);
}
