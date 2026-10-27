//! The tool layer's one reading and rendering of numbers (the web's
//! `as_number`, `format_number` and `format_price` in `tools/base.py`).
//!
//! A broker sends a price as a float, an integer or a numeric string
//! depending on the adapter; every toolkit that does arithmetic on one reads
//! it through [`as_number`], and every one-line confirmation renders it
//! through [`format_number`] or [`format_price`], so two toolkits never
//! disagree about the same figure.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata as IST;
use serde_json::Value;

/// Epoch values at or above this are milliseconds rather than seconds (far
/// past any seconds timestamp, year 5138, and far below any milliseconds
/// one, 1973).
pub const EPOCH_MILLISECOND_FLOOR: f64 = 100_000_000_000.0;

/// A service field as a finite float: `None` when it is missing, a boolean,
/// not a number at all, or a NaN or an infinity (the honest reading of a
/// figure the exchange did not supply, and what a chart draws as a gap).
/// Text is read as Python's `float()` reads it.
pub fn as_number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => parse_float(s)?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// Python's `float(text)`: surrounding spaces ignored, an underscore allowed
/// between two digits.
fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.contains('_') {
        let b = t.as_bytes();
        let ok = b.iter().enumerate().all(|(i, c)| {
            *c != b'_'
                || (i > 0
                    && i + 1 < b.len()
                    && b[i - 1].is_ascii_digit()
                    && b[i + 1].is_ascii_digit())
        });
        if !ok {
            return None;
        }
        return t.replace('_', "").parse().ok();
    }
    t.parse().ok()
}

/// A naive date-time read as an IST wall clock.
fn ist_wall(naive: NaiveDateTime) -> Option<i64> {
    IST.from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp())
}

/// Python's `datetime.fromisoformat`, a naive result read as IST.
fn iso_seconds(text: &str) -> Option<i64> {
    let t = text.replacen(' ', "T", 1);
    let t = t
        .strip_suffix('Z')
        .map(|s| format!("{}+00:00", s))
        .unwrap_or(t);
    if let Ok(dt) = DateTime::parse_from_str(&t, "%Y-%m-%dT%H:%M:%S%.f%:z") {
        return Some(dt.timestamp());
    }
    if let Ok(dt) = DateTime::parse_from_str(&t, "%Y-%m-%dT%H:%M%:z") {
        return Some(dt.timestamp());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&t, format) {
            return ist_wall(naive);
        }
    }
    let date = NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok()?;
    ist_wall(date.and_hms_opt(0, 0, 0)?)
}

/// A candle timestamp as UTC epoch seconds for a chart: epoch values
/// verbatim (milliseconds scaled down), text and naive date-times read as
/// IST; `None` drops the candle rather than plotting it at the epoch.
pub fn epoch_seconds(value: &Value) -> Option<i64> {
    let number = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            let text = s.trim();
            if text.is_empty() {
                return None;
            }
            match text.parse::<f64>() {
                Ok(n) => n,
                Err(_) => return iso_seconds(text),
            }
        }
        _ => return None,
    };
    if !number.is_finite() {
        return None;
    }
    let seconds = if number.abs() >= EPOCH_MILLISECOND_FLOOR {
        number / 1000.0
    } else {
        number
    };
    Some(seconds.trunc() as i64)
}

/// A number for a one-line confirmation: no decimals from 1,000 up, at most
/// two below (trailing zeros dropped), thousands separated; `unknown` for
/// anything that is not a number.
pub fn format_number(value: &Value) -> String {
    let Some(number) = as_number(value) else {
        return "unknown".into();
    };
    if number.abs() >= 1000.0 {
        return grouped(&format!("{:.0}", number));
    }
    let text = grouped(&format!("{:.2}", number));
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// A traded price or a rupee figure: thousands separated, always two
/// decimals (the paise are part of a price); `unknown` when not a number.
pub fn format_price(value: &Value) -> String {
    match as_number(value) {
        Some(number) => grouped(&format!("{:.2}", number)),
        None => "unknown".into(),
    }
}

/// [`format_price`] of a float already read.
pub fn price(number: f64) -> String {
    format_price(&Value::from(number))
}

/// Python's `round(x, digits)`: the nearest value with that many decimals,
/// ties to even on the exact binary value (both languages print the exact
/// decimal expansion and round it the same way).
pub fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", digits, x).parse().unwrap_or(x)
}

/// Thousands separators in the integer part of a formatted number.
fn grouped(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let (int, frac) = match rest.find('.') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let mut out = String::with_capacity(text.len() + int.len() / 3);
    out.push_str(sign);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.push_str(frac);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_number_is_read_whatever_it_arrived_as() {
        assert_eq!(as_number(&json!(5)), Some(5.0));
        assert_eq!(as_number(&json!(1520.5)), Some(1520.5));
        assert_eq!(as_number(&json!(" 1520.5 ")), Some(1520.5));
        assert_eq!(as_number(&json!("1_000")), Some(1000.0));
        assert_eq!(as_number(&json!("1e3")), Some(1000.0));
        for none in [
            json!(null),
            json!(true),
            json!("abc"),
            json!("nan"),
            json!("inf"),
            json!("_1"),
            json!("1__0"),
            json!([1]),
            json!({"a": 1}),
            json!(""),
        ] {
            assert_eq!(as_number(&none), None, "{}", none);
        }
    }

    /// The values are Python's own output for the same calls.
    #[test]
    fn numbers_render_as_the_web_renders_them() {
        let cases = [
            (json!(1234567.5), "1,234,568", "1,234,567.50"),
            (json!(2.5), "2.5", "2.50"),
            (json!(999.999), "1,000", "1,000.00"),
            (json!(12.5), "12.5", "12.50"),
            (json!(100), "100", "100.00"),
            (json!(0), "0", "0.00"),
            (json!(-0.001), "-0", "-0.00"),
            (json!(0.125), "0.12", "0.12"),
            (json!(-1302.5), "-1,302", "-1,302.50"),
            (json!("24500"), "24,500", "24,500.00"),
            (json!(null), "unknown", "unknown"),
        ];
        for (v, number, price) in cases {
            assert_eq!(format_number(&v), number, "{}", v);
            assert_eq!(format_price(&v), price, "{}", v);
        }
    }

    #[test]
    fn rounding_matches_python() {
        assert_eq!(py_round(0.125, 2), 0.12);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(1.23456789, 4), 1.2346);
        assert_eq!(py_round(-0.5, 0), -0.0);
        assert_eq!(py_round(12.0, 6), 12.0);
    }
}
