//! History helpers: request chunking and candle normalisation.
//!
//! Brokers cap the date range of one history call (Kite: 60 days intraday,
//! 2000 days daily). `chunks` splits an inclusive `[start, end]` range into
//! inclusive sub-ranges of at most `max_days`, exactly like the web's
//! `current_end = min(current_start + chunk_days - 1, end)` loop.

use crate::brokers::types::Candle;
use chrono::NaiveDate;

/// Inclusive date ranges of at most `max_days` days covering `[start, end]`.
pub fn chunks(start: NaiveDate, end: NaiveDate, max_days: i64) -> Vec<(NaiveDate, NaiveDate)> {
    let step = max_days.max(1);
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= end {
        let chunk_end = (cur + chrono::Duration::days(step - 1)).min(end);
        out.push((cur, chunk_end));
        match chunk_end.succ_opt() {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// Sort by timestamp and drop duplicate timestamps (first wins), as the
/// web's `sort_values("timestamp").drop_duplicates(subset=["timestamp"])`.
pub fn sort_dedupe(mut candles: Vec<Candle>) -> Vec<Candle> {
    candles.sort_by_key(|c| c.timestamp);
    candles.dedup_by_key(|c| c.timestamp);
    candles
}

/// The exchange whose session an exchange's bars follow: an index
/// exchange shares its cash exchange's (web dhan `_SESSION_EXCHANGE`).
pub fn session_exchange(exchange: &str) -> &str {
    match exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => other,
    }
}

/// Drop intraday candles that start at or after the session close of their
/// IST day (web dhan `_drop_bars_after_close`, openalgo#2308). `close_of`
/// answers a day's close in epoch milliseconds, or `None` when the calendar
/// has no session that day, which leaves the day as it is; it is asked once
/// per day.
pub fn drop_bars_after_close(
    candles: Vec<Candle>,
    mut close_of: impl FnMut(NaiveDate) -> Option<i64>,
) -> Vec<Candle> {
    let mut closes: Vec<(NaiveDate, Option<i64>)> = Vec::new();
    let before = candles.len();
    let kept: Vec<Candle> = candles
        .into_iter()
        .filter(|c| {
            let Some(day) = ist_day(c.timestamp) else {
                return true;
            };
            let close = match closes.iter().find(|(d, _)| *d == day) {
                Some((_, close)) => *close,
                None => {
                    let close = close_of(day);
                    closes.push((day, close));
                    close
                }
            };
            close.is_none_or(|ms| c.timestamp.saturating_mul(1000) < ms)
        })
        .collect();
    if kept.len() < before {
        tracing::debug!(
            "Dropped {} history bar(s) after the session close",
            before - kept.len()
        );
    }
    kept
}

/// The IST calendar day of an epoch-seconds timestamp.
fn ist_day(ts: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono_tz::Asia::Kolkata).date_naive())
}

/// Candles a chart accepts (web `_repair_candles` of the Shoonya, Kotak and
/// Tradejini adapters): `/trading` refuses a whole series over one bar
/// outside `low <= open, close <= high` ("History contains an invalid
/// candle"). A bar whose open, high, low or close is missing (NaN) or
/// infinite is dropped, since nothing honest can stand in for it; the high
/// and low are widened to cover the open and close, prices that traded;
/// a negative volume is zeroed rather than costing the bar its prices.
/// `what` names the series in the log (`MCX:GOLDPETAL30NOV26FUT 1m`).
pub fn repair_candles(candles: Vec<Candle>, what: &str) -> Vec<Candle> {
    let before = candles.len();
    let (mut widened, mut zeroed) = (0usize, 0usize);
    let out: Vec<Candle> = candles
        .into_iter()
        .filter(|c| {
            [c.open, c.high, c.low, c.close]
                .iter()
                .all(|v| v.is_finite())
        })
        .map(|mut c| {
            let high = c.open.max(c.high).max(c.low).max(c.close);
            let low = c.open.min(c.high).min(c.low).min(c.close);
            if high != c.high || low != c.low {
                widened += 1;
                c.high = high;
                c.low = low;
            }
            if c.volume < 0 {
                zeroed += 1;
                c.volume = 0;
            }
            c
        })
        .collect();
    if out.len() < before {
        tracing::warn!(
            "History: dropped {} candle(s) with a missing price for {}",
            before - out.len(),
            what
        );
    }
    if widened > 0 {
        tracing::info!(
            "History: widened {} candle(s) whose high or low left out their own open or close for {}",
            widened,
            what
        );
    }
    if zeroed > 0 {
        tracing::warn!("History: zeroed {} negative volume(s) for {}", zeroed, what);
    }
    out
}

/// Parse an ISO-8601 timestamp with offset (`2024-03-28T09:15:00+0530` or
/// `+05:30`) to epoch seconds.
pub fn parse_iso_epoch(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64) -> Candle {
        Candle {
            timestamp: ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            ..Default::default()
        }
    }

    /// Epoch seconds of an IST wall-clock time.
    fn ist(y: i32, m: u32, day: u32, h: u32, min: u32) -> i64 {
        d(y, m, day)
            .and_hms_opt(h, min, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            - 19_800
    }

    /// Web openalgo#2308: SENSEX 15m after the close ends 15:15, then Dhan's
    /// 16:00 bar; the 16:00 bar goes, a bar at the close goes, the session's
    /// bars stay, and a day the calendar has no session for is left alone.
    #[test]
    fn bars_from_the_close_on_are_dropped_day_by_day() {
        let close =
            |day: NaiveDate| (day == d(2026, 10, 27)).then(|| ist(2026, 10, 27, 15, 30) * 1000);
        let candles = vec![
            bar(ist(2026, 10, 27, 9, 15)),
            bar(ist(2026, 10, 27, 15, 15)),
            bar(ist(2026, 10, 27, 15, 30)),
            bar(ist(2026, 10, 27, 16, 0)),
            // No session in the calendar: left as it is.
            bar(ist(2026, 10, 25, 16, 0)),
        ];
        let kept: Vec<i64> = drop_bars_after_close(candles, close)
            .iter()
            .map(|c| c.timestamp)
            .collect();
        assert_eq!(
            kept,
            vec![
                ist(2026, 10, 27, 9, 15),
                ist(2026, 10, 27, 15, 15),
                ist(2026, 10, 25, 16, 0)
            ]
        );
    }

    #[test]
    fn the_calendar_is_asked_once_per_day() {
        let mut asked = Vec::new();
        let candles = (0..4)
            .map(|i| bar(ist(2026, 10, 27, 10, i * 15)))
            .chain([bar(ist(2026, 10, 28, 10, 0))])
            .collect();
        let kept = drop_bars_after_close(candles, |day| {
            asked.push(day);
            None
        });
        assert_eq!(kept.len(), 5);
        assert_eq!(asked, vec![d(2026, 10, 27), d(2026, 10, 28)]);
    }

    #[test]
    fn index_exchanges_follow_their_cash_session() {
        assert_eq!(session_exchange("NSE_INDEX"), "NSE");
        assert_eq!(session_exchange("BSE_INDEX"), "BSE");
        assert_eq!(session_exchange("MCX_INDEX"), "MCX");
        assert_eq!(session_exchange("MCX"), "MCX");
    }

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn chunks_cover_range_without_overlap() {
        let c = chunks(d(2024, 1, 1), d(2024, 3, 31), 60);
        assert_eq!(
            c,
            vec![
                (d(2024, 1, 1), d(2024, 2, 29)),
                (d(2024, 3, 1), d(2024, 3, 31))
            ]
        );
        assert_eq!(chunks(d(2024, 1, 1), d(2024, 1, 1), 60).len(), 1);
        assert!(chunks(d(2024, 2, 1), d(2024, 1, 1), 60).is_empty());
        // 2000-day daily chunks: 10 years -> 2 calls.
        assert_eq!(chunks(d(2015, 1, 1), d(2024, 12, 31), 2000).len(), 2);
    }

    #[test]
    fn chunk_invariants_hold_for_many_ranges() {
        for len in 0..400 {
            for max in [1, 7, 25, 60, 300] {
                let s = d(2023, 6, 1);
                let e = s + chrono::Duration::days(len);
                let c = chunks(s, e, max);
                assert_eq!(c.first().unwrap().0, s);
                assert_eq!(c.last().unwrap().1, e);
                for w in c.windows(2) {
                    assert_eq!(w[0].1.succ_opt().unwrap(), w[1].0);
                }
                for (a, b) in &c {
                    assert!((*b - *a).num_days() < max);
                }
            }
        }
    }

    #[test]
    fn sorts_and_dedupes() {
        let c = |t| Candle {
            timestamp: t,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: t as f64,
            volume: 0,
            oi: 0,
        };
        let out = sort_dedupe(vec![c(3), c(1), c(3), c(2)]);
        let ts: Vec<i64> = out.iter().map(|c| c.timestamp).collect();
        assert_eq!(ts, [1, 2, 3]);
    }

    #[test]
    fn iso_timestamps() {
        assert_eq!(
            parse_iso_epoch("2024-03-28T09:15:00+0530"),
            Some(1711597500)
        );
        assert_eq!(
            parse_iso_epoch("2024-03-28T09:15:00+05:30"),
            Some(1711597500)
        );
        assert_eq!(parse_iso_epoch("nope"), None);
    }
}
