//! Web `test/test_angel_quote_feed.py` (openalgo#2377, `e2e31e91f`):
//! SmartStream's quote and snap-quote fields reaching Quote and Depth
//! subscribers. The packets are built byte for byte from the SmartAPI
//! WebSocket 2.0 layout with the web test's values, go through the real
//! parser and come out as the 8765 frames the bridge makes of them
//! (`feed::testkit`).
//!
//! The web fixed a snap quote whose OHLC read keys its parser never makes
//! (so depth OHLC was 0), a currency price divided by 100, and a depth
//! frame without the average price and book totals. The desktop's parser
//! reads the day OHLC and the previous close at their offsets in both
//! packets, divides every currency price (exchange type 13) by 10^7 and
//! builds a depth frame on the tick's quote, so these cases pin behaviour
//! the desktop already had. The web's `BCD` divisor has no counterpart:
//! Angel's master lists no BCD contract.

use super::*;
use crate::feed::testkit::Frames;
use crate::feed::Mode;
use serde_json::{json, Value};

/// The web test's packet fields, in paise (10^-7 rupees for currencies).
struct Fields {
    ltp: i64,
    ltq: i64,
    atp: i64,
    vol: i64,
    tbq: f64,
    tsq: f64,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
}

/// One best-five packet: buy/sell flag (1 buy, 0 sell), quantity, price,
/// orders.
type Level = (u16, i64, i64, u16);

/// The web test's `_packet`: a Quote (mode 2) or Snap Quote (mode 3).
fn build(mode: u8, et: u8, token: &str, q: &Fields, levels: &[Level]) -> Vec<u8> {
    let len = if mode == 3 { SNAP_LEN } else { QUOTE_LEN };
    let mut p = packet(mode, et, token, len);
    put_i64(&mut p, 35, 1_746_171_226_000);
    for (o, v) in [
        (43, q.ltp),
        (51, q.ltq),
        (59, q.atp),
        (67, q.vol),
        (91, q.open),
        (99, q.high),
        (107, q.low),
        (115, q.close),
    ] {
        put_i64(&mut p, o, v);
    }
    put_f64(&mut p, 75, q.tbq);
    put_f64(&mut p, 83, q.tsq);
    if mode == 3 {
        put_i64(&mut p, 123, 1_746_171_225_000);
        put_i64(&mut p, 131, 5000);
        for (i, (flag, qty, price, orders)) in levels.iter().enumerate() {
            let o = 147 + 20 * i;
            p[o..o + 2].copy_from_slice(&flag.to_le_bytes());
            put_i64(&mut p, o + 2, *qty);
            put_i64(&mut p, o + 10, *price);
            p[o + 18..o + 20].copy_from_slice(&orders.to_le_bytes());
        }
        put_i64(&mut p, 347, q.high * 2);
        put_i64(&mut p, 355, q.low / 2);
    }
    p
}

/// The web test's RELIANCE values.
fn reliance() -> Fields {
    Fields {
        ltp: 150_005,
        ltq: 25,
        atp: 149_876,
        vol: 123_456,
        tbq: 1000.0,
        tsq: 2000.0,
        open: 149_000,
        high: 151_000,
        low: 148_500,
        close: 148_800,
    }
}

/// The web test's `NSE_LEVELS`: bids below asks, as a real book is.
fn nse_levels() -> Vec<Level> {
    let bids = (0..5u16).map(|i| (1, 100 + i64::from(i), 150_000 - 5 * i64::from(i), 3 + i));
    let asks = (0..5u16).map(|i| (0, 200 + i64::from(i), 150_010 + 5 * i64::from(i), 4 + i));
    bids.chain(asks).collect()
}

/// The frames a subscriber at `mode` receives for one packet.
fn frame(symbol: &str, exchange: &str, packet: Vec<u8>, mode: Mode) -> Value {
    let feed_mode = match mode {
        Mode::Ltp => FeedMode::Ltp,
        Mode::Quote => FeedMode::Quote,
        Mode::Depth => FeedMode::Depth,
    };
    let mut f = feed();
    f.subscribe_frames(&[sub(symbol, exchange, feed_mode)]);
    let events = f.parse(&Message::Binary(packet));
    let frames = Frames::new("angel").at(&events, mode);
    match frames.as_slice() {
        [d] => d.clone(),
        other => panic!("one frame expected: {:?}", other),
    }
}

fn snap_quote() -> Value {
    let p = build(3, 1, "2885", &reliance(), &nse_levels());
    frame("RELIANCE", "NSE", p, Mode::Depth)
}

/// Web test_snap_quote_publishes_ohlc_from_parser_keys.
#[test]
fn a_snap_quote_publishes_the_day_ohlc_and_previous_close() {
    let d = snap_quote();
    assert_eq!(
        (d["open"].as_f64(), d["high"].as_f64(), d["low"].as_f64()),
        (Some(1490.0), Some(1510.0), Some(1485.0))
    );
    assert_eq!(d["close"], 1488.0, "the previous close (closed_price)");
    assert_eq!(d["ltp"], 1500.05);
}

/// Web test_snap_quote_keeps_quote_fields: each side of the book exactly,
/// by the documented flag.
#[test]
fn a_snap_quote_keeps_the_quote_fields_and_both_sides() {
    let d = snap_quote();
    assert_eq!(d["average_price"], 1498.76);
    assert_eq!(
        (
            d["total_buy_quantity"].as_i64(),
            d["total_sell_quantity"].as_i64()
        ),
        (Some(1000), Some(2000))
    );
    assert_eq!(d["volume"], 123_456);
    assert_eq!(
        (
            d["last_quantity"].as_i64(),
            d["last_trade_quantity"].as_i64()
        ),
        (Some(25), Some(25))
    );
    assert_eq!(d["depth"]["buy"], side(|i| 150_000 - 5 * i, 100, 3));
    assert_eq!(d["depth"]["sell"], side(|i| 150_010 + 5 * i, 200, 4));
}

/// Five levels as a frame carries them: the i-th at `price(i)` paise,
/// `qty + i` and `orders + i`.
fn side(price: impl Fn(i64) -> i64, qty: i64, orders: i64) -> Value {
    (0..5)
        .map(|i| json!({"price": price(i) as f64 / 100.0, "quantity": qty + i, "orders": orders + i}))
        .collect()
}

/// Web test_quote_publishes_last_quantity_next_to_last_trade_quantity.
#[test]
fn a_quote_publishes_last_quantity_next_to_last_trade_quantity() {
    let p = build(2, 1, "2885", &reliance(), &[]);
    let d = frame("RELIANCE", "NSE", p, Mode::Quote);
    assert_eq!(
        (
            d["last_quantity"].as_i64(),
            d["last_trade_quantity"].as_i64()
        ),
        (Some(25), Some(25))
    );
}

/// Web test_currency_prices_divide_by_1e7: cde_fo (exchange type 13),
/// "For currencies, the int 32 price values should be divided by
/// 10000000.0" (SmartAPI WebSocket 2.0), on every price, the book's too.
#[test]
fn currency_prices_divide_by_ten_million() {
    let usd = Fields {
        ltp: 835_000_000,
        ltq: 1,
        atp: 834_900_000,
        vol: 900,
        tbq: 10.0,
        tsq: 12.0,
        open: 834_000_000,
        high: 836_000_000,
        low: 833_000_000,
        close: 833_500_000,
    };
    let levels: Vec<Level> = [(0, 10, 834_950_000, 2); 5]
        .into_iter()
        .chain([(1, 12, 835_050_000, 3); 5])
        .collect();
    let p = build(3, 13, "1170", &usd, &levels);
    let d = frame("USDINR27OCT26FUT", "CDS", p, Mode::Depth);
    let near = |k: &str, v: f64| {
        let got = d[k].as_f64().unwrap_or(f64::NAN);
        assert!((got - v).abs() < 1e-9, "{}: {} against {}", k, got, v);
    };
    near("ltp", 83.5);
    near("open", 83.4);
    near("close", 83.35);
    near("average_price", 83.49);
    for side in ["buy", "sell"] {
        for lvl in d["depth"][side].as_array().into_iter().flatten() {
            let price = lvl["price"].as_f64().unwrap_or(0.0);
            assert!(83.0 < price && price < 84.0, "{}: {}", side, lvl);
        }
    }
}

/// Web test_non_currency_prices_divide_by_100.
#[test]
fn non_currency_prices_divide_by_a_hundred() {
    assert_eq!(price_divisor(1), 100.0);
    assert_eq!(price_divisor(5), 100.0);
    assert_eq!(price_divisor(13), 10_000_000.0);
}
