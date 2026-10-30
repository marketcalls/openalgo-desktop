//! The walk's rules (parity E-22..E-30, N-gate-*, N-common-03/04) and a
//! run's lifecycle (E-31..E-41), ported from the web's executor tests
//! (`test_flow_qa_regressions.py`, `test_flow_lifecycle_guards.py`) and the
//! defects their comments name.

use super::walk::{Walk, MAX_NODE_DEPTH, MAX_NODE_VISITS};
use super::*;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::model::Graph;
use crate::flow::contract::node::{FlowNode, NodeOutcome};
use crate::flow::nodes::{NodeFamily, NodeRegistry, FAMILIES};
use async_trait::async_trait;
use serde_json::Map;

/// A node that logs `ran <id-from-data>` and answers what its data says:
/// `{"answer": <dict>}` as its result, `{"abort": "..."}` to end the run.
struct Scripted(&'static str);

#[async_trait]
impl FlowNode for Scripted {
    fn node_type(&self) -> &'static str {
        self.0
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        ctx.info(format!(
            "ran {}",
            pyfmt::str(data.get("tag").unwrap_or(&json!("?")))
        ));
        if let Some(reason) = data.get("abort").and_then(Value::as_str) {
            return NodeOutcome::abort(reason);
        }
        match data.get("answer") {
            Some(v) => NodeOutcome::result(v.clone()),
            None => NodeOutcome::none(),
        }
    }
}

const TEST_FAMILY: NodeFamily = NodeFamily {
    key: "test",
    build: || {
        vec![
            Arc::new(Scripted("placeOrder")),
            Arc::new(Scripted("priceCondition")),
            Arc::new(Scripted("getQuote")),
            Arc::new(Scripted("telegramAlert")),
            Arc::new(Scripted("priceAlert")),
        ]
    },
};

/// The scripted nodes first: a type registered twice keeps the first, and
/// the logic family registers the real priceCondition and telegramAlert.
fn registry() -> NodeRegistry {
    NodeRegistry::new(&[TEST_FAMILY, FAMILIES[0], FAMILIES[1]])
}

/// A node as the editor saves it, with a position.
fn node(id: &str, t: &str, data: Value) -> Value {
    json!({"id": id, "type": t, "position": {"x": 0, "y": 0}, "data": data})
}

fn edge(s: &str, t: &str, handle: Option<&str>) -> Value {
    match handle {
        Some(h) => {
            json!({"id": format!("{}-{}", s, t), "source": s, "target": t, "sourceHandle": h})
        }
        None => json!({"id": format!("{}-{}", s, t), "source": s, "target": t}),
    }
}

fn cond(met: bool) -> Value {
    json!({"answer": {"status": "success", "condition": met}})
}

fn tagged(tag: &str) -> Value {
    json!({"tag": tag})
}

/// Walk `nodes`/`edges` from `start` with a bare context: the messages
/// logged, the run's errors, and the abort if any.
async fn walk(nodes: Value, edges: Value) -> (Vec<String>, Vec<Value>, Option<String>) {
    walk_with(
        &NodeCtx::bare(Arc::new(FakeFlowClient::new())),
        nodes,
        edges,
    )
    .await
}

async fn walk_with(
    ctx: &NodeCtx,
    nodes: Value,
    edges: Value,
) -> (Vec<String>, Vec<Value>, Option<String>) {
    let graph = Graph::parse(&nodes, &edges).unwrap();
    let reg = registry();
    let mut w = Walk::new(&graph, &reg, ctx);
    let abort = w.visit("start", 0).await.err();
    let logs = ctx
        .logs()
        .iter()
        .map(|l| l["message"].as_str().unwrap_or_default().to_string())
        .collect();
    (logs, w.errors, abort)
}

fn ran(logs: &[String], tag: &str) -> bool {
    logs.iter().any(|l| l == &format!("ran {}", tag))
}

#[tokio::test]
async fn a_condition_takes_its_branch_and_the_pass_through() {
    for (met, taken, skipped) in [(true, "yes", "no"), (false, "no", "yes")] {
        let nodes = json!([
            node("start", "start", json!({})),
            node("c", "priceCondition", cond(met)),
            node("t", "getQuote", tagged("yes")),
            node("f", "getQuote", tagged("no")),
            node("p", "getQuote", tagged("pass")),
        ]);
        let edges = json!([
            edge("start", "c", None),
            edge("c", "t", Some("true")),
            edge("c", "f", Some("false")),
            edge("c", "p", None),
        ]);
        let (logs, errors, _) = walk(nodes, edges).await;
        assert!(
            ran(&logs, taken) && ran(&logs, "pass") && !ran(&logs, skipped),
            "{:?}",
            logs
        );
        assert!(errors.is_empty());
    }
}

/// test_flow_qa_regressions.py:1687-1715: an unevaluatable condition fails
/// the run and takes no branch, but its pass-through wire still runs.
#[tokio::test]
async fn a_condition_that_could_not_be_evaluated_takes_neither_branch() {
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "c",
            "priceCondition",
            json!({"answer": {"status": "error", "condition": false, "message": "Broker session expired"}})
        ),
        node("t", "getQuote", tagged("yes")),
        node("f", "getQuote", tagged("no")),
        node("p", "getQuote", tagged("pass")),
    ]);
    let edges = json!([
        edge("start", "c", None),
        edge("c", "t", Some("yes")),
        edge("c", "f", Some("no")),
        edge("c", "p", None),
    ]);
    let (logs, errors, _) = walk(nodes, edges).await;
    assert!(!ran(&logs, "yes") && !ran(&logs, "no") && ran(&logs, "pass"));
    assert_eq!(
        errors,
        vec![json!({"node": "c", "type": "priceCondition", "message": "Broker session expired"})]
    );
    assert!(logs
        .contains(&"priceCondition could not be evaluated (Broker session expired)".to_string()));
}

/// test_flow_qa_regressions.py:277-313: a broker rejection stops its branch
/// (the hedge leg and the "placed" alert never run); siblings still run.
#[tokio::test]
async fn a_failed_node_stops_its_branch_only() {
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "o",
            "placeOrder",
            json!({"tag": "entry", "answer": {"status": "error", "error": "RMS: insufficient funds", "code": 400}})
        ),
        node("a", "telegramAlert", tagged("alert")),
        node("s", "getQuote", tagged("sibling")),
    ]);
    let edges = json!([
        edge("start", "o", None),
        edge("o", "a", None),
        edge("start", "s", None)
    ]);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert!(abort.is_none());
    assert!(ran(&logs, "entry") && !ran(&logs, "alert") && ran(&logs, "sibling"));
    assert_eq!(errors[0]["type"], "placeOrder");
    assert_eq!(errors[0]["message"], "RMS: insufficient funds");
    assert!(logs.contains(
        &"Stopping this branch: placeOrder node failed (RMS: insufficient funds)".to_string()
    ));
}

/// The run's errors keep a node's failure as it gave it, a dict of field
/// errors included (golden data/multiQuotes__empty); the log line and the
/// run summary read it as Python's `str()`.
#[tokio::test]
async fn a_failed_node_is_recorded_with_its_raw_message() {
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "q",
            "getQuote",
            json!({"tag": "quotes", "answer": {"status": "error",
            "error": {"symbols": ["Shorter than minimum length 1."]}, "code": 400}})
        ),
    ]);
    let (logs, errors, _) = walk(nodes, json!([edge("start", "q", None)])).await;
    assert_eq!(
        errors,
        vec![json!({"node": "q", "type": "getQuote",
                    "message": {"symbols": ["Shorter than minimum length 1."]}})]
    );
    assert!(logs.contains(
        &"Stopping this branch: getQuote node failed ({'symbols': ['Shorter than minimum length 1.']})".to_string()
    ));
}

/// The web's diamond defect: a condition reached by two paths placed two
/// orders. It is evaluated once per run.
#[tokio::test]
async fn a_condition_reached_twice_runs_once() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("a", "getQuote", tagged("a")),
        node("b", "getQuote", tagged("b")),
        node(
            "c",
            "priceCondition",
            json!({"tag": "cond", "answer": {"status": "success", "condition": true}})
        ),
        node(
            "o",
            "placeOrder",
            json!({"tag": "order", "answer": {"status": "success", "orderid": "1"}})
        ),
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("start", "b", None),
        edge("a", "c", None),
        edge("b", "c", None),
        edge("c", "o", Some("true")),
    ]);
    let (logs, _, _) = walk(nodes, edges).await;
    assert_eq!(
        logs.iter().filter(|l| *l == "ran order").count(),
        1,
        "{:?}",
        logs
    );
    assert_eq!(logs.iter().filter(|l| *l == "ran cond").count(), 1);
}

/// A priceAlert reached by two paths is evaluated once too (the web leaves
/// it out of its branching set: parity N-logic-04, lead decision 8).
#[tokio::test]
async fn a_price_alert_reached_twice_runs_once() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("a", "getQuote", tagged("a")),
        node("b", "getQuote", tagged("b")),
        node(
            "p",
            "priceAlert",
            json!({"tag": "alert", "answer": {"status": "success", "condition": true}})
        ),
        node(
            "o",
            "placeOrder",
            json!({"tag": "order", "answer": {"status": "success"}})
        ),
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("start", "b", None),
        edge("a", "p", None),
        edge("b", "p", None),
        edge("p", "o", Some("true")),
    ]);
    let (logs, _, _) = walk(nodes, edges).await;
    assert_eq!(
        logs.iter().filter(|l| *l == "ran alert").count(),
        1,
        "{:?}",
        logs
    );
    assert_eq!(logs.iter().filter(|l| *l == "ran order").count(), 1);
}

/// A non-branching node reached by two paths runs once per path, as on the
/// web (parity E-29, kept for parity by the lead).
#[tokio::test]
async fn an_action_reached_twice_runs_twice_as_on_the_web() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("a", "getQuote", tagged("a")),
        node("b", "getQuote", tagged("b")),
        node("q", "getQuote", tagged("merge")),
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("start", "b", None),
        edge("a", "q", None),
        edge("b", "q", None)
    ]);
    let (logs, _, _) = walk(nodes, edges).await;
    assert_eq!(logs.iter().filter(|l| *l == "ran merge").count(), 2);
}

fn gate_graph(gate: &str, gate_data: Value, a: bool, b: bool) -> (Value, Value) {
    let nodes = json!([
        node("start", "start", json!({})),
        node("ca", "priceCondition", cond(a)),
        node("cb", "priceCondition", cond(b)),
        node("g", gate, gate_data),
        node("t", "getQuote", tagged("gate-true")),
        node("f", "getQuote", tagged("gate-false")),
    ]);
    let (yes, no) = if gate == "notGate" {
        ("yes", "no")
    } else {
        ("true", "false")
    };
    let edges = json!([
        edge("start", "ca", None),
        edge("start", "cb", None),
        // The wire leaves the FALSE handle: it still carries the value.
        edge("ca", "g", Some("false")),
        edge("cb", "g", Some("true")),
        edge("g", "t", Some(yes)),
        edge("g", "f", Some(no)),
    ]);
    (nodes, edges)
}

#[tokio::test]
async fn gates_wait_for_every_input_and_fire_once() {
    for (gate, a, b, expect) in [
        ("andGate", true, true, true),
        ("andGate", true, false, false),
        ("orGate", false, true, true),
        ("orGate", false, false, false),
    ] {
        let (nodes, edges) = gate_graph(gate, json!({}), a, b);
        let (logs, errors, _) = walk(nodes, edges).await;
        assert!(errors.is_empty(), "{:?}", errors);
        let fired_true = logs.iter().filter(|l| *l == "ran gate-true").count();
        let fired_false = logs.iter().filter(|l| *l == "ran gate-false").count();
        assert_eq!(
            (fired_true, fired_false),
            if expect { (1, 0) } else { (0, 1) },
            "{} {} {}: {:?}",
            gate,
            a,
            b,
            logs
        );
        assert!(
            logs.iter()
                .any(|l| l.contains("waiting for 1 more input(s)")),
            "{:?}",
            logs
        );
    }
    let (nodes, edges) = gate_graph("andGate", json!({}), true, false);
    let (logs, _, _) = walk(nodes, edges).await;
    assert!(
        logs.contains(&"AND Gate: [True, False] -> False".to_string()),
        "{:?}",
        logs
    );
}

#[tokio::test]
async fn a_not_gate_inverts_its_first_input() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("c", "priceCondition", cond(true)),
        node("g", "notGate", json!({})),
        node("y", "getQuote", tagged("yes")),
        node("n", "getQuote", tagged("no")),
    ]);
    let edges = json!([
        edge("start", "c", None),
        edge("c", "g", Some("true")),
        edge("g", "y", Some("yes")),
        edge("g", "n", Some("no"))
    ]);
    let (logs, _, _) = walk(nodes, edges).await;
    assert!(ran(&logs, "no") && !ran(&logs, "yes"));
    assert!(logs.contains(&"NOT Gate: True -> False".to_string()));
}

/// A gate configured for more inputs than are wired fails rather than
/// deciding on part of its condition; an absent count is the editor's two
/// (parity N-gate-04, a deliberate departure from the web).
#[tokio::test]
async fn a_gate_with_missing_wires_refuses_to_decide() {
    for data in [json!({"inputCount": 3}), json!({})] {
        let nodes = json!([
            node("start", "start", json!({})),
            node("c", "priceCondition", cond(true)),
            node("g", "andGate", data.clone()),
            node("t", "getQuote", tagged("fired")),
        ]);
        let edges = json!([
            edge("start", "c", None),
            edge("c", "g", Some("true")),
            edge("g", "t", Some("true"))
        ]);
        let (logs, errors, _) = walk(nodes, edges).await;
        assert!(!ran(&logs, "fired"), "{:?}", data);
        assert_eq!(errors.len(), 1);
        let want = if data.get("inputCount").is_some() {
            3
        } else {
            2
        };
        assert_eq!(
            errors[0]["message"],
            format!("andGate is configured for {} inputs but only 1 are wired. Wire the rest, or lower the input count, rather than evaluating on part of the condition.", want)
        );
    }
}

/// N-common-03/04: an order with an unresolved reference in a field that
/// reaches the broker fails before it is sent.
#[tokio::test]
async fn an_order_with_an_unresolved_field_is_not_sent() {
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    ctx.vars.set("webhook", json!({"symbol": "SBIN"}));
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "o",
            "placeOrder",
            json!({"tag": "order", "symbol": "{{webhook.symbol}}", "quantity": "{{webhook.qty}}", "strategy": "{{nope}}"})
        ),
    ]);
    let (logs, errors, _) = walk_with(&ctx, nodes, json!([edge("start", "o", None)])).await;
    assert!(!ran(&logs, "order"));
    assert_eq!(
        errors[0]["message"],
        "placeOrder has unresolved variable(s) in quantity='{{webhook.qty}}'. Refusing to send it with substituted values."
    );
}

#[tokio::test]
async fn an_unknown_node_type_is_logged_and_passed_through() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("x", "fancyNode", json!({})),
        node("q", "getQuote", tagged("after"))
    ]);
    let (logs, errors, _) = walk(
        nodes,
        json!([edge("start", "x", None), edge("x", "q", None)]),
    )
    .await;
    assert!(logs.contains(&"Unknown node type: fancyNode".to_string()));
    assert!(ran(&logs, "after") && errors.is_empty());
}

/// Children run depth first in the order their edges are stored.
#[tokio::test]
async fn children_run_depth_first_in_edge_order() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("b", "getQuote", tagged("b")),
        node("a", "getQuote", tagged("a")),
        node("a1", "getQuote", tagged("a1")),
    ]);
    let edges = json!([
        edge("start", "b", None),
        edge("start", "a", None),
        edge("b", "a1", None)
    ]);
    let (logs, _, _) = walk(nodes, edges).await;
    let order: Vec<&str> = logs.iter().filter_map(|l| l.strip_prefix("ran ")).collect();
    assert_eq!(order, vec!["b", "a1", "a"]);
}

#[tokio::test]
async fn runaway_graphs_end_the_run() {
    // A chain longer than the depth limit.
    let mut nodes = vec![node("start", "start", json!({}))];
    let mut edges = vec![edge("start", "n0", None)];
    for i in 0..=MAX_NODE_DEPTH {
        nodes.push(node(&format!("n{}", i), "getQuote", json!({})));
        edges.push(edge(&format!("n{}", i), &format!("n{}", i + 1), None));
    }
    let (_, _, abort) = walk(json!(nodes), json!(edges)).await;
    assert_eq!(abort.as_deref(), Some("Maximum node depth (100) exceeded"));
    // A loop through a pass-through node.
    let nodes = json!([
        node("start", "start", json!({})),
        node("a", "getQuote", json!({})),
        node("b", "getQuote", json!({}))
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("a", "b", None),
        edge("b", "a", None)
    ]);
    let (_, _, abort) = walk(nodes, edges).await;
    assert!(
        abort.is_some_and(|a| a == "Maximum node depth (100) exceeded"
            || a == format!("Maximum node visits ({}) exceeded", MAX_NODE_VISITS))
    );
}

/// A handler that raises on the web ends the whole run, branches not yet
/// visited included (parity E-30).
#[tokio::test]
async fn an_abort_ends_the_whole_run() {
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "x",
            "getQuote",
            json!({"abort": "Symbol master is not loaded"})
        ),
        node("later", "getQuote", tagged("later")),
    ]);
    let edges = json!([edge("start", "x", None), edge("start", "later", None)]);
    let (logs, _, abort) = walk(nodes, edges).await;
    assert_eq!(abort.as_deref(), Some("Symbol master is not loaded"));
    assert!(!ran(&logs, "later"));
}

/// A stop while the walk waits in a Delay ends the wait at once: the Delay
/// fails with its own sentence and nothing after it runs. The stop is sent
/// only once the Delay has logged that it is waiting.
#[tokio::test]
async fn a_stop_during_a_delay_ends_the_wait_with_the_delays_sentence() {
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    let nodes = json!([
        node("start", "start", json!({})),
        node("d", "delay", json!({"delayValue": 60})),
        node("after", "getQuote", tagged("after")),
    ]);
    let edges = json!([edge("start", "d", None), edge("d", "after", None)]);
    let graph = Graph::parse(&nodes, &edges).unwrap();
    let reg = registry();
    let mut w = Walk::new(&graph, &reg, &ctx);
    let waiting = async {
        for _ in 0..5000 {
            if ctx
                .logs()
                .iter()
                .any(|l| l["message"] == "Waiting for 60 seconds")
            {
                ctx.cancel.cancel();
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        false
    };
    let (walked, saw_wait) = tokio::join!(w.visit("start", 0), waiting);
    assert!(saw_wait, "the Delay never logged that it was waiting");
    assert_eq!(walked, Ok(()));
    assert_eq!(
        w.errors,
        vec![
            json!({"node": "d", "type": "delay", "message": crate::flow::nodes::logic::STOPPED_WHILE_WAITING})
        ]
    );
    assert!(!ctx.logs().iter().any(|l| l["message"] == "ran after"));
}

#[tokio::test]
async fn a_stopped_run_visits_nothing_more() {
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    ctx.cancel.cancel();
    let (logs, _, abort) =
        walk_with(&ctx, json!([node("start", "start", json!({}))]), json!([])).await;
    assert_eq!(abort.as_deref(), Some(walk::STOPPED));
    assert!(logs.is_empty());
}

// --- a run, end to end ------------------------------------------------------

mod run;
