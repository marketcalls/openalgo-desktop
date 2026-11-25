//! The model's arguments, checked as the web checks them
//! (`_validated_max_results`, `_constrained`).

use crate::agent::contract::tool::ToolOutcome;
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

/// The taint boundary on one argument; an empty one is refused.
pub(super) fn constrained(
    args: &Map<String, Value>,
    field: &str,
    operator: &str,
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
    Ok(taint::constrain(&text, operator))
}
