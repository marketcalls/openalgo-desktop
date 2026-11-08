//! Web `test/test_tradejini_stream_lifecycle.py` (openalgo#2341,
//! `f5805220b`): Depth rides L1 as well as L5, its view carries the L1
//! price fields and is sent again on each price tick, and the caches go
//! with the subscription. The packets go through the real decoder and come
//! out as the 8765 frames the bridge makes of them (`feed::testkit`).
//!
//! What has no counterpart: the web's socket races (a connect racing a
//! disconnect, a close mid-handshake, a reconnect after close) live in its
//! NxtradStream thread; the desktop's live feed runs on the shared manager
//! (one owned task, the socket closed before every reconnect), and a quote
//! or depth request's socket is closed on every path or dropped with its
//! future (`tests/it/brokers_direct_batch_b/tradejini.rs` drives that).

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

/// The web test's `L1_TICK`: INFY (token 1594, NSE) at 1155.
fn l1_tick(ltp: i32) -> Vec<u8> {
    build::packet(
        10,
        &[
            (26, V::U8(1)),
            (27, V::I32(1594)),
            (29, V::I32(ltp)),
            (30, V::I32(119_190)),
            (31, V::I32(119_450)),
            (32, V::I32(115_100)),
            (33, V::I32(118_500)),
            (40, V::U32(1_100_000)),
        ],
    )
}

/// The web test's `L5_TICK`.
fn l5_tick() -> Vec<u8> {
    build::packet(
        11,
        &[
            (26, V::U8(1)),
            (27, V::I32(1594)),
            (47, V::U32(543_853)),
            (48, V::U32(600_000)),
            (55, V::U8(1)),
            (49, V::I32(115_480)),
            (50, V::U32(123)),
            (51, V::U32(3)),
            (52, V::I32(115_500)),
            (53, V::U32(50)),
            (54, V::U32(1)),
        ],
    )
}

fn infy(mode: FeedMode) -> FeedSubscription {
    sub("INFY", "NSE", "1594", "NSE", mode)
}

fn depth_feed() -> TradejiniFeed {
    let mut f = TradejiniFeed::new(streaming::STREAM_URL, "key", "tok");
    f.subscribe_frames(&[infy(FeedMode::Depth)]);
    f
}

fn parse(f: &mut TradejiniFeed, packet: Vec<u8>) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(build::frame(&[packet], false)))
}

/// Web `test_depth_rides_the_l1_feed_too`.
#[test]
fn depth_rides_l1_as_well_as_l5() {
    let mut f = TradejiniFeed::new(streaming::STREAM_URL, "key", "tok");
    let frames: Vec<Value> = f
        .subscribe_frames(&[infy(FeedMode::Depth)])
        .iter()
        .map(text)
        .collect();
    let tokens = |feed: &str| {
        frames
            .iter()
            .find(|f| f["type"] == feed)
            .map(|f| f["tokens"].clone())
    };
    assert_eq!(tokens("L1"), Some(serde_json::json!([{"t": "1594_NSE"}])));
    assert_eq!(tokens("L5"), Some(serde_json::json!([{"t": "1594_NSE"}])));
}

/// Web `test_depth_update_carries_l1_price_fields`: L5 has no LTP or OHLC,
/// they come from L1; a later price tick sends the Depth view again with
/// the cached book.
#[test]
fn a_depth_view_carries_the_l1_price_fields_and_follows_price_ticks() {
    let mut f = depth_feed();
    let mut frames = Frames::new("tradejini");
    let ev = parse(&mut f, l1_tick(115_500));
    assert!(frames.at(&ev, Mode::Depth).is_empty(), "no book yet");

    let ev = parse(&mut f, l5_tick());
    let depth = frames.at(&ev, Mode::Depth).pop().expect("a Depth frame");
    assert_eq!(depth["ltp"], 1155.0);
    assert_eq!(depth["open"], 1191.9);
    assert_eq!(depth["volume"], 1_100_000);
    assert_eq!(depth["total_buy_quantity"], 543_853);
    assert_eq!(
        depth["depth"]["buy"][0],
        serde_json::json!({"price": 1154.8, "quantity": 123, "orders": 3})
    );

    let ev = parse(&mut f, l1_tick(115_600));
    let depth = frames
        .at(&ev, Mode::Depth)
        .pop()
        .expect("a price tick sends the Depth view again");
    assert_eq!(depth["ltp"], 1156.0);
    assert_eq!(depth["depth"]["buy"][0]["price"], 1154.8);
}

/// Web `test_unsubscribe_drops_cached_ticks` and
/// `test_subscribe_l1_drops_quotes_for_tokens_no_longer_listed`.
#[test]
fn unsubscribing_drops_the_cached_quote_and_book() {
    let mut f = depth_feed();
    parse(&mut f, l1_tick(115_500));
    parse(&mut f, l5_tick());
    assert_eq!(f.cached(), 2);
    f.unsubscribe_frames(&[infy(FeedMode::Depth)]);
    assert_eq!(f.cached(), 0);
}

// Depth cache review fixes (openalgo#2359, `a92418967`). The web's retry
// of a failed list send has no counterpart: a send that fails ends the
// desktop's session, and the manager replays every subscription on the
// next socket.

/// Web `test_depth_waits_for_l1_when_the_book_arrives_first`: a book alone
/// would show LTP and OHLC as 0.
#[test]
fn a_depth_view_waits_for_the_quote_when_the_book_comes_first() {
    let mut f = depth_feed();
    let mut frames = Frames::new("tradejini");
    let ev = parse(&mut f, l5_tick());
    assert!(frames.at(&ev, Mode::Depth).is_empty(), "{:?}", ev);

    let ev = parse(&mut f, l1_tick(115_500));
    let depth = frames.at(&ev, Mode::Depth).pop().expect("a Depth frame");
    assert_eq!(depth["ltp"], 1155.0);
    assert_eq!(depth["open"], 1191.9);
    assert_eq!(depth["depth"]["buy"][0]["price"], 1154.8);
}

/// Web `test_closed_session_drops_cached_halves`: a new connection pairs
/// no fresh quote with a book from before the outage.
#[test]
fn a_new_connection_drops_the_cached_quote_and_book() {
    let mut f = depth_feed();
    parse(&mut f, l1_tick(115_500));
    parse(&mut f, l5_tick());
    f.on_connected();
    assert_eq!(f.cached(), 0);
    let ev = parse(&mut f, l1_tick(116_000));
    assert!(
        Frames::new("tradejini").at(&ev, Mode::Depth).is_empty(),
        "{:?}",
        ev
    );
}

/// Web `test_dropping_depth_keeps_the_quote_but_not_the_book`: Depth
/// dropped while LTP stays (the instrument moves down to LTP) keeps the L1
/// quote, loses the book, and a book still in flight is not cached again.
#[test]
fn dropping_depth_keeps_the_quote_but_not_the_book() {
    let mut f = depth_feed();
    parse(&mut f, l1_tick(115_500));
    parse(&mut f, l5_tick());
    f.mode_change_frames(&infy(FeedMode::Depth), &infy(FeedMode::Ltp));
    assert_eq!(f.cached(), 1, "the L1 quote stays, the book goes");

    let ev = parse(&mut f, l5_tick());
    assert!(ev.is_empty(), "{:?}", ev);
    assert_eq!(f.cached(), 1, "a book in flight is not cached again");
}
