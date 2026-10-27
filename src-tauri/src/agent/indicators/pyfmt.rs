//! Python's renderings of a value, for the sentences the web builds with
//! f-strings (`got {value!r}`, `got {multiplier}`) and for `round(x, 6)`.
//! A sentence that reads `got 1e-05` on the web reads the same here.

use serde_json::Value;

/// `repr(float)`: the shortest text that reads back as the same float, in
/// fixed notation from 1e-4 up to 1e16 and in exponent notation (`1e-05`,
/// `1.5e+16`) outside it, with `.0` on a whole number.
pub(crate) fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    let scientific = format!("{:e}", x);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let sign = if mantissa.starts_with('-') { "-" } else { "" };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent + 1;
    if point > -4 && point <= 16 {
        let body = if point <= 0 {
            format!("0.{}{}", "0".repeat((-point) as usize), digits)
        } else if point as usize >= digits.len() {
            format!("{}{}.0", digits, "0".repeat(point as usize - digits.len()))
        } else {
            let (whole, fraction) = digits.split_at(point as usize);
            format!("{}.{}", whole, fraction)
        };
        return format!("{}{}", sign, body);
    }
    let (lead, rest) = digits.split_at(1);
    let mantissa = if rest.is_empty() {
        lead.to_string()
    } else {
        format!("{}.{}", lead, rest)
    };
    let shown = point - 1;
    format!(
        "{}{}e{}{:02}",
        sign,
        mantissa,
        if shown < 0 { '-' } else { '+' },
        shown.abs()
    )
}

/// `round(x, ndigits)` for a float: correctly rounded, ties to even, as
/// CPython does it through its decimal conversion.
pub(crate) fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", ndigits, x).parse().unwrap_or(x)
}

/// `repr(str)`: single quotes unless the text holds one and no double quote.
fn py_str_repr(s: &str) -> String {
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
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn py_number(n: &serde_json::Number) -> String {
    if n.is_i64() || n.is_u64() {
        n.to_string()
    } else {
        py_float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `repr()` of a JSON-decoded value: `'abc'`, `14`, `14.0`, `True`, `None`,
/// `[14]`, `{'a': 1}`.
pub(crate) fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => py_number(n),
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str()` of a JSON-decoded value: text as is, anything else as `repr()`.
pub(crate) fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (2.0, "2.0"),
            (-1.0, "-1.0"),
            (14.5, "14.5"),
            (0.1, "0.1"),
            (1e-4, "0.0001"),
            (1e-5, "1e-05"),
            (1.5e-7, "1.5e-07"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (1.2345678901234568e17, "1.2345678901234568e+17"),
            (123.456, "123.456"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ];
        for (value, text) in cases {
            assert_eq!(py_float_repr(value), text, "{value}");
        }
    }

    #[test]
    fn repr_of_json_values() {
        assert_eq!(py_repr(&json!("abc")), "'abc'");
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!(14)), "14");
        assert_eq!(py_repr(&json!(14.0)), "14.0");
        assert_eq!(py_repr(&json!([14])), "[14]");
        assert_eq!(py_repr(&json!({"a": 1})), "{'a': 1}");
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!(true)), "True");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!("SMA")), "SMA");
    }
}
