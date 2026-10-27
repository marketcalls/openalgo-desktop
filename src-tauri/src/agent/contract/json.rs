//! Tool results as the model reads them (the web's `json_safe`,
//! `dumps_capped` and `strip_code_fence` in `tools/base.py`).
//!
//! A result is compact JSON of at most a budget of characters. One that
//! would not fit comes back as a well-formed envelope carrying the start of
//! the text as a JSON string, never JSON cut off mid-value:
//! `{"ok":true,"truncated":true,"dropped_chars":N,"partial":"...","note":"..."}`.

use serde_json::Value;

/// Lists longer than this are cut while serialising, with a marker.
pub const MAX_SEQUENCE_ITEMS: usize = 2000;

/// Nesting deeper than this is replaced by a marker.
pub const MAX_JSON_DEPTH: usize = 16;

/// What the model is told when a result was cut (the web's
/// `_TRUNCATION_NOTE`).
pub const TRUNCATION_NOTE: &str = "The result was too large to return in full. 'partial' holds the beginning of the JSON and may stop mid-value. Narrow the request (fewer symbols, a shorter date range, a filter) and call the tool again.";

/// Bound a value's lists and depth the way the web's `json_safe` does.
pub fn json_safe(value: &Value) -> Value {
    bounded(value, 0)
}

fn bounded(value: &Value, depth: usize) -> Value {
    match value {
        Value::Array(_) | Value::Object(_) if depth >= MAX_JSON_DEPTH => {
            let kind = if value.is_array() { "list" } else { "dict" };
            Value::String(format!("<max depth {} exceeded: {}>", MAX_JSON_DEPTH, kind))
        }
        Value::Array(items) => {
            let mut out: Vec<Value> = items
                .iter()
                .take(MAX_SEQUENCE_ITEMS)
                .map(|v| bounded(v, depth + 1))
                .collect();
            if items.len() > MAX_SEQUENCE_ITEMS {
                out.push(Value::String(format!(
                    "... {} more items omitted",
                    items.len() - MAX_SEQUENCE_ITEMS
                )));
            }
            Value::Array(out)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), bounded(v, depth + 1)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn envelope(partial: &str, dropped_chars: usize) -> String {
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    format!(
        "{{\"ok\":true,\"truncated\":true,\"dropped_chars\":{},\"partial\":{},\"note\":{}}}",
        dropped_chars,
        quote(partial),
        quote(TRUNCATION_NOTE)
    )
}

/// Compact JSON of `value` in at most `limit` characters, or the truncation
/// envelope with the longest prefix that still fits.
pub fn dumps_capped(value: &Value, limit: usize) -> String {
    let text = serde_json::to_string(&json_safe(value)).unwrap_or_else(|_| "null".into());
    let total = text.chars().count();
    if total <= limit {
        return text;
    }
    // Byte offset of each character boundary, so a prefix of `n`
    // characters is `&text[..bounds[n]]`.
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    // The envelope's length moves with both the prefix and the digits of
    // the dropped count, so each candidate is measured, not estimated.
    let (mut low, mut high) = (0usize, total);
    let mut best = envelope("", total);
    while low <= high {
        let middle = (low + high) / 2;
        let candidate = envelope(&text[..bounds[middle]], total - middle);
        if candidate.chars().count() <= limit {
            best = candidate;
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    best
}

/// Remove a Markdown code fence the model wrapped around a whole-document
/// argument (the web's `strip_code_fence`).
pub fn strip_code_fence(text: &str) -> String {
    let stripped = text.trim();
    if !stripped.starts_with("```") {
        return stripped.to_string();
    }
    let lines: Vec<&str> = stripped.lines().collect();
    if lines.len() < 2 {
        return stripped.to_string();
    }
    let mut body = &lines[1..];
    if body.last().is_some_and(|l| l.trim().starts_with("```")) {
        body = &body[..body.len() - 1];
    }
    body.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_small_result_is_compact_json_in_its_own_order() {
        let v = json!({"symbol": "INFY", "ltp": 1520.5, "a": [1, null]});
        assert_eq!(
            dumps_capped(&v, 12_000),
            r#"{"symbol":"INFY","ltp":1520.5,"a":[1,null]}"#
        );
    }

    #[test]
    fn a_large_result_becomes_a_well_formed_envelope_within_the_cap() {
        let rows: Vec<Value> = (0..3000)
            .map(|i| json!({"i": i, "s": "\u{20b9}x"}))
            .collect();
        let text = dumps_capped(&Value::Array(rows), 12_000);
        assert!(text.chars().count() <= 12_000);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["note"], TRUNCATION_NOTE);
        let partial = v["partial"].as_str().unwrap();
        let dropped = v["dropped_chars"].as_u64().unwrap() as usize;
        // The kept prefix and the dropped count add up to the whole text,
        // which was itself cut at 2000 items with the web's marker.
        let whole = serde_json::to_string(&json_safe(&Value::Array(
            (0..3000)
                .map(|i| json!({"i": i, "s": "\u{20b9}x"}))
                .collect(),
        )))
        .unwrap();
        assert!(whole.ends_with(r#""... 1000 more items omitted"]"#));
        assert_eq!(partial.chars().count() + dropped, whole.chars().count());
        // The longest prefix that fits: one more character would not.
        let one_more: String = whole.chars().take(partial.chars().count() + 1).collect();
        assert!(envelope(&one_more, dropped - 1).chars().count() > 12_000);
    }

    #[test]
    fn deep_nesting_is_cut_with_a_marker() {
        let mut v = json!(1);
        for _ in 0..20 {
            v = json!([v]);
        }
        let text = serde_json::to_string(&json_safe(&v)).unwrap();
        assert!(text.contains("<max depth 16 exceeded: list>"), "{}", text);
    }

    #[test]
    fn a_fence_is_unwrapped() {
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("  plain  "), "plain");
        assert_eq!(strip_code_fence("```"), "```");
    }
}
