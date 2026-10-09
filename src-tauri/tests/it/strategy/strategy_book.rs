//! web: subscribers/strategy_book_subscriber.py, test_strategy_book_prune_lock.py

use super::*;
use openalgo_desktop_lib::events::{Event, Mode, OrderMeta, OrderUpdate};

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
