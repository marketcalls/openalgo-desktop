//! Python formatting rules the web's message templates rely on.
//!
//! The web builds every Telegram and WhatsApp message with f-strings over the
//! JSON it got back from `/api/v1`, so a value prints the way Python prints
//! it: `str(10.0)` is `10.0`, `str(None)` is `None`, `f"{x:,.2f}"` groups
//! thousands. These helpers reproduce that so the texts match the web.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;

/// Python `float(v)`, `None` where Python raises.
pub fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim().replace('_', "");
            match t.to_ascii_lowercase().as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Some(f64::NAN),
                _ => t.parse::<f64>().ok(),
            }
        }
        _ => None,
    }
}

/// Python `int(v)`, `None` where Python raises (`int("1.5")` raises,
/// `int(1.5)` truncates).
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => s.trim().replace('_', "").parse::<i64>().ok(),
        _ => None,
    }
}

/// `float(d.get(key, default))` with the web's `except: fallback`.
pub fn get_f64(obj: &Value, key: &str, fallback: f64) -> f64 {
    match obj.get(key) {
        None => fallback,
        Some(v) => py_float(v).unwrap_or(fallback),
    }
}

/// Python `repr(float)` for the values a trading API returns.
pub fn py_float_str(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f.fract() == 0.0 && f.abs() < 1e16 {
        return format!("{:.1}", f);
    }
    let s = format!("{}", f);
    if s.contains('e') || s.contains('E') {
        return s;
    }
    s
}

/// Python `str(v)` of a JSON-decoded value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float_str(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => py_repr(v),
    }
}

/// Python `repr(v)` of a JSON-decoded value (what a list or dict prints).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::with_capacity(s.len() + 2);
            out.push(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `d.get(key, "N/A")` printed with `str()`.
pub fn get_str(obj: &Value, key: &str, default: &str) -> String {
    match obj.get(key) {
        None => default.to_string(),
        Some(v) => py_str(v),
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
    }
}

fn group_thousands(int_part: &str) -> String {
    let bytes = int_part.as_bytes();
    let mut out = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Python `f"{x:,.2f}"`.
pub fn comma2(x: f64) -> String {
    if !x.is_finite() {
        return py_float_str(x);
    }
    let s = format!("{:.2}", x.abs());
    let (int_part, frac) = s.split_once('.').unwrap_or((&s, "00"));
    let neg = x.is_sign_negative();
    format!(
        "{}{}.{}",
        if neg { "-" } else { "" },
        group_thousands(int_part),
        frac
    )
}

/// Python `f"{x:+.2f}"`.
pub fn signed2(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "+nan".into()
        } else if x > 0.0 {
            "+inf".into()
        } else {
            "-inf".into()
        };
    }
    let s = format!("{:.2}", x);
    if s.starts_with('-') {
        s
    } else {
        format!("+{}", s)
    }
}

/// Python `f"{n:,}"` for an int.
pub fn comma_int(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    format!("{}{}", if n < 0 { "-" } else { "" }, group_thousands(&s))
}

/// Python `str.title()`.
pub fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_cased = true;
        } else {
            out.push(c);
            prev_cased = false;
        }
    }
    out
}

/// The `datetime` a SQLite `CURRENT_TIMESTAMP` column holds.
pub fn parse_db_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    for f in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(n.and_utc());
        }
    }
    None
}

/// How Flask's `jsonify` writes a `datetime`: `Wed, 07 Oct 2026 10:00:00 GMT`.
pub fn http_date(s: Option<&str>) -> Value {
    match s.and_then(parse_db_time) {
        Some(t) => Value::String(t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()),
        None => Value::Null,
    }
}

/// Phone numbers and JIDs in logs: keep the first two and last two digits.
pub fn redact_phone(raw: &str) -> String {
    let digits: String = raw
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.len() <= 4 {
        return "****".into();
    }
    format!(
        "{}{}{}",
        &digits[..2],
        "*".repeat(digits.len() - 4),
        &digits[digits.len() - 2..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_number_formats() {
        assert_eq!(comma2(1234567.891), "1,234,567.89");
        assert_eq!(comma2(-1234.5), "-1,234.50");
        assert_eq!(comma2(0.0), "0.00");
        assert_eq!(comma2(-0.001), "-0.00");
        assert_eq!(comma2(999.999), "1,000.00");
        assert_eq!(signed2(1.5), "+1.50");
        assert_eq!(signed2(-0.25), "-0.25");
        assert_eq!(signed2(0.0), "+0.00");
        assert_eq!(comma_int(1234567), "1,234,567");
        assert_eq!(comma_int(-1000), "-1,000");
        assert_eq!(comma_int(12), "12");
        assert_eq!(py_float_str(100.0), "100.0");
        assert_eq!(py_float_str(2500.5), "2500.5");
        assert_eq!(py_float_str(0.1), "0.1");
    }

    #[test]
    fn python_str_of_json() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(10)), "10");
        assert_eq!(py_str(&json!(10.0)), "10.0");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(["a", 1])), "['a', 1]");
        assert_eq!(py_str(&json!({"k": "v"})), "{'k': 'v'}");
    }

    #[test]
    fn python_conversions() {
        assert_eq!(py_int(&json!("10")), Some(10));
        assert_eq!(py_int(&json!("10.5")), None);
        assert_eq!(py_int(&json!(10.9)), Some(10));
        assert_eq!(py_float(&json!("12.5")), Some(12.5));
        assert_eq!(py_float(&json!(null)), None);
        assert_eq!(get_f64(&json!({"a": "x"}), "a", 0.0), 0.0);
        assert_eq!(title("complete"), "Complete");
        assert_eq!(title("trigger pending"), "Trigger Pending");
        assert_eq!(title("trigger_pending"), "Trigger_Pending");
    }

    #[test]
    fn dates_and_redaction() {
        assert_eq!(
            http_date(Some("2026-10-07 10:00:00")),
            json!("Wed, 07 Oct 2026 10:00:00 GMT")
        );
        assert_eq!(http_date(None), Value::Null);
        assert_eq!(redact_phone("919876543210@s.whatsapp.net"), "91********10");
        assert_eq!(redact_phone("12"), "****");
    }
}
