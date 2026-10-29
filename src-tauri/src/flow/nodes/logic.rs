//! Log, Delay and Variable (the web's `execute_log`, `execute_delay`,
//! `execute_variable`, `flow_executor_service.py` 2860-3057). The
//! nodes-logic lane adds the rest of the family here (conditions, math,
//! alerts, HTTP, Wait Until).

use super::NodeFamily;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::time::DELAY_MAX_SECONDS;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "logic",
    build: || vec![Arc::new(Log), Arc::new(Delay), Arc::new(Variable)],
};

/// What a wait cut short by the run stopping answers.
pub const STOPPED_WHILE_WAITING: &str = "The workflow was stopped while it was waiting.";

/// Log: write the interpolated message at the node's level.
struct Log;

#[async_trait]
impl FlowNode for Log {
    fn node_type(&self) -> &'static str {
        "log"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let message = ctx
            .vars
            .interpolate_value(data.get("message").unwrap_or(&json!("")));
        let level = match data.get("level") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => pyfmt::str(other),
            None => "info".into(),
        };
        ctx.log(format!("[LOG] {}", pyfmt::str(&message)), &level);
        NodeOutcome::result(json!({"status": "success", "message": message}))
    }
}

/// Delay: wait `delayValue` `delayUnit`s (or the legacy `delayMs`), at most
/// [`DELAY_MAX_SECONDS`].
struct Delay;

impl Delay {
    /// The wait as the node computes it: whole seconds from `delayValue`,
    /// fractional from `delayMs`; `None` when the value is not a number.
    fn seconds(data: &Map<String, Value>) -> Option<Value> {
        match data.get("delayValue") {
            Some(v) if !v.is_null() => {
                let n = pyfmt::to_int(v)?;
                let unit = match data.get("delayUnit").and_then(Value::as_str) {
                    Some("minutes") => 60,
                    Some("hours") => 3600,
                    _ => 1,
                };
                Some(json!(n.checked_mul(unit)?))
            }
            _ => {
                let ms = match data.get("delayMs") {
                    None => 1000,
                    Some(v) => pyfmt::to_int(v)?,
                };
                Some(json!(ms as f64 / 1000.0))
            }
        }
    }
}

#[async_trait]
impl FlowNode for Delay {
    fn node_type(&self) -> &'static str {
        "delay"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let Some(mut seconds) = Self::seconds(data) else {
            let message = "Delay needs a whole number of seconds, minutes or hours.";
            ctx.error(message);
            return NodeOutcome::error(message);
        };
        if seconds.as_f64().unwrap_or(0.0) > DELAY_MAX_SECONDS as f64 {
            ctx.log(
                format!(
                    "Delay of {}s exceeds the {}s maximum; waiting the maximum instead.",
                    pyfmt::str(&seconds),
                    DELAY_MAX_SECONDS
                ),
                WARNING,
            );
            seconds = json!(DELAY_MAX_SECONDS);
        }
        let text = pyfmt::str(&seconds);
        ctx.info(format!("Waiting for {} seconds", text));
        let wait = Duration::from_secs_f64(seconds.as_f64().unwrap_or(0.0).max(0.0));
        if !ctx.sleep(wait).await {
            ctx.error(STOPPED_WHILE_WAITING);
            return NodeOutcome::error(STOPPED_WHILE_WAITING);
        }
        NodeOutcome::result(json!({"status": "success", "message": format!("Waited {}s", text)}))
    }
}

/// Variable: set, get, arithmetic, JSON parse/stringify and append on a
/// run variable, stored only when the whole operation succeeded.
struct Variable;

#[async_trait]
impl FlowNode for Variable {
    fn node_type(&self) -> &'static str {
        "variable"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let name = data
            .get("variableName")
            .filter(|v| pyfmt::truthy(v))
            .or_else(|| data.get("name"))
            .map(pyfmt::str)
            .unwrap_or_default();
        let operation = data
            .get("operation")
            .map(pyfmt::str)
            .unwrap_or_else(|| "set".into());
        let value = ctx
            .vars
            .interpolate_value(data.get("value").unwrap_or(&json!("")));
        let result = match variable_operation(ctx, data, &name, &operation, &value) {
            Ok(r) => r,
            Err(e) => {
                let message = format!(
                    "Variable operation {} failed: {}",
                    pyfmt::quote(&operation),
                    e
                );
                ctx.error(message.clone());
                return NodeOutcome::error(message);
            }
        };
        ctx.vars.set(&name, result.clone());
        ctx.info(format!("Set variable {} = {}", name, pyfmt::str(&result)));
        NodeOutcome::result(json!({"status": "success", "variable": name, "value": result}))
    }
}

/// Python's `float(value)` with its error sentence.
fn float_of(v: &Value) -> Result<f64, String> {
    match v {
        Value::String(s) => pyfmt::parse_float(s)
            .ok_or_else(|| format!("could not convert string to float: {}", pyfmt::quote(s))),
        Value::Number(_) | Value::Bool(_) => {
            pyfmt::to_float(v).ok_or_else(|| "float() argument is not a number".into())
        }
        Value::Null => {
            Err("float() argument must be a string or a real number, not 'NoneType'".into())
        }
        Value::Array(_) => {
            Err("float() argument must be a string or a real number, not 'list'".into())
        }
        Value::Object(_) => {
            Err("float() argument must be a string or a real number, not 'dict'".into())
        }
    }
}

/// The result of one Variable operation, or the reason it failed (the
/// text of the exception the web catches).
fn variable_operation(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    name: &str,
    operation: &str,
    value: &Value,
) -> Result<Value, String> {
    let current = || ctx.vars.get(name).unwrap_or(json!(0));
    let number = |n: f64| json!(n);
    match operation {
        "set" => Ok(match value {
            Value::String(s) if s.starts_with('{') || s.starts_with('[') => {
                serde_json::from_str(s).unwrap_or_else(|_| value.clone())
            }
            other => other.clone(),
        }),
        "get" => lookup_source(ctx, data.get("sourceVariable"), data.get("jsonPath")),
        "add" | "subtract" | "multiply" | "divide" => {
            let (a, b) = (float_of(&current())?, float_of(value)?);
            match operation {
                "add" => Ok(number(a + b)),
                "subtract" => Ok(number(a - b)),
                "multiply" => Ok(number(a * b)),
                _ if b == 0.0 => Err("Cannot divide by zero".into()),
                _ => Ok(number(a / b)),
            }
        }
        "increment" => Ok(number(float_of(&current())? + 1.0)),
        "decrement" => Ok(number(float_of(&current())? - 1.0)),
        "parse_json" => match value {
            Value::String(s) => serde_json::from_str(s).map_err(|e| e.to_string()),
            other => Err(format!(
                "the JSON object must be str, bytes or bytearray, not {}",
                pyfmt::type_name(other)
            )),
        },
        "stringify" => Ok(json!(pyfmt::json_dumps(&lookup_source(
            ctx,
            data.get("sourceVariable"),
            None
        )?))),
        "append" => Ok(if ctx.vars.contains(name) {
            let before = ctx.vars.get(name).unwrap_or(json!(""));
            json!(format!("{}{}", pyfmt::str(&before), pyfmt::str(value)))
        } else {
            json!(pyfmt::str(value))
        }),
        other => Err(format!("Unknown variable operation: {}", other)),
    }
}

/// A raw source variable, missing told apart from `null`, then walked by
/// `jsonPath` (`_lookup_variable_source`).
fn lookup_source(
    ctx: &NodeCtx,
    source: Option<&Value>,
    path: Option<&Value>,
) -> Result<Value, String> {
    let name = match source {
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err("sourceVariable is required".into()),
    };
    if !ctx.vars.contains(&name) {
        // str(KeyError(...)) quotes its message.
        return Err(pyfmt::quote(&format!(
            "Source variable not found: {}",
            name
        )));
    }
    let value = ctx.vars.get(&name).unwrap_or(Value::Null);
    match path {
        None | Some(Value::Null) => Ok(value),
        Some(Value::String(p)) if p.is_empty() => Ok(value),
        Some(Value::String(p)) => walk_json_path(value, p),
        Some(_) => Err("jsonPath must be a string".into()),
    }
}

/// Dotted keys and bracketed indexes, no evaluation
/// (`_walk_variable_json_path`).
fn walk_json_path(mut value: Value, json_path: &str) -> Result<Value, String> {
    let path: Vec<char> = json_path.trim().chars().collect();
    let invalid = || format!("Invalid jsonPath: {}", json_path);
    let mut pos = 0;
    while pos < path.len() {
        if path[pos] == '[' {
            let close = path[pos + 1..]
                .iter()
                .position(|c| *c == ']')
                .map(|i| i + pos + 1)
                .ok_or_else(invalid)?;
            let index: String = path[pos + 1..close].iter().collect();
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("Invalid jsonPath index: {}", index));
            }
            let Value::Array(list) = value else {
                return Err("jsonPath index requires a list or tuple".into());
            };
            let i: usize = index
                .parse()
                .map_err(|_| "list index out of range".to_string())?;
            value = list.into_iter().nth(i).ok_or("list index out of range")?;
            pos = close + 1;
            if pos < path.len() && path[pos] != '.' && path[pos] != '[' {
                return Err(invalid());
            }
            continue;
        }
        if pos > 0 {
            if path[pos] != '.' {
                return Err(invalid());
            }
            pos += 1;
        }
        let start = pos;
        while pos < path.len() && !matches!(path[pos], '.' | '[' | ']') {
            pos += 1;
        }
        let key: String = path[start..pos].iter().collect();
        if key.is_empty() {
            return Err(invalid());
        }
        let Value::Object(mut map) = value else {
            return Err("jsonPath key requires an object".into());
        };
        value = map.remove(&key).ok_or_else(|| pyfmt::quote(&key))?;
        if pos < path.len() && path[pos] == ']' {
            return Err(invalid());
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::contract::client::fake::FakeFlowClient;

    fn ctx() -> NodeCtx {
        NodeCtx::bare(Arc::new(FakeFlowClient::new()))
    }

    fn data(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    async fn variable(c: &NodeCtx, d: Value) -> NodeOutcome {
        Variable.run(c, &data(d)).await
    }

    #[tokio::test]
    async fn log_writes_the_interpolated_message_at_its_level() {
        let c = ctx();
        c.vars.set("sym", json!("SBIN"));
        let out = Log
            .run(
                &c,
                &data(json!({"message": "Bought {{sym}}", "level": "warning"})),
            )
            .await;
        assert_eq!(
            out.value(),
            Some(&json!({"status": "success", "message": "Bought SBIN"}))
        );
        let entry = &c.logs()[0];
        assert_eq!(entry["message"], "[LOG] Bought SBIN");
        assert_eq!(entry["level"], "warning");
    }

    #[test]
    fn delays_are_read_as_the_web_reads_them() {
        assert_eq!(
            Delay::seconds(&data(json!({"delayValue": 2, "delayUnit": "minutes"}))),
            Some(json!(120))
        );
        assert_eq!(
            Delay::seconds(&data(json!({"delayValue": "5"}))),
            Some(json!(5))
        );
        assert_eq!(Delay::seconds(&data(json!({}))), Some(json!(1.0)));
        assert_eq!(
            Delay::seconds(&data(json!({"delayMs": 1500}))),
            Some(json!(1.5))
        );
        assert_eq!(Delay::seconds(&data(json!({"delayValue": "soon"}))), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_long_delay_waits_the_maximum() {
        let c = ctx();
        let out = Delay
            .run(&c, &data(json!({"delayValue": 2, "delayUnit": "hours"})))
            .await;
        assert_eq!(
            out.value(),
            Some(&json!({"status": "success", "message": "Waited 300s"}))
        );
        let logs = c.logs();
        assert_eq!(
            logs[0]["message"],
            "Delay of 7200s exceeds the 300s maximum; waiting the maximum instead."
        );
        assert_eq!(logs[0]["level"], "warning");
        assert_eq!(logs[1]["message"], "Waiting for 300 seconds");
    }

    #[tokio::test]
    async fn a_stopped_run_ends_a_delay() {
        let c = ctx();
        c.cancel.cancel();
        let out = Delay.run(&c, &data(json!({"delayValue": 60}))).await;
        assert!(out.is_error());
        assert_eq!(out.failure_message(), STOPPED_WHILE_WAITING);
    }

    #[tokio::test]
    async fn variables_set_compute_and_read_paths() {
        let c = ctx();
        let set = variable(
            &c,
            json!({"variableName": "cfg", "value": "{\"legs\": [{\"qty\": 50}]}"}),
        )
        .await;
        assert_eq!(
            set.value().unwrap()["value"],
            json!({"legs": [{"qty": 50}]})
        );
        let got = variable(&c, json!({"variableName": "q", "operation": "get", "sourceVariable": "cfg", "jsonPath": "legs[0].qty"})).await;
        assert_eq!(got.value().unwrap()["value"], json!(50));
        variable(&c, json!({"variableName": "n", "value": "4"})).await;
        let added = variable(
            &c,
            json!({"variableName": "n", "operation": "add", "value": "2.5"}),
        )
        .await;
        assert_eq!(added.value().unwrap()["value"], json!(6.5));
        assert_eq!(c.logs().last().unwrap()["message"], "Set variable n = 6.5");
        let inc = variable(
            &c,
            json!({"variableName": "fresh", "operation": "increment"}),
        )
        .await;
        assert_eq!(inc.value().unwrap()["value"], json!(1.0));
        let appended = variable(
            &c,
            json!({"variableName": "msg", "operation": "append", "value": "a"}),
        )
        .await;
        assert_eq!(appended.value().unwrap()["value"], json!("a"));
        let text = variable(
            &c,
            json!({"variableName": "s", "operation": "stringify", "sourceVariable": "cfg"}),
        )
        .await;
        assert_eq!(
            text.value().unwrap()["value"],
            json!("{\"legs\": [{\"qty\": 50}]}")
        );
    }

    #[tokio::test]
    async fn a_failed_operation_stores_nothing_and_says_why() {
        let c = ctx();
        let div = variable(
            &c,
            json!({"variableName": "x", "operation": "divide", "value": "0"}),
        )
        .await;
        assert_eq!(
            div.failure_message(),
            "Variable operation 'divide' failed: Cannot divide by zero"
        );
        assert!(!c.vars.contains("x"));
        let missing = variable(
            &c,
            json!({"variableName": "y", "operation": "get", "sourceVariable": "nope"}),
        )
        .await;
        assert_eq!(
            missing.failure_message(),
            "Variable operation 'get' failed: 'Source variable not found: nope'"
        );
        let nan = variable(
            &c,
            json!({"variableName": "z", "operation": "add", "value": "abc"}),
        )
        .await;
        assert_eq!(
            nan.failure_message(),
            "Variable operation 'add' failed: could not convert string to float: 'abc'"
        );
        let bad = variable(&c, json!({"variableName": "z", "operation": "pow"})).await;
        assert_eq!(
            bad.failure_message(),
            "Variable operation 'pow' failed: Unknown variable operation: pow"
        );
    }

    #[test]
    fn json_paths_refuse_what_they_cannot_walk() {
        let v = json!({"a": [{"b": 1}]});
        assert_eq!(walk_json_path(v.clone(), "a[0].b"), Ok(json!(1)));
        assert_eq!(
            walk_json_path(v.clone(), "a[x]"),
            Err("Invalid jsonPath index: x".into())
        );
        assert_eq!(
            walk_json_path(v.clone(), "a.b"),
            Err("jsonPath key requires an object".into())
        );
        assert_eq!(
            walk_json_path(v.clone(), "a[3]"),
            Err("list index out of range".into())
        );
        assert_eq!(walk_json_path(v.clone(), "c"), Err("'c'".into()));
        assert_eq!(walk_json_path(v, "a]"), Err("Invalid jsonPath: a]".into()));
    }
}
