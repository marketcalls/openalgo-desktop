//! The chart's own indicators (the web's `ChartToolkit.list_chart_indicators`,
//! `add_chart_indicator`, `remove_chart_indicator`, `_indicator_id` and
//! `_settings`).
//!
//! These are the indicators the chart can draw (`chart_catalogue`), not
//! the ones the indicator tools compute. An id this side has never heard of
//! is still sent: the trader's own indicator modules load in the browser,
//! and the chart ignores an id it does not know.

use super::{answer, no_chart, ChartRun, NO_SINK};
use crate::agent::chart_catalogue;
use crate::agent::chart_contract::{Command, IndicatorAction};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::tools::values::{py_repr, py_str, truthy};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// A chart indicator id: kebab-case, as the chart's registry spells them.
static INDICATOR_ID: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\A[a-z][a-z0-9-]{1,31}\z").ok());

/// The ids the chart can draw, grouped by category (`list_chart_indicators`).
pub(super) fn list_chart_indicators(args: &Map<String, Value>) -> ToolOutcome {
    let rows = chart_catalogue::catalogue();
    if rows.is_empty() {
        return answer(
            "list_chart_indicators",
            "The chart indicator catalogue could not be read on this install, so use add_chart_indicator with the name the operator gave and let the chart accept or ignore it.",
            None,
        );
    }
    let query = args
        .get("query")
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default();
    let needle = query.trim().to_lowercase();
    let matching: Vec<_> = rows
        .iter()
        .filter(|r| {
            needle.is_empty() || r.id.contains(&needle) || r.name.to_lowercase().contains(&needle)
        })
        .collect();
    if matching.is_empty() {
        return answer(
            "list_chart_indicators",
            &format!(
                "No chart indicator matches '{}'. Try a shorter query, or list them all.",
                query
            ),
            None,
        );
    }
    let mut by_category: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for r in &matching {
        by_category
            .entry(r.category.as_str())
            .or_default()
            .push(format!("{} ({})", r.id, r.name));
    }
    let parts: Vec<String> = by_category
        .iter()
        .map(|(category, items)| format!("{}: {}", category, items.join(", ")))
        .collect();
    answer(
        "list_chart_indicators",
        &format!(
            "{} chart indicator(s). {}",
            matching.len(),
            parts.join(" | ")
        ),
        None,
    )
}

/// An indicator name spelled as a chart id (`_indicator_id`): lower case,
/// spaces and underscores to hyphens. A bare number after a space is a
/// setting (`EMA 20`), so that name is left as it is and refused.
fn indicator_id(name: Option<&Value>) -> String {
    let text = name
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let has_period = text
        .split_whitespace()
        .skip(1)
        .any(|w| w.chars().all(|c| c.is_ascii_digit()));
    if has_period {
        return text;
    }
    let mut id = String::with_capacity(text.len());
    let mut in_gap = false;
    for c in text.chars() {
        if c.is_whitespace() || c == '_' {
            if !in_gap {
                id.push('-');
            }
            in_gap = true;
        } else {
            id.push(c);
            in_gap = false;
        }
    }
    id
}

/// The id, or the correction the model reads.
fn checked_id(args: &Map<String, Value>, fix: &str) -> Result<String, ToolOutcome> {
    let id = indicator_id(args.get("name"));
    if INDICATOR_ID.as_ref().is_some_and(|re| re.is_match(&id)) {
        return Ok(id);
    }
    let given = args.get("name").map(py_str).unwrap_or_default();
    Err(ToolOutcome::invalid_argument(
        "name",
        &format!("'{}' is not an indicator id.", given),
        Some(fix),
    ))
}

/// An indicator's inputs, a JSON object sent as text included
/// (`_settings`).
fn settings(value: Option<&Value>) -> Result<Map<String, Value>, ToolOutcome> {
    match value {
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(Value::String(text)) if !text.trim().is_empty() => {
            match serde_json::from_str::<Value>(text) {
                Ok(Value::Object(map)) => Ok(map),
                _ => {
                    let head: String = text.trim().chars().take(60).collect();
                    Err(ToolOutcome::invalid_argument(
                        "settings",
                        &format!(
                            "{} is not a set of named inputs",
                            py_repr(&Value::String(head))
                        ),
                        Some(
                            "Pass an object, for example {\"period\": 20}, or leave it out for the indicator's own defaults.",
                        ),
                    ))
                }
            }
        }
        _ => Ok(Map::new()),
    }
}

/// Add an indicator to the chart (`add_chart_indicator`).
pub(super) fn add_chart_indicator(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("add_chart_indicator"));
    }
    let id = checked_id(
        args,
        "Pass an id from list_chart_indicators, such as alphatrend. A number such as a period goes in settings, not in the name.",
    )?;
    let inputs = settings(args.get("settings"))?;
    let command = Command::Indicator {
        action: IndicatorAction::Add,
        id: id.clone(),
        settings: Some(inputs),
    };
    if !run.emit(ctx, vec![command]) {
        return Ok(answer("add_chart_indicator", NO_SINK, None));
    }
    let note = if chart_catalogue::ids().contains(&id) {
        ""
    } else {
        " It is not in the built-in catalogue, so it will draw only if the operator has an indicator module of that name; the chart ignores an id it does not know."
    };
    Ok(answer(
        "add_chart_indicator",
        &format!(
            "Added {} to the {} {} chart.{}",
            id, view.symbol, view.interval, note
        ),
        Some(&view.symbol),
    ))
}

/// Remove an indicator from the chart (`remove_chart_indicator`).
pub(super) fn remove_chart_indicator(
    run: &ChartRun,
    ctx: &ToolCtx,
    args: &Map<String, Value>,
) -> Result<ToolOutcome, ToolOutcome> {
    let view = run.view();
    if !view.is_open() {
        return Ok(no_chart("remove_chart_indicator"));
    }
    let id = checked_id(args, "Pass the id it was added under, such as alphatrend.")?;
    let command = Command::Indicator {
        action: IndicatorAction::Remove,
        id: id.clone(),
        settings: None,
    };
    if !run.emit(ctx, vec![command]) {
        return Ok(answer("remove_chart_indicator", NO_SINK, None));
    }
    Ok(answer(
        "remove_chart_indicator",
        &format!(
            "Removed {} from the {} {} chart.",
            id, view.symbol, view.interval
        ),
        Some(&view.symbol),
    ))
}
