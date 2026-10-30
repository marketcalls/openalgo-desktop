//! The workflow as the web stores, exports and imports it, and the records
//! a run leaves (`database/flow_db.py`, `docs/prompt/flow-import-format.md`).
//!
//! The stored graph is the web's JSON, kept as JSON: `nodes` and `edges`
//! are saved and served exactly as the editor sent them, so every field the
//! desktop does not read (positions, sizes, styles, a future web field)
//! survives a save, an export and an import, in either direction. The walk
//! reads a typed view of it, [`Graph`], whose nodes and edges keep their
//! other fields in `extra`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Every node type the editor can draw and the executor can run (the web's
/// `VALID_NODE_TYPES`, kept in lockstep with the editor's node index).
pub const NODE_TYPES: &[&str] = &[
    "andGate",
    "barOffset",
    "basketOrder",
    "calendar",
    "cancelAllOrders",
    "cancelOrder",
    "closePositions",
    "delay",
    "expiry",
    "fundCheck",
    "funds",
    "getDepth",
    "getOrderStatus",
    "getQuote",
    "group",
    "history",
    "holdings",
    "holidays",
    "httpRequest",
    "indicator",
    "intervals",
    "log",
    "margin",
    "mathExpression",
    "modifyOrder",
    "multiQuotes",
    "notGate",
    "openPosition",
    "optionChain",
    "optionSymbol",
    "optionsMultiOrder",
    "optionsOrder",
    "orGate",
    "orderBook",
    "orderUpdateTrigger",
    "placeOrder",
    "positionBook",
    "positionCheck",
    "priceAlert",
    "priceCondition",
    "priorPeriodOhlc",
    "smartOrder",
    "splitOrder",
    "start",
    "strategyPnl",
    "subscribeDepth",
    "subscribeLtp",
    "subscribeQuote",
    "symbol",
    "syntheticFuture",
    "telegramAlert",
    "timeCondition",
    "timeWindow",
    "timings",
    "tradeBook",
    "unsubscribe",
    "varCondition",
    "variable",
    "waitUntil",
    "webhookTrigger",
    "whatsappAlert",
];

/// The node types a run can start from (the web's trigger list; the first
/// one in the stored node order is the run's start).
pub const TRIGGER_TYPES: &[&str] = &[
    "start",
    "webhookTrigger",
    "priceAlert",
    "orderUpdateTrigger",
];

/// Logic gates: combine the stored booleans of their inputs; a wire into
/// one carries a value, not control (`GATE_NODE_TYPES`).
pub const GATE_TYPES: &[&str] = &["andGate", "orGate", "notGate"];

/// Nodes that fork into a TRUE and a FALSE branch, evaluated once per run
/// however many paths reach them (`BRANCHING_NODE_TYPES`).
pub const BRANCHING_TYPES: &[&str] = &[
    "priceCondition",
    "timeCondition",
    "timeWindow",
    "positionCheck",
    "fundCheck",
    "varCondition",
];

/// Nodes evaluated once per run, however many paths reach them: the
/// branching nodes and priceAlert, which branches too. The web leaves
/// priceAlert out, so one reached by two paths fetched the quote and fired
/// its branch per path (parity N-logic-04, lead decision 8).
pub const ONCE_PER_RUN_TYPES: &[&str] = &[
    "priceCondition",
    "timeCondition",
    "timeWindow",
    "positionCheck",
    "fundCheck",
    "varCondition",
    "priceAlert",
];

/// Nodes that place, change or cancel something at the broker. An
/// unresolved `{{...}}` in one of their [`ORDER_CRITICAL_FIELDS`] fails the
/// node before it is sent (`ORDER_NODE_TYPES`).
pub const ORDER_TYPES: &[&str] = &[
    "placeOrder",
    "smartOrder",
    "optionsOrder",
    "optionsMultiOrder",
    "basketOrder",
    "splitOrder",
    "modifyOrder",
    "cancelOrder",
    "closePositions",
];

/// The fields of an order node that decide what reaches the broker
/// (`ORDER_CRITICAL_FIELDS`). Labels (`strategy`, `strategyTag`,
/// `outputVariable`) are left out on purpose.
pub const ORDER_CRITICAL_FIELDS: &[&str] = &[
    "symbol",
    "exchange",
    "action",
    "quantity",
    "product",
    "priceType",
    "pricetype",
    "price",
    "triggerPrice",
    "splitSize",
    "positionSize",
    "underlying",
    "strike",
    "optionType",
    "expiryDate",
    "orderId",
    "newQuantity",
    "newPrice",
    "newTriggerPrice",
    "orders",
    "legs",
];

/// Edge handles that mean "the condition held" and "it did not"; nodes use
/// either vocabulary (`yes`/`no` on NotGate and TimeCondition).
pub const TRUE_HANDLES: &[&str] = &["yes", "true"];
pub const FALSE_HANDLES: &[&str] = &["no", "false"];

/// Whether `t` is a node type the executor knows.
pub fn is_node_type(t: &str) -> bool {
    NODE_TYPES.contains(&t)
}

/// One node of the graph: its id, type and `data`, with every other field
/// the editor stored (`position`, `width`, `parentId`, ...) kept as is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type", default)]
    pub node_type: String,
    #[serde(default, deserialize_with = "object_or_empty")]
    pub data: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One edge: source, target and the source handle the branch filter
/// reads, with every other field kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub source: String,
    pub target: String,
    #[serde(
        rename = "sourceHandle",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_handle: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `data: null` (or any non-object) reads as `{}`, as the web's
/// `node.get("data", {})` then `.get(...)` would fail closed on it only
/// after the validator has already refused it.
fn object_or_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Map<String, Value>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Object(m) => m,
        _ => Map::new(),
    })
}

/// The typed view of a stored graph the walk reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    /// Read stored `nodes` and `edges` JSON. A malformed node or edge is an
    /// error naming it; the validator refuses such a graph before any run,
    /// so this only fails on a graph stored before validation existed.
    pub fn parse(nodes: &Value, edges: &Value) -> Result<Graph, String> {
        let list = |v: &Value, what: &str| -> Result<Vec<Value>, String> {
            match v {
                Value::Array(a) => Ok(a.clone()),
                Value::Null => Ok(Vec::new()),
                _ => Err(format!("The workflow's {} are not a list", what)),
            }
        };
        let nodes = list(nodes, "nodes")?
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                serde_json::from_value::<Node>(n)
                    .map_err(|e| format!("Node {} is malformed: {}", i, e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let edges = list(edges, "edges")?
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                serde_json::from_value::<Edge>(e)
                    .map_err(|e| format!("Edge {} is malformed: {}", i, e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Graph { nodes, edges })
    }

    /// The first node with this id (the web's `next(n for n in nodes ...)`).
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The run's start: the first trigger node in stored order.
    pub fn trigger(&self) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| TRIGGER_TYPES.contains(&n.node_type.as_str()))
    }
}

/// A stored workflow (`flow_workflows`). The webhook secret is held
/// encrypted at rest and decrypted only to answer the signed-in trader and
/// to check a webhook.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    /// The stored `nodes` JSON (an array), served back as stored.
    pub nodes: Value,
    /// The stored `edges` JSON (an array), served back as stored.
    pub edges: Value,
    pub is_active: bool,
    pub schedule_job_id: Option<String>,
    pub webhook_token: Option<String>,
    /// `None` when there is none yet, or it was made with keys that have
    /// since changed (a fresh start): a webhook then never runs.
    pub webhook_secret: Option<crate::security::Secret>,
    pub webhook_enabled: bool,
    /// `payload` (secret in the JSON body) or `url` (`?secret=`).
    pub webhook_auth_type: String,
    /// `YYYY-MM-DDTHH:MM:SS` in UTC, as the web serves SQLite's timestamps.
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// One row of the workflow list, with its newest run's status.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSummary {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub webhook_enabled: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_execution_status: Option<String>,
}

impl Workflow {
    /// The typed graph the walk reads.
    pub fn graph(&self) -> Result<Graph, String> {
        Graph::parse(&self.nodes, &self.edges)
    }
}

/// How a run ended, or that it is still going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        }
    }
}

/// One run of a workflow (`flow_workflow_executions`): its status, its
/// times and the node trace the run wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Execution {
    pub id: i64,
    pub workflow_id: i64,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// The run's log entries, `[{time, message, level}]`.
    pub logs: Value,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let nodes = json!([{
            "id": "n1", "type": "start", "position": {"x": 1, "y": 2},
            "data": {"scheduleType": "manual", "label": "Start"},
            "measured": {"width": 10}, "selected": false
        }]);
        let edges = json!([{"id": "e1", "source": "n1", "target": "n2",
            "sourceHandle": "true", "animated": true, "style": {"stroke": "#fff"}}]);
        let g = Graph::parse(&nodes, &edges).unwrap();
        assert_eq!(g.nodes[0].node_type, "start");
        assert_eq!(g.edges[0].source_handle.as_deref(), Some("true"));
        assert_eq!(serde_json::to_value(&g.nodes).unwrap(), nodes);
        assert_eq!(serde_json::to_value(&g.edges).unwrap(), edges);
    }

    #[test]
    fn the_first_trigger_in_stored_order_starts_the_run() {
        let nodes = json!([
            {"id": "a", "type": "log", "data": {}},
            {"id": "w", "type": "webhookTrigger", "data": null},
            {"id": "s", "type": "start", "data": {}}
        ]);
        let g = Graph::parse(&nodes, &json!([])).unwrap();
        assert_eq!(g.trigger().map(|n| n.id.as_str()), Some("w"));
        assert!(g.node("w").is_some_and(|n| n.data.is_empty()));
    }

    #[test]
    fn the_type_lists_agree_with_each_other() {
        for list in [
            TRIGGER_TYPES,
            GATE_TYPES,
            BRANCHING_TYPES,
            ONCE_PER_RUN_TYPES,
            ORDER_TYPES,
        ] {
            for t in list {
                assert!(is_node_type(t), "{} is not a node type", t);
            }
        }
        let mut sorted = NODE_TYPES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), NODE_TYPES.len());
        assert_eq!(NODE_TYPES.len(), 61);
    }
}
