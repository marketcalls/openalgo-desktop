//! Web `test/test_sahi_order_adapter.py` (openalgo#2474, f0a50b038): frame
//! normalisation and the handshake; the order feed the broker builds is in
//! `tests/it/broker_sahi_orders.rs`. The frames follow the sample under
//! "WSS /v1/stream/orders -- Order updates" in Sahi's API reference: one
//! JSON order object shaped like an element of `GET /v1/orders`, prices in
//! paise. An empty master stands in for the web test's symbol lookup,
//! which hands Sahi's symbol back.

use super::*;
use crate::brokers::common::symbols::SymToken;
use serde_json::{json, Value};

/// The documented sample frame (the web test's `DOC_FRAME`).
fn doc_frame() -> Value {
    json!({
        "order_id": "26101900133912345678",
        "exchange_order_id": "1300000123456789",
        "exchange_segment": "NSE",
        "symbol": "RELIANCE",
        "txn_type": "B",
        "order_type": "LMT",
        "product": "I",
        "validity": "DAY",
        "qty": 10,
        "disclosed_qty": 0,
        "filled_qty": 10,
        "price": 250000,
        "trigger_price": 0,
        "avg_price": 249975,
        "status": "Completed",
        "rejection_reason": null,
        "created_at": 1747798201123i64,
        "placed_at": 1747798201456i64,
        "completed_at": 1747798205890i64,
        "cancelled_at": null,
        "rejected_at": null,
        "last_modified_at": 1747798205890i64
    })
}

/// The sample with `overrides`, as frame text.
fn frame(overrides: Value) -> String {
    let mut f = doc_frame();
    for (k, v) in overrides.as_object().unwrap() {
        f[k] = v.clone();
    }
    f.to_string()
}

fn feed_with(symbols: SymbolResolver) -> SahiOrderFeed {
    SahiOrderFeed::new(
        "jwt-token",
        symbols,
        "wss://direct.sahi.com/v1/stream/orders".into(),
    )
}

fn feed() -> SahiOrderFeed {
    feed_with(SymbolResolver::new())
}

/// The update `feed` makes of one text frame.
fn event(feed: &mut SahiOrderFeed, text: &str) -> Option<OrderUpdate> {
    match feed.parse(&Message::Text(text.into())).as_slice() {
        [] => None,
        [FeedEvent::OrderUpdate(u)] => Some(u.clone()),
        other => panic!("one order update expected: {:?}", other),
    }
}

fn normalize(overrides: Value) -> OrderUpdate {
    event(&mut feed(), &frame(overrides)).expect("an order update")
}

/// Web test_sahi_order_adapter.py::test_doc_sample_maps_to_openalgo_fields
#[test]
fn the_doc_sample_maps_to_openalgo_fields() {
    assert_eq!(
        normalize(json!({})),
        OrderUpdate {
            orderid: "26101900133912345678".into(),
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            quantity: 10,
            price: 2500.0,
            trigger_price: 0.0,
            pricetype: "LIMIT".into(),
            product: "MIS".into(),
            order_status: "complete".into(),
            filled_quantity: 10,
            pending_quantity: 0,
            average_price: 2499.75,
            rejection_reason: String::new(),
        }
    );
}

/// Web test_sahi_order_adapter.py::test_status_matches_the_order_book
#[test]
fn the_status_matches_the_order_book() {
    for (status, expected) in [
        ("Open", "open"),
        ("OpenPartial", "open"),
        ("AwaitingTrigger", "trigger pending"),
        ("PartialFilled", "cancelled"),
        ("Completed", "complete"),
        ("Cancelled", "cancelled"),
        ("Rejected", "rejected"),
    ] {
        let event = normalize(json!({"status": status}));
        assert_eq!(event.order_status, expected, "{}", status);
        assert_eq!(
            event.order_status,
            mapping::order_status(status),
            "{}",
            status
        );
    }
}

/// Web test_sahi_order_adapter.py::test_working_order_reports_unfilled_quantity_as_pending
#[test]
fn a_working_order_reports_its_unfilled_quantity_as_pending() {
    for status in ["OpenPartial", "AwaitingTrigger"] {
        let event = normalize(json!({"status": status, "qty": 10, "filled_qty": 4}));
        assert_eq!(event.filled_quantity, 4);
        assert_eq!(event.pending_quantity, 6, "{}", status);
    }
}

/// Web test_sahi_order_adapter.py::test_finished_order_has_nothing_pending
#[test]
fn a_finished_order_has_nothing_pending() {
    for status in ["PartialFilled", "Cancelled", "Rejected", "Completed"] {
        let event = normalize(json!({"status": status, "qty": 10, "filled_qty": 4}));
        assert_eq!(event.pending_quantity, 0, "{}", status);
    }
}

/// Web test_sahi_order_adapter.py::test_rejection_reason_passes_through
#[test]
fn the_rejection_reason_passes_through() {
    let event = normalize(json!({
        "status": "Rejected", "filled_qty": 0, "avg_price": 0,
        "rejection_reason": "RMS: margin shortfall"
    }));
    assert_eq!(event.order_status, "rejected");
    assert_eq!(event.rejection_reason, "RMS: margin shortfall");
}

/// Web test_sahi_order_adapter.py::test_order_type_and_product_codes
#[test]
fn order_type_and_product_codes_map_to_openalgo_names() {
    for (order_type, product, pricetype, oa_product) in [
        ("MKT", "C", "MARKET", "CNC"),
        ("SL-LMT", "M", "SL", "NRML"),
        ("SL-MKT", "I", "SL-M", "MIS"),
    ] {
        let event = normalize(json!({
            "order_type": order_type, "product": product, "trigger_price": 249000
        }));
        assert_eq!(event.pricetype, pricetype);
        assert_eq!(event.product, oa_product);
        assert_eq!(event.trigger_price, 2490.0);
    }
}

/// Web test_sahi_order_adapter.py::test_sell_and_derivative_segment: the
/// symbol maps through the master, as the order book's does.
#[test]
fn a_sell_on_a_derivative_segment_names_its_openalgo_contract() {
    let symbols = SymbolResolver::new();
    symbols.load(vec![SymToken {
        symbol: "NIFTY25NOV2624000CE".into(),
        brsymbol: "NIFTY 24000 CE".into(),
        name: "NIFTY".into(),
        exchange: "NFO".into(),
        brexchange: "NFO".into(),
        token: "1".into(),
        expiry: "25-NOV-26".into(),
        strike: 24000.0,
        lot_size: 75,
        instrument_type: "CE".into(),
        tick_size: 0.05,
    }]);
    let text =
        frame(json!({"txn_type": "S", "exchange_segment": "NFO", "symbol": "NIFTY 24000 CE"}));
    let event = event(&mut feed_with(symbols), &text).expect("an order update");
    assert_eq!(event.action, "SELL");
    assert_eq!(event.exchange, "NFO");
    assert_eq!(event.symbol, "NIFTY25NOV2624000CE");
}

/// Web test_sahi_order_adapter.py::test_bytes_frame_is_accepted
#[test]
fn a_binary_frame_is_accepted() {
    let events = feed().parse(&Message::Binary(frame(json!({})).into_bytes()));
    assert!(
        matches!(events.as_slice(), [FeedEvent::OrderUpdate(u)] if u.orderid == "26101900133912345678"),
        "{:?}",
        events
    );
}

/// Web test_sahi_order_adapter.py::test_non_order_frames_are_ignored
#[test]
fn frames_that_are_not_orders_are_ignored() {
    let no_order = json!({"status": "Open"}).to_string();
    let empty_id = frame(json!({"order_id": ""}));
    for raw in [
        "not json",
        "[]",
        "{}",
        no_order.as_str(),
        "null",
        empty_id.as_str(),
    ] {
        assert_eq!(event(&mut feed(), raw), None, "{}", raw);
    }
}

/// Web test_sahi_order_adapter.py::test_handshake_targets_order_stream_with_bearer_and_user_agent:
/// no login or subscribe frame and no frame of its own as a heartbeat; the
/// socket pings as the web's order sockets do.
#[test]
fn the_handshake_targets_the_order_stream_with_bearer_and_user_agent() {
    let mut f = feed();
    let req = f.ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://direct.sahi.com/v1/stream/orders"
    );
    assert_eq!(req.headers()["authorization"], "Bearer jwt-token");
    assert!(!req.headers()["user-agent"].is_empty());
    assert!(f.on_connected().is_empty());
    assert!(f.on_authenticated().is_empty());
    assert!(matches!(
        f.heartbeat(),
        Some((d, Message::Ping(_))) if d == WEB_ORDER_PING_INTERVAL
    ));
    assert!(!format!("{:?}", f).contains("jwt-token"));
}

/// Web test_sahi_order_adapter.py::test_message_publishes_order_update_event
#[test]
fn a_message_becomes_an_order_update_event() {
    let update = event(
        &mut feed(),
        &frame(json!({"status": "Open", "filled_qty": 0})),
    )
    .expect("an order update");
    assert_eq!(update.orderid, "26101900133912345678");
    assert_eq!(update.order_status, "open");
}
