//! The logic family (lane nodes-logic): Log and Variable here (the web's
//! `execute_log`, `execute_variable` and its JSON path walk,
//! `flow_executor_service.py` 2935-3057), and the registration of the
//! family's other nodes: the conditions, Math Expression, the timing
//! nodes, the alerts and HTTP Request.

use super::alerts::{TelegramAlert, WhatsappAlert};
use super::conditions::{FundCheck, PositionCheck, PriceCondition, VarCondition};
use super::http::HttpRequest;
use super::math::MathExpression;
use super::pyjson::{self, float_value};
use super::timing::{Delay, TimeCondition, TimeWindow, WaitUntil};
use super::NodeFamily;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// What a wait cut short by the run stopping answers (core's walk tests
/// read it here, where core's Delay kept it).
pub use super::timing::STOPPED_WHILE_WAITING;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "logic",
    build: || {
        vec![
            Arc::new(Log),
            Arc::new(Variable),
            Arc::new(Delay),
            Arc::new(WaitUntil),
            Arc::new(TimeWindow),
            Arc::new(TimeCondition),
            Arc::new(PositionCheck),
            Arc::new(FundCheck),
            Arc::new(PriceCondition),
            Arc::new(VarCondition),
            Arc::new(MathExpression),
            Arc::new(TelegramAlert),
            Arc::new(WhatsappAlert),
            Arc::new(HttpRequest),
        ]
    },
};

/// Log: write the interpolated message at the node's level (any text the
/// author chose; only `error` also goes to the app's own log as a warning).
pub struct Log;

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

/// Variable: set, get, arithmetic, JSON parse/stringify and append on a
/// run variable, stored only when the whole operation succeeded.
pub struct Variable;

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
        let operation = data.get("operation").cloned().unwrap_or(json!("set"));
        let value = ctx
            .vars
            .interpolate_value(data.get("value").unwrap_or(&json!("")));
        let result = match variable_operation(ctx, data, &name, &operation, &value) {
            Ok(r) => r,
            Err(e) => {
                let message = format!(
                    "Variable operation {} failed: {}",
                    pyfmt::repr(&operation),
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
        other => Err(format!(
            "float() argument must be a string or a real number, not '{}'",
            pyfmt::type_name(other)
        )),
    }
}

/// The result of one Variable operation, or the text of the exception the
/// web catches.
fn variable_operation(
    ctx: &NodeCtx,
    data: &Map<String, Value>,
    name: &str,
    operation: &Value,
    value: &Value,
) -> Result<Value, String> {
    let current = || ctx.vars.get(name).unwrap_or(json!(0));
    match operation.as_str() {
        Some("set") => Ok(match value {
            Value::String(s) if s.starts_with('{') || s.starts_with('[') => {
                pyjson::loads(s).unwrap_or_else(|_| value.clone())
            }
            other => other.clone(),
        }),
        Some("get") => lookup_source(ctx, data.get("sourceVariable"), data.get("jsonPath")),
        Some(op @ ("add" | "subtract" | "multiply" | "divide")) => {
            let (a, b) = (float_of(&current())?, float_of(value)?);
            let r = match op {
                "add" => a + b,
                "subtract" => a - b,
                "multiply" => a * b,
                _ if b == 0.0 => return Err("Cannot divide by zero".into()),
                _ => a / b,
            };
            Ok(float_value(r))
        }
        Some("increment") => Ok(float_value(float_of(&current())? + 1.0)),
        Some("decrement") => Ok(float_value(float_of(&current())? - 1.0)),
        Some("parse_json") => match value {
            Value::String(s) => pyjson::loads(s),
            other => Err(format!(
                "the JSON object must be str, bytes or bytearray, not {}",
                pyfmt::type_name(other)
            )),
        },
        Some("stringify") => Ok(json!(pyfmt::json_dumps(&lookup_source(
            ctx,
            data.get("sourceVariable"),
            None
        )?))),
        Some("append") => Ok(if ctx.vars.contains(name) {
            let before = ctx.vars.get(name).unwrap_or(json!(""));
            json!(format!("{}{}", pyfmt::str(&before), pyfmt::str(value)))
        } else {
            json!(pyfmt::str(value))
        }),
        _ => Err(format!(
            "Unknown variable operation: {}",
            pyfmt::str(operation)
        )),
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
mod tests;
