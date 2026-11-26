//! Sessions opening at the pre-open discovered price (web flattrade
//! `_open_at_discovered_price`), on the bars of the web's
//! test/test_flattrade_preopen_history.py and on cases worked by hand.
//! The end-to-end ports, through Flattrade's adapter and the real market
//! calendar, are in `brokers/flattrade/tests.rs`.

use super::open_at_discovered_price;
use crate::brokers::types::Candle;
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use std::collections::HashMap;

/// 14:20 IST on 8 Nov 2026, Muhurat, and 09:15 IST on 9 Nov 2026, as the
/// shipped calendar holds them (web `MUHURAT_TIMINGS`, `REGULAR_TIMINGS`).
const MUHURAT_OPEN: i64 = 1_794_127_800;
const REGULAR_OPEN: i64 = 1_794_195_900;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

fn opens() -> HashMap<NaiveDate, i64> {
    HashMap::from([(day(8), MUHURAT_OPEN), (day(9), REGULAR_OPEN)])
}

/// A bar at `DD-MM-YYYY HH:MM:SS` IST, as TPSeries stamps it.
fn bar(time: &str, o: f64, h: f64, l: f64, c: f64, v: i64) -> Candle {
    let naive = NaiveDateTime::parse_from_str(time, "%d-%m-%Y %H:%M:%S").unwrap();
    Candle {
        timestamp: Kolkata.from_local_datetime(&naive).unwrap().timestamp(),
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        oi: 0,
    }
}

type Row = (String, f64, f64, f64, f64, i64);

/// `bars` (newest first, as TPSeries answers) through the fold, oldest
/// first as the adapter hands them over, read back as the web's test does.
fn fold(mut bars: Vec<Candle>, minutes: i64, opens: &HashMap<NaiveDate, i64>) -> Vec<Row> {
    bars.sort_by_key(|c| c.timestamp);
    open_at_discovered_price(bars, minutes, opens)
        .iter()
        .map(|c| {
            let t = Kolkata.timestamp_opt(c.timestamp, 0).unwrap();
            let t = t.format("%d %H:%M:%S").to_string();
            (t, c.open, c.high, c.low, c.close, c.volume)
        })
        .collect()
}

fn row(t: &str, o: f64, h: f64, l: f64, c: f64, v: i64) -> Row {
    (t.to_string(), o, h, l, c, v)
}

/// Web `test_muhurat_opens_at_the_discovered_price_without_pre_open_bars`:
/// SBIN's real 8 Nov 2026 bars.
#[test]
fn muhurat_opens_at_the_discovered_price_without_pre_open_bars() {
    let bars = vec![
        bar(
            "08-11-2026 14:21:00",
            1022.95,
            1023.0,
            1020.0,
            1020.15,
            60619,
        ),
        bar(
            "08-11-2026 14:20:00",
            1023.7,
            1025.0,
            1020.05,
            1023.0,
            124262,
        ),
        bar("08-11-2026 14:19:00", 1023.2, 1023.2, 1023.2, 1023.2, 0),
        bar("08-11-2026 14:14:05", 1023.2, 1023.2, 1023.2, 1023.2, 1039),
        bar("08-11-2026 14:06:00", 1018.5, 1018.5, 1018.5, 1018.5, 2125),
        bar(
            "08-11-2026 14:05:00",
            1024.95,
            1018.5,
            1018.5,
            1018.5,
            15721,
        ),
    ];
    assert_eq!(
        fold(bars, 1, &opens()),
        [
            row("08 14:20:00", 1023.2, 1025.0, 1020.05, 1023.0, 124262),
            row("08 14:21:00", 1022.95, 1023.0, 1020.0, 1020.15, 60619),
        ]
    );
}

/// Web `test_regular_day_opens_at_the_09_14_price`.
#[test]
fn regular_day_opens_at_the_09_14_price() {
    let bars = vec![
        bar("09-11-2026 09:16:00", 1029.0, 1030.0, 1028.0, 1029.5, 10),
        bar("09-11-2026 09:15:00", 1027.6, 1029.0, 1027.0, 1028.5, 10),
        bar("09-11-2026 09:14:00", 1030.0, 1030.0, 1030.0, 1030.0, 0),
    ];
    assert_eq!(
        fold(bars, 1, &opens()),
        [
            row("09 09:15:00", 1030.0, 1030.0, 1027.0, 1028.5, 10),
            row("09 09:16:00", 1029.0, 1030.0, 1028.0, 1029.5, 10),
        ]
    );
}

/// Web `test_coarser_interval_uses_the_last_pre_open_close`.
#[test]
fn coarser_interval_uses_the_last_pre_open_close() {
    let bars = vec![
        bar(
            "08-11-2026 14:20:00",
            1023.7,
            1025.0,
            1020.0,
            1022.6,
            375072,
        ),
        bar("08-11-2026 14:10:00", 1018.5, 1023.2, 1018.5, 1023.2, 5353),
        bar(
            "08-11-2026 14:05:00",
            1024.95,
            1018.5,
            1018.5,
            1018.5,
            26911,
        ),
    ];
    assert_eq!(
        fold(bars, 5, &opens()),
        [row("08 14:20:00", 1023.2, 1025.0, 1020.0, 1022.6, 375072)]
    );
}

/// Web `test_a_bar_running_past_the_open_is_moved_to_it_not_left_out`.
#[test]
fn a_bar_running_past_the_open_is_moved_to_it_not_left_out() {
    let bars = vec![
        bar("08-11-2026 14:30:00", 1015.3, 1021.0, 1015.3, 1020.4, 500),
        bar("08-11-2026 14:15:00", 1023.2, 1025.0, 1015.0, 1016.0, 900),
    ];
    assert_eq!(
        fold(bars, 15, &opens()),
        [
            row("08 14:20:00", 1023.2, 1025.0, 1015.0, 1016.0, 900),
            row("08 14:30:00", 1015.3, 1021.0, 1015.3, 1020.4, 500),
        ]
    );
}

/// Web `test_a_day_the_calendar_does_not_cover_keeps_every_bar`.
#[test]
fn a_day_the_calendar_does_not_cover_keeps_every_bar() {
    let bars = vec![
        bar("09-11-2026 09:15:00", 101.0, 102.0, 100.0, 101.0, 10),
        bar("09-11-2026 09:14:00", 100.0, 100.0, 100.0, 100.0, 0),
    ];
    let times: Vec<String> = fold(bars, 1, &HashMap::new())
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(times, ["09 09:14:00", "09 09:15:00"]);
}

/// Worked by hand: a bar moved to the open meets the one the broker put
/// there (a 3m bar stamped 14:18:30 runs to 14:21:30, past the 14:20
/// open). They fold in the order they traded, the later close and OI kept
/// and the volumes added, and the merged bar still opens at the
/// discovered price. The web folds them in TPSeries' newest-first order
/// and so would keep the earlier bar's close; none of the bars its tests
/// recorded meet this way.
#[test]
fn a_bar_moved_onto_the_open_folds_with_the_bar_already_there() {
    let bars = vec![
        bar("08-11-2026 14:20:00", 1023.5, 1025.0, 1021.0, 1024.5, 200),
        bar("08-11-2026 14:18:30", 1023.0, 1024.0, 1022.0, 1023.5, 100),
        bar("08-11-2026 14:10:00", 1023.2, 1023.2, 1023.2, 1023.2, 50),
    ];
    assert_eq!(
        fold(bars, 3, &opens()),
        [row("08 14:20:00", 1023.2, 1025.0, 1021.0, 1024.5, 300)]
    );
}

/// Worked by hand: pre-open bars are left out even on a day with no
/// session bar yet (read during the pre-open), and another day's bars are
/// untouched by them.
#[test]
fn pre_open_bars_alone_leave_their_day_empty() {
    let bars = vec![
        bar("09-11-2026 09:14:00", 1030.0, 1030.0, 1030.0, 1030.0, 0),
        bar(
            "08-11-2026 14:21:00",
            1022.95,
            1023.0,
            1020.0,
            1020.15,
            60619,
        ),
    ];
    assert_eq!(
        fold(bars, 1, &opens()),
        [row("08 14:21:00", 1022.95, 1023.0, 1020.0, 1020.15, 60619)]
    );
}
