//! Numbers and text as the web's guard and order tools read and print them.
//!
//! The web's sentences carry Python's renderings: `str(Decimal)`,
//! `format(d, ".2f")` (half to even), `repr(float)`, `f"{x:g}"`, `str(None)`.
//! The model and the audit trail read those sentences, and the contract
//! fixtures recorded from the web pin them, so the desktop prints numbers
//! the same way. Pure: no I/O.

use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::Value;
use std::str::FromStr;

/// Python's `repr()` of a float: the shortest text that reads back as the
/// same number, fixed notation between 1e-4 and 1e16, else `1e-05` style.
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let (negative, digits, exp) = shortest_digits(f);
    let sign = if negative { "-" } else { "" };
    let point = exp + 1; // digits before the decimal point
    if point > -4 && point <= 16 {
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            format!(
                "{}.{}",
                &digits[..point as usize],
                &digits[point as usize..]
            )
        };
        format!("{}{}", sign, body)
    } else {
        format!("{}{}", sign, scientific(&digits, exp))
    }
}

/// Python's `f"{x:g}"`: six significant digits, trailing zeros dropped,
/// scientific below 1e-4 and from 1e6.
pub fn py_g(f: f64) -> String {
    if !f.is_finite() {
        return py_float_repr(f);
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    let rounded = format!("{:.5e}", f);
    let (mantissa, exp) = rounded.split_once('e').unwrap_or((rounded.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        strip_zeros(&format!("{:.*}", decimals, f))
    } else {
        let m = strip_zeros(mantissa);
        format!("{}e{}{:02}", m, if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

fn strip_zeros(text: &str) -> String {
    if !text.contains('.') {
        return text.to_string();
    }
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The sign, the shortest significant digits and the decimal exponent of
/// the first digit.
fn shortest_digits(f: f64) -> (bool, String, i32) {
    let text = format!("{:e}", f.abs());
    let (mantissa, exp) = text.split_once('e').unwrap_or((text.as_str(), "0"));
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    (
        f.is_sign_negative(),
        digits.to_string(),
        exp.parse().unwrap_or(0),
    )
}

fn scientific(digits: &str, exp: i32) -> String {
    let (head, tail) = digits.split_at(1);
    let mantissa = if tail.is_empty() {
        head.to_string()
    } else {
        format!("{}.{}", head, tail)
    };
    format!(
        "{}e{}{:02}",
        mantissa,
        if exp < 0 { '-' } else { '+' },
        exp.abs()
    )
}

/// Python's `str()` of a decoded JSON value (`None`, `True`, `1.0`).
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr()` of a decoded JSON value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Number(n) if !(n.is_i64() || n.is_u64()) => {
            py_float_repr(n.as_f64().unwrap_or(f64::NAN))
        }
        other => crate::agent::tools::service::py_repr(other),
    }
}

/// Python's truth value of a decoded JSON value.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value or "")` from the web: a falsy value reads as empty.
pub fn py_text_or_empty(v: &Value) -> String {
    if py_truthy(v) {
        py_str(v)
    } else {
        String::new()
    }
}

/// Decimal text as Python's `Decimal(text)` reads it: plain or scientific.
pub fn parse_decimal(text: &str) -> Option<Decimal> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    Decimal::from_str(t)
        .ok()
        .or_else(|| Decimal::from_scientific(t).ok())
}

/// The web's `_decimal`: a finite number, never a bool. A float is read
/// through its Python text, so `1450.0` stays `1450.0`, as the web's
/// `Decimal(str(value))` keeps it.
pub fn py_decimal(v: &Value) -> Option<Decimal> {
    match v {
        Value::Null | Value::Bool(_) => None,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Decimal::from(i))
            } else if let Some(u) = n.as_u64() {
                Decimal::from_str(&u.to_string()).ok()
            } else {
                let f = n.as_f64()?;
                if f.is_finite() {
                    parse_decimal(&py_float_repr(f))
                } else {
                    None
                }
            }
        }
        Value::String(s) => parse_decimal(s),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// The value only when it is a usable, strictly positive price.
pub fn positive(d: Option<Decimal>) -> Option<Decimal> {
    d.filter(|x| *x > Decimal::ZERO)
}

/// `d.quantize(Decimal("0.01"))` under Python's default context.
pub fn quantize2(d: Decimal) -> Decimal {
    let mut r = d.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
    r.rescale(2);
    r
}

/// `format(d, ".2f")`.
pub fn fmt2(d: Decimal) -> String {
    quantize2(d).to_string()
}

/// `f"{seconds:.1f}"` and `round(seconds, 1)` for an age in seconds.
pub fn one_decimal(seconds: f64) -> (String, f64) {
    let text = format!("{:.1}", seconds);
    let value = text.parse().unwrap_or(seconds);
    (text, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_as_python_prints_them() {
        for (f, want) in [
            (1450.5, "1450.5"),
            (10.0, "10.0"),
            (1e-05, "1e-05"),
            (0.0001, "0.0001"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (-1.5, "-1.5"),
            (1.5e20, "1.5e+20"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(py_float_repr(f), want, "{}", f);
        }
        for (f, want) in [
            (1450.5, "1450.5"),
            (100.0, "100"),
            (1234567.0, "1.23457e+06"),
            (1e-05, "1e-05"),
            (0.05, "0.05"),
            (99.0, "99"),
        ] {
            assert_eq!(py_g(f), want, "{}", f);
        }
    }

    #[test]
    fn decimals_read_and_round_as_python_does() {
        assert_eq!(
            py_decimal(&json!(1450.0)).map(|d| d.to_string()),
            Some("1450.0".into())
        );
        assert_eq!(
            py_decimal(&json!(1450)).map(|d| d.to_string()),
            Some("1450".into())
        );
        assert_eq!(
            py_decimal(&json!(" 12.50 ")).map(|d| d.to_string()),
            Some("12.50".into())
        );
        assert_eq!(py_decimal(&json!(true)), None);
        assert_eq!(py_decimal(&json!("abc")), None);
        assert_eq!(py_decimal(&json!("NaN")), None);
        assert_eq!(fmt2(Decimal::from_str("2.675").unwrap()), "2.68");
        assert_eq!(fmt2(Decimal::from_str("2.665").unwrap()), "2.66");
        assert_eq!(fmt2(Decimal::from(600000)), "600000.00");
        assert_eq!(one_decimal(9.94), ("9.9".into(), 9.9));
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!("HOLD")), "'HOLD'");
        assert_eq!(py_str(&json!(10.0)), "10.0");
        assert_eq!(py_text_or_empty(&json!(0)), "");
    }
}
