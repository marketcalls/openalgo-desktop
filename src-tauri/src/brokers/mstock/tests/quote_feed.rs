//! Web `test/test_mstock_quote_feed.py` (openalgo#2383, `0faa9dbf3`): a
//! tick goes to the instrument of its exchange type and token, and carries
//! the trade time its packet holds, or none. The web test's packets are
//! built at the web's byte offsets, go through the real parser and come out
//! as the 8765 frames the bridge makes of them (`feed::testkit`).
//!
//! What has no counterpart: the routing and the subscriptions were keyed
//! on exchange type and token in the desktop already, and the web's own
//! stream ids and retry budgets are the manager's here, so those cases pin
//! behaviour the desktop already had. The web's `exchange_timestamp` key
//! was never in the desktop's frames; its `last_trade_time` is `ltt`'s
//! value, in epoch milliseconds in every packet.

use super::*;
use crate::brokers::common::streaming::BrokerFeed;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const EXCHANGE_TIME: u64 = 1_730_784_000_000;

/// A packet with the web test's `_quote_packet` values: INFY at 1500.50
/// (token 1594, exchange type 1). A full packet (mode 3) carries
/// `last_traded` at its trade-time offset and an empty book.
fn quote_packet(mode: u8, token: &str, exchange_time: u64, last_traded: u64) -> Vec<u8> {
    let len = if mode == 3 { SNAP_LEN } else { QUOTE_LEN };
    let mut p = vec![0u8; len];
    p[0] = mode;
    p[1] = 1;
    p[2..2 + token.len()].copy_from_slice(token.as_bytes());
    let mut put = |o: usize, v: u64| p[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put(35, exchange_time);
    put(43, 150_050);
    put(51, 25);
    put(59, 149_800);
    put(67, 1000);
    put(91, 149_000);
    put(99, 150_500);
    put(107, 148_500);
    put(115, 149_500);
    if mode == 3 {
        put(123, last_traded);
    }
    p[75..83].copy_from_slice(&10.0f64.to_le_bytes());
    p[83..91].copy_from_slice(&20.0f64.to_le_bytes());
    p
}

/// The web fixture's two subscriptions: one token on NSE and on BSE.
fn feed() -> MstockFeed {
    let mut f = MstockFeed::new("wss://ws.mstock.trade", "jwt.x", "pk");
    f.subscribe_frames(&[
        sub("INFY", "NSE", "1594", FeedMode::Quote),
        sub("OTHER", "BSE", "1594", FeedMode::Quote),
    ]);
    f
}

fn quote_frames(packet: Vec<u8>) -> Vec<Value> {
    let ev = feed().parse(&Message::Binary(packet));
    Frames::new("mstock").at(&ev, Mode::Quote)
}

/// Web `test_tick_routes_on_exchange_type_and_token`.
#[test]
fn a_tick_goes_to_its_exchange_type_and_token() {
    let ev = feed().parse(&Message::Binary(quote_packet(2, "1594", EXCHANGE_TIME, 0)));
    let to: Vec<(String, String)> = ev
        .iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some((t.symbol.clone(), t.exchange.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(to, [("INFY".to_string(), "NSE".to_string())]);
}

/// Web `test_quote_carries_exchange_time_and_last_quantity`.
#[test]
fn a_quote_carries_the_exchange_time_and_the_last_quantity() {
    let d = &quote_frames(quote_packet(2, "1594", EXCHANGE_TIME, 0))[0];
    assert_eq!(d["last_quantity"], 25);
    assert_eq!(d["last_trade_quantity"], 25);
    assert_eq!(d["ltt"], 1_730_784_000_000_i64);
    assert_eq!(d["last_trade_time"], 1_730_784_000_000_i64);
}

/// Web `test_full_packet_prefers_last_traded_timestamp`: the trade time,
/// epoch seconds in the packet, in milliseconds.
#[test]
fn a_full_packet_carries_its_last_traded_timestamp() {
    let d = &quote_frames(quote_packet(3, "1594", EXCHANGE_TIME, 1_730_783_990))[0];
    assert_eq!(d["ltt"], 1_730_783_990_000_i64);
    assert_eq!(d["last_trade_time"], 1_730_783_990_000_i64);
}

/// Web `test_no_time_published_when_feed_sends_none`.
#[test]
fn no_time_is_published_when_the_packet_has_none() {
    let d = &quote_frames(quote_packet(2, "1594", 0, 0))[0];
    assert!(
        d.get("ltt").is_none() && d.get("last_trade_time").is_none(),
        "{}",
        d
    );
}

/// Web `test_a_tick_for_another_token_on_the_same_exchange_is_not_published`.
#[test]
fn another_token_on_the_same_exchange_is_not_published() {
    let ev = feed().parse(&Message::Binary(quote_packet(2, "2885", EXCHANGE_TIME, 0)));
    assert!(ev.is_empty(), "{:?}", ev);
}

/// Web `test_full_packet_without_a_trade_publishes_no_trade_time`: a full
/// packet with no trade yet does not pass its exchange time off as one.
#[test]
fn a_full_packet_without_a_trade_has_no_trade_time() {
    let d = &quote_frames(quote_packet(3, "1594", EXCHANGE_TIME, 0))[0];
    assert!(
        d.get("ltt").is_none() && d.get("last_trade_time").is_none(),
        "{}",
        d
    );
    assert_eq!(d["ltp"], 1500.5);
}

/// Web `test_same_token_on_two_exchanges_is_subscribed_for_both`.
#[test]
fn one_token_on_two_exchanges_is_subscribed_for_both() {
    let mut f = MstockFeed::new("wss://ws.mstock.trade", "jwt.x", "pk");
    let frames = f.subscribe_frames(&[
        sub("INFY", "NSE", "1594", FeedMode::Quote),
        sub("OTHER", "BSE", "1594", FeedMode::Quote),
    ]);
    let sent: Vec<Value> = frames
        .iter()
        .map(|m| match m {
            Message::Text(t) => serde_json::from_str(t).unwrap(),
            _ => Value::Null,
        })
        .collect();
    assert_eq!(
        sent,
        [json!({"action": 1, "params": {"mode": 2, "tokenList": [
            {"exchangeType": 1, "tokens": ["1594"]},
            {"exchangeType": 3, "tokens": ["1594"]}
        ]}})]
    );
    assert_eq!(f.registered(), 2);
}
