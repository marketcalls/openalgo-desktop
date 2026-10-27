//! The readings of a history frame every tool shares (the web's
//! `market.py`: `candle_columns`, `summarise_candles`, `chart_bars`,
//! `roll_up_candles`, `epoch_seconds`, `ist_label`, `fit_to_budget`).
//!
//! The summary, the chart series, the instrument card's strip and the
//! indicator input must agree about which bars a broker returned, so there
//! is one reading of each here.

use super::{IST, JSON_BUDGET};
use crate::agent::contract::json::json_safe;
use crate::agent::tools::numbers::{as_number, py_round};
use chrono::{DateTime, Datelike, Utc};
use serde_json::{json, Map, Value};

/// The candle fields, in the order a candle is emitted.
const CANDLE_FIELDS: [&str; 7] = ["timestamp", "open", "high", "low", "close", "volume", "oi"];

pub use crate::agent::tools::numbers::epoch_seconds;
use crate::agent::tools::numbers::EPOCH_MILLISECOND_FLOOR;

/// Which of a frame's own keys holds each candle field (built from the
/// first row; a frame's rows are uniform).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Columns(Vec<(&'static str, String)>);

impl Columns {
    /// The key holding `field`, when the frame has it.
    pub fn get(&self, field: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, k)| k.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn has(&self, field: &str) -> bool {
        self.get(field).is_some()
    }

    fn iter(&self) -> impl Iterator<Item = &(&'static str, String)> {
        self.0.iter()
    }
}

/// The candle fields of one row, matched case-insensitively.
pub fn candle_columns(row: &Map<String, Value>) -> Columns {
    let mut lowered: Vec<(String, &String)> = Vec::new();
    for key in row.keys() {
        let low = key.trim().to_lowercase();
        lowered.retain(|(l, _)| *l != low);
        lowered.push((low, key));
    }
    Columns(
        CANDLE_FIELDS
            .iter()
            .filter_map(|f| {
                lowered
                    .iter()
                    .find(|(l, _)| l == f)
                    .map(|(_, k)| (*f, (*k).clone()))
            })
            .collect(),
    )
}

/// An instant from epoch seconds, microseconds rounded as Python's
/// `datetime.fromtimestamp` rounds them.
fn instant(seconds: f64) -> Option<DateTime<Utc>> {
    let micros = (seconds * 1_000_000.0).round_ties_even();
    if !micros.is_finite() || micros.abs() > 1e17 {
        return None;
    }
    let micros = micros as i64;
    DateTime::from_timestamp(
        micros.div_euclid(1_000_000),
        (micros.rem_euclid(1_000_000) * 1000) as u32,
    )
}

/// A candle timestamp as an ISO-8601 string in IST (epoch seconds or
/// milliseconds); anything that is not a number is passed through.
pub fn ist_timestamp(value: &Value) -> Value {
    let Some(number) = value.as_f64().filter(|_| value.is_number()) else {
        return value.clone();
    };
    let seconds = if number.abs() >= EPOCH_MILLISECOND_FLOOR {
        number / 1000.0
    } else {
        number
    };
    match instant(seconds) {
        Some(t) => Value::String(iso_ist(t)),
        None => value.clone(),
    }
}

/// Python's `datetime.isoformat()` of an instant in IST.
pub fn iso_ist(t: DateTime<Utc>) -> String {
    let local = t.with_timezone(&IST);
    if local.timestamp_subsec_micros() == 0 {
        local.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
    } else {
        local.format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string()
    }
}

/// UTC epoch seconds as the IST wall clock a person reads
/// (`YYYY-MM-DD HH:MM`), or empty when not a timestamp.
pub fn ist_label(seconds: &Value) -> String {
    as_number(seconds)
        .and_then(instant)
        .map(|t| t.with_timezone(&IST).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn field_of<'a>(row: &'a Map<String, Value>, columns: &Columns, field: &str) -> Option<&'a Value> {
    columns.get(field).and_then(|k| row.get(k))
}

fn number_of(row: &Map<String, Value>, columns: &Columns, field: &str) -> Option<f64> {
    field_of(row, columns, field).and_then(as_number)
}

/// One returned candle, its timestamp in IST; a row whose columns were not
/// recognised is returned as it is.
pub fn candle(row: &Map<String, Value>, columns: &Columns) -> Value {
    if columns.is_empty() {
        return Value::Object(row.clone());
    }
    let mut out = Map::new();
    for (field, key) in columns.iter() {
        let value = row.get(key).cloned().unwrap_or(Value::Null);
        let value = if *field == "timestamp" {
            ist_timestamp(&value)
        } else {
            value
        };
        out.insert((*field).to_string(), value);
    }
    Value::Object(out)
}

/// The numbers an answer usually needs, over every row (not only the ones
/// returned): first open, last close, highest high and lowest low with the
/// bars they were set on, total volume and the change. A statistic no row
/// supplied is left out rather than reported as zero.
pub fn summarise_candles(rows: &[Map<String, Value>], columns: &Columns) -> Map<String, Value> {
    let mut summary = Map::new();
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return summary;
    };
    let mut first_open: Option<f64> = None;
    let mut last_close: Option<f64> = None;
    let mut highest: Option<(f64, Value)> = None;
    let mut lowest: Option<(f64, Value)> = None;
    let mut volume: Option<f64> = None;
    let stamp = |row: &Map<String, Value>| field_of(row, columns, "timestamp").cloned();
    for row in rows {
        if columns.has("open") && first_open.is_none() {
            first_open = number_of(row, columns, "open");
        }
        if let Some(close) = number_of(row, columns, "close") {
            last_close = Some(close);
        }
        if let Some(high) = number_of(row, columns, "high") {
            if highest.as_ref().is_none_or(|(h, _)| high > *h) {
                highest = Some((high, stamp(row).unwrap_or(Value::Null)));
            }
        }
        if let Some(low) = number_of(row, columns, "low") {
            if lowest.as_ref().is_none_or(|(l, _)| low < *l) {
                lowest = Some((low, stamp(row).unwrap_or(Value::Null)));
            }
        }
        if let Some(v) = number_of(row, columns, "volume") {
            volume = Some(volume.unwrap_or(0.0) + v);
        }
    }
    if columns.has("timestamp") {
        let at = |row: &Map<String, Value>| ist_timestamp(&stamp(row).unwrap_or(Value::Null));
        summary.insert("first_timestamp".into(), at(first));
        summary.insert("last_timestamp".into(), at(last));
    }
    if let Some(o) = first_open {
        summary.insert("first_open".into(), json!(o));
    }
    if let Some(c) = last_close {
        summary.insert("last_close".into(), json!(c));
    }
    for (name, extreme) in [("highest_high", highest), ("lowest_low", lowest)] {
        if let Some((value, at)) = extreme {
            summary.insert(name.into(), json!(value));
            if columns.has("timestamp") && !at.is_null() {
                summary.insert(format!("{}_timestamp", name), ist_timestamp(&at));
            }
        }
    }
    if let Some(v) = volume {
        summary.insert("total_volume".into(), json!(py_round(v, 4)));
    }
    if let (Some(o), Some(c)) = (first_open, last_close) {
        summary.insert("change".into(), json!(py_round(c - o, 6)));
        if o != 0.0 {
            summary.insert(
                "change_percent".into(),
                json!(py_round((c - o) / o * 100.0, 4)),
            );
        }
    }
    summary
}

/// The bar series a chart draws (`openalgo-charts`' `Bar`): `time` in UTC
/// epoch seconds, `open`, `high`, `low`, `close`, an optional `volume`,
/// oldest first; a row with no usable timestamp or close is dropped, and a
/// missing open, high or low reads as the close.
pub fn chart_bars(records: &[Map<String, Value>], columns: &Columns) -> Vec<Map<String, Value>> {
    if !columns.has("timestamp") || !columns.has("close") {
        return Vec::new();
    }
    let mut bars: Vec<(i64, Map<String, Value>)> = Vec::new();
    for row in records {
        let moment = field_of(row, columns, "timestamp").and_then(epoch_seconds);
        let close = number_of(row, columns, "close");
        let (Some(moment), Some(close)) = (moment, close) else {
            continue;
        };
        let mut bar = Map::new();
        bar.insert("time".into(), json!(moment));
        for field in ["open", "high", "low"] {
            let v = number_of(row, columns, field).unwrap_or(close);
            bar.insert(field.into(), json!(v));
        }
        bar.insert("close".into(), json!(close));
        if let Some(v) = number_of(row, columns, "volume") {
            bar.insert("volume".into(), json!(v));
        }
        bars.push((moment, bar));
    }
    bars.sort_by_key(|(t, _)| *t);
    bars.into_iter().map(|(_, b)| b).collect()
}

/// A history frame cleaned for an indicator (the web's `candle_frame`): one
/// row per unique timestamp (the last kept), oldest first, the `open`,
/// `high`, `low`, `close` and `volume` columns the broker supplied as
/// floats, and no gap anywhere. A column no row supplied is dropped rather
/// than emptying the frame (an index has no volume), then a row missing any
/// remaining column is dropped: one gap would poison every later value of
/// a running sum.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CandleFrame {
    /// UTC epoch seconds, ascending, unique.
    pub time: Vec<i64>,
    /// The columns kept, each as long as `time`.
    pub columns: Vec<(&'static str, Vec<f64>)>,
}

impl CandleFrame {
    /// One column, when the frame kept it.
    pub fn column(&self, name: &str) -> Option<&[f64]> {
        self.columns
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_slice())
    }

    pub fn len(&self) -> usize {
        self.time.len()
    }

    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }
}

/// [`CandleFrame`] from a history frame's rows (any order); empty when the
/// rows carry no usable timestamp or no close column.
pub fn candle_frame(records: &[Map<String, Value>], columns: &Columns) -> CandleFrame {
    let fields: Vec<&'static str> = ["open", "high", "low", "close", "volume"]
        .into_iter()
        .filter(|f| columns.has(f))
        .collect();
    if records.is_empty() || !columns.has("timestamp") || !fields.contains(&"close") {
        return CandleFrame::default();
    }
    let rows: Vec<(i64, Vec<Option<f64>>)> = records
        .iter()
        .filter_map(|row| {
            let t = field_of(row, columns, "timestamp").and_then(epoch_seconds)?;
            Some((
                t,
                fields.iter().map(|f| number_of(row, columns, f)).collect(),
            ))
        })
        .collect();
    if rows.is_empty() {
        return CandleFrame::default();
    }
    let kept: Vec<usize> = (0..fields.len())
        .filter(|i| rows.iter().any(|(_, v)| v[*i].is_some()))
        .collect();
    let mut by_time: std::collections::BTreeMap<i64, Vec<f64>> = std::collections::BTreeMap::new();
    for (t, values) in rows {
        // A row with a gap is dropped first; among what is left the last
        // row of a repeated timestamp wins.
        let complete: Option<Vec<f64>> = kept.iter().map(|i| values[*i]).collect();
        if let Some(v) = complete {
            by_time.insert(t, v);
        }
    }
    let mut frame = CandleFrame {
        time: by_time.keys().copied().collect(),
        columns: kept.iter().map(|i| (fields[*i], Vec::new())).collect(),
    };
    for values in by_time.into_values() {
        for (slot, v) in frame.columns.iter_mut().zip(values) {
            slot.1.push(v);
        }
    }
    frame
}

/// Daily candles rolled up into ISO weeks (`W`) or calendar months (`M`):
/// the first day's timestamp and open, the highest high, the lowest low,
/// the last day's close, the summed volume and the last open interest,
/// under the frame's own keys. A row whose timestamp cannot be read is
/// dropped.
pub fn roll_up_candles(
    records: &[Map<String, Value>],
    columns: &Columns,
    target: &str,
) -> Vec<Map<String, Value>> {
    let Some(stamp_key) = columns.get("timestamp") else {
        return records.to_vec();
    };
    let mut periods: Vec<((i32, u32), Vec<&Map<String, Value>>)> = Vec::new();
    for row in records {
        let Some(day) = row
            .get(stamp_key)
            .and_then(epoch_seconds)
            .and_then(|s| instant(s as f64))
            .map(|t| t.with_timezone(&IST).date_naive())
        else {
            continue;
        };
        let key = if target == "W" {
            let iso = day.iso_week();
            (iso.year(), iso.week())
        } else {
            (day.year(), day.month())
        };
        match periods.iter_mut().find(|(k, _)| *k == key) {
            Some((_, rows)) => rows.push(row),
            None => periods.push((key, vec![row])),
        }
    }
    periods.sort_by_key(|(k, _)| *k);
    periods
        .into_iter()
        .filter_map(|(_, rows)| roll_period(&rows, columns, stamp_key))
        .collect()
}

fn roll_period(
    rows: &[&Map<String, Value>],
    columns: &Columns,
    stamp_key: &str,
) -> Option<Map<String, Value>> {
    let (first, last) = (*rows.first()?, *rows.last()?);
    let numbers = |field: &str| -> Vec<f64> {
        rows.iter()
            .filter_map(|r| number_of(r, columns, field))
            .collect()
    };
    let raw = |row: &Map<String, Value>, key: &str| row.get(key).cloned().unwrap_or(Value::Null);
    let mut candle = Map::new();
    candle.insert(stamp_key.to_string(), raw(first, stamp_key));
    if let Some(k) = columns.get("open") {
        candle.insert(k.to_string(), raw(first, k));
    }
    if let Some(k) = columns.get("high") {
        let highs = numbers("high");
        let high = highs.iter().copied().reduce(f64::max);
        candle.insert(k.to_string(), high.map_or(Value::Null, |h| json!(h)));
    }
    if let Some(k) = columns.get("low") {
        let lows = numbers("low");
        let low = lows.iter().copied().reduce(f64::min);
        candle.insert(k.to_string(), low.map_or(Value::Null, |l| json!(l)));
    }
    if let Some(k) = columns.get("close") {
        candle.insert(k.to_string(), raw(last, k));
    }
    if let Some(k) = columns.get("volume") {
        let volumes = numbers("volume");
        // Python's sum() of an empty list is the integer 0.
        let total = match volumes.is_empty() {
            true => json!(0),
            false => json!(volumes.iter().sum::<f64>()),
        };
        candle.insert(k.to_string(), total);
    }
    if let Some(k) = columns.get("oi") {
        candle.insert(k.to_string(), raw(last, k));
    }
    Some(candle)
}

/// How many characters a payload takes as compact JSON.
pub fn rendered_length(payload: &Value) -> usize {
    serde_json::to_string(&json_safe(payload))
        .map(|t| t.chars().count())
        .unwrap_or(JSON_BUDGET + 1)
}

/// The payload for the largest row count that fits the character budget,
/// down to none at all, so a result is always well formed rather than a
/// truncation envelope. `build` says in the payload how many were left out.
pub fn fit_to_budget(mut build: impl FnMut(usize) -> Value, start: usize) -> Value {
    let mut limit = start;
    loop {
        let payload = build(limit);
        let length = rendered_length(&payload);
        if limit == 0 || length <= JSON_BUDGET {
            return payload;
        }
        let scaled = (limit as f64 * JSON_BUDGET as f64 / length as f64) as usize;
        limit = (limit - 1).min(scaled);
    }
}
