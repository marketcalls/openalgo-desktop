//! Comparing what the desktop did with what the web recorded.
//!
//! - JSON values: equal shape; numbers of the same kind (an integer is not
//!   a float: Python prints `10` and `10.0` differently, and a value stored
//!   by one node is printed by the next), floats within the 1.0.4 indicator
//!   tolerance (`1e-6 + 1e-9 * |want|`).
//! - Wildcards a `desktop_expected` may use where the desktop's own words
//!   replace the web's: `{"$any": ..}` matches anything, `{"$prefix": p}` a
//!   string that starts with `p`.
//! - Log lines (lead decision 19): a Python dict or list written inside a
//!   line is compared as a value, dict keys in any order (the desktop keeps
//!   JSON maps in key order, the web in insertion order); everything else in
//!   the line byte for byte, including how the repr itself is spelled
//!   (quotes, `True`/`None`, `100.0`).

use serde_json::Value;

const ABS: f64 = 1e-6;
const REL: f64 = 1e-9;

fn close(got: f64, want: f64) -> bool {
    (got - want).abs() <= ABS + REL * want.abs()
}

/// Every difference between `want` and `got`, each prefixed with its path.
pub fn diff(want: &Value, got: &Value, path: &str, out: &mut Vec<String>) {
    if let Some(m) = want.as_object() {
        if m.contains_key("$any") {
            return;
        }
        if let Some(p) = m.get("$prefix").and_then(Value::as_str) {
            if !got.as_str().is_some_and(|g| g.starts_with(p)) {
                out.push(format!("{path}: want a text starting {p:?}, got {got}"));
            }
            return;
        }
    }
    match (want, got) {
        (Value::Number(w), Value::Number(g)) => {
            if w.is_f64() != g.is_f64() {
                out.push(format!(
                    "{path}: want {w}, got {g} (integer and float differ)"
                ));
            } else if let (Some(a), Some(b)) = (w.as_f64(), g.as_f64()) {
                if !close(b, a) {
                    out.push(format!("{path}: want {w}, got {g}"));
                }
            }
        }
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => diff(wv, gv, &format!("{path}.{k}"), out),
                    None => out.push(format!("{path}.{k}: missing (want {wv})")),
                }
            }
            for k in g.keys().filter(|k| !w.contains_key(*k)) {
                out.push(format!("{path}.{k}: not expected (got {})", g[k]));
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                out.push(format!("{path}: want {} items, got {}", w.len(), g.len()));
            }
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                diff(wv, gv, &format!("{path}[{i}]"), out);
            }
        }
        _ if want == got => {}
        _ => out.push(format!("{path}: want {want}, got {got}")),
    }
}

/// The differences between two run logs (`[[level, message], ...]`).
pub fn diff_logs(want: &Value, got: &[(String, String)], out: &mut Vec<String>) {
    if want.get("$any").is_some() {
        return;
    }
    let want = want.as_array().cloned().unwrap_or_default();
    for (i, w) in want.iter().enumerate() {
        let level = w.get(0).cloned().unwrap_or(Value::Null);
        let message = w.get(1).cloned().unwrap_or(Value::Null);
        let Some((gl, gm)) = got.get(i) else {
            out.push(format!("logs[{i}]: missing {level} {message}"));
            continue;
        };
        if level.as_str() != Some(gl.as_str()) {
            out.push(format!("logs[{i}].level: want {level}, got {gl:?} ({gm})"));
        }
        match message.as_str() {
            Some(m) if !same_line(m, gm) => {
                out.push(format!("logs[{i}]:\n      want {m}\n      got  {gm}"));
            }
            Some(_) => {}
            None => diff(
                &message,
                &Value::String(gm.clone()),
                &format!("logs[{i}]"),
                out,
            ),
        }
    }
    for (i, (gl, gm)) in got.iter().enumerate().skip(want.len()) {
        out.push(format!("logs[{i}]: not expected {gl} {gm}"));
    }
}

/// One log line: the text byte for byte, a Python dict/list repr as a value.
pub fn same_line(want: &str, got: &str) -> bool {
    if want == got {
        return true;
    }
    let (w, g) = (super::pyrepr::split(want), super::pyrepr::split(got));
    w.len() == g.len()
        && w.iter()
            .zip(&g)
            .all(|(a, b)| super::pyrepr::same_piece(a, b, close))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn d(w: Value, g: Value) -> Vec<String> {
        let mut out = Vec::new();
        diff(&w, &g, "$", &mut out);
        out
    }

    #[test]
    fn numbers_keep_their_kind_and_floats_their_tolerance() {
        assert!(d(json!(1.0), json!(1.0000000001)).is_empty());
        assert!(!d(json!(1.0), json!(1.001)).is_empty());
        assert!(!d(json!(10), json!(10.0)).is_empty());
        assert!(d(json!({"a": [1, "x"]}), json!({"a": [1, "x"]})).is_empty());
        assert!(!d(json!({"a": 1}), json!({"a": 1, "b": 2})).is_empty());
        assert!(d(json!({"$any": "string"}), json!(42)).is_empty());
        assert!(d(json!({"$prefix": "Invalid"}), json!("Invalid x")).is_empty());
        assert!(!d(json!({"$prefix": "Invalid"}), json!("x")).is_empty());
    }

    #[test]
    fn a_log_line_compares_its_dict_as_a_value() {
        assert!(same_line(
            "Order result: {'status': 'success', 'orderid': '1'}",
            "Order result: {'orderid': '1', 'status': 'success'}"
        ));
        assert!(!same_line(
            "Order result: {'status': 'success', 'qty': 10}",
            "Order result: {'qty': 10.0, 'status': 'success'}"
        ));
        assert!(!same_line(
            "AND Gate: [True, False] -> False",
            "AND Gate: [False, True] -> False"
        ));
        assert!(!same_line("x: {'a': True}", "x: {\"a\": true}"));
        assert!(same_line(
            "refused {{webhook.qty}} in {'k': [1, {'b': None, 'a': 'x'}]}",
            "refused {{webhook.qty}} in {'k': [1, {'a': 'x', 'b': None}]}"
        ));
    }
}
