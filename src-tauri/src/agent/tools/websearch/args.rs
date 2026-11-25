//! The model's arguments, checked as the web checks them
//! (`_validated_max_results`, `_validated_topic`, `_validated_fetch_chars`,
//! `_validated_offset`, `_constrained`).

use crate::agent::contract::tool::ToolOutcome;
use crate::agent::settings::pyrepr::repr_str;
use crate::agent::websearch::taint::{self, Constrained};
use serde_json::{Map, Value};

const DEFAULT_MAX_RESULTS: i64 = 5;
const MIN_MAX_RESULTS: i64 = 1;
const MAX_MAX_RESULTS: i64 = 10;

/// The count the model asked for, clamped to 1..10 with a notice; a value
/// that is not a whole number is refused (the web's
/// `_validated_max_results`).
pub(super) fn max_results(args: &Map<String, Value>) -> Result<(i64, Option<String>), ToolOutcome> {
    let fix = format!(
        "Pass an integer from {} to {}, for example {}.",
        MIN_MAX_RESULTS, MAX_MAX_RESULTS, DEFAULT_MAX_RESULTS
    );
    let refuse = || {
        ToolOutcome::invalid_argument(
            "max_results",
            "it is not a whole number.",
            Some(fix.as_str()),
        )
    };
    let asked = match args.get("max_results") {
        None => DEFAULT_MAX_RESULTS,
        Some(Value::Number(n)) => n.as_i64().ok_or_else(refuse)?,
        Some(Value::String(s)) => s.trim().parse::<i64>().map_err(|_| refuse())?,
        Some(_) => return Err(refuse()),
    };
    let clamped = asked.clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS);
    let notice = (clamped != asked).then(|| {
        format!(
            "{} results were asked for; searches return {} to {}, so {} were requested.",
            asked, MIN_MAX_RESULTS, MAX_MAX_RESULTS, clamped
        )
    });
    Ok((clamped, notice))
}

/// The taint boundary on one argument, with words from the turn's message
/// and the trader's earlier ones; an empty argument is refused.
pub(super) fn constrained(
    args: &Map<String, Value>,
    field: &str,
    operator: &str,
    earlier: &str,
) -> Result<Constrained, ToolOutcome> {
    let text = match args.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(v) if crate::agent::models::truthy(v) => crate::agent::settings::pyrepr::py_str(v),
        _ => String::new(),
    };
    if text.trim().is_empty() {
        return Err(ToolOutcome::invalid_argument(
            field,
            "it is empty.",
            Some("Pass the operator's own question as plain text."),
        ));
    }
    Ok(taint::constrain_with_history(&text, operator, earlier))
}

/// `web_search` topics (`TOPICS`): `fundamentals` looks on the trader's
/// preferred sources first and only then on the open web.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Topic {
    General,
    Fundamentals,
}

/// The topic the model asked for; an empty one is `general`, anything
/// else is refused naming the two (`_validated_topic`).
pub(super) fn topic(args: &Map<String, Value>) -> Result<Topic, ToolOutcome> {
    let name = match args.get("topic") {
        Some(v) if crate::agent::models::truthy(v) => crate::agent::settings::pyrepr::py_str(v)
            .trim()
            .to_lowercase(),
        _ => "general".to_string(),
    };
    match name.as_str() {
        "general" => Ok(Topic::General),
        "fundamentals" => Ok(Topic::Fundamentals),
        _ => Err(ToolOutcome::invalid_argument(
            "topic",
            &format!("{} is not a topic.", repr_str(&name)),
            Some("Pass 'general', or 'fundamentals' for a company's figures."),
        )),
    }
}

/// How much page text one `web_fetch` returns (`DEFAULT_FETCH_CHARS` and
/// its bounds): a data page runs to tens of thousands of characters, so a
/// call returns a slice and says where the next starts.
const DEFAULT_FETCH_CHARS: i64 = 10_000;
const MIN_FETCH_CHARS: i64 = 1_000;
const MAX_FETCH_CHARS: i64 = 30_000;

/// A whole number as Python's `int(str(value).strip())` reads one; a
/// boolean is not one.
fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The slice size the model asked for, clamped with a notice like a
/// result count (`_validated_fetch_chars`).
pub(super) fn fetch_chars(
    args: &Map<String, Value>,
) -> Result<(usize, Option<String>), ToolOutcome> {
    let fix = format!(
        "Pass an integer from {} to {}, for example {}.",
        MIN_FETCH_CHARS, MAX_FETCH_CHARS, DEFAULT_FETCH_CHARS
    );
    let asked = match args.get("max_chars") {
        None => DEFAULT_FETCH_CHARS,
        Some(v) => whole(v).ok_or_else(|| {
            ToolOutcome::invalid_argument(
                "max_chars",
                "it is not a whole number.",
                Some(fix.as_str()),
            )
        })?,
    };
    let clamped = asked.clamp(MIN_FETCH_CHARS, MAX_FETCH_CHARS);
    let notice = (clamped != asked).then(|| {
        format!(
            "{} characters were asked for; a page slice is {} to {}, so {} were returned.",
            asked, MIN_FETCH_CHARS, MAX_FETCH_CHARS, clamped
        )
    });
    Ok((clamped as usize, notice))
}

/// Where in the page to start reading, never below the top
/// (`_validated_offset`).
pub(super) fn offset(args: &Map<String, Value>) -> Result<usize, ToolOutcome> {
    let asked = match args.get("offset") {
        None => 0,
        Some(v) => whole(v).ok_or_else(|| {
            ToolOutcome::invalid_argument(
                "offset",
                "it is not a whole number.",
                Some("Pass 0 for the top of the page, or the next_offset a previous call gave."),
            )
        })?,
    };
    Ok(usize::try_from(asked.max(0)).unwrap_or(usize::MAX))
}
