//! The web executor's Indicator, Bar Offset and Prior Period OHLC nodes
//! (`executor.json`), composed from this API the way the nodes-data lane
//! composes them: the params text, the `sourceSeries` reading, the history
//! window the node asks for, and the result it stores. What only the node
//! decides (a missing symbol, a failed fetch, log lines) is the nodes-data
//! lane's and is not checked here.

use chrono::NaiveDate;
use serde_json::{json as value, Map, Value};

use super::support::{diff, json, records, wall_time, Failures, EXECUTOR};
use crate::agent::tools::values::{py_str, truthy};
use crate::flow::indicator_service::compute::compute_indicator_as_web;
use crate::flow::indicator_service::records::py_float;
use crate::flow::indicator_service::{
    bar_at_offset, clamp_bars, history_window, prior_period_ohlc, source_series_numbers,
    trim_records, IndicatorParams, IndicatorRequest, IndicatorSource,
};

/// Cases whose answer is decided by the node before this API is reached.
const NODE_ONLY: &[&str] = &[
    "no_symbol",
    "history_error",
    "history_error_db",
    "bar_no_symbol",
    "bar_history_error",
    "prior_no_symbol",
];

/// `NodeExecutor.get_int`.
fn get_int(node: &Value, key: &str, default: i64) -> i64 {
    match node.get(key) {
        None | Some(Value::Null) => default,
        Some(Value::String(text)) if text.is_empty() => default,
        Some(Value::String(text)) => py_float(text).map_or(default, |f| f as i64),
        Some(Value::Bool(flag)) => i64::from(*flag),
        Some(other) => other
            .as_i64()
            .or(other.as_f64().map(|f| f as i64))
            .unwrap_or(default),
    }
}

/// `NodeExecutor.get_str` with interpolation as the identity (the recorder's
/// context resolves only whole tokens).
fn get_str(node: &Value, key: &str, default: &str) -> String {
    match node.get(key) {
        Some(value) if truthy(value) => py_str(value),
        _ => default.to_string(),
    }
}

/// `WorkflowContext.resolve_raw` for the recorder's variables.
fn resolve_raw(text: &str, variables: &Map<String, Value>) -> Value {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("{{")
        .and_then(|t| t.strip_suffix("}}"))
    {
        if let Some(found) = variables.get(inner.trim()) {
            return found.clone();
        }
    }
    Value::String(text.to_string())
}

fn node_error(message: &str) -> Value {
    value!({"status": "error", "message": message})
}

/// `execute_indicator` from `IndicatorParams::parse`, `source_series_numbers`
/// and `compute_indicator`, with the web's reading of rows.
fn indicator_node(node: &Value, variables: &Map<String, Value>, history: &[Value]) -> Value {
    let name = get_str(node, "indicatorName", "sma");
    let params = match node.get("params") {
        Some(Value::Object(map)) => IndicatorParams::from_map(map),
        _ => match IndicatorParams::parse(&get_str(node, "params", "{}")) {
            Ok(params) => params,
            Err(message) => return node_error(&message),
        },
    };
    let numbers;
    let trimmed;
    let source = match node.get("sourceSeries") {
        Some(raw) if !raw.is_null() && *raw != value!("") => {
            let resolved = match raw {
                Value::String(text) => resolve_raw(text, variables),
                other => other.clone(),
            };
            match source_series_numbers(&resolved, &get_str(node, "sourceField", "")) {
                Ok(values) => numbers = values,
                Err(e) => return node_error(&e.message),
            }
            IndicatorSource::Nested(&numbers)
        }
        _ => {
            trimmed = trim_records(history).to_vec();
            IndicatorSource::Candles {
                records: &trimmed,
                lookback_bars: get_int(node, "lookbackBars", 100),
            }
        }
    };
    let request = IndicatorRequest {
        indicator_name: &name,
        params: &params,
        source,
        tail_bars: get_int(node, "tailBars", 5),
        offset_bars: get_int(node, "offsetBars", 0),
    };
    match compute_indicator_as_web(&request) {
        Ok(out) => out.to_value(),
        Err(e) if e.is_value_error() => node_error(&e.message),
        Err(e) => value!({"kind": e.kind, "message": e.message}),
    }
}

/// The history window each node asks for, from `history_window`.
fn window(case: &Value, today: NaiveDate) -> (String, String) {
    let node = &case["node_data"];
    let interval = get_str(node, "interval", "D");
    match case["method"].as_str() {
        Some("execute_bar_offset") => {
            let offset = clamp_bars(get_int(node, "offsetBars", 0) + 1, "offset bars") - 1;
            history_window(offset + 5, None, &interval, today)
        }
        Some("execute_prior_period_ohlc") => {
            let (interval, days) = match get_str(node, "period", "previous_day").as_str() {
                "previous_hour" => ("1h", 5),
                "previous_month" => ("D", 60),
                "previous_week" => ("D", 21),
                _ => ("D", 10),
            };
            history_window(100, Some(days), interval, today)
        }
        _ => history_window(get_int(node, "lookbackBars", 100), None, &interval, today),
    }
}

/// The row or candle the bar and prior-period nodes report, against the
/// web's output fields.
fn bar_fields(case: &Value, history: &[Value], now: chrono::NaiveDateTime) -> Option<String> {
    let node = &case["node_data"];
    let want = &case["result"];
    let rows = trim_records(history);
    if case["method"] == "execute_bar_offset" {
        let offset = clamp_bars(get_int(node, "offsetBars", 0) + 1, "offset bars") - 1;
        let interval = get_str(node, "interval", "D");
        let row = bar_at_offset(rows, offset, &interval, now).ok()?;
        let Some(row) = row else {
            return (want["status"] != "error").then(|| format!("no bar, web {want}"));
        };
        let fields = ["timestamp", "open", "high", "low", "close", "volume"];
        return fields
            .iter()
            .find(|f| row[**f] != want[**f])
            .map(|f| format!("{f}: {} != {}", row[*f], want[*f]));
    }
    let period = get_str(node, "period", "previous_day");
    match prior_period_ohlc(rows, &period, now) {
        Ok(out) => {
            let mut map = out.to_map();
            map.insert("pdh".into(), out.high.clone());
            map.insert("pdl".into(), out.low.clone());
            map.insert("pdc".into(), out.close.clone());
            let shown: Map<String, Value> = want
                .as_object()?
                .iter()
                .filter(|(k, _)| map.contains_key(*k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            diff(&Value::Object(map), &Value::Object(shown))
        }
        Err(e) => (want["message"] != value!(e.message)).then(|| format!("{e:?} vs {want}")),
    }
}

#[test]
fn the_nodes_answer_as_the_webs() {
    let fixture = json(EXECUTOR);
    let variables = fixture["variables"].as_object().expect("variables");
    let cases = fixture["cases"].as_array().expect("cases");
    let mut failures = Failures::default();
    let mut checked = 0;
    for case in cases {
        let label = case["label"].as_str().expect("label");
        if NODE_ONLY.contains(&label) {
            continue;
        }
        checked += 1;
        let history = match case["history"].as_str() {
            Some("trend") => records(&value!({"shape": "trend"})),
            Some("gaps") => records(&value!({"shape": "gaps"})),
            _ => Vec::new(),
        };
        let now = wall_time(&case["now"]);
        for request in case["history_requests"].as_array().expect("requests") {
            let (start, end) = window(case, now.date());
            if request["start_date"] != value!(start) || request["end_date"] != value!(end) {
                failures.push(label, format!("window {start}..{end} vs {request}"));
            }
        }
        let problem = if case["method"] == "execute_indicator" {
            let got = indicator_node(&case["node_data"], variables, &history);
            match (case.get("result"), case.get("error")) {
                (Some(want), _) => diff(&got, want),
                (None, Some(want)) => (got != *want).then(|| format!("{got} vs {want}")),
                _ => Some("malformed case".into()),
            }
        } else {
            bar_fields(case, &history, now)
        };
        if let Some(problem) = problem {
            failures.push(label, problem);
        }
    }
    failures.check(checked);
}
