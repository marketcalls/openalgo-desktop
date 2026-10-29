//! The graph walk (the web's `execute_node_chain`,
//! `flow_executor_service.py` 4348-4673): depth first from the trigger,
//! each node's outcome deciding which wires the walk follows.
//!
//! The rules the web learned from defects, kept here:
//!
//! - A condition or gate reached by several paths is evaluated once per
//!   run; a later arrival returns (two orders from one crossover otherwise).
//! - A gate waits until every wired input has a result; the input that
//!   finishes last evaluates it. A gate configured for more inputs than are
//!   wired fails rather than deciding on part of its condition.
//! - A wire into a gate carries a value, not control: it is always
//!   followed from an evaluated condition, whichever branch it leaves.
//! - A failed node stops its branch; a condition that could not be
//!   evaluated takes neither branch and records no result (so a gate it
//!   feeds stays waiting), but its unhandled wires still run.
//! - An order node with an unresolved `{{...}}` in a field that reaches the
//!   broker fails before it is sent.

use crate::flow::contract::model::{
    Edge, Graph, FALSE_HANDLES, GATE_TYPES, ONCE_PER_RUN_TYPES, ORDER_CRITICAL_FIELDS, ORDER_TYPES,
    TRUE_HANDLES,
};
use crate::flow::contract::node::{NodeCtx, NodeOutcome, ERROR, WARNING};
use crate::flow::contract::pyfmt;
use crate::flow::contract::vars::has_unresolved;
use crate::flow::nodes::NodeRegistry;
use futures_util::future::BoxFuture;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Deepest chain a run may follow (`MAX_NODE_DEPTH`).
pub const MAX_NODE_DEPTH: usize = 100;
/// Most node visits in one run (`MAX_NODE_VISITS`).
pub const MAX_NODE_VISITS: usize = 500;

/// What ends a run before the walk finishes (the web raises): the text is
/// the run's error.
pub type Abort = String;

/// What a stopped run reads.
pub const STOPPED: &str = "The workflow was stopped because OpenAlgo was signed out or is closing.";

/// One run's walk over a graph.
pub struct Walk<'a> {
    graph: &'a Graph,
    nodes: &'a NodeRegistry,
    ctx: &'a NodeCtx,
    outgoing: HashMap<&'a str, Vec<&'a Edge>>,
    incoming: HashMap<&'a str, Vec<&'a Edge>>,
    gates: HashSet<&'a str>,
    visits: usize,
    /// The nodes that failed, `{node, type, message}` (`executor.errors`).
    pub errors: Vec<Value>,
}

impl<'a> Walk<'a> {
    pub fn new(graph: &'a Graph, nodes: &'a NodeRegistry, ctx: &'a NodeCtx) -> Self {
        let mut outgoing: HashMap<&str, Vec<&Edge>> = HashMap::new();
        let mut incoming: HashMap<&str, Vec<&Edge>> = HashMap::new();
        for e in &graph.edges {
            outgoing.entry(e.source.as_str()).or_default().push(e);
            incoming.entry(e.target.as_str()).or_default().push(e);
        }
        let gates = graph
            .nodes
            .iter()
            .filter(|n| GATE_TYPES.contains(&n.node_type.as_str()))
            .map(|n| n.id.as_str())
            .collect();
        Self {
            graph,
            nodes,
            ctx,
            outgoing,
            incoming,
            gates,
            visits: 0,
            errors: Vec::new(),
        }
    }

    fn record_error(&mut self, node_id: &str, node_type: &str, message: &str) {
        self.errors
            .push(json!({"node": node_id, "type": node_type, "message": message}));
    }

    /// Visit `node_id` and everything its outcome leads to.
    pub fn visit<'s>(
        &'s mut self,
        node_id: &'a str,
        depth: usize,
    ) -> BoxFuture<'s, Result<(), Abort>> {
        Box::pin(async move {
            if depth > MAX_NODE_DEPTH {
                return Err(format!("Maximum node depth ({}) exceeded", MAX_NODE_DEPTH));
            }
            if self.visits >= MAX_NODE_VISITS {
                return Err(format!(
                    "Maximum node visits ({}) exceeded",
                    MAX_NODE_VISITS
                ));
            }
            if self.ctx.cancel.is_cancelled() {
                return Err(STOPPED.into());
            }
            self.visits += 1;
            let Some(node) = self.graph.node(node_id) else {
                return Ok(());
            };
            let node_type = node.node_type.as_str();
            if ONCE_PER_RUN_TYPES.contains(&node_type) && self.ctx.vars.condition(node_id).is_some()
            {
                return Ok(());
            }
            let Some(outcome) = self.evaluate(node_id, node_type, &node.data).await else {
                return Ok(());
            };
            if let Some(reason) = outcome.aborted() {
                return Err(reason.to_string());
            }
            if outcome.is_error() {
                let message = outcome.failure_message();
                self.record_error(node_id, node_type, &message);
                if outcome.condition_met().is_some() {
                    self.ctx.log(
                        format!("{} could not be evaluated ({})", node_type, message),
                        ERROR,
                    );
                } else {
                    self.ctx.log(
                        format!(
                            "Stopping this branch: {} node failed ({})",
                            node_type, message
                        ),
                        ERROR,
                    );
                    return Ok(());
                }
            }
            for target in self.next(node_id, &outcome) {
                self.visit(target, depth + 1).await?;
            }
            Ok(())
        })
    }

    /// Run the node; `None` when the walk returns here without following
    /// any wire (a gate still waiting, or one refused for missing inputs).
    async fn evaluate(
        &mut self,
        node_id: &str,
        node_type: &str,
        data: &Map<String, Value>,
    ) -> Option<NodeOutcome> {
        if ORDER_TYPES.contains(&node_type) {
            let blocked = self.unresolved_order_fields(data);
            if !blocked.is_empty() {
                return Some(self.unresolved_error(node_type, &blocked));
            }
        }
        if GATE_TYPES.contains(&node_type) {
            return self.gate(node_id, node_type, data);
        }
        match self.nodes.get(node_type) {
            Some(n) => Some(n.run(self.ctx, data).await),
            None => {
                self.ctx
                    .log(format!("Unknown node type: {}", node_type), WARNING);
                Some(NodeOutcome::none())
            }
        }
    }

    /// The wires to follow from a node with this outcome.
    fn next(&self, node_id: &str, outcome: &NodeOutcome) -> Vec<&'a str> {
        let edges = self.outgoing.get(node_id).cloned().unwrap_or_default();
        let Some(met) = outcome.condition_met() else {
            return edges.into_iter().map(|e| e.target.as_str()).collect();
        };
        let errored = outcome.is_error();
        if !errored {
            self.ctx.vars.set_condition(node_id, met);
        }
        edges
            .into_iter()
            .filter(|e| {
                let handle = e.source_handle.as_deref().unwrap_or("");
                let is_true = TRUE_HANDLES.contains(&handle);
                let is_false = FALSE_HANDLES.contains(&handle);
                let branch = is_true || is_false;
                if errored {
                    !branch
                } else {
                    self.gates.contains(e.target.as_str())
                        || (met && is_true)
                        || (!met && is_false)
                        || !branch
                }
            })
            .map(|e| e.target.as_str())
            .collect()
    }

    /// Order fields still holding an unresolved reference, sorted
    /// (`unresolved_order_fields`).
    fn unresolved_order_fields(&self, data: &Map<String, Value>) -> Vec<(String, String)> {
        let mut found: Vec<(String, String)> = ORDER_CRITICAL_FIELDS
            .iter()
            .filter_map(|key| {
                let Some(Value::String(raw)) = data.get(*key) else {
                    return None;
                };
                if !raw.contains("{{") {
                    return None;
                }
                match self.ctx.vars.resolve_raw(&Value::String(raw.clone())) {
                    Value::String(s) if has_unresolved(&s) => Some((key.to_string(), s)),
                    _ => None,
                }
            })
            .collect();
        found.sort();
        found
    }

    /// The failure of an order node blocked by unresolved references
    /// (`unresolved_error`).
    fn unresolved_error(&self, node_type: &str, fields: &[(String, String)]) -> NodeOutcome {
        let detail = fields
            .iter()
            .map(|(k, t)| format!("{}={}", k, pyfmt::quote(t)))
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "{} has unresolved variable(s) in {}. Refusing to send it with substituted values.",
            node_type, detail
        );
        self.ctx.error(message.clone());
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        NodeOutcome::result(json!({"status": "error", "message": message, "unresolved": keys}))
    }

    /// An AND, OR or NOT gate over its inputs' stored results, once every
    /// wired input has one.
    fn gate(
        &mut self,
        node_id: &str,
        node_type: &str,
        data: &Map<String, Value>,
    ) -> Option<NodeOutcome> {
        if self.ctx.vars.condition(node_id).is_some() {
            return None;
        }
        let wired = self.incoming.get(node_id).cloned().unwrap_or_default();
        let mut inputs = Vec::new();
        let mut pending = 0;
        for e in &wired {
            match self.ctx.vars.condition(&e.source) {
                Some(r) => inputs.push(r),
                None => pending += 1,
            }
        }
        if pending > 0 {
            self.ctx.info(format!(
                "{}: waiting for {} more input(s) before evaluating",
                node_type, pending
            ));
            return None;
        }
        if let Some(declared) = declared_inputs(node_type, data).filter(|d| *d > wired.len() as i64)
        {
            let message = format!(
                "{} is configured for {} inputs but only {} are wired. Wire the rest, or lower the input count, rather than evaluating on part of the condition.",
                node_type,
                declared,
                wired.len()
            );
            self.ctx.error(message.clone());
            self.record_error(node_id, node_type, &message);
            return None;
        }
        Some(self.evaluate_gate(node_type, &inputs))
    }

    fn evaluate_gate(&self, node_type: &str, inputs: &[bool]) -> NodeOutcome {
        let shown = pyfmt::repr(&json!(inputs));
        let met = match node_type {
            "andGate" | "orGate" if inputs.is_empty() => return NodeOutcome::condition(false),
            "andGate" => {
                let met = inputs.iter().all(|b| *b);
                self.ctx
                    .info(format!("AND Gate: {} -> {}", shown, py_bool(met)));
                met
            }
            "orGate" => {
                let met = inputs.iter().any(|b| *b);
                self.ctx
                    .info(format!("OR Gate: {} -> {}", shown, py_bool(met)));
                met
            }
            _ => {
                let input = inputs.first().copied().unwrap_or(false);
                self.ctx.info(format!(
                    "NOT Gate: {} -> {}",
                    py_bool(input),
                    py_bool(!input)
                ));
                !input
            }
        };
        NodeOutcome::condition(met)
    }
}

/// The inputs a gate is configured for. AND and OR read it as the
/// validator and the editor do: absent, blank or unreadable is the two
/// slots the editor draws, else at least 1 (the web's executor skipped the
/// check when it was absent, so a default AND gate with one wire deleted
/// decided on one input: parity N-gate-04, lead decision 8). NOT keeps the
/// web's rule: only a whole number (or a boolean, as Python counts it).
fn declared_inputs(node_type: &str, data: &Map<String, Value>) -> Option<i64> {
    let raw = data.get("inputCount");
    if node_type == "notGate" {
        return match raw {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::Bool(b)) => Some(i64::from(*b)),
            _ => None,
        };
    }
    let slots = match raw {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(Value::Number(n)) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.is_finite())
                .map(|f| f.trunc() as i64)
        }),
        Some(Value::Bool(b)) => Some(i64::from(*b)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().or(Some(2)),
        Some(_) => Some(2),
    };
    Some(slots.map(|n| n.max(1)).unwrap_or(2))
}

fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}
