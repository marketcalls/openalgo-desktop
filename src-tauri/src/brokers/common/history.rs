//! History helpers: request chunking and candle normalisation.
//!
//! Brokers cap the date range of one history call (Kite: 60 days intraday,
//! 2000 days daily). `chunks` splits an inclusive `[start, end]` range into
//! inclusive sub-ranges of at most `max_days`, exactly like the web's
//! `current_end = min(current_start + chunk_days - 1, end)` loop.

use crate::brokers::types::Candle;
use crate::clock::IST_OFFSET_SECS;
use chrono::{NaiveDate, NaiveTime};

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

/// Merge time-ordered candles that fall in the same bar into that bar, as
/// the web's pandas `agg` does: open first, high max, low min, close last,
/// volume summed, OI last. Each bar is stamped with the time `bar_of`
/// gives its candles, and a candle it gives none for is dropped. A bar no
/// candle fell in does not appear (pandas drops empty bins).
pub fn merge_bars(
    candles: impl IntoIterator<Item = Candle>,
    bar_of: impl Fn(i64) -> Option<i64>,
) -> Vec<Candle> {
    let mut out: Vec<Candle> = Vec::new();
    for c in candles {
        let Some(at) = bar_of(c.timestamp) else {
            continue;
        };
        match out.last_mut() {
            Some(bar) if bar.timestamp == at => {
                bar.high = bar.high.max(c.high);
                bar.low = bar.low.min(c.low);
                bar.close = c.close;
                bar.volume = bar.volume.saturating_add(c.volume);
                bar.oi = c.oi;
            }
            _ => out.push(Candle { timestamp: at, ..c }),
        }
    }
    out
}

/// `minutes`-minute bars from time-ordered candles: pandas
/// `resample(f"{minutes}min", origin="start_day", offset=f"{offset}min",
/// closed="left", label="left")` on IST times. Bins are counted from the
/// IST midnight of the first candle's day plus `offset_minutes` (a session
/// open: 15 puts NSE's bars at 09:15, 10:15, ...), and each bar is
/// [`merge_bars`]. One minute or less leaves the candles as they are.
pub fn resample(candles: Vec<Candle>, minutes: i64, offset_minutes: i64) -> Vec<Candle> {
    if minutes <= 1 {
        return candles;
    }
    let Some(first_day) = candles.first().and_then(|c| ist_day(c.timestamp)) else {
        return candles;
    };
    let origin = first_day.and_time(NaiveTime::MIN).and_utc().timestamp() - IST_OFFSET_SECS
        + offset_minutes * 60;
    let span = minutes * 60;
    merge_bars(candles, |t| {
        Some(origin + (t - origin).div_euclid(span) * span)
    })
}

/// The bar length of an OpenAlgo intraday interval Nuvama's chart serves,
/// in minutes (web nuvama `_INTRADAY_MINUTES`); `None` for any other.
pub fn intraday_minutes(interval: &str) -> Option<i64> {
    match interval {
        "1m" => Some(1),
        "3m" => Some(3),
        "5m" => Some(5),
        "15m" => Some(15),
        "30m" => Some(30),
        "1h" => Some(60),
        _ => None,
    }
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
pub(crate) fn ist_day(ts: i64) -> Option<NaiveDate> {
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

    /// A one-minute candle at an IST time: open `p`, high `p + 1`, low
    /// `p - 1`, close `p + 0.5`, volume 10, OI `p`.
    fn minute(at: i64, p: f64) -> Candle {
        Candle {
            timestamp: at,
            open: p,
            high: p + 1.0,
            low: p - 1.0,
            close: p + 0.5,
            volume: 10,
            oi: p as i64,
        }
    }

    fn times(bars: &[Candle]) -> Vec<i64> {
        bars.iter().map(|c| c.timestamp).collect()
    }

    /// pandas `resample("5min", origin="start_day", offset="15min")`:
    /// bins from 09:15, open first, high max, low min, close last, volume
    /// summed, OI last, and an empty bin (09:25) left out.
    #[test]
    fn resampled_bars_count_from_the_offset_and_drop_empty_bins() {
        let candles = vec![
            minute(ist(2026, 10, 27, 9, 15), 100.0),
            minute(ist(2026, 10, 27, 9, 16), 103.0),
            minute(ist(2026, 10, 27, 9, 19), 98.0),
            minute(ist(2026, 10, 27, 9, 20), 101.0),
            minute(ist(2026, 10, 27, 9, 31), 105.0),
        ];
        let bars = resample(candles, 5, 15);
        assert_eq!(
            times(&bars),
            vec![
                ist(2026, 10, 27, 9, 15),
                ist(2026, 10, 27, 9, 20),
                ist(2026, 10, 27, 9, 30)
            ]
        );
        let first = bars[0];
        assert_eq!(
            (first.open, first.high, first.low, first.close),
            (100.0, 104.0, 97.0, 98.5)
        );
        assert_eq!((first.volume, first.oi), (30, 98));
    }

    /// Hourly bars sit on the session open: 09:15 and 10:15 on NSE (offset
    /// 15), 09:00 on MCX (offset 0), and the grid carries into the next day.
    #[test]
    fn hourly_bars_start_at_the_session_open_every_day() {
        let candles = || {
            vec![
                minute(ist(2026, 10, 27, 9, 15), 1.0),
                minute(ist(2026, 10, 27, 10, 14), 2.0),
                minute(ist(2026, 10, 27, 10, 15), 3.0),
                minute(ist(2026, 10, 28, 9, 20), 4.0),
            ]
        };
        assert_eq!(
            times(&resample(candles(), 60, 15)),
            vec![
                ist(2026, 10, 27, 9, 15),
                ist(2026, 10, 27, 10, 15),
                ist(2026, 10, 28, 9, 15)
            ]
        );
        assert_eq!(
            times(&resample(candles(), 60, 0)),
            vec![
                ist(2026, 10, 27, 9, 0),
                ist(2026, 10, 27, 10, 0),
                ist(2026, 10, 28, 9, 0)
            ]
        );
    }

    /// pandas counts back from the origin too: with bins from 00:15, a
    /// candle at 00:05 falls in the bin of 23:45 the day before.
    #[test]
    fn a_candle_before_the_offset_falls_in_the_bin_before_it() {
        let bars = resample(vec![minute(ist(2026, 10, 27, 0, 5), 1.0)], 30, 15);
        assert_eq!(times(&bars), vec![ist(2026, 10, 26, 23, 45)]);
    }

    #[test]
    fn one_minute_bars_and_no_candles_are_left_as_they_are() {
        let candles = vec![
            minute(ist(2026, 10, 27, 9, 15), 1.0),
            minute(ist(2026, 10, 27, 9, 16), 2.0),
        ];
        assert_eq!(resample(candles.clone(), 1, 15), candles);
        assert!(resample(Vec::new(), 5, 15).is_empty());
    }

    /// A candle `bar_of` places nowhere is dropped; the rest merge by key.
    #[test]
    fn merged_bars_take_the_key_bar_of_gives() {
        let candles = vec![
            minute(1_000, 1.0),
            minute(1_060, 2.0),
            minute(1_120, 3.0),
            minute(5_000, 4.0),
        ];
        let bars = merge_bars(candles, |t| (t != 1_120).then_some(t - t % 1_000));
        assert_eq!(times(&bars), vec![1_000, 5_000]);
        assert_eq!(
            (bars[0].open, bars[0].close, bars[0].volume),
            (1.0, 2.5, 20)
        );
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
