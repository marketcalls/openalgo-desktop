//! Exact comparison of a frame with the case's expected frame: the same
//! keys at every level (a field the expected frame leaves out must be
//! absent), the same values with the same JSON types (`2950.0` is not
//! `2950`), arrays of the same length in the same order.
//!
//! The one value a case cannot know is a time the desktop stamps when it
//! receives a packet. The expected frame writes it as `"<receipt_ms>"`,
//! which matches an integer epoch-millisecond time between the push that
//! made the frame and its arrival at the client: a receipt time, so never
//! 0, a trade time from the packet or a time in seconds.

use serde_json::Value;

/// The placeholder for a time stamped on receipt.
pub(crate) const RECEIPT_MS: &str = "<receipt_ms>";

/// When a frame can have been stamped: from just before its packet was
/// pushed to just after the client read it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    pub(crate) from_ms: i64,
    pub(crate) to_ms: i64,
}

/// `Ok` when `got` is `want`; otherwise every difference, one per line,
/// and both frames.
pub(crate) fn exact(want: &Value, got: &Value, window: Option<Window>) -> Result<(), String> {
    let mut diffs = Vec::new();
    walk("", want, got, window, &mut diffs);
    if diffs.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\n  expected {}\n  received {}",
        diffs.join("\n"),
        want,
        got
    ))
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

fn walk(path: &str, want: &Value, got: &Value, window: Option<Window>, out: &mut Vec<String>) {
    let at = if path.is_empty() { "frame" } else { path };
    match (want, got) {
        (Value::String(p), _) if p == RECEIPT_MS => match (window, got.as_i64()) {
            (Some(w), Some(t)) if (w.from_ms..=w.to_ms).contains(&t) => {}
            (Some(w), Some(t)) => out.push(format!(
                "{}: {} is not a receipt time of this frame ({}..={})",
                at, t, w.from_ms, w.to_ms
            )),
            (Some(_), None) => out.push(format!(
                "{}: expected a receipt time in epoch ms, received {}",
                at, got
            )),
            (None, _) => out.push(format!(
                "{}: {} has no packet to be timed by",
                at, RECEIPT_MS
            )),
        },
        (Value::Object(w), Value::Object(g)) => {
            for (k, wv) in w {
                match g.get(k) {
                    Some(gv) => walk(&join(path, k), wv, gv, window, out),
                    None => out.push(format!("{}: missing (expected {})", join(path, k), wv)),
                }
            }
            for (k, gv) in g {
                if !w.contains_key(k) {
                    out.push(format!("{}: not expected (received {})", join(path, k), gv));
                }
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if w.len() != g.len() {
                out.push(format!(
                    "{}: {} items expected, {} received",
                    at,
                    w.len(),
                    g.len()
                ));
            }
            for (i, (wv, gv)) in w.iter().zip(g).enumerate() {
                walk(&format!("{}[{}]", path, i), wv, gv, window, out);
            }
        }
        _ if want == got => {}
        _ => out.push(format!("{}: expected {}, received {}", at, want, got)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const W: Option<Window> = Some(Window {
        from_ms: 1_000,
        to_ms: 2_000,
    });

    #[test]
    fn numbers_compare_by_value_and_type() {
        assert!(exact(&json!({"ltp": 2950.0}), &json!({"ltp": 2950.0}), W).is_ok());
        let e = exact(&json!({"ltp": 2950.0}), &json!({"ltp": 2950}), W).unwrap_err();
        assert!(e.contains("ltp: expected 2950.0, received 2950"), "{}", e);
        assert!(exact(&json!({"q": 7}), &json!({"q": 7.0}), W).is_err());
    }

    #[test]
    fn an_absent_field_must_be_absent_and_a_present_one_present() {
        let e = exact(&json!({"a": 1}), &json!({"a": 1, "close": 0.0}), W).unwrap_err();
        assert!(e.contains("close: not expected (received 0.0)"), "{}", e);
        let e = exact(&json!({"a": 1, "oi": 5}), &json!({"a": 1}), W).unwrap_err();
        assert!(e.contains("oi: missing (expected 5)"), "{}", e);
    }

    #[test]
    fn a_receipt_time_is_an_integer_inside_the_window() {
        let want = json!({"timestamp": RECEIPT_MS});
        assert!(exact(&want, &json!({"timestamp": 1_500}), W).is_ok());
        assert!(exact(&want, &json!({"timestamp": 0}), W).is_err());
        assert!(exact(&want, &json!({"timestamp": 2_001}), W).is_err());
        assert!(exact(&want, &json!({"timestamp": 1_500.0}), W).is_err());
        assert!(exact(&want, &json!({"timestamp": 1_500}), None).is_err());
    }

    #[test]
    fn books_compare_level_by_level() {
        let want = json!({"depth": {"buy": [{"price": 1.0}, {"price": 2.0}]}});
        let e = exact(&want, &json!({"depth": {"buy": [{"price": 1.0}]}}), W).unwrap_err();
        assert!(
            e.contains("depth.buy: 2 items expected, 1 received"),
            "{}",
            e
        );
        let e = exact(
            &want,
            &json!({"depth": {"buy": [{"price": 1.0}, {"price": 2.5}]}}),
            W,
        )
        .unwrap_err();
        assert!(
            e.contains("depth.buy[1].price: expected 2.0, received 2.5"),
            "{}",
            e
        );
    }
}
