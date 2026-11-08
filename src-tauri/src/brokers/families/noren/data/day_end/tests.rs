//! Web test/test_shoonya_history_aggregation.py (marketcalls/openalgo#2343,
//! #2360, 0e4a488cc): the bars are real SBIN NSE replies from 2026-10-30 to
//! 2026-11-03; the 5m 30-10 15:25 bar came back with high = close = 965.00,
//! the 02-11 09:14 pre-open price, against its own close of 961.85.

use super::*;
use chrono::NaiveDateTime;

/// `YYYY-MM-DD HH:MM` IST as epoch seconds.
fn ts(text: &str) -> i64 {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
    Kolkata.from_local_datetime(&naive).unwrap().timestamp()
}

fn bars(rows: &[(&str, f64, f64, f64, f64, i64)]) -> Vec<Candle> {
    rows.iter()
        .map(|&(t, open, high, low, close, volume)| Candle {
            timestamp: ts(t),
            open,
            high,
            low,
            close,
            volume,
            oi: 0,
        })
        .collect()
}

/// Shoonya's 5m reply for 30-10 to 02-11, as sent (last bars contaminated).
fn sbin_5m() -> Vec<Candle> {
    bars(&[
        ("2026-10-30 15:20", 960.05, 960.05, 960.05, 960.05, 0),
        ("2026-10-30 15:25", 960.05, 965.0, 960.05, 965.0, 6971359),
        ("2026-11-02 09:15", 968.75, 976.4, 964.65, 976.35, 453904),
        ("2026-11-02 15:25", 985.0, 992.7, 985.0, 992.7, 9608879),
    ])
}

fn sbin_1m() -> Vec<Candle> {
    bars(&[
        ("2026-10-30 15:24", 960.05, 960.05, 960.05, 960.05, 2653),
        ("2026-10-30 15:25", 960.05, 960.05, 960.05, 960.05, 495),
        ("2026-10-30 15:26", 960.05, 960.05, 960.05, 960.05, 1201),
        ("2026-10-30 15:27", 960.05, 960.05, 960.05, 960.05, 10506),
        ("2026-10-30 15:28", 961.85, 961.85, 961.85, 961.85, 6959157),
        ("2026-10-30 15:29", 961.85, 961.85, 961.85, 961.85, 0),
        ("2026-11-02 09:14", 965.0, 965.0, 965.0, 965.0, 0),
        ("2026-11-02 09:15", 968.75, 973.75, 964.65, 973.7, 102611),
        ("2026-11-02 15:25", 985.0, 985.0, 985.0, 985.0, 3184),
        ("2026-11-02 15:26", 985.0, 985.0, 985.0, 985.0, 531),
        ("2026-11-02 15:27", 985.0, 985.0, 985.0, 985.0, 0),
        ("2026-11-02 15:28", 985.0, 985.0, 985.0, 985.0, 2357),
        ("2026-11-02 15:29", 987.0, 987.0, 987.0, 987.0, 9602870),
        ("2026-11-03 09:14", 992.7, 992.7, 992.7, 992.7, 0),
    ])
}

fn rebuilt(mut out: Vec<Candle>, minute: &[Candle]) -> Vec<Candle> {
    rebuild_day_last_bars(&mut out, minute, &[]);
    out
}

fn at(out: &[Candle], t: &str) -> Candle {
    out.iter()
        .find(|c| c.timestamp == ts(t))
        .cloned()
        .unwrap_or_else(|| panic!("no bar at {}", t))
}

fn ohlc(c: &Candle) -> (f64, f64, f64, f64) {
    (c.open, c.high, c.low, c.close)
}

/// `test_last_bar_of_each_day_loses_the_next_days_pre_open_price`
#[test]
fn the_last_bar_of_each_day_loses_the_next_days_pre_open_price() {
    let out = rebuilt(sbin_5m(), &sbin_1m());
    let first = at(&out, "2026-10-30 15:25");
    assert_eq!(ohlc(&first), (960.05, 961.85, 960.05, 961.85));
    assert_eq!(
        first.volume, 6971359,
        "the same as Shoonya's own bar volume"
    );
    let second = at(&out, "2026-11-02 15:25");
    assert_eq!(ohlc(&second), (985.0, 987.0, 985.0, 987.0));
    assert_eq!(second.volume, 9608942);
}

/// `test_other_bars_are_left_as_shoonya_sent_them`
#[test]
fn other_bars_are_left_as_shoonya_sent_them() {
    let out = rebuilt(sbin_5m(), &sbin_1m());
    assert_eq!(out.len(), 4);
    let bar = at(&out, "2026-11-02 09:15");
    assert_eq!(
        (bar.open, bar.high, bar.low, bar.close, bar.volume),
        (968.75, 976.4, 964.65, 976.35, 453904)
    );
    assert_eq!(at(&out, "2026-10-30 15:20").close, 960.05);
}

/// `test_a_special_session_day_is_handled_like_any_other`: Sunday
/// 01-02-2026 (Budget day) traded a full session; nothing assumes which
/// days or hours trade.
#[test]
fn a_special_session_day_is_handled_like_any_other() {
    let out = rebuilt(
        bars(&[
            ("2026-02-01 15:25", 1016.9, 1025.0, 1015.0, 1025.0, 100),
            ("2026-02-02 09:15", 1028.5, 1030.0, 1027.0, 1029.0, 50),
        ]),
        &bars(&[
            ("2026-02-01 15:25", 1016.9, 1018.0, 1015.0, 1017.0, 60),
            ("2026-02-01 15:29", 1017.0, 1020.0, 1016.0, 1019.8, 40),
            ("2026-02-02 09:14", 1025.0, 1025.0, 1025.0, 1025.0, 0),
        ]),
    );
    let sunday = at(&out, "2026-02-01 15:25");
    assert_eq!(
        (sunday.high, sunday.close, sunday.volume),
        (1020.0, 1019.8, 100)
    );
}

/// `test_a_day_without_1m_bars_keeps_the_bar_shoonya_sent`
#[test]
fn a_day_without_1m_bars_keeps_the_bar_shoonya_sent() {
    let cut = ts("2026-11-02 00:00");
    let minute: Vec<Candle> = sbin_1m()
        .into_iter()
        .filter(|c| c.timestamp < cut)
        .collect();
    let out = rebuilt(sbin_5m(), &minute);
    assert_eq!(at(&out, "2026-11-02 15:25").close, 992.7);
    assert_eq!(at(&out, "2026-10-30 15:25").close, 961.85);
}

/// `test_empty_frames_pass_through`
#[test]
fn empty_frames_pass_through() {
    assert!(rebuilt(Vec::new(), &sbin_1m()).is_empty());
    assert_eq!(rebuilt(sbin_5m(), &[]), sbin_5m());
}

/// `test_repair_window_skips_a_single_day_reply`
#[test]
fn the_repair_window_skips_a_single_day_reply() {
    let cut = ts("2026-10-31 00:00");
    let one_day: Vec<Candle> = sbin_5m()
        .into_iter()
        .filter(|c| c.timestamp < cut)
        .collect();
    assert_eq!(repair_window(&one_day), None);
    assert_eq!(repair_window(&[]), None);
}

/// `test_repair_window_covers_only_the_returned_span`
#[test]
fn the_repair_window_covers_only_the_returned_span() {
    let (start, end) = repair_window(&sbin_5m()).unwrap();
    assert_eq!(start, ts("2026-10-30 15:25"), "the first day's last bar");
    assert_eq!(
        end,
        ts("2026-11-02 09:15") - 1,
        "just before the last day's first bar"
    );
}

/// The failed-chunk rule of `test_a_failed_1m_chunk_keeps_the_affected_day_end_bar`
/// on its own: a day whose span (its last bar to just before the next
/// bar) overlaps a failed 1m chunk keeps the bar Shoonya sent; the others
/// are still repaired.
#[test]
fn a_failed_1m_chunk_keeps_the_day_end_bar_it_overlaps() {
    let mut out = sbin_5m();
    let failed = [(ts("2026-11-02 15:25") + 1, ts("2026-11-07 15:25"))];
    let done = rebuild_day_last_bars(&mut out, &sbin_1m(), &failed);
    assert_eq!(
        done,
        Rebuilt {
            changed: 1,
            kept: 1
        }
    );
    assert_eq!(at(&out, "2026-11-02 15:25").close, 992.7, "kept as sent");
    assert_eq!(at(&out, "2026-10-30 15:25").close, 961.85, "repaired");
}
