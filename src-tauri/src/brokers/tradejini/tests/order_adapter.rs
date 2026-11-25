//! Web `test/test_tradejini_order_adapter.py` (openalgo#2477, `33158269c`):
//! the order-update socket's frame decoding, event mapping, refusals and
//! address, through the feed `TradejiniBroker::create_order_feed` builds.
//! Frames are built here as the web test builds them (the SDK's layout: a
//! 6-byte header, then packets of length, type and key/value fields); the
//! event fields are the ones `12-websocket-sdks.md` documents.
//!
//! Where the web asserts a key is left out of its event, the desktop's
//! update carries the web's `OrderUpdateEvent` default (0), which is what
//! the web's `order_update` frame sends for it too.
//!
//! Not ported: `test_4001_from_a_superseded_socket_is_ignored`. The web
//! wraps each socket's close callback and must ignore an old socket's; the
//! desktop's manager closes a socket before it opens the next and asks
//! `on_ended` only about the one it holds.

use super::*;
use crate::brokers::common::streaming::{
    BrokerFeed, Ended, OrderFeed, OrderUpdate, Reconnect, StopKind,
};
use serde_json::json;

fn row(symbol: &str, brsymbol: &str, token: &str) -> SymbolData {
    SymbolData {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn resolver(rows: Vec<SymbolData>) -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

/// The web test's symbol lookup: the trading symbol it names maps to
/// RELIANCE (`symbol_lookup`).
fn lookup() -> SymbolResolver {
    resolver(vec![row("RELIANCE", "RELIANCE-EQ", "2885")])
}

/// The order feed of a session with this stored token (`api_key:token`).
fn order_feed_for(symbols: SymbolResolver, token: &str) -> Box<dyn BrokerFeed> {
    match TradejiniBroker::new(symbols).create_order_feed(&AuthToken::new(token)) {
        Ok(OrderFeed::Socket(feed)) => feed,
        other => panic!("Tradejini's order updates come on a socket of their own: {other:?}"),
    }
}

fn order_feed() -> Box<dyn BrokerFeed> {
    order_feed_for(lookup(), "KEY:tok")
}

fn packet(kind: u8, fields: &[u8]) -> Vec<u8> {
    let mut p = ((fields.len() + 3) as i16).to_le_bytes().to_vec();
    p.push(kind);
    p.extend_from_slice(fields);
    p
}

/// web `_event_packet`: the message's length (56), then the message (61).
fn event_packet(message: &str) -> Vec<u8> {
    let body = message.as_bytes();
    let mut fields = vec![56];
    fields.extend_from_slice(&(body.len() as u16).to_le_bytes());
    fields.push(61);
    fields.extend_from_slice(body);
    packet(15, &fields)
}

fn auth_packet(status: u8) -> Vec<u8> {
    packet(13, &[25, status])
}

fn ping_packet() -> Vec<u8> {
    packet(16, &[62, 1])
}

/// web `_frame`, compressed with zlib by default.
fn frame(packets: &[Vec<u8>]) -> Vec<u8> {
    build::frame(packets, true)
}

/// web `ORDER_EVENT` with `overrides`.
fn event(overrides: Value) -> String {
    let mut e = json!({
        "evntType": "orders",
        "type": "limit",
        "trdSym": "RELIANCE-EQ",
        "exch": "NSE",
        "qty": 10,
        "product": "intraday",
        "price": 2450.5,
        "side": "buy",
        "status": "open",
        "orderId": "210115000000001",
        "orderTime": "09:15:00",
        "actStatus": "",
        "msg": "Order placed",
        "reason": "",
    });
    if let (Some(e), Some(o)) = (e.as_object_mut(), overrides.as_object()) {
        for (k, v) in o {
            e.insert(k.clone(), v.clone());
        }
    }
    e.to_string()
}

fn parsed(feed: &mut dyn BrokerFeed, frame: Vec<u8>) -> Vec<FeedEvent> {
    feed.parse(&Message::Binary(frame))
}

/// The updates one frame makes; anything else in it fails the test.
fn updates(frame: Vec<u8>) -> Vec<OrderUpdate> {
    parsed(order_feed().as_mut(), frame)
        .into_iter()
        .map(|e| match e {
            FeedEvent::OrderUpdate(u) => u,
            other => panic!("an order update, not {other:?}"),
        })
        .collect()
}

/// The update one event makes, if any (web `normalize_event`).
fn normalized(event: &str) -> Option<OrderUpdate> {
    let mut all = updates(frame(&[event_packet(event)]));
    assert!(all.len() <= 1, "one event, one update: {all:?}");
    all.pop()
}

fn ids(updates: &[OrderUpdate]) -> Vec<&str> {
    updates.iter().map(|u| u.orderid.as_str()).collect()
}

// -- frame decoding -----------------------------------------------------------

/// Web test/test_tradejini_order_adapter.py::test_decoder_matches_the_vendored_sdk.
/// The packet before the event is skipped by its length, and the event is read
/// whole.
#[test]
fn an_event_after_another_packet_is_read_whole() {
    let got = updates(frame(&[ping_packet(), event_packet(&event(json!({})))]));
    assert_eq!(ids(&got), ["210115000000001"]);
}

/// Web test/test_tradejini_order_adapter.py::test_decoder_reads_every_event_in_a_frame.
/// Compressed and not.
#[test]
fn every_event_in_a_frame_is_read() {
    for compress in [true, false] {
        let f = build::frame(
            &[
                event_packet(&event(json!({"orderId": "1"}))),
                ping_packet(),
                event_packet(&event(json!({"orderId": "2"}))),
            ],
            compress,
        );
        assert_eq!(ids(&updates(f)), ["1", "2"], "compressed: {compress}");
    }
}

/// Web test/test_tradejini_order_adapter.py::test_decoder_ignores_other_versions_and_short_frames.
#[test]
fn other_versions_and_short_frames_are_ignored() {
    let mut f = frame(&[event_packet(&event(json!({})))]);
    f[4] = 2;
    assert!(updates(f).is_empty());
    assert!(updates(vec![0, 1]).is_empty());
}

/// The SDK spec's message layout (`binaryspec-doc.md`): without a length
/// field a message is 100 bytes, and the NULs padding it out are not part
/// of the text.
#[test]
fn a_message_padded_with_nuls_is_read_to_its_text() {
    let text = json!({"evntType": "orders", "orderId": "7", "status": "open"}).to_string();
    assert!(text.len() < 100);
    let mut padded = text.clone().into_bytes();
    padded.resize(100, 0);
    let mut fields = vec![61];
    fields.extend_from_slice(&padded);
    assert_eq!(ids(&updates(frame(&[packet(15, &fields)]))), ["7"]);
    // With a length field the message is that long.
    let mut longer = text.into_bytes();
    longer.resize(140, 0);
    let mut fields = vec![56];
    fields.extend_from_slice(&140u16.to_le_bytes());
    fields.push(61);
    fields.extend_from_slice(&longer);
    assert_eq!(ids(&updates(frame(&[packet(15, &fields)]))), ["7"]);
}

// -- normalization ------------------------------------------------------------

/// Web test/test_tradejini_order_adapter.py::test_order_event_maps_to_openalgo_vocabulary.
#[test]
fn an_order_event_maps_to_openalgo_constants() {
    let got = updates(frame(&[event_packet(&event(json!({})))]));
    assert_eq!(
        got,
        [OrderUpdate {
            orderid: "210115000000001".into(),
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            pricetype: "LIMIT".into(),
            product: "MIS".into(),
            order_status: "open".into(),
            quantity: 10,
            price: 2450.5,
            ..OrderUpdate::default()
        }]
    );
}

/// Web test/test_tradejini_order_adapter.py::test_symid_is_preferred_when_the_broker_sends_it.
/// The scrip id is looked up first; the trading symbol here names another
/// instrument.
#[test]
fn the_scrip_id_is_looked_up_before_the_trading_symbol() {
    let symbols = resolver(vec![
        row("RELIANCE", "EQT_RELIANCE_EQ_NSE", "2885"),
        row("OTHER", "X", "1"),
    ]);
    let mut feed = order_feed_for(symbols, "KEY:tok");
    let e = event(json!({"symId": "EQT_RELIANCE_EQ_NSE", "trdSym": "X"}));
    let got = parsed(feed.as_mut(), frame(&[event_packet(&e)]));
    let [FeedEvent::OrderUpdate(u)] = got.as_slice() else {
        panic!("one update: {got:?}");
    };
    assert_eq!(u.symbol, "RELIANCE");
}

/// Web test/test_tradejini_order_adapter.py::test_unmapped_symbol_passes_through.
#[test]
fn a_symbol_the_master_does_not_know_is_reported_as_sent() {
    let u = normalized(&event(json!({"trdSym": "UNKNOWN-EQ"}))).unwrap();
    assert_eq!(u.symbol, "UNKNOWN-EQ");
}

/// Web test/test_tradejini_order_adapter.py::test_undocumented_fill_fields_are_not_invented.
/// The documented event has no fill fields, so they stay at the web's defaults.
#[test]
fn fill_fields_the_event_does_not_carry_stay_at_their_defaults() {
    let u = normalized(&event(json!({"status": "completed"}))).unwrap();
    assert_eq!(u.order_status, "complete");
    assert_eq!((u.filled_quantity, u.pending_quantity), (0, 0));
    assert_eq!((u.average_price, u.trigger_price), (0.0, 0.0));
}

/// Web test/test_tradejini_order_adapter.py::test_order_book_field_names_are_read_when_present.
#[test]
fn the_order_books_field_names_are_read_when_present() {
    let u = normalized(&event(json!({
        "status": "completed", "fillQty": 10, "pendingQty": 0, "avgPrice": 2451.0,
        "trigPrice": 2440,
    })))
    .unwrap();
    assert_eq!((u.filled_quantity, u.pending_quantity), (10, 0));
    assert_eq!((u.average_price, u.trigger_price), (2451.0, 2440.0));
}

/// Web test/test_tradejini_order_adapter.py::test_status_mapping.
#[test]
fn statuses_map_to_the_web_vocabulary() {
    for (raw, want) in [
        ("open", "open"),
        ("pending", "open"),
        ("trigger pending", "trigger pending"),
        ("completed", "complete"),
        ("filled", "complete"),
        ("rejected", "rejected"),
        ("cancelled", "cancelled"),
        ("canceled", "cancelled"),
        ("Expired", "expired"),
    ] {
        let u = normalized(&event(json!({ "status": raw }))).unwrap();
        assert_eq!(u.order_status, want, "{raw}");
    }
}

/// Web test/test_tradejini_order_adapter.py::test_side_type_product.
#[test]
fn side_type_and_product_map_to_openalgo() {
    for (side, kind, product, want) in [
        ("sell", "market", "delivery", ("SELL", "MARKET", "CNC")),
        ("b", "stoplimit", "normal", ("BUY", "SL", "NRML")),
        ("s", "stopmarket", "intraday", ("SELL", "SL-M", "MIS")),
    ] {
        let u = normalized(&event(
            json!({"side": side, "type": kind, "product": product}),
        ))
        .unwrap();
        let got = (u.action.as_str(), u.pricetype.as_str(), u.product.as_str());
        assert_eq!(got, want, "{side} {kind} {product}");
    }
}

/// Web test/test_tradejini_order_adapter.py::test_rejection_reason_only_on_rejected.
#[test]
fn a_reason_is_reported_only_for_a_rejection() {
    let rejected = normalized(&event(
        json!({"status": "rejected", "reason": "Insufficient funds"}),
    ))
    .unwrap();
    assert_eq!(rejected.rejection_reason, "Insufficient funds");
    let open = normalized(&event(json!({"reason": "x"}))).unwrap();
    assert_eq!(open.rejection_reason, "");
}

/// Web test/test_tradejini_order_adapter.py::test_non_order_events_are_ignored.
#[test]
fn events_other_than_orders_are_ignored() {
    for kind in ["positions", "trades", ""] {
        assert_eq!(
            normalized(&event(json!({ "evntType": kind }))),
            None,
            "{kind}"
        );
    }
}

/// Web test/test_tradejini_order_adapter.py::test_non_json_event_is_ignored.
#[test]
fn an_event_that_is_not_json_is_ignored() {
    assert_eq!(normalized("not json"), None);
}

/// Web test/test_tradejini_order_adapter.py::test_every_event_in_a_frame_is_published.
#[test]
fn every_order_event_in_a_frame_is_published() {
    let f = frame(&[
        event_packet(&event(json!({"orderId": "1"}))),
        event_packet(&event(json!({"orderId": "2"}))),
    ]);
    assert_eq!(ids(&updates(f)), ["1", "2"]);
}

/// Web test/test_tradejini_order_adapter.py::test_malformed_event_does_not_drop_the_rest_of_the_frame.
#[test]
fn a_malformed_event_does_not_drop_the_rest_of_the_frame() {
    let f = frame(&[
        event_packet(&event(json!({"orderId": "1"}))),
        event_packet(&event(json!({"orderId": "2", "qty": "not-a-number"}))),
        event_packet(&event(json!({"orderId": "3"}))),
    ]);
    assert_eq!(ids(&updates(f)), ["1", "3"]);
}

/// Web test/test_tradejini_order_adapter.py::test_close_4001_stands_the_adapter_down.
#[test]
fn a_close_with_4001_stops_until_the_next_login() {
    let ended = Ended::Closed {
        code: Some(4001),
        reason: "Unauthorized Access",
    };
    match order_feed().on_ended(ended) {
        Reconnect::Stop { kind, message } => {
            assert_eq!(kind, StopKind::Auth);
            assert!(message.contains("Log in to Tradejini again"), "{message}");
        }
        other => panic!("a refused session is not retried: {other:?}"),
    }
}

/// Web test/test_tradejini_order_adapter.py::test_other_closes_still_reconnect.
#[test]
fn any_other_close_reconnects() {
    for code in [Some(1000), Some(1006), None] {
        let ended = Ended::Closed { code, reason: "" };
        assert_eq!(order_feed().on_ended(ended), Reconnect::Backoff, "{code:?}");
    }
    assert_eq!(
        order_feed().on_ended(Ended::Refused(502)),
        Reconnect::Backoff
    );
}

/// Web test/test_tradejini_order_adapter.py::test_auth_accepted_packet_is_quiet.
#[test]
fn an_accepted_session_is_quiet() {
    assert!(parsed(order_feed().as_mut(), frame(&[auth_packet(1)])).is_empty());
}

/// Web test/test_tradejini_order_adapter.py::test_auth_refused_packet_stands_the_adapter_down.
/// The refusal ends the session (the manager closes the socket and does not
/// connect again), and the frame's events go with it.
#[test]
fn a_refused_session_stops_and_drops_its_frame() {
    let f = frame(&[auth_packet(0), event_packet(&event(json!({})))]);
    let got = parsed(order_feed().as_mut(), f);
    assert!(
        matches!(got.as_slice(), [FeedEvent::AuthFailed(m)] if m.contains("Log in to Tradejini again")),
        "{got:?}"
    );
}

// -- wire ---------------------------------------------------------------------

/// Web test/test_tradejini_order_adapter.py::test_subscribe_frame_matches_the_sdk.
#[test]
fn the_socket_asks_for_order_events_as_the_sdk_does() {
    let sent = order_feed().on_connected();
    let [Message::Text(t)] = sent.as_slice() else {
        panic!("one text frame: {sent:?}");
    };
    assert!(t.ends_with('\n'), "{t:?}");
    let v: Value = serde_json::from_str(t.trim_end()).unwrap();
    assert_eq!(
        v,
        json!({"type": "event", "action": "sub", "events": ["orders"]})
    );
}

/// Web test/test_tradejini_order_adapter.py::test_ws_url_uses_fresh_token_joined_to_api_key.
/// The session's token (`api_key:access_token`) is the stream's token, in the
/// URL.
#[test]
fn the_address_carries_the_api_key_and_the_sessions_token() {
    let req = order_feed_for(lookup(), "KEY:fresh").ws_request().unwrap();
    assert_eq!(
        req.uri().to_string(),
        "wss://api.tradejini.com/v2.1/stream?token=KEY:fresh&version=3.1"
    );
}

/// Web test/test_tradejini_order_adapter.py::test_ws_url_keeps_a_token_that_already_has_the_key.
#[test]
fn a_token_joined_to_another_key_is_sent_as_stored() {
    let req = order_feed_for(lookup(), "OTHER:tok").ws_request().unwrap();
    assert!(req.uri().to_string().contains("token=OTHER:tok&"));
}

/// Web test/test_tradejini_order_adapter.py::test_factory_returns_none_without_session and
/// test/test_tradejini_order_adapter.py::test_factory_returns_none_without_api_key.
#[test]
fn no_order_feed_without_a_session_or_an_api_key() {
    let b = TradejiniBroker::new(lookup());
    for token in ["", "tok", ":tok"] {
        assert!(
            b.create_order_feed(&AuthToken::new(token)).is_err(),
            "{token:?}"
        );
    }
}

/// Web test/test_tradejini_order_adapter.py::test_factory_builds_adapter.
#[test]
fn a_session_gets_a_tradejini_order_socket() {
    assert_eq!(order_feed().broker(), "tradejini");
    assert!(TradejiniBroker::new(lookup()).capabilities().order_feed);
}
