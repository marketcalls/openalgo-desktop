//! Web `test/test_definedge_quote_feed.py` (openalgo#2383, `0faa9dbf3`):
//! every mode is built from the merged snapshot, Quote rides the depth feed
//! as well as the touchline, and a field Definedge has not sent is left
//! out rather than sent as 0. The web test's frames go through the real
//! parser and come out as the 8765 frames the bridge makes of them
//! (`feed::testkit`).
//!
//! What has no counterpart: the web matched a tick to the first
//! subscription of its scrip and counted feed references per client; the
//! desktop's manager subscribes one effective mode per instrument and
//! counts references itself, so those cases become the feed frames for an
//! effective mode and its changes. The web's own names `total_buy_qty`,
//! `ltt_time`, `prev_oi` and `bid` were never in the desktop's frames, an
//! equity's change fields are not either, and the desktop's `ltt` is epoch
//! milliseconds: the depth feed's `HH:MM:SS` on the day it arrived.

use super::*;
use crate::brokers::definedge::streaming::clock_on_day_of;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

fn reliance(mode: FeedMode) -> FeedSubscription {
    sub("RELIANCE", "NSE", "NSE", "2885", mode)
}

/// A feed holding RELIANCE at `mode`, the instrument's effective mode.
fn feed(mode: FeedMode) -> DefinedgeFeed {
    let mut f = DefinedgeFeed::new("wss://x/NorenWSTRTP/", "UID1", "suser");
    f.subscribe_frames(&[reliance(mode)]);
    f
}

const TOUCHLINE: &str = r#"{"t":"tf","e":"NSE","tk":"2885","ft":"1730784000","lp":"1395.40","pc":"0.32","v":"11238602","o":"1382.60","h":"1398.00","l":"1378.50","c":"1391.00","ap":"1391.24"}"#;

const DEPTH: &str = r#"{"t":"df","e":"NSE","tk":"2885","lp":"1395.50","ltt":"14:25:31","ltq":"7","tbq":"120952","tsq":"131730","bp1":"1395.40","bq1":"10","sp1":"1395.60","sq1":"12","bp2":"1395.35","sp2":"1395.65"}"#;

/// The IST time of day of an epoch-ms `ltt`, in seconds.
fn ist_seconds_of_day(ltt: &Value) -> i64 {
    let ms = ltt.as_i64().expect("ltt is epoch ms");
    (ms / 1000 + 19_800).rem_euclid(86_400)
}

const AT_14_25_31: i64 = 14 * 3600 + 25 * 60 + 31;

fn last(frames: Vec<Value>) -> Value {
    frames.last().cloned().expect("a frame")
}

/// Web `test_touchline_reaches_quote_even_when_depth_subscribed_first`:
/// with Depth held (the instrument's effective mode), a touchline tick
/// reaches Quote, and Depth gets nothing until a book is in the snapshot.
#[test]
fn a_touchline_reaches_quote_and_no_empty_book_goes_to_depth() {
    let mut f = feed(FeedMode::Depth);
    let ev = f.parse_text(TOUCHLINE);
    assert!(
        ev.iter().all(|e| matches!(e, FeedEvent::Tick(_))),
        "{:?}",
        ev
    );
    let mut frames = Frames::new("definedge");
    let quotes = frames.at(&ev, Mode::Quote);
    assert_eq!(quotes.len(), 1);
    assert_eq!(quotes[0]["ltp"], 1395.4);
    assert!(frames.at(&ev, Mode::Depth).is_empty());
}

/// Web `test_depth_tick_publishes_full_quote_and_depth`: after a depth
/// message, Quote and Depth carry the merged snapshot, and `ltt` is the
/// broker's trade time (14:25:31), not the feed time.
#[test]
fn a_depth_message_carries_the_full_quote_and_the_trade_time() {
    let mut f = feed(FeedMode::Depth);
    let mut frames = Frames::new("definedge");
    frames.at(&f.parse_text(TOUCHLINE), Mode::Quote);
    let ev = f.parse_text(DEPTH);

    let quote = last(frames.at(&ev, Mode::Quote));
    assert_eq!(quote["ltp"], 1395.5);
    assert_eq!(quote["open"], 1382.6);
    assert_eq!(quote["average_price"], 1391.24);
    assert_eq!(quote["last_quantity"], 7);
    assert_eq!(quote["ltq"], 7);
    assert_eq!(ist_seconds_of_day(&quote["ltt"]), AT_14_25_31);

    let depth = last(frames.at(&ev, Mode::Depth));
    assert_eq!(depth["average_price"], 1391.24);
    assert_eq!(depth["total_buy_quantity"], 120_952);
    assert_eq!(depth["total_sell_quantity"], 131_730);
    assert_eq!(depth["last_quantity"], 7);
    assert_eq!(ist_seconds_of_day(&depth["ltt"]), AT_14_25_31);
    assert_eq!(depth["depth"]["buy"][0]["price"], 1395.4);

    assert_eq!(last(frames.at(&ev, Mode::Ltp))["ltp"], 1395.5);
}

/// Web `test_touchline_after_depth_refreshes_depth_with_quote_fields`.
#[test]
fn a_touchline_after_a_book_refreshes_depth() {
    let mut f = feed(FeedMode::Depth);
    let mut frames = Frames::new("definedge");
    frames.at(&f.parse_text(DEPTH), Mode::Depth);
    let ev = f.parse_text(r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1396.00"}"#);

    let depth = last(frames.at(&ev, Mode::Depth));
    assert_eq!(depth["ltp"], 1396.0);
    assert_eq!(depth["total_buy_quantity"], 120_952);
    assert_eq!(last(frames.at(&ev, Mode::Quote))["ltp"], 1396.0);
}

/// Web `test_reference_keys_absent_when_feed_did_not_send_them`: the feed
/// time stands in for `ltt` when no trade time came.
#[test]
fn keys_the_feed_did_not_send_are_left_out() {
    let mut f = feed(FeedMode::Quote);
    let ev = f.parse_text(r#"{"t":"tf","e":"NSE","tk":"2885","lp":"10","ft":"123"}"#);
    let quote = last(Frames::new("definedge").at(&ev, Mode::Quote));
    for k in ["last_quantity", "ltq", "last_trade_time", "percent_change"] {
        assert!(quote.get(k).is_none(), "{}: {}", k, quote);
    }
    assert_eq!(quote["ltt"], 123_000);
}

/// Web `test_other_exchange_token_is_not_published`.
#[test]
fn the_same_token_on_another_exchange_is_not_published() {
    let mut f = feed(FeedMode::Quote);
    assert!(f
        .parse_text(r#"{"t":"tf","e":"BSE","tk":"2885","lp":"10"}"#)
        .is_empty());
}

/// Web `test_quote_subscribes_the_touchline_and_the_depth_feed` and
/// `test_ltp_subscribes_only_the_touchline`.
#[test]
fn quote_subscribes_both_feeds_and_ltp_only_the_touchline() {
    let mut f = DefinedgeFeed::new("wss://x/", "UID1", "suser");
    let frames: Vec<Value> = f
        .subscribe_frames(&[reliance(FeedMode::Quote)])
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(
        frames,
        [
            json!({"t":"t","k":"NSE|2885"}),
            json!({"t":"d","k":"NSE|2885"})
        ]
    );
    let mut f = DefinedgeFeed::new("wss://x/", "UID1", "suser");
    let frames: Vec<Value> = f
        .subscribe_frames(&[reliance(FeedMode::Ltp)])
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(frames, [json!({"t":"t","k":"NSE|2885"})]);
}

/// Web `test_depth_ack_fills_a_quote_only_subscriber_without_a_depth_topic`:
/// a touchline acknowledgement without OHLC, then the depth one with the
/// full snapshot, fill a Quote, and no Depth is published.
#[test]
fn the_depth_ack_fills_a_quote_and_publishes_no_depth() {
    let mut f = feed(FeedMode::Quote);
    let mut frames = Frames::new("definedge");
    let mut ev = f.parse_text(r#"{"t":"tk","e":"NSE","tk":"2885","lp":"1395.40"}"#);
    let ack = DEPTH.replace(r#""t":"df""#, r#""t":"dk""#).replace(
        r#""lp":"1395.50""#,
        r#""lp":"1395.50","o":"1382.60","c":"1391.00""#,
    );
    ev.extend(f.parse_text(&ack));
    assert!(
        ev.iter().all(|e| matches!(e, FeedEvent::Tick(_))),
        "{:?}",
        ev
    );

    let quote = last(frames.at(&ev, Mode::Quote));
    assert_eq!(quote["open"], 1382.6);
    assert_eq!(quote["close"], 1391.0);
    assert_eq!(quote["last_quantity"], 7);
    assert_eq!(quote["total_buy_quantity"], 120_952);
    assert_eq!(ist_seconds_of_day(&quote["ltt"]), AT_14_25_31);
}

/// Web `test_quote_never_publishes_fields_not_received_as_zero`.
#[test]
fn a_quote_never_sends_a_field_not_received_as_zero() {
    let mut f = feed(FeedMode::Quote);
    let ev = f.parse_text(r#"{"t":"tf","e":"NSE","tk":"2885","lp":"1800.10"}"#);
    let quote = last(Frames::new("definedge").at(&ev, Mode::Quote));
    assert_eq!(quote["ltp"], 1800.1);
    for k in [
        "open",
        "high",
        "low",
        "close",
        "volume",
        "average_price",
        "oi",
        "ltt",
    ] {
        assert!(quote.get(k).is_none(), "{}: {}", k, quote);
    }
}

/// Web `test_unsubscribing_quote_releases_both_feeds`.
#[test]
fn unsubscribing_quote_releases_both_feeds() {
    let mut f = feed(FeedMode::Quote);
    let frames: Vec<Value> = f
        .unsubscribe_frames(&[reliance(FeedMode::Quote)])
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(
        frames,
        [
            json!({"t":"u","k":"NSE|2885"}),
            json!({"t":"ud","k":"NSE|2885"})
        ]
    );
    assert_eq!(f.cached(), 0);
}

/// Web `test_unsubscribing_quote_keeps_the_depth_feed_a_depth_subscriber_needs`:
/// the instrument moving between Quote and Depth changes only the
/// touchline; the depth feed (and the snapshot) stay.
#[test]
fn moving_between_quote_and_depth_keeps_the_depth_feed() {
    let mut f = feed(FeedMode::Quote);
    f.parse_text(DEPTH);
    let up: Vec<Value> = f
        .mode_change_frames(&reliance(FeedMode::Quote), &reliance(FeedMode::Depth))
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(up, [json!({"t":"u","k":"NSE|2885"})]);
    assert_eq!(f.cached(), 1, "the depth feed's snapshot stays");
    let down: Vec<Value> = f
        .mode_change_frames(&reliance(FeedMode::Depth), &reliance(FeedMode::Quote))
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(down, [json!({"t":"t","k":"NSE|2885"})]);
    let ltp: Vec<Value> = f
        .mode_change_frames(&reliance(FeedMode::Quote), &reliance(FeedMode::Ltp))
        .iter()
        .map(text_of)
        .collect();
    assert_eq!(ltp, [json!({"t":"ud","k":"NSE|2885"})]);
}

/// Web `test_a_dash_ltp_does_not_stop_the_other_modes`: no price, nothing
/// on LTP; the Quote still carries what came.
#[test]
fn a_dash_price_sends_no_ltp_and_still_a_quote() {
    let dashed = TOUCHLINE.replace(r#""lp":"1395.40""#, r#""lp":"-""#);
    assert!(feed(FeedMode::Ltp).parse_text(&dashed).is_empty());
    let ev = feed(FeedMode::Quote).parse_text(&dashed);
    assert_eq!(
        last(Frames::new("definedge").at(&ev, Mode::Quote))["open"],
        1382.6
    );
}

/// Web `test_a_level_one_only_depth_message_publishes_depth`.
#[test]
fn a_level_one_depth_message_publishes_depth() {
    let mut f = feed(FeedMode::Depth);
    let ev = f.parse_text(
        r#"{"t":"df","e":"NSE","tk":"2885","lp":"1395.50","bp1":"1395.40","sp1":"1395.60"}"#,
    );
    let depth = last(Frames::new("definedge").at(&ev, Mode::Depth));
    assert_eq!(depth["depth"]["buy"][0]["price"], 1395.4);
    assert_eq!(depth["depth"]["sell"][0]["price"], 1395.6);
}

/// The depth feed's `HH:MM:SS` is a time on the IST day the tick came, or
/// the day before when it would lie ahead of the tick (a trade at
/// 23:59:59 heard just after midnight).
#[test]
fn a_trade_clock_is_placed_on_the_day_it_was_heard() {
    // 2026-11-05 14:30:00 IST.
    let heard = 1_793_869_200_000;
    assert_eq!(
        clock_on_day_of("14:25:31", heard),
        Some(heard - (4 * 60 + 29) * 1000)
    );
    // 2026-11-06 00:00:05 IST: 23:59:59 was the evening before.
    let after_midnight = 1_793_903_405_000;
    assert_eq!(
        clock_on_day_of("23:59:59", after_midnight),
        Some(after_midnight - 6_000)
    );
    assert_eq!(clock_on_day_of("2:25 PM", heard), None);
}

/// Web `_QUOTE_FIELDS` sends `oi` in Quote whenever Definedge sent it: a
/// Quote packet carries its OI (the packet's mode, not the
/// subscription's), so a Quote holder never keeps an older OI.
#[test]
fn a_quote_carries_the_oi_definedge_sent() {
    let mut f = DefinedgeFeed::new("wss://x/NorenWSTRTP/", "UID1", "suser");
    f.subscribe_frames(&[sub(
        "NIFTY25NOV25FUT",
        "NFO",
        "NFO",
        "52345",
        FeedMode::Quote,
    )]);
    let ev = f.parse_text(r#"{"t":"tf","e":"NFO","tk":"52345","lp":"24415.00","oi":"14262000"}"#);
    let quote = last(Frames::new("definedge").at(&ev, Mode::Quote));
    assert_eq!(quote["oi"], 14_262_000);
}

/// A message without a price publishes nothing, never an LTP of 0 (the
/// web leaves `ltp` out of such a Quote; the desktop's frame always has
/// one, so it waits for the first price).
#[test]
fn a_quote_without_a_price_publishes_nothing() {
    let mut f = feed(FeedMode::Quote);
    let ev = f.parse_text(r#"{"t":"tk","e":"NSE","tk":"2885","o":"1382.60","c":"1391.00"}"#);
    assert!(Frames::new("definedge").at(&ev, Mode::Quote).is_empty());
}
