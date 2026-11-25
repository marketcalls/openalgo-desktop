//! The shape rule of web tradesmart `_drop_preopen_bars`
//! (marketcalls/openalgo#2475, 9a12bd0f7), bar by bar. The bars are the
//! commit's own example (RELIANCE 18-11-2026) and the cases its docstring
//! names; `brokers::tradesmart` runs the same rule through the history.

use super::*;
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;

/// `YYYY-MM-DD HH:MM` IST as epoch seconds.
fn ts(text: &str) -> i64 {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
    Kolkata.from_local_datetime(&naive).unwrap().timestamp()
}

fn bar(t: &str, open: f64, high: f64, low: f64, close: f64, volume: i64) -> Candle {
    Candle {
        timestamp: ts(t),
        open,
        high,
        low,
        close,
        volume,
        oi: 0,
    }
}

fn kept(bars: Vec<Candle>) -> Vec<i64> {
    let cfg = crate::brokers::tradesmart::config();
    without_flat_open_bars(cfg, bars)
        .iter()
        .map(|c| c.timestamp)
        .collect()
}

/// RELIANCE 18-11-2026: the 09:14 bar at 1370.0 with no volume is the
/// pre-open price; the session opens at 09:15 at 1367.0. Each date loses
/// its own.
#[test]
fn the_flat_bar_without_volume_opening_each_date_is_dropped() {
    let bars = vec![
        bar("2026-11-18 09:14", 1370.0, 1370.0, 1370.0, 1370.0, 0),
        bar("2026-11-18 09:15", 1367.0, 1371.0, 1366.2, 1369.5, 182_340),
        bar("2026-11-18 09:16", 1369.5, 1369.9, 1368.0, 1368.4, 96_210),
        bar("2026-11-19 09:14", 1374.0, 1374.0, 1374.0, 1374.0, 0),
        bar("2026-11-19 09:15", 1374.5, 1376.0, 1373.1, 1375.2, 150_002),
    ];
    assert_eq!(
        kept(bars),
        vec![
            ts("2026-11-18 09:15"),
            ts("2026-11-18 09:16"),
            ts("2026-11-19 09:15"),
        ]
    );
}

/// Kept: an index's first trading bar (zero volume, not flat), a flat
/// opening bar that traded, a flat bar later in the day, and a date's only
/// bar, flat or not (nothing follows it that date).
#[test]
fn only_the_pre_open_shape_is_dropped() {
    let index = vec![
        bar(
            "2026-11-18 09:15",
            25_910.4,
            25_951.0,
            25_900.2,
            25_940.7,
            0,
        ),
        bar(
            "2026-11-18 09:16",
            25_940.7,
            25_948.3,
            25_930.0,
            25_935.1,
            0,
        ),
    ];
    assert_eq!(kept(index).len(), 2);
    let traded = vec![
        bar("2026-11-18 09:15", 12.5, 12.5, 12.5, 12.5, 1_000),
        bar("2026-11-18 09:16", 12.5, 12.6, 12.5, 12.6, 4_000),
    ];
    assert_eq!(kept(traded).len(), 2);
    let quiet_later = vec![
        bar("2026-11-18 09:15", 12.5, 12.6, 12.5, 12.6, 4_000),
        bar("2026-11-18 09:16", 12.6, 12.6, 12.6, 12.6, 0),
        bar("2026-11-18 09:17", 12.6, 12.7, 12.6, 12.7, 2_000),
    ];
    assert_eq!(kept(quiet_later).len(), 3);
    let alone = vec![
        bar("2026-11-17 15:29", 1366.0, 1366.0, 1366.0, 1366.0, 0),
        bar("2026-11-18 09:14", 1370.0, 1370.0, 1370.0, 1370.0, 0),
        bar("2026-11-18 09:15", 1367.0, 1371.0, 1366.2, 1369.5, 182_340),
    ];
    assert_eq!(
        kept(alone),
        vec![ts("2026-11-17 15:29"), ts("2026-11-18 09:15")],
        "17-11's only bar stays; 18-11's opening flat bar goes"
    );
    assert!(kept(Vec::new()).is_empty());
}

/// The date is IST's: a bar at 00:10 IST (18:40 UTC the day before) opens
/// the IST date it falls on, not the UTC one.
#[test]
fn dates_are_counted_in_ist() {
    let bars = vec![
        bar("2026-11-18 23:50", 7_010.0, 7_012.0, 7_008.0, 7_011.0, 40),
        bar("2026-11-19 00:10", 7_011.0, 7_011.0, 7_011.0, 7_011.0, 0),
        bar("2026-11-19 00:11", 7_011.0, 7_015.0, 7_010.0, 7_014.0, 25),
    ];
    assert_eq!(
        kept(bars),
        vec![ts("2026-11-18 23:50"), ts("2026-11-19 00:11")]
    );
}
