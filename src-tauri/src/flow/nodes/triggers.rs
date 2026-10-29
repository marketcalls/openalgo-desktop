//! The priceAlert node (the web's `execute_price_alert`,
//! `flow_executor_service.py` 3672-3755). Its registration as a trigger,
//! the 5 s poll, is the price monitor's (`flow::triggers::price_monitor`);
//! here is what the node does when a run reaches it.
//!
//! Departures from the web, each a decided fix:
//!
//! - Only a run the price monitor started skips the quote (N-priceAlert-04):
//!   the web trusted any `webhook` payload saying `trigger_type:
//!   price_alert`, so a webhook caller could post one and take the `true`
//!   branch without the price ever being checked. The desktop reads the
//!   run's own trigger, which no payload can set.
//! - A failure carries its reason as `message` (N-priceAlert-05,
//!   N-priceAlert-07), so the run log says why instead of `node failed`.
//! - A level that is given but does not resolve to a number fails closed
//!   (N-priceAlert-09): the web read `above {{typo}}` as `above 0`, true on
//!   any positive price.

use super::NodeFamily;
use crate::flow::contract::client::failure_text;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome, RunTrigger};
use crate::flow::contract::pyfmt;
use crate::flow::node_contracts::normalize_condition;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "triggers",
    build: || vec![Arc::new(PriceAlert)],
};

/// The conditions that compare against the previous tick, which only the
/// monitor keeps.
const NEEDS_PREVIOUS_TICK: &[&str] = &[
    "moving_up",
    "moving_down",
    "moving_up_percent",
    "moving_down_percent",
];

/// A price level as the node reads it: absent (the web's default 0), a
/// number, or given but not a number.
#[derive(Debug, Clone, PartialEq)]
enum Level {
    Absent,
    Number(f64),
    NotANumber(String),
}

impl Level {
    fn read(ctx: &NodeCtx, data: &Map<String, Value>, key: &str) -> Level {
        match data.get(key) {
            None | Some(Value::Null) => Level::Absent,
            Some(Value::String(s)) if s.is_empty() => Level::Absent,
            Some(Value::String(s)) => {
                let text = ctx.vars.interpolate(s);
                pyfmt::parse_float(&text)
                    .map(Level::Number)
                    .unwrap_or(Level::NotANumber(text))
            }
            Some(other) => pyfmt::to_float(other)
                .map(Level::Number)
                .unwrap_or_else(|| Level::NotANumber(pyfmt::str(other))),
        }
    }

    fn value(&self) -> f64 {
        match self {
            Level::Number(f) => *f,
            _ => 0.0,
        }
    }

    /// As the web's log line prints it: a float, or the default `0`.
    fn text(&self) -> String {
        match self {
            Level::Number(f) => pyfmt::float(*f),
            _ => "0".into(),
        }
    }
}

/// Whether `condition` reads the channel bounds.
fn uses_channel(condition: &str) -> bool {
    matches!(
        condition,
        "entering_channel" | "inside_channel" | "exiting_channel" | "outside_channel"
    )
}

/// The first level the condition reads that is given but not a number, as
/// the sentence that refuses the alert.
fn unreadable_level(condition: &str, levels: [(&str, &Level); 3]) -> Option<String> {
    levels
        .into_iter()
        .filter(|(name, _)| *name == "price" || uses_channel(condition))
        .find_map(|(name, level)| match level {
            Level::NotANumber(text) => Some(format!(
                "Price alert {} {} is not a number, so the alert was not evaluated.",
                name,
                pyfmt::quote(text)
            )),
            _ => None,
        })
}

/// The level test on one quote (no previous tick: a crossing is read as
/// its level, as the web does).
fn level_met(condition: &str, ltp: f64, price: f64, lower: f64, upper: f64) -> Option<bool> {
    Some(match condition {
        "greater_than" | "crossing_up" => ltp > price,
        "less_than" | "crossing_down" => ltp < price,
        "crossing" => (ltp - price).abs() <= price * 0.001,
        "entering_channel" | "inside_channel" => lower <= ltp && ltp <= upper,
        "exiting_channel" | "outside_channel" => ltp < lower || ltp > upper,
        _ => return None,
    })
}

/// `{"status": "error", "condition": false, "ltp": ltp, "message": ...}`.
fn failed_with_ltp(ltp: f64, message: String) -> NodeOutcome {
    NodeOutcome::result(
        json!({"status": "error", "condition": false, "ltp": ltp, "message": message}),
    )
}

struct PriceAlert;

impl PriceAlert {
    /// A run the price monitor started: its tick already met the alert.
    fn fired_by_monitor(&self, ctx: &NodeCtx) -> Option<f64> {
        if ctx.trigger != RunTrigger::PriceAlert {
            return None;
        }
        let webhook = ctx.vars.get("webhook")?;
        if webhook.get("trigger_type").and_then(Value::as_str) != Some("price_alert") {
            return None;
        }
        Some(
            webhook
                .get("trigger_price")
                .filter(|v| pyfmt::truthy(v))
                .and_then(pyfmt::to_float)
                .unwrap_or(0.0),
        )
    }
}

#[async_trait]
impl FlowNode for PriceAlert {
    fn node_type(&self) -> &'static str {
        "priceAlert"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let symbol = ctx.get_str(data, "symbol", "");
        let exchange = ctx.get_str(data, "exchange", "NSE");
        let condition = normalize_condition(&ctx.get_str(data, "condition", "greater_than"));
        let price = Level::read(ctx, data, "price");
        let lower = Level::read(ctx, data, "priceLower");
        let upper = Level::read(ctx, data, "priceUpper");
        if symbol.is_empty() {
            return NodeOutcome::result(json!({"status": "error", "condition": false}));
        }
        if let Some(refusal) = unreadable_level(
            &condition,
            [
                ("price", &price),
                ("lower price", &lower),
                ("upper price", &upper),
            ],
        ) {
            ctx.error(refusal.clone());
            return NodeOutcome::undecided(refusal);
        }
        if let Some(ltp) = self.fired_by_monitor(ctx) {
            ctx.info(format!(
                "Price alert fired by monitor: {} LTP={} {} {}",
                symbol,
                pyfmt::float(ltp),
                condition,
                price.text()
            ));
            ctx.store_output(data, json!({"ltp": ltp, "condition_met": true}));
            return NodeOutcome::result(
                json!({"status": "success", "condition": true, "ltp": ltp}),
            );
        }
        let quote = ctx.client.get_quotes(&symbol, &exchange).await;
        if quote.get("status").and_then(Value::as_str) != Some("success") {
            return match failure_text(&quote) {
                Some(reason) => NodeOutcome::undecided(reason),
                None => NodeOutcome::result(json!({"status": "error", "condition": false})),
            };
        }
        let ltp = quote
            .get("data")
            .filter(|d| pyfmt::truthy(d))
            .and_then(|d| d.get("ltp"))
            .and_then(pyfmt::to_float)
            .unwrap_or(0.0);
        let Some(met) = level_met(&condition, ltp, price.value(), lower.value(), upper.value())
        else {
            let message = if NEEDS_PREVIOUS_TICK.contains(&condition.as_str()) {
                format!(
                    "Price alert condition {} needs the previous tick and can only be evaluated by the price monitor; wire this node as the trigger.",
                    pyfmt::quote(&condition)
                )
            } else {
                format!(
                    "Price alert has an unrecognized condition {}; it can never be true.",
                    pyfmt::repr(data.get("condition").unwrap_or(&Value::Null))
                )
            };
            ctx.error(message.clone());
            return failed_with_ltp(ltp, message);
        };
        ctx.info(format!(
            "Price alert: {} LTP={} {} {} = {}",
            symbol,
            pyfmt::float(ltp),
            condition,
            price.text(),
            if met { "True" } else { "False" }
        ));
        ctx.store_output(data, json!({"ltp": ltp, "condition_met": met}));
        NodeOutcome::result(json!({"status": "success", "condition": met, "ltp": ltp}))
    }
}

#[cfg(test)]
mod tests;
