//! Position Check, Fund Check, Price Condition and Var Condition (the
//! web's `execute_position_check`, `execute_fund_check`,
//! `execute_price_condition`, `execute_var_condition`,
//! `flow_executor_service.py` 3254-3585).
//!
//! Each answers `condition` and the walk takes the `true` or `false`
//! branch. A check that cannot be made (a failed read, a value that does
//! not resolve, a configuration it cannot honour) answers
//! `{"status": "error", "condition": false}`: the run records the failure
//! and takes neither branch, never the `false` branch as if the check had
//! run. Fixed web defects, each failing closed where the web answered on a
//! zero: an unreadable Fund Check minimum (N-fundCheck-06), an unreadable
//! Position Check threshold and a decimal quantity (N-positionCheck-08),
//! a quote missing the field or the previous close (N-priceCondition-05).

use super::pyjson::float_value;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::default_product_for_exchange;
use async_trait::async_trait;
use serde_json::{json, Map, Value};

/// The quote fields Price Condition reads (`PRICE_CONDITION_FIELDS`).
pub const PRICE_FIELDS: &[&str] = &["change_percent", "high", "low", "ltp", "open", "prev_close"];

/// The operators the comparisons understand, sorted as the web lists them
/// (`COMPARISON_OPERATORS`).
pub const COMPARISON_OPERATORS: &[&str] = &[
    "!=", "<", "<=", "==", ">", ">=", "eq", "gt", "gte", "lt", "lte", "neq",
];

/// `_compare`: the editor's symbols or the legacy words, on floats.
pub fn compare(left: f64, operator: &str, right: f64) -> bool {
    match operator {
        ">" | "gt" => left > right,
        "<" | "lt" => left < right,
        "==" | "eq" => left == right,
        "!=" | "neq" => left != right,
        ">=" | "gte" => left >= right,
        "<=" | "lte" => left <= right,
        _ => false,
    }
}

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// A numeric field read as `get_float` reads it, except that a value given
/// but unreadable is told apart from one not given.
enum Number {
    Missing,
    Value(f64),
    Unreadable,
}

fn read_number(ctx: &NodeCtx, data: &Map<String, Value>, key: &str) -> Number {
    match data.get(key) {
        None | Some(Value::Null) => Number::Missing,
        Some(Value::String(s)) if s.is_empty() => Number::Missing,
        Some(Value::String(s)) => match pyfmt::parse_float(&ctx.vars.interpolate(s)) {
            Some(f) => Number::Value(f),
            None => Number::Unreadable,
        },
        Some(other) => pyfmt::to_float(other)
            .map(Number::Value)
            .unwrap_or(Number::Unreadable),
    }
}

/// A condition that could not be evaluated, logged with its prefix.
fn undecided(ctx: &NodeCtx, prefix: &str, message: String) -> NodeOutcome {
    ctx.error(format!("{}{}", prefix, message));
    NodeOutcome::undecided(message)
}

/// The failure of a client read: `result.get("error") or
/// result.get("message") or fallback` (the nodes read `error` first; the
/// walk reads `message` first).
fn read_failure(result: &Value, fallback: &str) -> String {
    ["error", "message"]
        .iter()
        .filter_map(|k| result.get(*k))
        .find(|v| pyfmt::truthy(v))
        .map(pyfmt::str)
        .unwrap_or_else(|| fallback.to_string())
}

fn is_success(result: &Value) -> bool {
    result.get("status").and_then(Value::as_str) == Some("success")
}

pub struct PositionCheck;

const POSITION_CONDITIONS: &str =
    "(exists, not_exists, quantity_above, quantity_below, pnl_above, pnl_below)";

impl PositionCheck {
    /// The position's quantity as a whole number: a number truncated, text
    /// read as a number first (`"10.0"`, parity N-positionCheck-08).
    fn quantity(v: Option<&Value>) -> Result<i64, String> {
        let v = match v {
            Some(v) if pyfmt::truthy(v) => v,
            _ => return Ok(0),
        };
        pyfmt::to_float(v)
            .filter(|f| f.is_finite())
            .map(|f| f.trunc() as i64)
            .ok_or_else(|| {
                format!(
                    "the position's quantity is not a number: {}",
                    pyfmt::repr(v)
                )
            })
    }

    fn pnl(v: Option<&Value>) -> Result<f64, String> {
        let v = match v {
            Some(v) if pyfmt::truthy(v) => v,
            _ => return Ok(0.0),
        };
        pyfmt::to_float(v)
            .ok_or_else(|| format!("the position's P&L is not a number: {}", pyfmt::repr(v)))
    }
}

#[async_trait]
impl FlowNode for PositionCheck {
    fn node_type(&self) -> &'static str {
        "positionCheck"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        const ABORT: &str = "Position check aborted: ";
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let product = ctx.get_str(
            data,
            "product",
            default_product_for_exchange(Some(exchange.as_str())),
        );
        let condition = ctx.get_str(data, "condition", "exists");
        // The web's get_float raises on a list or dict here, before the
        // symbol is even checked, and the run ends (decisions.md 24).
        if let Err(abort) = ctx.try_get_float(data, "threshold", 0.0) {
            return abort.into();
        }
        let threshold = read_number(ctx, data, "threshold");
        if symbol.is_empty() {
            return undecided(
                ctx,
                "",
                "Position Check has no symbol set, so it cannot guard anything.".into(),
            );
        }
        ctx.info(format!("Checking position for: {}", symbol));
        let result = ctx
            .client
            .get_open_position(&symbol, &exchange, Some(&product))
            .await;
        if !is_success(&result) {
            return undecided(
                ctx,
                ABORT,
                read_failure(&result, "the position lookup failed"),
            );
        }
        let read = Self::quantity(result.get("quantity"))
            .and_then(|q| Self::pnl(result.get("pnl")).map(|p| (q, p)));
        let (qty, pnl) = match read {
            Ok(v) => v,
            Err(message) => return undecided(ctx, ABORT, message),
        };
        let needs_threshold = matches!(
            condition.as_str(),
            "quantity_above" | "quantity_below" | "pnl_above" | "pnl_below"
        );
        let t = match threshold {
            Number::Unreadable if needs_threshold => {
                let raw = data.get("threshold").map(pyfmt::repr).unwrap_or_default();
                return undecided(
                    ctx,
                    ABORT,
                    format!("threshold did not resolve to a number: {}", raw),
                );
            }
            Number::Value(f) => f,
            _ => 0.0,
        };
        let (met, line) = match condition.as_str() {
            "exists" => (qty != 0, format!("qty={} != 0", qty)),
            "not_exists" => (qty == 0, format!("qty={} == 0", qty)),
            "quantity_above" => (
                (qty as f64) > t,
                format!("qty={} > {}", qty, pyfmt::float(t)),
            ),
            "quantity_below" => (
                (qty as f64) < t,
                format!("qty={} < {}", qty, pyfmt::float(t)),
            ),
            "pnl_above" => (
                pnl > t,
                format!("pnl={} > {}", pyfmt::float(pnl), pyfmt::float(t)),
            ),
            "pnl_below" => (
                pnl < t,
                format!("pnl={} < {}", pyfmt::float(pnl), pyfmt::float(t)),
            ),
            other => {
                return undecided(
                    ctx,
                    ABORT,
                    format!(
                        "condition {} is not one this node can evaluate {}",
                        pyfmt::quote(other),
                        POSITION_CONDITIONS
                    ),
                )
            }
        };
        ctx.info(format!("Position check: {} = {}", line, py_bool(met)));
        NodeOutcome::result(json!({
            "status": "success",
            "condition": met,
            "quantity": qty,
            "pnl": float_value(pnl),
        }))
    }
}

pub struct FundCheck;

const NO_MINIMUM: &str = "Fund Check has no Minimum Available set, so it cannot guard anything. Open the node and set a minimum.";

impl FundCheck {
    /// The minimum to compare against, or the node's answer when there is
    /// none it can use.
    fn minimum(ctx: &NodeCtx, data: &Map<String, Value>) -> Result<f64, NodeOutcome> {
        let unreadable = |key: &str| {
            let raw = data.get(key).map(pyfmt::repr).unwrap_or_default();
            undecided(
                ctx,
                "",
                format!(
                    "Fund Check's minimum did not resolve to a number: {}. Open the node and set Minimum Available.",
                    raw
                ),
            )
        };
        if data.contains_key("minAvailable") {
            ctx.try_get_float(data, "minAvailable", 0.0)
                .map_err(NodeOutcome::from)?;
            return match read_number(ctx, data, "minAvailable") {
                Number::Value(f) => Ok(f),
                Number::Missing => Err(undecided(ctx, "", NO_MINIMUM.into())),
                Number::Unreadable => Err(unreadable("minAvailable")),
            };
        }
        if !data.contains_key("threshold") {
            return Err(undecided(ctx, "", NO_MINIMUM.into()));
        }
        let op = data
            .get("operator")
            .filter(|v| pyfmt::truthy(v))
            .map(pyfmt::str)
            .unwrap_or_else(|| "gt".into())
            .trim()
            .to_lowercase();
        if !matches!(op.as_str(), "gt" | "gte" | "ge" | ">" | ">=") {
            let message = format!(
                "Fund Check has a legacy '{}' comparison, which this node cannot express - it only supports a minimum. Open the node and set Minimum Available.",
                op
            );
            ctx.error(message.clone());
            return Err(NodeOutcome::error(message));
        }
        ctx.try_get_float(data, "threshold", 0.0)
            .map_err(NodeOutcome::from)?;
        let min = match read_number(ctx, data, "threshold") {
            Number::Value(f) => f,
            Number::Missing => return Err(undecided(ctx, "", NO_MINIMUM.into())),
            Number::Unreadable => return Err(unreadable("threshold")),
        };
        ctx.log(
            format!(
                "Fund Check is using its legacy threshold of {}. Re-save the node to store it as Minimum Available.",
                pyfmt::float(min)
            ),
            WARNING,
        );
        Ok(min)
    }
}

#[async_trait]
impl FlowNode for FundCheck {
    fn node_type(&self) -> &'static str {
        "fundCheck"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        const ABORT: &str = "Fund check aborted: ";
        let min = match Self::minimum(ctx, data) {
            Ok(m) => m,
            Err(answer) => return answer,
        };
        ctx.info("Checking funds");
        let result = ctx.client.funds().await;
        if !is_success(&result) {
            return undecided(ctx, ABORT, read_failure(&result, "the funds lookup failed"));
        }
        let cash = result
            .get("data")
            .and_then(|d| d.get("availablecash"))
            .filter(|v| pyfmt::truthy(v));
        let available = match cash {
            None => 0.0,
            Some(v) => match pyfmt::to_float(v) {
                Some(f) => f,
                None => {
                    return undecided(
                        ctx,
                        ABORT,
                        format!("the available cash is not a number: {}", pyfmt::repr(v)),
                    )
                }
            },
        };
        let met = available >= min;
        ctx.info(format!(
            "Fund check: available={} >= {} = {}",
            pyfmt::float(available),
            pyfmt::float(min),
            py_bool(met)
        ));
        NodeOutcome::result(json!({
            "status": "success",
            "condition": met,
            "available": float_value(available),
        }))
    }
}

pub struct PriceCondition;

impl PriceCondition {
    /// The threshold: `value`, else the legacy `threshold`, else 0.
    fn threshold(ctx: &NodeCtx, data: &Map<String, Value>) -> Result<f64, String> {
        let value = data
            .get("value")
            .or_else(|| data.get("threshold"))
            .cloned()
            .unwrap_or(json!(0));
        if value.is_null() || value == json!("") {
            return Ok(0.0);
        }
        pyfmt::parse_float(&ctx.vars.interpolate(&pyfmt::str(&value)))
            .ok_or_else(|| format!("value did not resolve to a number: {}", pyfmt::repr(&value)))
    }

    /// A quote field the check needs; missing or empty fails the check
    /// rather than reading zero (parity N-priceCondition-05).
    fn field(quote: &Value, key: &str) -> Result<f64, String> {
        match quote.get(key) {
            None | Some(Value::Null) => Err(format!("the quote has no {}", key)),
            Some(Value::String(s)) if s.is_empty() => Err(format!("the quote has no {}", key)),
            Some(v) => pyfmt::to_float(v)
                .ok_or_else(|| format!("the quote's {} is not a number: {}", key, pyfmt::repr(v))),
        }
    }

    fn field_value(quote: &Value, field: &str) -> Result<f64, String> {
        if field != "change_percent" {
            return Self::field(quote, field);
        }
        let ltp = Self::field(quote, "ltp")?;
        let prev = Self::field(quote, "prev_close")?;
        if prev == 0.0 {
            return Err("the quote has no prev_close".into());
        }
        Ok((ltp - prev) / prev * 100.0)
    }
}

#[async_trait]
impl FlowNode for PriceCondition {
    fn node_type(&self) -> &'static str {
        "priceCondition"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        const ABORT: &str = "Price check aborted: ";
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let field = ctx.get_str(data, "field", "ltp");
        let operator = ctx.get_str(data, "operator", ">");
        if !PRICE_FIELDS.contains(&field.as_str()) {
            return undecided(
                ctx,
                ABORT,
                format!(
                    "field {} is not one of {}",
                    pyfmt::quote(&field),
                    PRICE_FIELDS.join(", ")
                ),
            );
        }
        if !COMPARISON_OPERATORS.contains(&operator.as_str()) {
            return undecided(
                ctx,
                ABORT,
                format!(
                    "operator {} is not one of {}",
                    pyfmt::quote(&operator),
                    COMPARISON_OPERATORS.join(", ")
                ),
            );
        }
        let threshold = match Self::threshold(ctx, data) {
            Ok(t) => t,
            Err(message) => return undecided(ctx, ABORT, message),
        };
        ctx.info(format!("Checking price for: {}", symbol));
        let result = ctx.client.get_quotes(&symbol, &exchange).await;
        if !is_success(&result) {
            return undecided(ctx, ABORT, read_failure(&result, "the price lookup failed"));
        }
        let quote = result.get("data").cloned().unwrap_or(Value::Null);
        let value = match Self::field_value(&quote, &field) {
            Ok(v) => v,
            Err(message) => return undecided(ctx, ABORT, message),
        };
        let met = compare(value, &operator, threshold);
        ctx.info(format!(
            "Price check: {}={} {} {} = {}",
            field,
            pyfmt::float(value),
            operator,
            pyfmt::float(threshold),
            py_bool(met)
        ));
        let ltp = quote
            .get("ltp")
            .filter(|v| pyfmt::truthy(v))
            .and_then(pyfmt::to_float)
            .unwrap_or(0.0);
        NodeOutcome::result(json!({
            "status": "success",
            "condition": met,
            "ltp": float_value(ltp),
            "field": field,
            "field_value": float_value(value),
        }))
    }
}

pub struct VarCondition;

impl VarCondition {
    /// One operand as a float (`_operand`): a JSON number as it is (a
    /// literal 0 included), else the interpolated, stripped text.
    fn operand(ctx: &NodeCtx, data: &Map<String, Value>, key: &str) -> Result<f64, String> {
        let raw = data.get(key).cloned().unwrap_or(json!(""));
        if let Value::Number(n) = &raw {
            return Ok(n.as_f64().unwrap_or(0.0));
        }
        let text = match &raw {
            Value::Null => String::new(),
            other => ctx.vars.interpolate(&pyfmt::str(other)),
        };
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("{} is empty", key));
        }
        pyfmt::parse_float(text).ok_or_else(|| {
            format!(
                "{} did not resolve to a number: {}",
                key,
                pyfmt::quote(text)
            )
        })
    }
}

#[async_trait]
impl FlowNode for VarCondition {
    fn node_type(&self) -> &'static str {
        "varCondition"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        const ABORT: &str = "Var check aborted: ";
        let left = match Self::operand(ctx, data, "leftValue") {
            Ok(v) => v,
            Err(message) => return undecided(ctx, ABORT, message),
        };
        let right = match Self::operand(ctx, data, "rightValue") {
            Ok(v) => v,
            Err(message) => return undecided(ctx, ABORT, message),
        };
        let operator = ctx.get_str(data, "operator", ">");
        if !COMPARISON_OPERATORS.contains(&operator.as_str()) {
            return undecided(
                ctx,
                ABORT,
                format!(
                    "operator {} is not one of {}",
                    pyfmt::quote(&operator),
                    COMPARISON_OPERATORS.join(", ")
                ),
            );
        }
        let met = compare(left, &operator, right);
        ctx.info(format!(
            "Var check: {} {} {} = {}",
            pyfmt::float(left),
            operator,
            pyfmt::float(right),
            py_bool(met)
        ));
        NodeOutcome::result(json!({
            "status": "success",
            "condition": met,
            "left": float_value(left),
            "operator": operator,
            "right": float_value(right),
        }))
    }
}

#[cfg(test)]
mod tests;
