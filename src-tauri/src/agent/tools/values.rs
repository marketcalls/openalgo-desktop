//! A model's argument read the way the web's Python reads it.
//!
//! The web's normalisers take `Any` and spell their refusals with Python's
//! `str()`, `repr()` and type names (`'ten' is not a whole number`, `entry 2
//! is int, not an object`). The desktop's arguments are JSON values, so
//! these give the same text for the same value.

use regex::{Captures, Regex};
use serde_json::Value;
use std::borrow::Cow;
use std::sync::OnceLock;

/// A regular expression compiled on first use. The patterns are literals,
/// so one that does not compile is a programming error the tests catch; it
/// then matches nothing rather than panicking.
pub struct Pattern {
    source: &'static str,
    cell: OnceLock<Option<Regex>>,
}

impl Pattern {
    pub const fn new(source: &'static str) -> Pattern {
        Pattern {
            source,
            cell: OnceLock::new(),
        }
    }

    fn get(&self) -> Option<&Regex> {
        self.cell
            .get_or_init(|| match Regex::new(self.source) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::error!("Agent tool pattern {:?} is invalid: {}", self.source, e);
                    None
                }
            })
            .as_ref()
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.get().is_some_and(|r| r.is_match(text))
    }

    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        self.get()?.captures(text)
    }

    /// The first match's text.
    pub fn find<'t>(&self, text: &'t str) -> Option<&'t str> {
        Some(self.get()?.find(text)?.as_str())
    }

    pub fn replace_all<'t>(&self, text: &'t str, with: &str) -> Cow<'t, str> {
        match self.get() {
            Some(r) => r.replace_all(text, with),
            None => Cow::Borrowed(text),
        }
    }

    pub fn split<'t>(&self, text: &'t str) -> Vec<&'t str> {
        match self.get() {
            Some(r) => r.split(text).collect(),
            None => vec![text],
        }
    }
}

/// Python's `str(value)`.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(_) => number_text(value),
        other => py_repr(other),
    }
}

/// Python's `repr(value)`.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Number(_) => number_text(value),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", quote(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::String(s) => quote(s),
        other => crate::agent::tools::service::py_repr(other),
    }
}

/// Python's `repr` of a string.
pub fn quote(text: &str) -> String {
    crate::agent::tools::service::py_repr(&Value::String(text.to_string()))
}

/// `"" if value is None else str(value).strip()`.
pub fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(v) => py_str(v).trim().to_string(),
    }
}

/// `str(value or "").strip()`: every falsy value reads as empty.
pub fn truthy_text(value: Option<&Value>) -> String {
    match value {
        Some(v) if truthy(v) => py_str(v).trim().to_string(),
        _ => String::new(),
    }
}

/// Python's truth value of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's type name of a JSON value (`type(value).__name__`).
pub fn type_name(value: &Value) -> &'static str {
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

/// A JSON number as Python prints it: an integer plainly, a float as its
/// shortest repr (`5.0`, `0.0001`, `1e-05`, `1e+16`).
fn number_text(value: &Value) -> String {
    let Value::Number(n) = value else {
        return String::new();
    };
    if !n.is_f64() {
        return n.to_string();
    }
    let x = n.as_f64().unwrap_or(0.0);
    float_repr(x)
}

/// Python's `repr(float)`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // Shortest round-trip digits and the decimal exponent.
    let sci = format!("{:e}", x);
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
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
        return format!("{}{}", sign, body);
    }
    let (first, rest) = digits.split_at(1);
    let mantissa = if rest.is_empty() {
        first.to_string()
    } else {
        format!("{}.{}", first, rest)
    };
    let esign = if exp < 0 { "-" } else { "+" };
    format!("{}{}e{}{:02}", sign, mantissa, esign, exp.abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_print_as_python_prints_them() {
        assert_eq!(py_repr(&json!("ten")), "'ten'");
        assert_eq!(py_repr(&json!(30)), "30");
        assert_eq!(py_repr(&json!(5.5)), "5.5");
        assert_eq!(py_repr(&json!(null)), "None");
        assert_eq!(py_repr(&json!(true)), "True");
        assert_eq!(py_repr(&json!({"a": [1, "b"]})), "{'a': [1, 'b']}");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(7)), "7");
        for (x, s) in [
            (5.0, "5.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (123456.789, "123456.789"),
            (1.5e-7, "1.5e-07"),
            (-2.5, "-2.5"),
            (1e15, "1000000000000000.0"),
        ] {
            assert_eq!(float_repr(x), s, "{}", x);
        }
    }

    #[test]
    fn empty_readings_follow_none_and_falsiness() {
        assert_eq!(text(None), "");
        assert_eq!(text(Some(&json!(null))), "");
        assert_eq!(text(Some(&json!(0))), "0");
        assert_eq!(truthy_text(Some(&json!(0))), "");
        assert_eq!(truthy_text(Some(&json!(" NSE "))), "NSE");
        assert_eq!(type_name(&json!(1)), "int");
        assert_eq!(type_name(&json!(1.0)), "float");
        assert_eq!(type_name(&json!([])), "list");
    }
}
