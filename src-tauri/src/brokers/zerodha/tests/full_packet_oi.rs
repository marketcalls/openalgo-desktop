//! Web `test/test_zerodha_full_packet_oi.py` (openalgo#2491, #2494,
//! `cbeb1378a`): Kite's 184-byte full packet carries the last traded time,
//! OI, OI day high, OI day low and the exchange time at bytes 44-64, then
//! ten 12-byte depth levels. The web read OI from bytes 180-184 (the fifth
//! ask level's order count and padding) and published the last traded time
//! divided by 100 as `price_change`; the desktop read OI from byte 48 from
//! the start. Every field has a distinct value and the fifth ask level one
//! order, so a parser reading the old offsets puts a recognisably wrong
//! number (65536 as OI, 17954258.0 as the change) in the frame.
//!
//! The web test's NIFTY future token is not in the recorded master; the
//! master's NIFTY future carries the same values here.

use super::*;
use crate::brokers::common::streaming::NormalizedTick;
use crate::feed::testkit::Frames;
use crate::feed::Mode;

const NIFTY_FUT_TOKEN: i32 = 10_011_906;
const LAST_TRADE_TIME: u32 = 1_795_425_800;
const EXCHANGE_TS: u32 = 1_795_425_819;
const OI: u32 = 4_932_265;
const OI_DAY_HIGH: u32 = 5_100_000;
const OI_DAY_LOW: u32 = 4_800_000;

fn level(qty: u32, price: u32, orders: u16) -> Vec<u8> {
    let mut v = qty.to_be_bytes().to_vec();
    v.extend(price.to_be_bytes());
    v.extend(orders.to_be_bytes());
    v.extend(0u16.to_be_bytes());
    v
}

/// The web test's `FULL`: the quote part, the OI block, five bids, five asks.
fn full_packet() -> Vec<u8> {
    // token, ltp, ltq, atp, volume, buy qty, sell qty, open, high, low, close
    let mut p = ints(&[
        NIFTY_FUT_TOKEN,
        2_539_500,
        65,
        2_559_263,
        4_688_645,
        83_395,
        107_575,
        2_596_900,
        2_600_000,
        2_537_750,
        2_577_530,
    ]);
    for v in [LAST_TRADE_TIME, OI, OI_DAY_HIGH, OI_DAY_LOW, EXCHANGE_TS] {
        p.extend(v.to_be_bytes());
    }
    for i in 0..5u32 {
        p.extend(level(130 * (i + 1), 2_539_300 - i * 100, i as u16 + 1));
    }
    for i in 0..5u32 {
        let orders = if i < 4 { 4 - i as u16 } else { 1 };
        p.extend(level(260 + i * 65, 2_539_890 + i * 10, orders));
    }
    p
}

fn parsed() -> Vec<FeedEvent> {
    let mut f = KiteFeed::new("k", "t", master());
    f.subscribe_frames(&[sub("NIFTY27OCT26FUT", "NFO", FeedMode::Depth)]);
    f.parse(&Message::Binary(frame(&[full_packet()])))
}

fn the_tick(events: &[FeedEvent]) -> NormalizedTick {
    events
        .iter()
        .find_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.clone()),
            _ => None,
        })
        .expect("a tick")
}

/// Web test_fixture_is_a_full_packet.
#[test]
fn the_fixture_is_a_full_packet() {
    let p = full_packet();
    assert_eq!(p.len(), 184);
    // The bytes the web's old parser took for OI.
    assert_eq!(u32::from_be_bytes([p[180], p[181], p[182], p[183]]), 65_536);
}

/// Web test_full_packet_reads_oi_block_from_documented_offsets.
#[test]
fn a_full_packet_reads_oi_and_the_trade_time_from_their_offsets() {
    let t = the_tick(&parsed());
    assert_eq!(t.mode, FeedMode::Depth.code());
    assert_eq!(t.oi, i64::from(OI));
    assert_eq!(t.last_trade_time_ms, i64::from(LAST_TRADE_TIME) * 1000);
}

/// Web test_full_packet_publishes_no_price_change: the full packet has no
/// change field; nothing is read from the trade time's bytes as one, and a
/// Zerodha future's frame carries no `price_change`, as its quote frame
/// does not.
#[test]
fn a_full_packet_publishes_no_price_change() {
    let events = parsed();
    let t = the_tick(&events);
    assert!(
        (t.change - (25_395.0 - 25_775.3)).abs() < 1e-6,
        "the change against the close, never the trade time / 100: {}",
        t.change
    );
    for mode in [Mode::Depth, Mode::Quote] {
        let frames = Frames::new("zerodha").at(&events, mode);
        assert!(!frames.is_empty(), "{:?}", mode);
        for d in frames {
            assert!(d.get("price_change").is_none(), "{:?}: {}", mode, d);
        }
    }
    let depth = Frames::new("zerodha").at(&events, Mode::Depth);
    let [d] = depth.as_slice() else {
        panic!("one depth frame: {:?}", depth)
    };
    assert_eq!(
        (d["oi"].as_u64(), d["open_interest"].as_u64()),
        (Some(u64::from(OI)), Some(u64::from(OI)))
    );
}

/// Web test_full_packet_keeps_quote_fields_and_depth.
#[test]
fn a_full_packet_keeps_the_quote_fields_and_the_book() {
    let events = parsed();
    let t = the_tick(&events);
    assert_eq!(t.ltp, 25_395.0);
    assert_eq!(t.volume, 4_688_645);
    assert_eq!(
        (t.open, t.high, t.low, t.close),
        (25_969.0, 26_000.0, 25_377.5, 25_775.3)
    );
    let book = events
        .iter()
        .find_map(|e| match e {
            FeedEvent::Depth(d) => Some(d.clone()),
            _ => None,
        })
        .expect("a book");
    assert_eq!((book.buy.len(), book.sell.len()), (5, 5));
    let (b, s) = (&book.buy[0], &book.sell[4]);
    assert_eq!((b.quantity, b.price, b.orders), (130, 25_393.0, 1));
    assert_eq!((s.quantity, s.price, s.orders), (520, 25_399.3, 1));
}
