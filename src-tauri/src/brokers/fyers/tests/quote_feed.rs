//! Web `test/test_fyers_quote_feed.py` (openalgo#2376): Fyers publishes the
//! quote fields it was sent on every topic, and nothing it was not. The
//! frames are built byte for byte the way HSM sends them, go through the
//! real parser and come out as the 8765 frames the bridge makes of them
//! (`feed::testkit`), with the web test's values.
//!
//! What has no counterpart: the web's LTP and Quote shared one callback
//! slot and routed by symbol-name fallbacks; the desktop's manager counts
//! references per mode and the parser routes by topic name only, so those
//! cases pin behaviour the desktop already had. The Fyers-era names
//! `last_traded_time` and `exchange_time` were never in the desktop's
//! frames, and an equity's change fields are not either (indices carry
//! them, with Fyers' `change`, `change_percent` and `percent_change`).

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const SF: &str = "sf|nse_cm|3045";
const DP: &str = "dp|nse_cm|3045";
const SF_ID: u16 = 11;
const DP_ID: u16 = 12;
const PRICE_SCALE: Option<(u16, u8)> = Some((1, 2));

// `DATA_FIELDS` positions the web test's `BASE` sets.
const LTP: usize = 0;
const VOLUME: usize = 1;
const LTT: usize = 2;
const FEED_TIME: usize = 3;
const BID_SIZE: usize = 4;
const ASK_SIZE: usize = 5;
const BID: usize = 6;
const ASK: usize = 7;
const LTQ: usize = 8;
const TBQ: usize = 9;
const TSQ: usize = 10;
const ATP: usize = 11;
const LOW: usize = 13;
const HIGH: usize = 14;
const OPEN: usize = 19;
const PREV_CLOSE: usize = 20;

/// A record of `len` fields: the given values, absent elsewhere.
fn record(len: usize, set: &[(usize, i32)]) -> Vec<i32> {
    let mut v = vec![NA; len];
    for (i, x) in set {
        v[*i] = *x;
    }
    v
}

/// The web test's `BASE`: SBIN at 801.50 against a previous close of 790.
fn base() -> Vec<(usize, i32)> {
    vec![
        (LTP, 80_150),
        (VOLUME, 523_000),
        (LTT, 1_793_170_000),
        (FEED_TIME, 1_793_170_001),
        (BID_SIZE, 10),
        (ASK_SIZE, 20),
        (BID, 80_145),
        (ASK, 80_150),
        (LTQ, 7),
        (TBQ, 91_000),
        (TSQ, 88_000),
        (ATP, 80_010),
        (LOW, 79_500),
        (HIGH, 80_500),
        (OPEN, 79_800),
        (PREV_CLOSE, 79_000),
    ]
}

fn quote_snapshot(set: &[(usize, i32)]) -> Vec<u8> {
    snapshot(SF_ID, SF, &record(DATA_FIELDS.len(), set), PRICE_SCALE)
}

/// The web test's `BOOK`: the best bid and ask with sizes and orders.
fn book_snapshot() -> Vec<u8> {
    let book = record(
        DEPTH_FIELD_COUNT,
        &[
            (0, 80_145),
            (5, 80_150),
            (10, 10),
            (15, 20),
            (20, 2),
            (25, 3),
        ],
    );
    snapshot(DP_ID, DP, &book, PRICE_SCALE)
}

fn sbin(mode: FeedMode) -> HsmFeed {
    let mut f = feed();
    f.subscribe_frames(&[sub("SBIN", "NSE", mode)]);
    f
}

fn parse(f: &mut HsmFeed, records: &[Vec<u8>]) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(data_frame(records)))
}

/// The web test's `QUOTE_KEYS`, less the names the desktop's frames never
/// had (module doc).
fn assert_full_quote(d: &Value) {
    let expected = json!({
        "ltp": 801.5,
        "open": 798.0,
        "high": 805.0,
        "low": 795.0,
        "close": 790.0,
        "volume": 523_000,
        "average_price": 800.1,
        "avg_trade_price": 800.1,
        "last_quantity": 7,
        "last_trade_quantity": 7,
        "last_trade_time": 1_793_170_000,
        "total_buy_quantity": 91_000,
        "total_sell_quantity": 88_000,
    });
    for (k, v) in expected.as_object().unwrap() {
        assert_eq!(d.get(k), Some(v), "{}: {}", k, d);
    }
}

/// Web `test_depth_subscription_also_subscribes_the_symbol_feed`.
#[test]
fn a_depth_subscription_also_subscribes_the_symbol_feed() {
    assert_eq!(
        hsm_topics("10100000003045", "NSE:SBIN-EQ", "", FeedMode::Depth),
        [SF, DP]
    );
}

/// Web `test_depth_message_carries_every_quote_field`: a depth frame
/// carries the symbol feed's quote and its book totals (the desktop summed
/// the five levels: 10 and 20 here).
#[test]
fn a_depth_frame_carries_every_quote_field() {
    let mut f = sbin(FeedMode::Depth);
    let mut frames = Frames::new("fyers");
    frames.at(&parse(&mut f, &[quote_snapshot(&base())]), Mode::Depth);
    let depth = frames.at(&parse(&mut f, &[book_snapshot()]), Mode::Depth);
    let [d] = &depth[..] else {
        panic!("one depth frame: {:?}", depth)
    };
    assert_full_quote(d);
    assert_eq!(
        d["depth"]["buy"][0],
        json!({"price": 801.45, "quantity": 10, "orders": 2})
    );
}

/// Web `test_depth_follows_the_latest_quote_update` (the desktop's bridge
/// already merged the latest tick).
#[test]
fn a_depth_frame_follows_the_latest_quote_update() {
    let mut f = sbin(FeedMode::Depth);
    let mut frames = Frames::new("fyers");
    frames.at(
        &parse(&mut f, &[quote_snapshot(&base()), book_snapshot()]),
        Mode::Depth,
    );
    let moved = record(
        DATA_FIELDS.len(),
        &[(LTP, 80_200), (VOLUME, 523_500), (HIGH, 80_600)],
    );
    frames.at(&parse(&mut f, &[update(SF_ID, &moved)]), Mode::Depth);
    let bid = record(DEPTH_FIELD_COUNT, &[(10, 15)]);
    let depth = frames.at(&parse(&mut f, &[update(DP_ID, &bid)]), Mode::Depth);
    let [d] = &depth[..] else {
        panic!("one depth frame: {:?}", depth)
    };
    assert_eq!(
        (d["ltp"].as_f64(), d["high"].as_f64()),
        (Some(802.0), Some(806.0))
    );
    assert_eq!(d["volume"], 523_500);
}

/// Web `test_depth_before_the_first_quote_is_sent_again_complete`: the
/// early book has no quote fields (the desktop's frame keeps `ltp`, at 0,
/// which every 8765 frame carries); once the quote comes the book goes
/// again, complete, and only once.
#[test]
fn a_depth_before_the_first_quote_is_sent_again_complete() {
    let mut f = sbin(FeedMode::Depth);
    let mut frames = Frames::new("fyers");
    let early = frames.at(&parse(&mut f, &[book_snapshot()]), Mode::Depth);
    let [e] = &early[..] else {
        panic!("one early depth frame: {:?}", early)
    };
    assert!(
        e.get("open").is_none() && e.get("volume").is_none(),
        "{}",
        e
    );
    assert_eq!(e["ltp"], 0.0);

    let events = parse(&mut f, &[quote_snapshot(&base())]);
    let depth = frames.at(&events, Mode::Depth);
    let [d] = &depth[..] else {
        panic!("the book again: {:?}", events)
    };
    assert_full_quote(d);
    assert_eq!(d["depth"]["buy"][0]["price"], 801.45);

    let next = record(DATA_FIELDS.len(), &[(LTP, 80_200)]);
    let events = parse(&mut f, &[update(SF_ID, &next)]);
    assert!(
        matches!(&events[..], [FeedEvent::Tick(_)]),
        "a later quote does not resend the book: {:?}",
        events
    );
}

/// Web `test_unsent_quote_field_is_left_out_of_depth`.
#[test]
fn a_quote_field_fyers_did_not_send_is_left_out() {
    let mut f = sbin(FeedMode::Depth);
    let mut frames = Frames::new("fyers");
    let partial: Vec<(usize, i32)> = base().into_iter().filter(|(i, _)| *i != ATP).collect();
    let depth = frames.at(
        &parse(&mut f, &[quote_snapshot(&partial), book_snapshot()]),
        Mode::Depth,
    );
    let [d] = &depth[..] else {
        panic!("one depth frame: {:?}", depth)
    };
    for k in ["average_price", "avg_trade_price"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["ltp"], 801.5);
    // SBIN's record carries no OI: none is sent, where Depth sent 0.
    assert!(
        d.get("oi").is_none() && d.get("open_interest").is_none(),
        "{}",
        d
    );
}

/// Web `test_unsubscribing_depth_keeps_quote` (the effective mode moves
/// from Depth to Quote): the quote keeps publishing, the book stops.
#[test]
fn leaving_depth_keeps_the_quote() {
    let mut f = sbin(FeedMode::Depth);
    parse(&mut f, &[quote_snapshot(&base()), book_snapshot()]);
    f.mode_change_frames(
        &sub("SBIN", "NSE", FeedMode::Depth),
        &sub("SBIN", "NSE", FeedMode::Quote),
    );
    let next = record(DATA_FIELDS.len(), &[(LTP, 80_200)]);
    let events = parse(&mut f, &[update(SF_ID, &next)]);
    assert!(
        matches!(&events[..], [FeedEvent::Tick(t)] if t.ltp == 802.0),
        "{:?}",
        events
    );
    let bid = record(DEPTH_FIELD_COUNT, &[(10, 15)]);
    assert!(parse(&mut f, &[update(DP_ID, &bid)]).is_empty());
}

/// Web `test_quote_carries_reference_and_existing_key_names`.
#[test]
fn a_quote_carries_the_reference_and_the_fyers_names() {
    let mut f = sbin(FeedMode::Quote);
    let quote = Frames::new("fyers").at(&parse(&mut f, &[quote_snapshot(&base())]), Mode::Quote);
    let [q] = &quote[..] else {
        panic!("one quote frame: {:?}", quote)
    };
    assert_full_quote(q);
}

/// Web `test_unmapped_topic_is_not_routed_to_the_only_subscription`.
#[test]
fn an_unmapped_topic_reaches_no_subscription() {
    let mut f = sbin(FeedMode::Quote);
    let mut other = base();
    other[0] = (LTP, 1_234_500);
    let rec = snapshot(
        21,
        "sf|nse_cm|99999",
        &record(DATA_FIELDS.len(), &other),
        PRICE_SCALE,
    );
    assert!(parse(&mut f, &[rec]).is_empty());
}

/// Web `test_index_quote_leaves_out_reference_fields_it_has_no_source_for`:
/// an index record has no trade time, average price or traded quantity,
/// and no volume, totals or OI; none is sent as 0. Its change goes out
/// under every name Fyers uses.
#[test]
fn an_index_quote_leaves_out_what_an_index_record_has_no_source_for() {
    let mut f = feed();
    f.subscribe_frames(&[sub("NIFTY", "NSE_INDEX", FeedMode::Quote)]);
    // INDEX_FIELDS: ltp, prev close, feed time, high, low, open.
    let fields = record(
        INDEX_FIELDS.len(),
        &[
            (0, 2_400_000),
            (1, 2_390_000),
            (2, 1_793_170_001),
            (3, 2_410_000),
            (4, 2_380_000),
            (5, 2_395_000),
        ],
    );
    let events = parse(&mut f, &[snapshot(3, "if|nse_cm|Nifty 50", &fields, None)]);
    let quote = Frames::new("fyers").at(&events, Mode::Quote);
    let [q] = &quote[..] else {
        panic!("one quote frame: {:?}", events)
    };
    for k in [
        "last_trade_time",
        "ltt",
        "average_price",
        "last_quantity",
        "volume",
        "oi",
    ] {
        assert!(q.get(k).is_none(), "{}: {}", k, q);
    }
    assert_eq!(
        (q["ltp"].as_f64(), q["open"].as_f64()),
        (Some(24000.0), Some(23950.0))
    );
    assert_eq!(q["price_change"], 100.0);
    for k in ["change_percent", "percent_change"] {
        assert_eq!(q.get(k), q.get("price_change_percent"), "{}: {}", k, q);
    }
}
