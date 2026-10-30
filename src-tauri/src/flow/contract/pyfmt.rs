//! Python's text forms of decoded JSON values, as the web's executor writes
//! them into interpolated fields, log lines and error sentences.
//!
//! The web formats values with `str()` and `repr()` (`f"{result}"`,
//! `{value!r}`), so a stored dict reads `{'ltp': 100.5}`, `True` and `None`
//! are capitalised and a float always shows a decimal point (`100.0`).
//! A workflow that interpolates `{{quote.data.ltp}}` into a Telegram message
//! or compares `{{flag}}` as text depends on those exact spellings.

use serde_json::Value;

/// Python's `str()` of a decoded JSON value: a string as itself, anything
/// else as [`repr`].
pub fn str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => repr(other),
    }
}

/// Python's `repr()` of a decoded JSON value.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => float(f),
            _ => n.to_string(),
        },
        Value::String(s) => quote(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", quote(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Python's `repr()` of a float: the shortest digits that read back the
/// same, in positional form for exponents -4 to 15 (always with a decimal
/// point) and as `1e-05` / `1e+16` otherwise.
pub fn float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    // `{:e}` gives the shortest round-trip digits as `d.ddde<exp>`.
    let sci = format!("{:e}", f);
    let (mantissa, exp) = match sci.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (sci.as_str(), 0),
    };
    if (-4..16).contains(&exp) {
        let text = format!("{}", f);
        if text.contains('.') {
            text
        } else {
            format!("{}.0", text)
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", mantissa, sign, exp.abs())
    }
}

/// Python's `repr()` of a string: single quotes unless the text holds a
/// single quote and no double quote; a character `str.isprintable()`
/// refuses is escaped as `\xNN`, `\uNNNN` or `\UNNNNNNNN`. The one
/// helper for Python text (decision 25): the validator and every node use it.
pub fn quote(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii() && (c as u32) >= 0x20 && c as u32 != 0x7f => out.push(c),
            c if !c.is_ascii() && is_printable(c) => out.push(c),
            c => {
                let code = c as u32;
                if code <= 0xff {
                    out.push_str(&format!("\\x{:02x}", code));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", code));
                } else {
                    out.push_str(&format!("\\U{:08x}", code));
                }
            }
        }
    }
    out.push(q);
    out
}

/// `str.isprintable()` for one non-ASCII character: false for control,
/// separator (other than the space), format, private-use and
/// non-characters. Unassigned code points, which Python also escapes, pass
/// through (Rust's standard library carries no category table).
fn is_printable(c: char) -> bool {
    let code = c as u32;
    let format = matches!(code, 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2
        | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f
        | 0xfeff | 0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f
        | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f);
    let private_use = matches!(code, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd);
    let non_character = matches!(code, 0xfdd0..=0xfdef) || code & 0xfffe == 0xfffe;
    !(c.is_control() || c.is_whitespace() || format || private_use || non_character)
}

/// Python's `json.dumps(value)` with its defaults: `", "` and `": "`
/// separators, non-ASCII escaped as `\uXXXX`, floats as Python writes them.
pub fn json_dumps(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (None, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.is_nan() => "NaN".into(),
            (_, _, Some(f)) if f.is_infinite() => {
                if f > 0.0 { "Infinity" } else { "-Infinity" }.into()
            }
            (_, _, Some(f)) => float(f),
            _ => n.to_string(),
        },
        Value::String(s) => json_string(s),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(json_dumps).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", json_string(k), json_dumps(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7f => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python's `float(text)`: surrounding whitespace ignored, `_` allowed
/// between digits, `inf`/`nan` in any case; `None` where Python raises.
pub fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let cleaned = if t.contains('_') {
        let chars: Vec<char> = t.chars().collect();
        let ok = chars.iter().enumerate().all(|(i, c)| {
            *c != '_'
                || (i > 0
                    && i + 1 < chars.len()
                    && chars[i - 1].is_ascii_digit()
                    && chars[i + 1].is_ascii_digit())
        });
        if !ok {
            return None;
        }
        t.replace('_', "")
    } else {
        t.to_string()
    };
    let lower = cleaned.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if matches!(unsigned, "inf" | "infinity" | "nan") {
        return lower.parse::<f64>().ok();
    }
    // Rust accepts forms Python refuses ("1." and ".5" both parse in each,
    // but Rust also takes "e5"-less oddities like "1e"); require a digit.
    if !unsigned
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '.')
    {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Python's `float(value)` of a decoded JSON value: numbers and booleans
/// as numbers, text through [`parse_float`], anything else `None`.
pub fn to_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => parse_float(s),
        _ => None,
    }
}

/// Python's `int(value)` of a decoded JSON value: whole numbers as they
/// are, floats truncated, booleans 0/1, text of a whole number (spaces and
/// `_` between digits allowed); `None` where Python raises.
pub fn to_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::String(s) => {
            let t = s.trim();
            let digits = t.trim_start_matches(['+', '-']);
            let chars: Vec<char> = digits.chars().collect();
            let well_formed = !chars.is_empty()
                && chars.iter().enumerate().all(|(i, c)| {
                    c.is_ascii_digit()
                        || (*c == '_'
                            && i > 0
                            && i + 1 < chars.len()
                            && chars[i - 1].is_ascii_digit()
                            && chars[i + 1].is_ascii_digit())
                });
            if !well_formed || t.len() - digits.len() > 1 {
                return None;
            }
            t.replace('_', "").parse::<i64>().ok()
        }
        _ => None,
    }
}

/// Python's `type(value).__name__` for a decoded JSON value.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python's truthiness of a decoded JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_read_as_python_prints_them() {
        assert_eq!(str(&json!("RELIANCE")), "RELIANCE");
        assert_eq!(str(&json!(100.0)), "100.0");
        assert_eq!(str(&json!(100)), "100");
        assert_eq!(str(&json!(true)), "True");
        assert_eq!(str(&json!(null)), "None");
        assert_eq!(
            str(&json!({"ltp": 1.5, "s": "it's"})),
            "{'ltp': 1.5, 's': \"it's\"}"
        );
        assert_eq!(str(&json!([1, "a", false])), "[1, 'a', False]");
    }

    /// CPython 3.12: `repr("caf\u00e9 \u00a0\u200b\u20b9\u00ad\u2028\U0001F600
    /// \U000E0001\ue000\ufffe\x85\u3000x")`, and the ASCII escapes.
    #[test]
    fn text_is_escaped_as_python_repr_escapes_it() {
        let s = "caf\u{e9} \u{a0}\u{200b}\u{20b9}\u{ad}\u{2028}\u{1F600}\u{E0001}\u{e000}\u{fffe}\u{85}\u{3000}x";
        assert_eq!(
            quote(s),
            "'caf\u{e9} \\xa0\\u200b\u{20b9}\\xad\\u2028\u{1F600}\\U000e0001\\ue000\\ufffe\\x85\\u3000x'"
        );
        assert_eq!(quote("a\tb\n\x01\x7f\\"), "'a\\tb\\n\\x01\\x7f\\\\'");
        assert_eq!(quote("it's"), "\"it's\"");
        assert_eq!(quote("'\""), "'\\'\"'");
    }

    #[test]
    fn floats_follow_python_repr() {
        assert_eq!(float(0.1), "0.1");
        assert_eq!(float(1e16), "1e+16");
        assert_eq!(float(1234567890123456.0), "1234567890123456.0");
        assert_eq!(float(0.0001), "0.0001");
        assert_eq!(float(0.00001), "1e-05");
        assert_eq!(float(-2.5e-7), "-2.5e-07");
        assert_eq!(float(f64::INFINITY), "inf");
    }

    #[test]
    fn json_is_written_as_python_dumps_it() {
        assert_eq!(
            json_dumps(&json!({"a": [1, 2.0, null, true], "b": "caf\u{e9} \"x\""})),
            "{\"a\": [1, 2.0, null, true], \"b\": \"caf\\u00e9 \\\"x\\\"\"}"
        );
    }

    #[test]
    fn text_parses_as_python_int_does() {
        assert_eq!(to_int(&json!(" 12 ")), Some(12));
        assert_eq!(to_int(&json!("1_000")), Some(1000));
        assert_eq!(to_int(&json!("-3")), Some(-3));
        assert_eq!(to_int(&json!("5.0")), None);
        assert_eq!(to_int(&json!("--3")), None);
        assert_eq!(to_int(&json!(5.9)), Some(5));
        assert_eq!(to_int(&json!(true)), Some(1));
        assert_eq!(to_int(&json!(null)), None);
    }

    #[test]
    fn text_parses_as_python_float_does() {
        assert_eq!(parse_float(" 12.5 "), Some(12.5));
        assert_eq!(parse_float("1_000"), Some(1000.0));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("{{x}}"), None);
        assert_eq!(parse_float(""), None);
        assert!(parse_float("Infinity").is_some_and(f64::is_infinite));
        assert_eq!(parse_float("1e3"), Some(1000.0));
    }
}
