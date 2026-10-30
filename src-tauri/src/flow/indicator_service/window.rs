//! The ceilings that keep a Flow node from pulling an open-ended range:
//! `clamp_bars`, `trim_records`, `calendar_days_for_bars`, `history_window`
//! and `clamp_history_range`, as the web's module sizes them.

use chrono::{Days, NaiveDate};
use serde_json::Value;

use super::records::parse_iso_date;
use super::{MAX_HISTORY_BARS, MAX_HISTORY_CALENDAR_DAYS};

/// `clamp_bars`: a requested bar count held to `1..=MAX_HISTORY_BARS`.
/// `what` names the count in the log line, as the web's does.
pub fn clamp_bars(requested: i64, what: &str) -> i64 {
    if requested > MAX_HISTORY_BARS {
        tracing::info!(
            "Requested {requested} {what} exceeds the {MAX_HISTORY_BARS}-bar ceiling; using the latest {MAX_HISTORY_BARS}"
        );
        return MAX_HISTORY_BARS;
    }
    requested.max(1)
}

/// `trim_records`: only the most recent `MAX_HISTORY_BARS` records.
pub fn trim_records(records: &[Value]) -> &[Value] {
    let keep = MAX_HISTORY_BARS as usize;
    if records.len() > keep {
        tracing::info!(
            "History returned {} bars; trimming to the latest {keep}",
            records.len()
        );
        return &records[records.len() - keep..];
    }
    records
}

/// `_BARS_PER_DAY`: approximate candles per NSE trading day, by interval.
fn bars_per_day(interval: &str) -> f64 {
    match interval.to_lowercase().as_str() {
        "1m" => 375.0,
        "3m" => 125.0,
        "5m" => 75.0,
        "10m" => 38.0,
        "15m" => 25.0,
        "30m" => 13.0,
        "1h" | "60m" => 7.0,
        "2h" => 4.0,
        "3h" => 3.0,
        "4h" => 2.0,
        "d" | "1d" | "day" => 1.0,
        "w" | "week" => 0.2,
        "m" | "month" => 0.05,
        "q" | "quarter" => 0.016,
        "y" | "year" => 0.004,
        _ => 75.0,
    }
}

/// `calendar_days_for_bars`: calendar days that cover `bars` candles, with
/// the 1.6 weekend-and-holiday allowance. Unclamped.
pub fn calendar_days_for_bars(bars: i64, interval: &str) -> i64 {
    ((bars as f64 / bars_per_day(interval)) * 1.6) as i64 + 5
}

/// `history_window`: the `(start_date, end_date)` pair, as `YYYY-MM-DD`,
/// that covers the lookback ending on `end` (the web's `date.today()` when
/// no end is given; the caller passes today in IST). A non-zero
/// `lookback_days` wins over `lookback_bars`; a bar count is clamped and
/// its span capped at `MAX_HISTORY_CALENDAR_DAYS`.
pub fn history_window(
    lookback_bars: i64,
    lookback_days: Option<i64>,
    interval: &str,
    end: NaiveDate,
) -> (String, String) {
    let days = match lookback_days {
        Some(days) if days != 0 => days,
        _ => {
            let bars = clamp_bars(lookback_bars, "history bars");
            let span = calendar_days_for_bars(bars, interval);
            if span > MAX_HISTORY_CALENDAR_DAYS {
                tracing::info!(
                    "{bars} {interval} bars would span {span} days; capping the request at {MAX_HISTORY_CALENDAR_DAYS} days"
                );
            }
            span.min(MAX_HISTORY_CALENDAR_DAYS)
        }
    };
    (iso(days_before(end, days)), iso(end))
}

/// `clamp_history_range`: an explicit range narrowed so it spans at most
/// `MAX_HISTORY_BARS` bars of `interval`, by moving the start forward. A
/// range whose dates do not read as ISO dates is returned unchanged.
pub fn clamp_history_range(start_date: &str, end_date: &str, interval: &str) -> (String, String) {
    let unchanged = || (start_date.to_string(), end_date.to_string());
    let (Some(start), Some(end)) = (iso_date_prefix(start_date), iso_date_prefix(end_date)) else {
        return unchanged();
    };
    let (floor, _) = history_window(MAX_HISTORY_BARS, None, interval, end);
    let Some(floor) = iso_date_prefix(&floor) else {
        return unchanged();
    };
    if start < floor {
        tracing::info!(
            "History range {start_date}..{end_date} at {interval} exceeds the {MAX_HISTORY_BARS}-bar ceiling; starting from {} instead",
            iso(floor)
        );
        return (iso(floor), iso(end));
    }
    unchanged()
}

/// `date.fromisoformat(str(value)[:10])`.
fn iso_date_prefix(value: &str) -> Option<NaiveDate> {
    let head: String = value.chars().take(10).collect();
    match parse_iso_date(&head) {
        Some((date, "")) => Some(date),
        _ => None,
    }
}

/// `end - timedelta(days=days)`; a negative count moves forward.
fn days_before(end: NaiveDate, days: i64) -> NaiveDate {
    let shifted = if days >= 0 {
        end.checked_sub_days(Days::new(days.unsigned_abs()))
    } else {
        end.checked_add_days(Days::new(days.unsigned_abs()))
    };
    shifted.unwrap_or(end)
}

fn iso(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}
