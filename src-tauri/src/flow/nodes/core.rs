//! The nodes whose run is only a log line (the web's `execute_node_chain`
//! arms for `start`, `group`, `webhookTrigger` and `orderUpdateTrigger`).
//! A trigger's registration (schedule, webhook, order watch) is the trigger
//! side's; when the run reaches the node it only records that it started.

use super::NodeFamily;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::Arc;

pub const FAMILY: NodeFamily = NodeFamily {
    key: "core",
    build: || {
        vec![
            Arc::new(LogOnly {
                node_type: "start",
                line: Some("Workflow started"),
            }),
            // A group is a container on the canvas; the run passes through.
            Arc::new(LogOnly {
                node_type: "group",
                line: None,
            }),
            Arc::new(LogOnly {
                node_type: "webhookTrigger",
                line: Some("Webhook trigger activated"),
            }),
            Arc::new(LogOnly {
                node_type: "orderUpdateTrigger",
                line: Some("Order-update trigger activated"),
            }),
        ]
    },
};

/// A node that writes one line (or nothing) and lets every child run.
struct LogOnly {
    node_type: &'static str,
    line: Option<&'static str>,
}

#[async_trait]
impl FlowNode for LogOnly {
    fn node_type(&self) -> &'static str {
        self.node_type
    }

    async fn run(&self, ctx: &NodeCtx, _data: &Map<String, Value>) -> NodeOutcome {
        if let Some(line) = self.line {
            ctx.info(line);
        }
        NodeOutcome::none()
    }
}
