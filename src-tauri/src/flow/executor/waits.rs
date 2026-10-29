//! How long a run of a workflow can sleep, read from its saved graph (the
//! web's `workflow_wait_seconds`, `flow_executor_service.py` 4760-4825).
//!
//! A run whose waits add up to more than [`INLINE_WAIT_SECONDS`] is answered
//! at once with `accepted` and finishes in the background; a shorter one is
//! answered with its result, so TradingView still gets the broker's answer
//! (lead decision 2: the web's 10 s rule, without its worker pools, since a
//! waiting task here costs nothing).

use crate::flow::contract::pyfmt;
use crate::flow::contract::time::{parse_time_string, DELAY_MAX_SECONDS, WAIT_UNTIL_MAX_SECONDS};
use chrono::{NaiveDateTime, Timelike};
use serde_json::{Map, Value};

/// Waits up to this many seconds in total still answer with the result
/// (`FLOW_INLINE_WAIT_SECONDS`).
pub const INLINE_WAIT_SECONDS: f64 = 10.0;

/// What the caller reads when the run goes on in the background. The web's
/// `FLOW_STARTED_IN_BACKGROUND_MESSAGE` points at an execution history view
/// the desktop's editor does not have, so this names where the result
/// shows instead, matching the editor (parity F-207).
pub const STARTED_IN_BACKGROUND: &str = "Workflow started. It waits on a Delay or Wait Until step and finishes in the background: its orders go out when the wait ends and show in the order book.";

/// One Delay node's wait, as the node computes it, capped and never
/// negative; an unreadable value counts as the maximum, so a doubtful
/// workflow is treated as a long one (`_delay_node_seconds`).
fn delay_seconds(data: &Map<String, Value>) -> f64 {
    let seconds = match data.get("delayValue").filter(|v| !v.is_null()) {
        Some(v) => {
            let unit = match data.get("delayUnit").and_then(Value::as_str) {
                Some("minutes") => 60,
                Some("hours") => 3600,
                _ => 1,
            };
            pyfmt::to_int(v)
                .and_then(|n| n.checked_mul(unit))
                .map(|n| n as f64)
        }
        None => match data.get("delayMs") {
            None => Some(1.0),
            Some(v) => pyfmt::to_int(v).map(|ms| ms as f64 / 1000.0),
        },
    };
    seconds
        .unwrap_or(DELAY_MAX_SECONDS as f64)
        .clamp(0.0, DELAY_MAX_SECONDS as f64)
}

/// How long a Wait Until node would sleep if it ran at `now` (IST),
/// capped (`_wait_until_node_seconds`).
fn wait_until_seconds(data: &Map<String, Value>, now: NaiveDateTime) -> f64 {
    let (h, m, s) = parse_time_string(data.get("targetTime").and_then(Value::as_str), 9, 30);
    let now_s = (now.hour() * 3600 + now.minute() * 60 + now.second()) as i64;
    let target = (h * 3600 + m * 60 + s) as i64;
    (target - now_s).clamp(0, WAIT_UNTIL_MAX_SECONDS) as f64
}

/// `(delays, wait_until)`: every Delay node added up, and the furthest Wait
/// Until target from `now`.
pub fn workflow_wait_seconds(nodes: &Value, now: NaiveDateTime) -> (f64, f64) {
    let mut delays = 0.0;
    let mut wait_until: f64 = 0.0;
    for node in nodes.as_array().map(Vec::as_slice).unwrap_or_default() {
        let empty = Map::new();
        let data = node
            .get("data")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        match node.get("type").and_then(Value::as_str) {
            Some("delay") => delays += delay_seconds(data),
            Some("waitUntil") => wait_until = wait_until.max(wait_until_seconds(data, now)),
            _ => {}
        }
    }
    (delays, wait_until)
}

/// Whether a triggered run of these nodes is answered at once and finishes
/// in the background (`workflow_runs_in_background`).
pub fn runs_in_background(nodes: &Value, now: NaiveDateTime) -> bool {
    let (delays, wait_until) = workflow_wait_seconds(nodes, now);
    delays + wait_until > INLINE_WAIT_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(hms: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("2026-10-30 {}", hms), "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn delays_add_up_and_wait_until_takes_the_furthest() {
        let nodes = json!([
            {"type": "delay", "data": {"delayValue": 4}},
            {"type": "delay", "data": {"delayValue": 1, "delayUnit": "minutes"}},
            {"type": "delay", "data": {"delayMs": 500}},
            {"type": "waitUntil", "data": {"targetTime": "09:16"}},
            {"type": "waitUntil", "data": {"targetTime": "09:20"}},
            {"type": "log", "data": {}}
        ]);
        assert_eq!(workflow_wait_seconds(&nodes, at("09:15:00")), (64.5, 300.0));
        assert!(runs_in_background(&nodes, at("09:15:00")));
    }

    #[test]
    fn short_or_passed_waits_run_inline() {
        let nodes = json!([{"type": "delay", "data": {"delayValue": 10}}, {"type": "waitUntil", "data": {"targetTime": "09:00"}}]);
        assert_eq!(workflow_wait_seconds(&nodes, at("09:15:00")), (10.0, 0.0));
        assert!(!runs_in_background(&nodes, at("09:15:00")));
        assert!(!runs_in_background(&json!([]), at("09:15:00")));
    }

    #[test]
    fn doubtful_waits_count_long_and_are_capped() {
        let nodes = json!([{"type": "delay", "data": {"delayValue": "soon"}}]);
        assert_eq!(workflow_wait_seconds(&nodes, at("09:15:00")).0, 300.0);
        let hours = json!([{"type": "delay", "data": {"delayValue": 2, "delayUnit": "hours"}}, {"type": "waitUntil", "data": {"targetTime": "23:00"}}]);
        assert_eq!(
            workflow_wait_seconds(&hours, at("09:15:00")),
            (300.0, 1800.0)
        );
        let negative = json!([{"type": "delay", "data": {"delayValue": -5}}]);
        assert_eq!(workflow_wait_seconds(&negative, at("09:15:00")).0, 0.0);
    }
}
