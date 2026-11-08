//! Web `test/test_dhan_quote_feed.py` (openalgo#2377, `e2e31e91f`): a
//! quote's close is the previous day's, from the previous-close packet
//! Dhan sends on every subscribe, never the day close a quote packet
//! carries after the market; it is left out until that packet has come.
//! The packets are built byte for byte as Dhan sends them, with the web
//! test's values, go through the real parser and come out as the 8765
//! frames the bridge makes of them (`feed::testkit`).
//!
//! What has no counterpart: the web publishes the day close as
//! `day_close` and the previous OI as `prev_oi`; the desktop's frames have
//! one schema and neither key (`feed::dialect`). The web's adapter keeps
//! one record per mode; the desktop's manager gives the feed one
//! subscription per instrument at its highest mode, so "unsubscribing one
//! mode while another remains" is a mode change here.

use super::*;
use crate::brokers::common::streaming::QuoteField;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const SEG_NSE: u8 = 1;
/// RELIANCE on NSE.
const SEC_ID: u32 = 2885;
/// Dhan's trade time, epoch seconds.
const LTT: u32 = 1_761_700_000;

fn prev_close_frame(prev_close: f32, prev_oi: u32) -> Vec<u8> {
    let mut v = header(6, 8, SEG_NSE, SEC_ID);
    v.extend(f32le(prev_close));
    v.extend(prev_oi.to_le_bytes());
    v
}

/// The fields a quote and a full packet share, up to the book totals:
/// ltp, last quantity, trade time, average price, volume, total sell and
/// total buy quantity.
fn trade_fields() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(f32le(1300.5));
    p.extend(25u16.to_le_bytes());
    p.extend(LTT.to_le_bytes());
    p.extend(f32le(1298.25));
    p.extend(40_000u32.to_le_bytes());
    p.extend(7_000u32.to_le_bytes());
    p.extend(9_000u32.to_le_bytes());
    p
}

/// The web test's `quote_frame`: open, the day close (sent after the
/// market only), high, low after the trade fields.
fn quote_frame(day_close: f32) -> Vec<u8> {
    let mut p = trade_fields();
    for v in [1290.0, day_close, 1310.0, 1285.0] {
        p.extend(f32le(v));
    }
    let mut v = header(4, p.len(), SEG_NSE, SEC_ID);
    v.extend(p);
    v
}

/// The web test's `full_frame`: OI, its high and low, then open, the day
/// close, high and low, then five levels (bid and ask quantity, orders,
/// price).
fn full_frame(day_close: f32) -> Vec<u8> {
    let mut p = trade_fields();
    for oi in [150u32, 160, 140] {
        p.extend(oi.to_le_bytes());
    }
    for v in [1290.0, day_close, 1310.0, 1285.0] {
        p.extend(f32le(v));
    }
    for i in 0..5u16 {
        p.extend(u32::from(10 + i).to_le_bytes());
        p.extend(u32::from(20 + i).to_le_bytes());
        p.extend(1u16.to_le_bytes());
        p.extend(2u16.to_le_bytes());
        p.extend(f32le(1300.0 - f32::from(i)));
        p.extend(f32le(1301.0 + f32::from(i)));
    }
    assert_eq!(p.len(), 154);
    let mut v = header(8, p.len(), SEG_NSE, SEC_ID);
    v.extend(p);
    v
}

fn reliance(mode: FeedMode) -> DhanFeed {
    let mut f = DhanFeed::with_url("wss://x", "t", "c");
    f.subscribe_frames(&[sub("RELIANCE", "NSE", mode)]);
    f
}

fn parse(f: &mut DhanFeed, frame: Vec<u8>) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(frame))
}

/// The one frame a subscriber at `mode` receives for `events`.
fn the_frame(events: &[FeedEvent], mode: Mode) -> Value {
    let frames = Frames::new("dhan").at(events, mode);
    match frames.as_slice() {
        [d] => d.clone(),
        other => panic!("one frame expected: {:?}", other),
    }
}

/// Web test_quote_close_is_previous_close_not_day_close: the previous
/// close is kept, not published alone, and is the quote's close; `ltt` is
/// the trade time, with Dhan's `last_trade_time` beside it in seconds.
#[test]
fn the_quote_close_is_the_previous_close() {
    let mut f = reliance(FeedMode::Quote);
    assert!(parse(&mut f, prev_close_frame(1280.5, 120)).is_empty());
    let d = the_frame(&parse(&mut f, quote_frame(0.0)), Mode::Quote);
    assert_eq!(d["close"], 1280.5);
    assert!(d.get("day_close").is_none(), "{}", d);
    assert_eq!(d["ltt"], i64::from(LTT) * 1000);
    assert_eq!(d["last_trade_time"], LTT);
}

/// Web test_day_close_kept_separately_after_market: after the market the
/// quote packet carries a day close; the close stays the previous one.
#[test]
fn the_day_close_after_the_market_is_not_the_close() {
    let mut f = reliance(FeedMode::Quote);
    parse(&mut f, prev_close_frame(1280.5, 120));
    let d = the_frame(&parse(&mut f, quote_frame(1301.0)), Mode::Quote);
    assert_eq!(d["close"], 1280.5);
}

/// Web test_close_omitted_until_prev_close_arrives: no previous close yet,
/// no close, whatever day close the packet carries.
#[test]
fn the_close_is_left_out_until_the_previous_close_comes() {
    let mut f = reliance(FeedMode::Quote);
    let events = parse(&mut f, quote_frame(1301.0));
    let d = the_frame(&events, Mode::Quote);
    assert!(d.get("close").is_none(), "{}", d);
    assert_eq!(d["open"], 1290.0);
    let FeedEvent::Tick(t) = &events[0] else {
        panic!("{:?}", events)
    };
    assert!(t.absent.contains(QuoteField::Close) && t.absent.contains(QuoteField::Change));
}

/// Web test_full_payload_carries_quote_fields_and_prev_close: a full
/// packet's Depth frame carries every quote field, the previous close and
/// the book.
#[test]
fn a_full_packet_carries_the_quote_fields_and_the_previous_close() {
    let mut f = reliance(FeedMode::Depth);
    parse(&mut f, prev_close_frame(1280.5, 120));
    let d = the_frame(&parse(&mut f, full_frame(0.0)), Mode::Depth);
    assert_eq!(d["close"], 1280.5);
    assert_eq!(d["last_quantity"], 25);
    assert_eq!(d["average_price"], 1298.25);
    assert_eq!(
        (
            d["total_buy_quantity"].as_i64(),
            d["total_sell_quantity"].as_i64()
        ),
        (Some(9000), Some(7000))
    );
    assert_eq!(d["ltt"], i64::from(LTT) * 1000);
    assert_eq!(d["last_trade_time"], LTT);
    assert_eq!(
        (d["oi"].as_i64(), d["volume"].as_i64()),
        (Some(150), Some(40_000))
    );
    assert_eq!(
        d["depth"]["buy"][0],
        json!({"price": 1300.0, "quantity": 10, "orders": 1})
    );
    assert_eq!(
        d["depth"]["sell"][0],
        json!({"price": 1301.0, "quantity": 20, "orders": 2})
    );
}

/// Web test_unsubscribe_clears_stored_prev_close and
/// test_unsubscribe_all_clears_prev_close: an instrument subscribed again
/// has no close until Dhan sends a fresh previous close.
#[test]
fn an_unsubscribe_forgets_the_previous_close() {
    let mut f = reliance(FeedMode::Quote);
    parse(&mut f, prev_close_frame(1280.5, 120));
    f.unsubscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    assert_eq!(f.subscription_count(), 0);
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    let d = the_frame(&parse(&mut f, quote_frame(0.0)), Mode::Quote);
    assert!(d.get("close").is_none(), "{}", d);
}

/// Web test_unsubscribe_keeps_prev_close_while_another_mode_remains: the
/// Depth subscriber leaves, the Quote one stays; Dhan is asked to move the
/// instrument (unsubscribe full, subscribe quote) and the close stays.
#[test]
fn a_mode_change_keeps_the_previous_close() {
    let mut f = reliance(FeedMode::Depth);
    parse(&mut f, prev_close_frame(1280.5, 120));
    let frames = texts(&f.mode_change_frames(
        &sub("RELIANCE", "NSE", FeedMode::Depth),
        &sub("RELIANCE", "NSE", FeedMode::Quote),
    ));
    let codes: Vec<_> = frames.iter().map(|v| v["RequestCode"].clone()).collect();
    assert_eq!(codes, [json!(22), json!(17)]);
    let d = the_frame(&parse(&mut f, quote_frame(0.0)), Mode::Quote);
    assert_eq!(d["close"], 1280.5);
}

/// Web test_disconnect_clears_prev_close: a new connection, on which the
/// manager subscribes everything again, has no close until Dhan sends it
/// again.
#[test]
fn a_new_connection_forgets_the_previous_close() {
    let mut f = reliance(FeedMode::Quote);
    parse(&mut f, prev_close_frame(1280.5, 120));
    f.on_connected();
    f.subscribe_frames(&[sub("RELIANCE", "NSE", FeedMode::Quote)]);
    let d = the_frame(&parse(&mut f, quote_frame(0.0)), Mode::Quote);
    assert!(d.get("close").is_none(), "{}", d);
    parse(&mut f, prev_close_frame(1281.0, 0));
    let d = the_frame(&parse(&mut f, quote_frame(0.0)), Mode::Quote);
    assert_eq!(d["close"], 1281.0);
}
