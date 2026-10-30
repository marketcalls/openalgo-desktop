//! How Python reads the JSON values a workflow carries, where core's
//! `flow::contract::pyfmt` (Python's `repr`, `str`, `type(...).__name__`,
//! truthiness, `float()` and `int()`, the one copy every Flow module uses,
//! lead decision 25) does not already answer: `str.strip()` and the blank
//! test the web's validator applies to text, its `{{` template test,
//! Python's `==` between decoded values, and the Unicode decimal digits its
//! `\d` and `int()` read.

use crate::agent::tools::values::Pattern;
use serde_json::Value;

/// One Unicode decimal digit (category Nd), what Python's `\d` matches.
static DECIMAL_DIGIT: Pattern = Pattern::new(r"^\p{Nd}$");

fn is_decimal_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && DECIMAL_DIGIT.is_match(c.encode_utf8(&mut [0; 4])))
}

/// The value of a Unicode decimal digit, as Python's `int()` reads it.
/// Unicode encodes every decimal digit in contiguous runs of ten from zero to
/// nine, so the value is how many digits precede it in its run, modulo ten.
fn decimal_value(c: char) -> Option<u32> {
    if let Some(d) = c.to_digit(10) {
        return Some(d);
    }
    if !is_decimal_digit(c) {
        return None;
    }
    let mut before = 0u32;
    let mut code = c as u32;
    while let Some(prev) = code.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal_digit(prev) {
            break;
        }
        before += 1;
        code -= 1;
    }
    Some(before % 10)
}

/// The text with every Unicode decimal digit written as its ASCII digit, so
/// `pyfmt::to_int` reads it as Python's `int()` does (`int('٣')` is 3).
pub(crate) fn ascii_digits(text: &str) -> String {
    text.chars()
        .map(|c| match decimal_value(c) {
            Some(d) if !c.is_ascii_digit() => char::from_digit(d, 10).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// `str.isspace()` for one character: Unicode white space plus the four
/// ASCII information separators Python also counts.
fn is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

/// `text.strip()`.
pub(crate) fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `not text.strip()`: empty or only white space.
pub(crate) fn is_blank(text: &str) -> bool {
    strip(text).is_empty()
}

/// True for a string holding a `{{variable}}` reference, which only the run
/// can resolve.
pub(crate) fn is_template(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.contains("{{"))
}

/// True for `None` or a blank string: the web's "not supplied".
pub(crate) fn is_unset(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => is_blank(s),
        _ => false,
    }
}

/// The number a scalar compares as under Python's `==`: `True == 1 == 1.0`.
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// Python's `==` between two decoded JSON values.
pub(crate) fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if !x.is_f64() && !y.is_f64() => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| eq(v, w)))
        }
        _ => match (numeric(a), numeric(b)) {
            (Some(x), Some(y)) => x == y,
            (None, None) => a == b,
            _ => false,
        },
    }
}

/// Whether two values are the same dictionary key: Python's `==` between
/// hashable values. A list or an object is unhashable, so it never matches
/// (where the web raises `TypeError` and answers a 500).
pub(crate) fn same_key(a: &Value, b: &Value) -> bool {
    let hashable = |v: &Value| !matches!(v, Value::Array(_) | Value::Object(_));
    hashable(a) && hashable(b) && eq(a, b)
}
