//! Web `test/test_tradejini_history_candles.py` (openalgo#2341,
//! `f5805220b`): every candle satisfies `low <= open, close <= high`, since
//! `/trading` refuses a whole series over one bad bar. Tradejini's chart
//! API returned MCX GOLDPETAL30NOV26FUT 1m at 2026-10-27 09:00 with open
//! 14328 below low 14334. A missing price is NaN in a desktop candle.

use super::*;
use crate::brokers::common::history::repair_candles;

fn bar(open: f64, high: f64, low: f64, close: f64, volume: i64) -> Candle {
    Candle {
        timestamp: 1_793_332_800,
        open,
        high,
        low,
        close,
        volume,
        oi: 0,
    }
}

fn repair(rows: Vec<Candle>) -> Vec<Candle> {
    repair_candles(rows, "MCX:GOLDPETAL30NOV26FUT 1m")
}

fn valid(c: &Candle) -> bool {
    [c.open, c.high, c.low, c.close]
        .iter()
        .all(|v| v.is_finite())
        && c.high >= c.open.max(c.close).max(c.low)
        && c.low <= c.open.min(c.close)
        && c.volume >= 0
}

/// Web `test_open_below_low_widens_the_wick`: the traded open is kept.
#[test]
fn an_open_below_the_low_widens_the_wick() {
    let out = repair(vec![bar(14328.0, 14426.0, 14334.0, 14365.0, 1033)]);
    assert_eq!(out, [bar(14328.0, 14426.0, 14328.0, 14365.0, 1033)]);
    assert!(valid(&out[0]));
}

/// Web `test_close_above_high_widens_the_wick`.
#[test]
fn a_close_above_the_high_widens_the_wick() {
    let out = repair(vec![bar(100.0, 101.0, 99.0, 102.0, 5)]);
    assert_eq!(out[0].high, 102.0);
}

/// Web `test_valid_candles_are_unchanged`.
#[test]
fn a_valid_candle_is_unchanged() {
    let rows = vec![bar(100.0, 105.0, 95.0, 101.0, 10)];
    assert_eq!(repair(rows.clone()), rows);
}

/// Web `test_negative_volume_is_zeroed_and_missing_prices_dropped`.
#[test]
fn a_negative_volume_is_zeroed_and_a_bar_missing_a_price_dropped() {
    let out = repair(vec![
        bar(100.0, 105.0, 95.0, 101.0, -7),
        bar(f64::NAN, 105.0, 95.0, 101.0, 3),
    ]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].volume, 0);
    assert!(out.iter().all(valid));
}

/// Web `test_infinite_prices_are_dropped_and_infinite_volume_zeroed`
/// (openalgo#2359, `a92418967`): an infinite price is as unplaceable as a
/// missing one; an infinite volume is 0.
#[test]
fn an_infinite_price_drops_the_bar_and_an_infinite_volume_is_zero() {
    let bars = parse_bars(&json(
        r#"{"s":"ok","d":{"bars":[
            {"time":1793332800,"open":"inf","high":105.0,"low":95.0,"close":101.0,"volume":3},
            {"time":1793332860,"open":100.0,"high":105.0,"low":"-inf","close":101.0,"volume":3},
            {"time":1793332920,"open":100.0,"high":105.0,"low":95.0,"close":101.0,"volume":"inf"}
        ]}}"#,
    ));
    let out = repair(bars);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].volume, 0);
    assert!(out.iter().all(valid));
}

/// Web `test_bar_missing_a_price_is_not_drawn_at_zero` (openalgo#2359): a
/// missing price stays missing so the repair drops the bar, and a bar
/// without a time is skipped rather than placed in 1970.
#[test]
fn a_bar_missing_a_price_is_not_drawn_at_zero() {
    let rows = parse_bars(&json(
        r#"{"s":"ok","d":{"bars":[
            {"time":1793332800,"open":100.0,"high":105.0,"low":95.0,"close":101.0},
            {"time":1793332860,"high":106.0,"low":96.0,"close":102.0,"volume":9},
            {"open":100.0,"high":105.0,"low":95.0,"close":101.0,"volume":1}
        ]}}"#,
    ));
    assert_eq!(rows.len(), 2, "the bar without a time is skipped");
    assert!(rows[1].open.is_nan());
    assert_eq!(rows[0].volume, 0);

    let out = repair(rows);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].open, 100.0);
    assert_eq!(out[0].volume, 0);
}
