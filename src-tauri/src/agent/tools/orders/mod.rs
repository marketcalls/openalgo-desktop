//! The order toolkit (the web's `services/agent/tools/orders.py`): the only
//! place the Agent can change a real account.
//!
//! Seven tools, every one of them pausing for the trader's approval before
//! it runs (static, so no content can waive it), then the risk guard inside
//! the tool, then the platform through `/api/v1`. The toolkit is built only
//! for a run whose trading capability is on (the trader's switch ANDed with
//! the request), so a run without trading has no order tool in its schema
//! at all, on any surface.
//!
//! The tools' names, descriptions and argument schemas are the web's,
//! recorded from its toolkit into `specs.json`
//! (`scripts/agent/capture_web_orders.py`); never edit that file by hand.
//!
//! What lives where: [`validate`] (arguments), [`reads`] (what the guard
//! is told), [`plans`] (what each tool means), [`smart`] (what a smart
//! order really sends), [`pipeline`] (the five steps every tool runs),
//! [`preview`] (the approval card's view of a smart order), and the
//! vocabulary shared with the read-only account tools in
//! `super::order_vocab`.

mod pipeline;
mod plans;
mod preview;
mod reads;
pub mod smart;
mod validate;

pub use preview::smart_order_preview;

#[cfg(test)]
mod tests;

use crate::agent::contract::tool::{
    AgentTool, Capability, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::safety::risk::num::py_str;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::{Arc, LazyLock};

/// The strategy every Agent order is tagged with, so the order book, the
/// logs and the alerts show it was the Agent's. The model cannot set it.
pub const AGENT_STRATEGY: &str = "OpenAlgo Agent";

/// Every order tool, in the order the model sees them. All of them change
/// the account, so all of them need approval: the two lists are one.
pub const MUTATING_TOOLS: [&str; 7] = [
    "place_order",
    "place_smart_order",
    "modify_order",
    "cancel_order",
    "cancel_all_orders",
    "close_position",
    "close_all_positions",
];

/// The toolkit (the web's `ToolkitSpec` for `orders`).
pub const GROUP: ToolGroup = ToolGroup {
    key: "orders",
    surfaces: Surfaces::ALL,
    requires: &[Capability::Trading],
    order: 50,
    description: "Place, modify, cancel and close real orders and positions. Every tool requires human approval and runs the risk guard.",
    build,
};

const SPECS_JSON: &str = include_str!("specs.json");

/// The recorded specs, one per [`MUTATING_TOOLS`] name, in that order.
static SPECS: LazyLock<Vec<ToolSpec>> = LazyLock::new(|| {
    let recorded: Vec<Value> = match serde_json::from_str(SPECS_JSON) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("The order tools' recorded specs could not be read: {}", e);
            return Vec::new();
        }
    };
    MUTATING_TOOLS
        .iter()
        .filter_map(|name| {
            let spec = recorded.iter().find(|s| s["name"] == *name)?;
            Some(ToolSpec {
                name,
                description: spec["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
                    .into(),
                parameters: spec["parameters"].clone(),
                needs_approval: true,
            })
        })
        .collect()
});

/// The order tools a run may use. An order tool whose recorded spec is
/// missing is not built at all, never built without its approval.
pub fn specs() -> &'static [ToolSpec] {
    &SPECS
}

fn build(_ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let tools: Vec<Arc<dyn AgentTool>> = specs()
        .iter()
        .map(|spec| Arc::new(OrderTool(spec.clone())) as Arc<dyn AgentTool>)
        .collect();
    if tools.len() != MUTATING_TOOLS.len() {
        tracing::error!(
            "Only {} of the {} order tools could be built; the rest are withheld",
            tools.len(),
            MUTATING_TOOLS.len()
        );
    }
    tools
}

/// One order tool.
pub struct OrderTool(ToolSpec);

/// The wrapper's labels: the raw arguments the web labels its result with.
fn attributes(
    args: &Map<String, Value>,
    keys: &[(&'static str, &str)],
) -> Vec<(&'static str, String)> {
    keys.iter()
        .map(|(label, key)| (*label, py_str(args.get(*key).unwrap_or(&Value::Null))))
        .collect()
}

#[async_trait]
impl AgentTool for OrderTool {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        let name = self.0.name;
        let a = &args;
        let place = [("symbol", "symbol"), ("exchange", "exchange")];
        match name {
            "place_order" => {
                pipeline::run_mutation(
                    ctx,
                    name,
                    a,
                    attributes(a, &place),
                    plans::place_order(ctx, a),
                )
                .await
            }
            "place_smart_order" => {
                pipeline::run_mutation(
                    ctx,
                    name,
                    a,
                    attributes(a, &place),
                    plans::place_smart_order(ctx, a),
                )
                .await
            }
            "modify_order" => {
                let labels = attributes(
                    a,
                    &[
                        ("symbol", "symbol"),
                        ("exchange", "exchange"),
                        ("order_id", "order_id"),
                    ],
                );
                pipeline::run_mutation(ctx, name, a, labels, plans::modify_order(ctx, a)).await
            }
            "cancel_order" => {
                let labels = attributes(a, &[("order_id", "order_id")]);
                pipeline::run_mutation(ctx, name, a, labels, plans::cancel_order(ctx, a)).await
            }
            "cancel_all_orders" => {
                pipeline::run_mutation(
                    ctx,
                    name,
                    &Map::new(),
                    Vec::new(),
                    plans::cancel_all_orders(ctx, a),
                )
                .await
            }
            "close_position" => {
                pipeline::run_mutation(
                    ctx,
                    name,
                    a,
                    attributes(a, &place),
                    plans::close_position(ctx, a),
                )
                .await
            }
            "close_all_positions" => {
                pipeline::run_mutation(
                    ctx,
                    name,
                    &Map::new(),
                    Vec::new(),
                    plans::close_all_positions(ctx, a),
                )
                .await
            }
            other => ToolOutcome::Retry(format!("{} is not an order tool.", other)),
        }
    }
}
