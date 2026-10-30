//! Reading single bars out of history: `bar_at_offset` (the Bar Offset
//! node: the Nth closed bar back) and `prior_period_ohlc` (the Prior Period
//! OHLC node: the last complete hour, day, week or month).

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde_json::{Map, Value};

use super::records::{from_epoch, numeric, parse_iso, parse_timestamp, py_float};
use super::IndicatorError;

/// `_INTERVAL_MINUTES`: minutes per bar for the intraday spellings.
fn interval_minutes(key: &str) -> Option<i64> {
    Some(match key {
        "1m" => 1,
        "3m" => 3,
        "5m" => 5,
        "10m" => 10,
        "15m" => 15,
        "30m" => 30,
        "1h" | "60m" => 60,
        "2h" => 120,
        "3h" => 180,
        "4h" => 240,
        _ => return None,
    })
}

/// `_drop_forming_bar`: the records without the last one when it is still
/// forming, so offset 0 means the last closed bar. A daily bar is forming
/// on its own date, a weekly or monthly bar while its week (Monday to
/// Sunday, pandas `W`) or month contains `now`, an intraday bar until one
/// bar's length after it opened. An interval spelled any other way keeps
/// every record.
///
/// The web treats weekly and monthly bars like daily ones, forming only on
/// the date their timestamp carries (the week's or month's first session),
/// so from the second day of a week `bar_at_offset(0)` on `W` returns the
/// still-forming week. Fixed here (IND-062, lead decision 10); the tests pin
/// the web's answer next to ours.
fn drop_forming_bar<'r>(
    records: &'r [Value],
    interval: &str,
    now: NaiveDateTime,
) -> Result<&'r [Value], IndicatorError> {
    let Some((last, closed)) = records.split_last() else {
        return Ok(records);
    };
    let last_ts = parse_timestamp(last.get("timestamp"))?;
    let key = interval.trim().to_lowercase();
    let forming = match key.as_str() {
        "d" | "1d" | "day" => last_ts.date() == now.date(),
        "w" | "week" => bucket_of(last_ts, true) == bucket_of(now, true),
        "m" | "month" => bucket_of(last_ts, false) == bucket_of(now, false),
        other => interval_minutes(other).is_some_and(|m| now - last_ts < Duration::minutes(m)),
    };
    Ok(if forming { closed } else { records })
}

/// `bar_at_offset`: the record `offset_bars` closed bars back from the most
/// recent closed one (0 is the most recent), or `None` when the history
/// does not reach that far. `records` ascend, as history returns them;
/// `now` is the IST wall time. A timestamp that cannot be read is the web's
/// `ValueError`, which its Bar Offset node does not catch.
pub fn bar_at_offset<'r>(
    records: &'r [Value],
    offset_bars: i64,
    interval: &str,
    now: NaiveDateTime,
) -> Result<Option<&'r Value>, IndicatorError> {
    if offset_bars < 0 {
        return Ok(None);
    }
    let closed = drop_forming_bar(records, interval, now)?;
    let back = usize::try_from(offset_bars).unwrap_or(usize::MAX);
    Ok(closed
        .len()
        .checked_sub(back.saturating_add(1))
        .map(|i| &closed[i]))
}

/// The previous period's candle, as the web's dict: `period`, `date`, then
/// open, high, low, close and volume. For an hour or a day these are the
/// record's own values as history returned them; for a week or a month the
/// aggregate as numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorPeriodOhlc {
    pub period: String,
    pub date: String,
    pub open: Value,
    pub high: Value,
    pub low: Value,
    pub close: Value,
    pub volume: Value,
}

impl PriorPeriodOhlc {
    /// The dict the node spreads into its output.
    pub fn to_map(&self) -> Map<String, Value> {
        [
            ("period", Value::from(self.period.clone())),
            ("date", Value::from(self.date.clone())),
            ("open", self.open.clone()),
            ("high", self.high.clone()),
            ("low", self.low.clone()),
            ("close", self.close.clone()),
            ("volume", self.volume.clone()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
}

/// `prior_period_ohlc`: the last complete `previous_hour`, `previous_day`,
/// `previous_week` or `previous_month` before `now` (IST wall time; the web
/// takes a date, so a caller passing a date passes its midnight). Every
/// refusal is the web's `ValueError`, which its node reports.
pub fn prior_period_ohlc(
    records: &[Value],
    period: &str,
    now: NaiveDateTime,
) -> Result<PriorPeriodOhlc, IndicatorError> {
    if records.is_empty() {
        return Err(IndicatorError::value(
            "no historical data available for this symbol/date range",
        ));
    }
    match period {
        "previous_hour" => last_record_before(records, period, now, "%Y-%m-%d %H", "%Y-%m-%d %H:%M"),
        "previous_day" => last_record_before(records, period, now, "%Y-%m-%d", "%Y-%m-%d"),
        "previous_week" | "previous_month" => aggregate_previous(records, period, now),
        _ => Err(IndicatorError::value(format!(
            "unknown period '{period}' \u{2014} expected previous_hour/previous_day/previous_week/previous_month"
        ))),
    }
}

/// The hour and day cases: the last record outside `now`'s bucket.
fn last_record_before(
    records: &[Value],
    period: &str,
    now: NaiveDateTime,
    bucket: &str,
    shown: &str,
) -> Result<PriorPeriodOhlc, IndicatorError> {
    let current = now.format(bucket).to_string();
    let mut closed = None;
    for record in records {
        let at = parse_timestamp(record.get("timestamp"))?;
        if at.format(bucket).to_string() != current {
            closed = Some((record, at));
        }
    }
    // Falling back to the newest record would report the still-forming
    // candle as the previous one.
    let Some((row, at)) = closed else {
        return Err(IndicatorError::value(format!(
            "not enough history to determine {period}"
        )));
    };
    let field = |key: &str| row.get(key).cloned().unwrap_or(Value::Null);
    Ok(PriorPeriodOhlc {
        period: period.to_string(),
        date: at.format(shown).to_string(),
        open: field("open"),
        high: field("high"),
        low: field("low"),
        close: field("close"),
        volume: field("volume"),
    })
}

/// A calendar bucket: a Monday-to-Sunday week (pandas `W`) by its Monday, or
/// a month by its first day.
fn bucket_of(at: NaiveDateTime, weekly: bool) -> NaiveDate {
    let date = at.date();
    if weekly {
        date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
    } else {
        date.with_day(1).unwrap_or(date)
    }
}

/// pandas' `str(Period)`: `2026-10-19/2026-10-25` for a week, `2026-10` for
/// a month.
fn bucket_label(start: NaiveDate, weekly: bool) -> String {
    if weekly {
        let end = start + Duration::days(6);
        format!("{}/{}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
    } else {
        start.format("%Y-%m").to_string()
    }
}

/// `_timestamp_series`: every record's time. When every timestamp is a
/// number they are epoch seconds in IST; otherwise text, read at the wall
/// time written (pandas keeps a written offset's own clock). A record whose
/// time cannot be read falls in no bucket.
fn record_times(records: &[Value]) -> Vec<Option<NaiveDateTime>> {
    let stamps: Vec<Option<&Value>> = records
        .iter()
        .map(|r| r.get("timestamp").filter(|v| !v.is_null()))
        .collect();
    let all_numbers = stamps
        .iter()
        .all(|s| s.is_none_or(|v| v.is_number() || v.is_boolean()));
    stamps
        .into_iter()
        .map(|stamp| {
            let stamp = stamp?;
            if all_numbers {
                return from_epoch(numeric(Some(stamp))).ok();
            }
            let text = stamp.as_str()?;
            if py_float(text).is_some() {
                return None;
            }
            parse_iso(text).map(|(wall, _)| wall)
        })
        .collect()
}

/// The week and month cases: every record of the latest complete bucket
/// before `now`'s, aggregated.
fn aggregate_previous(
    records: &[Value],
    period: &str,
    now: NaiveDateTime,
) -> Result<PriorPeriodOhlc, IndicatorError> {
    let weekly = period == "previous_week";
    let current = bucket_of(now, weekly);
    let mut rows: Vec<(NaiveDateTime, &Value)> = records
        .iter()
        .zip(record_times(records))
        .filter_map(|(record, at)| at.map(|at| (at, record)))
        .filter(|(at, _)| bucket_of(*at, weekly) < current)
        .collect();
    let Some(last) = rows.iter().map(|(at, _)| bucket_of(*at, weekly)).max() else {
        return Err(IndicatorError::value(format!(
            "not enough history to determine {period}"
        )));
    };
    rows.retain(|(at, _)| bucket_of(*at, weekly) == last);
    rows.sort_by_key(|(at, _)| *at);
    let column =
        |key: &str| -> Vec<f64> { rows.iter().map(|(_, r)| numeric(r.get(key))).collect() };
    let first = |values: &[f64]| values.first().copied().unwrap_or(f64::NAN);
    let opens = column("open");
    let closes = column("close");
    let has_volume = records
        .iter()
        .any(|r| r.as_object().is_some_and(|o| o.contains_key("volume")));
    Ok(PriorPeriodOhlc {
        period: period.to_string(),
        date: bucket_label(last, weekly),
        open: number(first(&opens)),
        high: number(nan_extreme(&column("high"), f64::max)),
        low: number(nan_extreme(&column("low"), f64::min)),
        close: number(closes.last().copied().unwrap_or(f64::NAN)),
        volume: if has_volume {
            number(column("volume").iter().filter(|v| !v.is_nan()).sum())
        } else {
            Value::Null
        },
    })
}

/// pandas' `max()`/`min()`: NaN skipped, NaN when nothing is left.
fn nan_extreme(values: &[f64], pick: fn(f64, f64) -> f64) -> f64 {
    values
        .iter()
        .copied()
        .filter(|v| !v.is_nan())
        .reduce(pick)
        .unwrap_or(f64::NAN)
}

/// A float as JSON (`float(...)` in the web's dict); NaN becomes null.
fn number(value: f64) -> Value {
    Value::from(value)
}
