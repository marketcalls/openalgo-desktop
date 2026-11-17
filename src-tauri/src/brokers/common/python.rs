//! Python's value rules that the web's broker adapters lean on, so a port
//! reads a broker answer the way its web adapter does.
//!
//! The adapters ported before this module keep their own copies
//! (`samco::mapping::truthy`, several `py_float`s); new code uses these.

use serde_json::Value;

/// Python's truth of a JSON value (`if value:`, `a or b`): `null`, `false`,
/// `0`, `""`, `[]` and `{}` are false; a text `"0"` is true.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A float as Python's `str()` writes it: `100.0`, `1503.45`.
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truth_and_float_text_follow_python() {
        for falsy in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(Some(&falsy)), "{falsy}");
        }
        assert!(!truthy(None));
        for set in [
            json!(true),
            json!(1),
            json!(-0.5),
            json!("0"),
            json!(" "),
            json!([0]),
            json!({"a": 0}),
        ] {
            assert!(truthy(Some(&set)), "{set}");
        }
        assert_eq!(py_float(100.0), "100.0");
        assert_eq!(py_float(1503.45), "1503.45");
        assert_eq!(py_float(-0.05), "-0.05");
    }
}
