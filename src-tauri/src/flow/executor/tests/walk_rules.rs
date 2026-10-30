//! The walk's rules the audit found pinned only by the goldens, each by a
//! test of its own (E-02, E-27, N-gate-06..08, N-group-00/01,
//! N-common-02).

use super::*;
use crate::flow::contract::model::ORDER_CRITICAL_FIELDS;

/// Walk from `from` (a node with no trigger in front of it, as a golden
/// starts at its node), with `conditions` already decided.
async fn walk_from(
    nodes: Value,
    edges: Value,
    from: &str,
    conditions: &[(&str, bool)],
) -> (Vec<String>, Vec<Value>) {
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    for (id, met) in conditions {
        ctx.vars.set_condition(id, *met);
    }
    let graph = Graph::parse(&nodes, &edges).unwrap();
    let reg = registry();
    let mut w = Walk::new(&graph, &reg, &ctx);
    assert_eq!(w.visit(from, 0).await, Ok(()));
    let logs = ctx
        .logs()
        .iter()
        .map(|l| l["message"].as_str().unwrap_or_default().to_string())
        .collect();
    (logs, w.errors)
}

/// E-02: a graph that stays shallow but fans out ends on the visit cap,
/// with the web's sentence (the depth cap is never reached).
#[tokio::test]
async fn a_wide_graph_ends_on_the_visit_cap() {
    let mut nodes = vec![node("start", "start", json!({}))];
    let mut edges = vec![edge("start", "a0", None), edge("start", "b0", None)];
    for i in 0..12 {
        for side in ["a", "b"] {
            nodes.push(node(&format!("{}{}", side, i), "group", json!({})));
            edges.push(edge(
                &format!("{}{}", side, i),
                &format!("a{}", i + 1),
                None,
            ));
            edges.push(edge(
                &format!("{}{}", side, i),
                &format!("b{}", i + 1),
                None,
            ));
        }
    }
    assert!(12 < MAX_NODE_DEPTH);
    let (_, _, abort) = walk(json!(nodes), json!(edges)).await;
    assert_eq!(
        abort.as_deref(),
        Some("Maximum node visits (500) exceeded"),
        "MAX_NODE_VISITS is {}",
        MAX_NODE_VISITS
    );
}

/// E-27 / N-gate-08: a condition that could not be evaluated delivers to
/// no gate, so the gate keeps waiting and never fires.
#[tokio::test]
async fn an_errored_input_keeps_its_gate_waiting() {
    let nodes = json!([
        node("start", "start", json!({})),
        node(
            "a",
            "priceCondition",
            json!({"answer": {"status": "error", "condition": false, "message": "no price"}})
        ),
        node("b", "priceCondition", cond(true)),
        node("g", "andGate", json!({"inputCount": 2})),
        node("t", "getQuote", tagged("fired")),
        node("f", "getQuote", tagged("not-fired")),
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("start", "b", None),
        edge("a", "g", Some("true")),
        edge("b", "g", Some("true")),
        edge("g", "t", Some("true")),
        edge("g", "f", Some("false")),
    ]);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert_eq!(abort, None);
    assert!(
        !ran(&logs, "fired") && !ran(&logs, "not-fired"),
        "{:?}",
        logs
    );
    assert_eq!(
        logs.last().map(String::as_str),
        Some("andGate: waiting for 1 more input(s) before evaluating"),
        "{:?}",
        logs
    );
    assert_eq!(errors.len(), 1);
}

/// N-gate-06: an OR gate logs its inputs and its answer as the web does.
#[tokio::test]
async fn an_or_gate_logs_its_inputs() {
    let (nodes, edges) = gate_graph("orGate", json!({}), false, true);
    let (logs, _, _) = walk(nodes, edges).await;
    assert!(
        logs.contains(&"OR Gate: [False, True] -> True".to_string()),
        "{:?}",
        logs
    );
}

fn not_gate_graph(inputs: &[&str]) -> (Value, Value) {
    let mut nodes = vec![
        node("n1", "notGate", json!({})),
        node("y", "getQuote", tagged("yes")),
        node("n", "getQuote", tagged("no")),
    ];
    let mut edges = vec![edge("n1", "y", Some("yes")), edge("n1", "n", Some("no"))];
    for i in inputs {
        nodes.push(node(i, "getQuote", json!({})));
        edges.push(edge(i, "n1", None));
    }
    (json!(nodes), json!(edges))
}

/// N-gate-07: a NOT gate with no input reads False and answers True; with
/// more than one, it reads only the first.
#[tokio::test]
async fn a_not_gate_reads_only_its_first_input() {
    let (nodes, edges) = not_gate_graph(&[]);
    let (logs, errors) = walk_from(nodes, edges, "n1", &[]).await;
    assert!(ran(&logs, "yes") && !ran(&logs, "no"), "{:?}", logs);
    assert!(
        logs.contains(&"NOT Gate: False -> True".to_string()),
        "{:?}",
        logs
    );
    assert!(errors.is_empty());
    let (nodes, edges) = not_gate_graph(&["in1", "in2"]);
    let (logs, _) = walk_from(nodes, edges, "n1", &[("in1", false), ("in2", true)]).await;
    assert!(ran(&logs, "yes") && !ran(&logs, "no"), "{:?}", logs);
    assert!(
        logs.contains(&"NOT Gate: False -> True".to_string()),
        "{:?}",
        logs
    );
}

/// N-group-00 / N-group-01: the group node is registered and passes its
/// children through without a line of its own.
#[tokio::test]
async fn a_group_is_registered_and_passes_through_silently() {
    assert!(NodeRegistry::new(FAMILIES).get("group").is_some());
    let nodes = json!([
        node("start", "start", json!({})),
        node("g", "group", json!({"label": "Entries"})),
        node("q", "getQuote", tagged("after")),
    ]);
    let edges = json!([edge("start", "g", None), edge("g", "q", None)]);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert_eq!(logs, vec!["Workflow started", "ran after"]);
    assert!(errors.is_empty() && abort.is_none());
}

/// N-common-02: every one of the web's 21 order fields is guarded: an
/// unresolved reference in any of them stops the order before it is sent.
#[tokio::test]
async fn every_order_field_is_guarded() {
    let mut web = vec![
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
    let mut ours = ORDER_CRITICAL_FIELDS.to_vec();
    web.sort_unstable();
    ours.sort_unstable();
    assert_eq!(ours, web);
    for field in ORDER_CRITICAL_FIELDS {
        let mut data = serde_json::Map::new();
        data.insert("tag".into(), json!("order"));
        data.insert(field.to_string(), json!("{{nope}}"));
        let nodes = json!([
            node("start", "start", json!({})),
            node("o", "placeOrder", Value::Object(data)),
        ]);
        let (logs, errors, _) = walk(nodes, json!([edge("start", "o", None)])).await;
        assert!(!ran(&logs, "order"), "{}", field);
        assert_eq!(
            errors[0]["message"],
            format!(
                "placeOrder has unresolved variable(s) in {}='{{{{nope}}}}'. Refusing to send it with substituted values.",
                field
            ),
            "{}",
            field
        );
    }
}
