//! How Python reads and prints the JSON values a workflow carries.
//!
//! The web's validator builds its sentences with f-strings over the decoded
//! request body, so a message quotes a value the way Python prints it:
//! `{value!r}` is `repr` (`'abc'`, `None`, `True`, `1.0`, `['a']`), `{value}`
//! is `str`, and `type(value).__name__` names `NoneType`, `bool`, `int`,
//! `float`, `str`, `list` or `dict`. Its numeric checks go through `float()`
//! and `int()`, which accept text such as `" 1_000 "` or `"Infinity"`, and
//! its blank checks through `str.strip()`. Reproducing those here is what
//! keeps every message identical to the web's.
//!
//! JSON numbers arrive as serde_json numbers: an integer literal is a Python
//! `int`, anything with a fraction or an exponent a Python `float`, as
//! `json.loads` decodes them. A JSON object's keys print in serde_json's
//! (sorted) order, since the desktop does not keep the order they were written
//! in; Unicode digits other than 0-9 are not read as numbers.

use serde_json::Value;

/// `type(value).__name__`.
pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `bool(value)`.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(value)`: a string as itself, anything else as its `repr`.
pub(crate) fn str_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// `repr(value)`. Every message's Python text goes through here (and
/// [`str_of`], built on it), so moving to core's shared `pyfmt` helper at
/// restack is a change to this one function.
pub(crate) fn repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) if !n.is_f64() => i.to_string(),
            (_, Some(u), _) if !n.is_f64() => u.to_string(),
            (_, _, Some(f)) => float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => str_repr(s),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), repr(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// `repr(float)`: the shortest text that reads back as the same number,
/// switching to an exponent below 1e-4 and from 1e16.
pub(super) fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    // `{:e}` prints the shortest round-trip digits, as d[.ddd]e<exp>.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    // Python's decimal point position: the value is 0.<digits> x 10^decpt.
    let decpt = exponent + 1;
    let body = if decpt <= -4 || decpt > 16 {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{first}{fraction}e{exp_sign}{:02}", exponent.abs())
    } else if decpt <= 0 {
        format!("0.{}{digits}", "0".repeat(decpt.unsigned_abs() as usize))
    } else {
        let point = decpt as usize;
        if point < digits.len() {
            format!("{}.{}", &digits[..point], &digits[point..])
        } else {
            format!("{digits}{}.0", "0".repeat(point - digits.len()))
        }
    };
    format!("{sign}{body}")
}

/// `repr(str)`: single quotes unless the text holds a single quote and no
/// double one, with Python's escapes for the quote, backslash and
/// non-printable characters.
fn str_repr(s: &str) -> String {
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
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_ascii() || is_printable(c) => out.push(c),
            c => {
                let code = c as u32;
                if code <= 0xff {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for one non-ASCII character: false for control,
/// separator (other than the space), format, private-use and non-characters.
fn is_printable(c: char) -> bool {
    let code = c as u32;
    let separator = c.is_whitespace();
    let format = matches!(
        code,
        0xad | 0x600..=0x605
            | 0x61c
            | 0x6dd
            | 0x70f
            | 0x890..=0x891
            | 0x8e2
            | 0x180e
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
    );
    let private_use = matches!(code, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd);
    let non_character = matches!(code, 0xfdd0..=0xfdef) || code & 0xfffe == 0xfffe;
    !(c.is_control() || separator || format || private_use || non_character)
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

/// `float(value)`, or None where Python raises `TypeError` or `ValueError`.
pub(crate) fn float_of(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => float_from_str(s),
        _ => None,
    }
}

/// `float(text)`: surrounding white space, a sign, `inf`, `infinity` and
/// `nan` in any case, and underscores between digits.
pub(crate) fn float_from_str(text: &str) -> Option<f64> {
    let digits = without_digit_underscores(strip(text))?;
    digits.parse::<f64>().ok()
}

/// `int(value)`, or None where Python raises. A float is truncated toward
/// zero; text takes a sign and digits only. Python refuses text of more than
/// 4,300 digits, and so does this; a longer number saturates.
pub(crate) fn int_of(value: &Value) -> Option<i128> {
    match value {
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => Some(i128::from(i)),
            (_, Some(u), _) => Some(i128::from(u)),
            (_, _, Some(f)) if f.is_finite() => Some(f.trunc() as i128),
            _ => None,
        },
        Value::String(s) => int_from_str(s),
        _ => None,
    }
}

/// `int(text)` in base 10.
pub(crate) fn int_from_str(text: &str) -> Option<i128> {
    let cleaned = without_digit_underscores(strip(text))?;
    let (negative, digits) = match cleaned.as_bytes().first() {
        Some(b'-') => (true, &cleaned[1..]),
        Some(b'+') => (false, &cleaned[1..]),
        _ => (false, cleaned.as_str()),
    };
    if digits.is_empty() || digits.len() > 4300 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.parse::<i128>().unwrap_or(i128::MAX);
    Some(if negative { -magnitude } else { magnitude })
}

/// The text with PEP 515 underscores removed, or None when one is not
/// between two digits.
fn without_digit_underscores(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_' {
            let before = i.checked_sub(1).and_then(|j| bytes.get(j));
            let after = bytes.get(i + 1);
            if !before.is_some_and(u8::is_ascii_digit) || !after.is_some_and(u8::is_ascii_digit) {
                return None;
            }
        }
    }
    Some(text.replace('_', ""))
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
