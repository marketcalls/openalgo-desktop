//! How Python writes a JSON value into a message (`str()` and `repr()`),
//! for the settings errors the web quotes the submitted value in:
//! `max_orders_per_session must be a whole number, got 'ten'`.

use serde_json::Value;

/// Python's `str(value)` for a value decoded from JSON.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr(value)` for a value decoded from JSON.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) => py_float(f),
            _ => n.to_string(),
        },
        Value::String(s) => repr_str(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Python's `repr(str)`: single quotes unless the text holds a single quote
/// and no double quote; backslash, quote and control characters escaped.
pub fn repr_str(s: &str) -> String {
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if is_unprintable(c) => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{:02x}", n));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", n));
                } else {
                    out.push_str(&format!("\\U{:08x}", n));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Characters Python's `repr` escapes beyond ASCII controls: the C1
/// controls, the line and paragraph separators and the other format
/// characters a reader cannot see.
fn is_unprintable(c: char) -> bool {
    matches!(c as u32, 0x80..=0xa0 | 0xad | 0x2028 | 0x2029 | 0x200b..=0x200f | 0xfeff)
}

/// Python's `repr(float)`: the shortest digits that round-trip, fixed
/// notation for exponents from -4 to 15 (with `.0` for a whole number),
/// scientific otherwise (`1e+16`, `1e-05`).
pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-inf" } else { "inf" }.into();
    }
    let sci = format!("{:e}", f);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if negative { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            let (a, b) = digits.split_at(point as usize);
            format!("{}.{}", a, b)
        };
        format!("{}{}", sign, body)
    } else {
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() {
            String::new()
        } else {
            format!(".{}", rest)
        };
        format!(
            "{}{}{}e{}{:02}",
            sign,
            first,
            frac,
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_values_as_python_does() {
        for (v, shown) in [
            (json!("ten"), "'ten'"),
            (json!("it's"), "\"it's\""),
            (json!("a\nb"), "'a\\nb'"),
            (json!(5), "5"),
            (json!(5.5), "5.5"),
            (json!(5.0), "5.0"),
            (json!(1e16), "1e+16"),
            (json!(0.00001), "1e-05"),
            (json!(0.0001), "0.0001"),
            (json!(-2.5), "-2.5"),
            (json!(true), "True"),
            (json!(null), "None"),
            (json!(["NSE", 1]), "['NSE', 1]"),
            (json!({"a": false}), "{'a': False}"),
        ] {
            assert_eq!(py_repr(&v), shown, "{}", v);
        }
        assert_eq!(py_str(&json!("NSE")), "NSE");
        assert_eq!(py_str(&json!(true)), "True");
    }
}
