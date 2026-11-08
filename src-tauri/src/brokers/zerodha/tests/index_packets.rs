//! Web `test/test_zerodha_index_packets.py` (openalgo#2347, `5372987aa`):
//! Kite sends an index a packet of its own, 28 bytes in quote mode and 32
//! in full mode (token, ltp, high, low, open, close, change, then the
//! exchange time). Every field has a distinct value, so a parser reading
//! the wrong offset, or the quote layout, puts a recognisably wrong number
//! in the wrong key. The packets go through the real parser and come out
//! as the 8765 frames the bridge makes of them (`feed::testkit`).
//!
//! The web test's regular quote packet uses INFY's token, which the
//! recorded master does not list; SBIN's carries the same values here.

use super::*;
use crate::brokers::common::streaming::{NormalizedTick, QuoteField};
use crate::feed::testkit::{keys, Frames};
use crate::feed::Mode;

const NIFTY_TOKEN: i32 = 256_265;
const SBIN_TOKEN: i32 = 779_521;
const EXCHANGE_TS: u32 = 1_762_156_800;

/// Token, ltp, high, low, open, close, change, in paise.
fn index_quote() -> Vec<u8> {
    ints(&[
        NIFTY_TOKEN,
        2_450_010,
        2_460_000,
        2_440_000,
        2_445_000,
        2_430_000,
        20_010,
    ])
}

fn index_full() -> Vec<u8> {
    let mut p = index_quote();
    p.extend(EXCHANGE_TS.to_be_bytes());
    p
}

fn subscribed(symbol: &str, exchange: &str, mode: FeedMode) -> KiteFeed {
    let mut f = KiteFeed::new("k", "t", master());
    f.subscribe_frames(&[sub(symbol, exchange, mode)]);
    f
}

fn parse(f: &mut KiteFeed, packet: Vec<u8>) -> Vec<FeedEvent> {
    f.parse(&Message::Binary(frame(&[packet])))
}

fn the_tick(events: &[FeedEvent]) -> NormalizedTick {
    match events {
        [FeedEvent::Tick(t)] => t.clone(),
        other => panic!("one tick expected: {:?}", other),
    }
}

/// The web test's `_assert_index_fields`: OHLC, the packet's change and
/// the subscription's exchange; no quantity, total or OI is invented.
fn assert_index_fields(t: &NormalizedTick) {
    assert_eq!(
        (t.symbol.as_str(), t.exchange.as_str()),
        ("NIFTY", "NSE_INDEX")
    );
    assert_eq!(t.ltp, 24_500.10);
    assert_eq!(
        (t.open, t.high, t.low, t.close),
        (24_450.0, 24_600.0, 24_400.0, 24_300.0)
    );
    assert_eq!(t.change, 200.10);
    for field in [
        QuoteField::LastQuantity,
        QuoteField::AveragePrice,
        QuoteField::TotalBuyQuantity,
        QuoteField::TotalSellQuantity,
        QuoteField::OpenInterest,
    ] {
        assert!(t.absent.contains(field), "{:?}", field);
    }
}

/// Web test_index_quote_packet_carries_ohlc_and_change: a 28-byte packet
/// is a quote, whatever mode the subscription was in (the web's has none
/// recorded), and its frame carries OHLC and the change.
#[test]
fn an_index_quote_packet_carries_ohlc_and_change() {
    let mut f = subscribed("NIFTY", "NSE_INDEX", FeedMode::Ltp);
    let events = parse(&mut f, index_quote());
    let t = the_tick(&events);
    assert_eq!(t.mode, FeedMode::Quote.code());
    assert_index_fields(&t);
    assert_eq!(t.last_trade_time_ms, 0, "no exchange time in quote mode");

    let frames = Frames::new("zerodha").at(&events, Mode::Quote);
    let [d] = frames.as_slice() else {
        panic!("one quote frame: {:?}", frames)
    };
    assert_eq!(
        keys(d),
        [
            "close",
            "exchange",
            "high",
            "low",
            "ltp",
            "ltt",
            "mode",
            "open",
            "price_change",
            "price_change_percent",
            "symbol",
            "timestamp",
            "volume",
        ]
    );
    assert_eq!(
        (d["open"].as_f64(), d["close"].as_f64()),
        (Some(24_450.0), Some(24_300.0))
    );
    assert_eq!(d["price_change"], 200.1);
    assert_eq!(
        d["volume"], 0,
        "the web's adapter sends an index's volume as 0"
    );
}

/// Web test_index_full_packet_adds_exchange_timestamp: a 32-byte packet is
/// full mode with the exchange time, and its Depth frame carries no OI
/// and no book (it sent `oi` and `open_interest` as 0).
#[test]
fn an_index_full_packet_adds_the_exchange_time_and_no_oi() {
    let mut f = subscribed("NIFTY", "NSE_INDEX", FeedMode::Depth);
    let events = parse(&mut f, index_full());
    let t = the_tick(&events);
    assert_eq!(t.mode, FeedMode::Depth.code());
    assert_index_fields(&t);
    assert_eq!(t.last_trade_time_ms, i64::from(EXCHANGE_TS) * 1000);

    let frames = Frames::new("zerodha").at(&events, Mode::Depth);
    let [d] = frames.as_slice() else {
        panic!("one depth frame: {:?}", frames)
    };
    for k in ["oi", "open_interest", "depth", "total_buy_quantity"] {
        assert!(d.get(k).is_none(), "{}: {}", k, d);
    }
    assert_eq!(d["ltt"], i64::from(EXCHANGE_TS) * 1000);
    assert_eq!(d["high"], 24_600.0);
}

/// Web test_regular_quote_packet_still_uses_quote_layout: a 44-byte packet
/// is a quote in the quote layout, every field carried.
#[test]
fn a_regular_quote_packet_keeps_the_quote_layout() {
    let mut f = subscribed("SBIN", "NSE", FeedMode::Ltp);
    // token, ltp, ltq, atp, volume, buy qty, sell qty, open, high, low, close
    let packet = ints(&[
        SBIN_TOKEN, 150_000, 10, 149_950, 5000, 1, 2, 149_000, 151_000, 148_000, 149_500,
    ]);
    let t = the_tick(&parse(&mut f, packet));
    assert_eq!(t.mode, FeedMode::Quote.code());
    assert_eq!(t.volume, 5000);
    assert_eq!(
        (t.open, t.high, t.low, t.close),
        (1490.0, 1510.0, 1480.0, 1495.0)
    );
    assert!(t.absent.is_empty());
}
