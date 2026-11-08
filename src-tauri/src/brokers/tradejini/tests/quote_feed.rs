//! Web `test/test_tradejini_quote_feed.py` (openalgo#2383, `0faa9dbf3`):
//! a Quote leaves out the book totals until an L5 book has come, and a
//! Depth frame keeps the L1 quote fields. The web test's packets go through
//! the real decoder and come out as the 8765 frames the bridge makes of
//! them (`feed::testkit`).
//!
//! What has no counterpart: the web's `change`, `change_percent` and
//! `percent_change` on an equity (the desktop sends the change fields for
//! indices only, with Tradejini's names), and its `last_trade_time`, the
//! trade time as the web server's local date text; the desktop sends that
//! time as `ltt`, in epoch milliseconds.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

/// 2026-11-05 14:25:31 IST, the web test's `ltt`.
const LTT: i32 = 1_793_868_931;

/// The web test's `L1`: SBIN at 810.50 (NSE, divisor 100).
fn l1() -> Vec<u8> {
    build::packet(
        10,
        &[
            (29, V::I32(81_050)),
            (30, V::I32(80_000)),
            (31, V::I32(81_500)),
            (32, V::I32(79_800)),
            (33, V::I32(80_500)),
            (34, V::I32(550)),
            (35, V::I32(68)),
            (36, V::I32(80_820)),
            (39, V::U32(15)),
            (40, V::U32(123_456)),
            (44, V::U32(0)),
            (46, V::I32(LTT)),
            (26, V::U8(1)),
            (27, V::I32(3045)),
        ],
    )
}

/// The web test's `L5`: totals 5000 / 7000 and one level a side.
fn l5() -> Vec<u8> {
    build::packet(
        11,
        &[
            (26, V::U8(1)),
            (27, V::I32(3045)),
            (47, V::U32(5000)),
            (48, V::U32(7000)),
            (55, V::U8(1)),
            (49, V::I32(81_040)),
            (50, V::U32(10)),
            (51, V::U32(2)),
            (52, V::I32(81_060)),
            (53, V::U32(12)),
            (54, V::U32(3)),
        ],
    )
}

/// A feed holding SBIN at `mode`, the instrument's effective mode.
fn feed(mode: FeedMode) -> TradejiniFeed {
    let mut f = TradejiniFeed::new(streaming::STREAM_URL, "key", "tok");
    f.subscribe_frames(&[sub("SBIN", "NSE", "3045", "NSE", mode)]);
    f
}

fn parse(f: &mut TradejiniFeed, packet: Vec<u8>) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(build::frame(&[packet], false)))
}

fn last(frames: Vec<Value>) -> Value {
    frames.last().cloned().expect("a frame")
}

/// Web `test_quote_omits_totals_without_a_book`.
#[test]
fn a_quote_leaves_out_the_totals_without_a_book() {
    let mut f = feed(FeedMode::Quote);
    let ev = parse(&mut f, l1());
    let quote = last(Frames::new("tradejini").at(&ev, Mode::Quote));
    for k in ["total_buy_quantity", "total_sell_quantity"] {
        assert!(quote.get(k).is_none(), "{}: {}", k, quote);
    }
    assert_eq!(quote["ltt"], i64::from(LTT) * 1000);
    assert_eq!(quote["average_price"], 808.2);
}

/// Web `test_quote_uses_book_totals_when_depth_subscribed`.
#[test]
fn a_quote_carries_the_book_totals_while_depth_is_held() {
    let mut f = feed(FeedMode::Depth);
    let mut frames = Frames::new("tradejini");
    let mut ev = parse(&mut f, l5());
    ev.extend(parse(&mut f, l1()));
    let quote = last(frames.at(&ev, Mode::Quote));
    assert_eq!(quote["total_buy_quantity"], 5000);
    assert_eq!(quote["total_sell_quantity"], 7000);
}

/// Web `test_depth_keeps_l1_quote_fields`, and the Quote the first book
/// completes with its totals.
#[test]
fn a_depth_frame_keeps_the_l1_quote_fields() {
    let mut f = feed(FeedMode::Depth);
    let mut frames = Frames::new("tradejini");
    let mut ev = parse(&mut f, l1());
    ev.extend(parse(&mut f, l5()));

    let depth = last(frames.at(&ev, Mode::Depth));
    assert_eq!(depth["average_price"], 808.2);
    assert_eq!(depth["last_quantity"], 15);
    assert_eq!(depth["total_buy_quantity"], 5000);
    assert_eq!(depth["total_sell_quantity"], 7000);
    assert_eq!(depth["ltt"], i64::from(LTT) * 1000);
    assert_eq!(depth["depth"]["buy"][0]["price"], 810.4);

    let quote = last(frames.at(&ev, Mode::Quote));
    assert_eq!(quote["total_buy_quantity"], 5000);
}

/// A quote without a trade time from Tradejini sends no `ltt` (web
/// `_add_reference_keys` sends no trade time it was not given), rather
/// than the time it was received.
#[test]
fn no_trade_time_from_tradejini_sends_no_ltt() {
    let mut f = feed(FeedMode::Quote);
    let ev = parse(
        &mut f,
        build::packet(
            10,
            &[(26, V::U8(1)), (27, V::I32(3045)), (29, V::I32(81_050))],
        ),
    );
    let mut frames = Frames::new("tradejini");
    for mode in [Mode::Ltp, Mode::Quote] {
        let d = last(frames.at(&ev, mode));
        assert!(d.get("ltt").is_none(), "{:?}: {}", mode, d);
        assert_eq!(d["ltp"], 810.5);
    }
}
