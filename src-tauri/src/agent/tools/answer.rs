//! How a read tool's answer leaves for the model: labelled as data, and
//! with anything secret-shaped removed.
//!
//! - [`json_result`]: a JSON payload (the web's `self._wrapped(tool,
//!   payload, **labels)`): redacted, serialised within the 12,000 character
//!   cap, wrapped in a `<tool_result tool=...>` block;
//!   [`json_result_ordered`] the same in the payload's own key order.
//! - [`tool_answer`]: the one or two lines a rendering tool returns while
//!   its chart or card travels on the viz side channel (the web's
//!   `viz.tool_answer`), notices appended as one trailing sentence.
//!
//! Redaction runs over every result (decision D-008: the web redacts only
//! the order tools' results), so a credential a broker echoes back in a
//! payload never reaches a third-party model provider.

use crate::agent::contract::json::dumps_capped;
use crate::agent::contract::tool::{ToolOutcome, MAX_RESULT_CHARS};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::redact::{redact, redact_text};
use serde::Serialize;
use serde_json::Value;

/// A tag attribute: its name and its value, or `None` to leave it out (the
/// web drops a label whose value is `None`).
pub type Label<'a> = (&'a str, Option<String>);

fn borrowed<'a>(labels: &'a [Label<'a>]) -> Vec<(&'a str, Option<&'a str>)> {
    labels.iter().map(|(k, v)| (*k, v.as_deref())).collect()
}

/// A JSON result, redacted, capped and labelled as data.
pub fn json_result(tool: &str, payload: &Value, labels: &[Label<'_>]) -> ToolOutcome {
    let text = dumps_capped(&redact(payload), MAX_RESULT_CHARS);
    ToolOutcome::Ok(wrap_tool_result(tool, &text, &borrowed(labels)))
}

/// [`json_result`] keeping the payload's own key order (a struct, or an
/// ordered map), so the model reads the keys in the web's order. When
/// redaction would change anything the redacted, sorted [`json_result`]
/// answers instead, so an ordered answer never skips it.
pub fn json_result_ordered<T: Serialize + ?Sized>(
    tool: &str,
    payload: &T,
    labels: &[Label<'_>],
) -> ToolOutcome {
    let value = serde_json::to_value(payload).unwrap_or(Value::Null);
    if redact(&value) != value {
        return json_result(tool, &value, labels);
    }
    let text = ToolOutcome::json_ordered(payload);
    ToolOutcome::Ok(wrap_tool_result(tool, text.text(), &borrowed(labels)))
}

/// A rendering tool's confirmation, with its notices appended as
/// `" Note: ..."`, labelled as data.
pub fn tool_answer(
    tool: &str,
    message: &str,
    notices: &[String],
    labels: &[Label<'_>],
) -> ToolOutcome {
    let mut text = message.to_string();
    if !notices.is_empty() {
        text = format!("{} Note: {}", text, notices.join(" "));
    }
    ToolOutcome::Ok(wrap_tool_result(
        tool,
        &redact_text(&text),
        &borrowed(labels),
    ))
}

/// A label from anything printable.
pub fn label<'a>(name: &'a str, value: &dyn std::fmt::Display) -> Label<'a> {
    (name, Some(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_result_is_labelled_data_with_secrets_removed() {
        let out = json_result(
            "get_quote",
            &json!({"symbol": "INFY", "quote": {"ltp": 1520.5}, "auth_token": "abc"}),
            &[label("symbol", &"INFY"), ("exchange", None)],
        );
        assert!(out.is_ok());
        let text = out.text();
        let (head, rest) = text.split_once('\n').unwrap_or_default();
        assert_eq!(head, "<tool_result tool=\"get_quote\" symbol=\"INFY\">");
        let body = rest.strip_suffix("\n</tool_result>").unwrap_or_default();
        let parsed: Value = serde_json::from_str(body).unwrap_or_default();
        assert_eq!(
            parsed,
            json!({"symbol": "INFY", "quote": {"ltp": 1520.5}, "auth_token": "[redacted]"})
        );
    }

    /// Every result is redacted, and redaction removes any run of 32 or
    /// more hex characters. That is safe for the books only because no
    /// desktop broker uses an order id of 32 or more continuous hex
    /// characters (numeric ids, or UUIDs, which carry hyphens): an order id
    /// must survive, or the trader could not be told which order is which.
    #[test]
    fn order_ids_survive_redaction() {
        let ids = [
            "250408000989443",
            "240108010918222",
            "GMK39038RDT490CCVRO",
            "3fa85f64-5717-4562-b3fc-2c963f66afa6",
        ];
        for id in ids {
            let out = json_result("get_order_status", &json!({"order_id": id}), &[]);
            assert!(out.text().contains(id), "{}", out.text());
        }
    }

    #[test]
    fn an_answer_carries_its_notices_as_one_sentence() {
        let out = tool_answer(
            "stream_quotes",
            "Opened a live Quote card on 1 instrument.",
            &["A.".into(), "B.".into()],
            &[label("count", &1), ("mode", None)],
        );
        assert_eq!(
            out.text(),
            "<tool_result tool=\"stream_quotes\" count=\"1\">\nOpened a live Quote card on 1 instrument. Note: A. B.\n</tool_result>"
        );
        assert!(tool_answer("x", "plain", &[], &[])
            .text()
            .contains("\nplain\n"));
    }

    #[test]
    fn a_large_result_stays_within_the_cap() {
        let rows: Vec<Value> = (0..1500)
            .map(|i| json!({"i": i, "pad": "x".repeat(100)}))
            .collect();
        let out = json_result("get_orderbook", &json!({"orders": rows}), &[]);
        let body = out
            .text()
            .trim_start_matches("<tool_result tool=\"get_orderbook\">\n")
            .trim_end_matches("\n</tool_result>");
        assert!(body.chars().count() <= MAX_RESULT_CHARS);
        assert!(body.contains("\"truncated\":true"));
    }
}
