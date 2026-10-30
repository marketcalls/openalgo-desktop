//! History records as the web's code reads them: the OHLCV columns through
//! `pd.to_numeric(errors="coerce")`, and timestamps through
//! `_parse_timestamp` (epoch seconds to IST wall time, or an ISO string).

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::Value;

use super::IndicatorError;
use crate::agent::tools::values::{py_str, quote};
use crate::clock::ist_offset;

/// The candle columns the web converts to numbers.
pub(crate) const OHLCV: [&str; 5] = ["open", "high", "low", "close", "volume"];

/// The records' OHLCV columns, each as long as the records. A column no
/// record carries is absent, as it is from the web's DataFrame; a record
/// without a usable value holds NaN.
pub(crate) struct Columns(Vec<(&'static str, Vec<f64>)>);

impl Columns {
    pub(crate) fn read(records: &[Value]) -> Self {
        let carried = |column: &str| {
            records
                .iter()
                .any(|r| r.as_object().is_some_and(|o| o.contains_key(column)))
        };
        Self(
            OHLCV
                .iter()
                .filter(|column| carried(column))
                .map(|column| {
                    let values = records.iter().map(|r| numeric(r.get(*column))).collect();
                    (*column, values)
                })
                .collect(),
        )
    }

    pub(crate) fn get(&self, column: &str) -> Option<&[f64]> {
        self.0
            .iter()
            .find(|(name, _)| *name == column)
            .map(|(_, values)| values.as_slice())
    }
}

/// `pd.to_numeric(value, errors="coerce")` for one cell.
pub(crate) fn numeric(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::String(text)) => py_float(text).unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Python's `float(text)`: surrounding whitespace ignored, `_` allowed
/// between two digits, `inf` and `nan` in any case.
pub(crate) fn py_float(text: &str) -> Option<f64> {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    let digit_at = |i: Option<usize>| {
        i.and_then(|i| chars.get(i))
            .is_some_and(char::is_ascii_digit)
    };
    let underscores_ok = chars
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == '_')
        .all(|(i, _)| digit_at(i.checked_sub(1)) && digit_at(Some(i + 1)));
    if !underscores_ok {
        return None;
    }
    text.replace('_', "").parse().ok()
}

/// `_parse_timestamp`: a record's timestamp as IST wall time. Epoch seconds
/// (a number, or text that reads as one) go through
/// `datetime.fromtimestamp`; other text through `datetime.fromisoformat`,
/// converted to IST when it carries an offset.
pub(crate) fn parse_timestamp(value: Option<&Value>) -> Result<NaiveDateTime, IndicatorError> {
    let text = match value {
        Some(Value::Number(n)) => return from_epoch(n.as_f64().unwrap_or(f64::NAN)),
        Some(Value::Bool(flag)) => return from_epoch(f64::from(u8::from(*flag))),
        None | Some(Value::Null) => "None".to_string(),
        Some(other) => py_str(other),
    };
    if let Some(epoch) = py_float(&text) {
        return from_epoch(epoch);
    }
    let text = text.replace('Z', "+00:00");
    match parse_iso(&text) {
        Some((wall, Some(offset))) => Ok(in_ist(wall, offset)),
        Some((wall, None)) => Ok(wall),
        None => Err(IndicatorError::value(match iso_range_error(&text) {
            Some(range) => range.to_string(),
            None => format!("Invalid isoformat string: {}", quote(&text)),
        })),
    }
}

/// Why `datetime.fromisoformat` refuses text laid out as an ISO date and
/// time with a part outside its range: the `datetime` constructor's own
/// sentence, checked in its order.
fn iso_range_error(text: &str) -> Option<&'static str> {
    let number = |part: Option<&str>| -> Option<u32> {
        let part = part?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day, rest) = if text.get(4..5) == Some("-") && text.get(7..8) == Some("-") {
        (
            text.get(..4),
            text.get(5..7),
            text.get(8..10),
            text.get(10..)?,
        )
    } else {
        (
            text.get(..4),
            text.get(4..6),
            text.get(6..8),
            text.get(8..)?,
        )
    };
    let (year, month, day) = (number(year)?, number(month)?, number(day)?);
    if !(1..=12).contains(&month) {
        return Some("month must be in 1..12");
    }
    if NaiveDate::from_ymd_opt(year as i32, month, day).is_none() {
        return Some("day is out of range for month");
    }
    let mut chars = rest.chars();
    chars.next()?;
    let clock = chars.as_str();
    let clock = &clock[..clock.find(['+', '-', '.', ',']).unwrap_or(clock.len())];
    let [hour, minute, second] = clock_fields(clock)?;
    match (hour, minute, second) {
        (24.., _, _) => Some("hour must be in 0..23"),
        (_, 60.., _) => Some("minute must be in 0..59"),
        (_, _, 60..) => Some("second must be in 0..59"),
        _ => None,
    }
}

/// `datetime.fromtimestamp(seconds)` in IST.
pub(crate) fn from_epoch(seconds: f64) -> Result<NaiveDateTime, IndicatorError> {
    if seconds.is_nan() {
        return Err(IndicatorError::value("cannot convert float NaN to integer"));
    }
    if seconds.is_infinite() {
        return Err(IndicatorError {
            kind: "OverflowError",
            message: "cannot convert float infinity to integer".into(),
        });
    }
    let whole = seconds.floor();
    let micros = ((seconds - whole) * 1e6).round_ties_even() as i64;
    let (whole, micros) = (whole as i64 + micros / 1_000_000, micros % 1_000_000);
    DateTime::from_timestamp(whole, (micros * 1000) as u32)
        .map(|utc| utc.with_timezone(&ist_offset()).naive_local())
        .ok_or_else(|| IndicatorError::value("year is out of range"))
}

/// Wall time written with a UTC offset, as IST wall time.
fn in_ist(wall: NaiveDateTime, offset_seconds: i64) -> NaiveDateTime {
    let utc = wall - Duration::seconds(offset_seconds);
    utc + Duration::seconds(i64::from(ist_offset().local_minus_utc()))
}

/// `datetime.fromisoformat` (Python 3.12) for the forms a history source
/// writes: a date (`2026-10-30`, `20261030`), optionally followed by any one
/// separator and a time (`09`, `09:15`, `09:15:00`, `091500`, with a
/// fraction), optionally followed by an offset (`+05:30`, `+0530`, `+05`).
/// The wall time as written, and the offset in seconds when there is one.
pub(crate) fn parse_iso(text: &str) -> Option<(NaiveDateTime, Option<i64>)> {
    let (date, rest) = parse_iso_date(text)?;
    let mut chars = rest.chars();
    let Some(_separator) = chars.next() else {
        return Some((date.and_time(NaiveTime::MIN), None));
    };
    let rest = chars.as_str();
    let (time, offset) = match rest.find(['+', '-']) {
        Some(at) => (&rest[..at], Some(&rest[at..])),
        None => (rest, None),
    };
    let time = parse_clock(time)?;
    let offset = match offset {
        Some(text) => Some(parse_offset(text)?),
        None => None,
    };
    Some((date.and_time(time), offset))
}

/// The leading date of an ISO string and what follows it.
pub(crate) fn parse_iso_date(text: &str) -> Option<(NaiveDate, &str)> {
    if let Some(head) = text.get(..10) {
        if let Ok(date) = NaiveDate::parse_from_str(head, "%Y-%m-%d") {
            return Some((date, &text[10..]));
        }
    }
    let head = text.get(..8)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let date = NaiveDate::parse_from_str(head, "%Y%m%d").ok()?;
    Some((date, &text[8..]))
}

/// `HH[:MM[:SS[.ffffff]]]` or `HH[MM[SS[.ffffff]]]`.
fn parse_clock(text: &str) -> Option<NaiveTime> {
    let (main, fraction) = match text.split_once(['.', ',']) {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (text, None),
    };
    let [hour, minute, second] = clock_fields(main)?;
    let micros = match fraction {
        None => 0,
        Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
            let six: String = digits.chars().chain("000000".chars()).take(6).collect();
            six.parse().ok()?
        }
        Some(_) => return None,
    };
    NaiveTime::from_hms_micro_opt(hour, minute, second, micros)
}

/// Hours, minutes and seconds from `HH`, `HH:MM`, `HH:MM:SS` or the same
/// without colons.
fn clock_fields(text: &str) -> Option<[u32; 3]> {
    let parts: Vec<&str> = if text.contains(':') {
        text.split(':').collect()
    } else {
        (0..text.len())
            .step_by(2)
            .map(|i| text.get(i..i + 2))
            .collect::<Option<Vec<&str>>>()?
    };
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut fields = [0u32; 3];
    for (slot, part) in fields.iter_mut().zip(&parts) {
        if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(fields)
}

/// `+HH[:MM[:SS]]` or `-HHMM` as seconds east of UTC.
fn parse_offset(text: &str) -> Option<i64> {
    let (sign, rest) = match text.split_at_checked(1)? {
        ("+", rest) => (1, rest),
        ("-", rest) => (-1, rest),
        _ => return None,
    };
    let [hours, minutes, seconds] = clock_fields(rest)?;
    Some(sign * i64::from(hours * 3600 + minutes * 60 + seconds))
}
