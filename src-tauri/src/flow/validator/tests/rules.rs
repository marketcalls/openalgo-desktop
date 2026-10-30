//! What the goldens cannot pin: the desktop's fixes of web defects (each
//! answered differently from the web on purpose, parity ids in the names),
//! the serialized error shape, and rules the web's own tests leave open.

use super::*;
use crate::flow::validator::{trigger_config, MAX_EDGES};
use serde_json::json;

// ---------------------------------------------------------------- V-005

#[test]
fn v005_positive_number_fields_are_reported_in_listed_order() {
    let errors = draft(&single_action(
        "splitOrder",
        json!({"lots": 0, "splitSize": -2, "quantity": "x"}),
    ));
    let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/nodes/1/data/quantity",
            "/nodes/1/data/splitSize",
            "/nodes/1/data/lots"
        ]
    );
}

fn cycle_message(order: [&str; 3]) -> String {
    let nodes = order
        .iter()
        .map(|id| node(id, if *id == "t" { "start" } else { "log" }, json!({})))
        .collect();
    let edges = vec![
        edge("e1", "t", "a"),
        edge("e2", "a", "b"),
        edge("e3", "b", "a"),
    ];
    let errors = strict(&wf(nodes, edges));
    let cycle = errors.iter().find(|e| e.code == "cycle").unwrap();
    cycle.message.rsplit(": ").next().unwrap().to_string()
}

#[test]
fn v005_the_cycle_search_starts_from_the_nodes_in_listed_order() {
    assert_eq!(cycle_message(["t", "a", "b"]), "a -> b -> a");
    assert_eq!(cycle_message(["b", "t", "a"]), "b -> a -> b");
    // The same graph gives the same sentence every time.
    for _ in 0..20 {
        assert_eq!(cycle_message(["t", "a", "b"]), "a -> b -> a");
    }
}

// ---------------------------------------------------------------- V-006

#[test]
fn v006_an_array_edge_source_with_a_handle_is_reported_not_raised() {
    let payload = wf(
        vec![node("t", "start", json!({}))],
        vec![json!({"id": "e", "source": ["t"], "target": "t", "sourceHandle": "x"})],
    );
    let errors = draft(&payload);
    assert_eq!(codes(&errors), vec!["required"]);
    assert_eq!(errors[0].message, "Edge needs a source node id");
}

#[test]
fn v006_an_object_gate_target_and_an_array_node_type_are_reported_not_raised() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            json!({"id": "c", "type": ["log"], "position": {"x": 0, "y": 0}, "data": {}}),
        ],
        vec![
            json!({"id": "e1", "source": "t", "target": {"a": 1}, "targetHandle": "input-1"}),
            json!({"id": "e2", "source": "c", "target": "t", "sourceHandle": "x"}),
        ],
    );
    let errors = draft(&payload);
    let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
    assert!(messages.contains(&"Node needs a type"), "{messages:?}");
    assert!(
        messages.contains(&"Edge needs a target node id"),
        "{messages:?}"
    );
}

// ---------------------------------------------------------------- V-053

#[test]
fn v053_a_nan_timeout_is_refused() {
    for timeout in ["NaN", "nan", " -nan "] {
        let errors = draft(&single_action(
            "httpRequest",
            json!({"url": "https://x.com", "timeout": timeout}),
        ));
        assert!(has(&errors, "invalid_timeout", "/timeout"), "{timeout}");
    }
}

#[test]
fn v053_a_strike_that_is_not_finite_names_no_contract() {
    for strike in ["NaN", "Infinity", "inf"] {
        let leg = json!({"optionType": "CE", "action": "BUY", "quantity": 1, "strike": strike});
        let data =
            json!({"strategy": "custom", "underlying": "NIFTY", "quantity": 1, "legs": [leg]});
        let errors = draft(&single_action("optionsMultiOrder", data));
        let error = errors.iter().find(|e| e.code == "invalid_strike").unwrap();
        assert_eq!(
            error.message,
            format!("strike must be a positive number; '{strike}' names no contract.")
        );
    }
}

// ---------------------------------------------------------------- V-059

#[test]
fn v059_an_options_multi_order_without_a_strategy_is_custom() {
    let data = json!({"underlying": "NIFTY", "quantity": 1});
    let errors = strict(&single_action("optionsMultiOrder", data.clone()));
    let error = errors
        .iter()
        .find(|e| e.path == "/nodes/1/data/legs")
        .unwrap();
    assert_eq!(error.code, "missing_required_field");
    assert_eq!(
        error.message,
        "A custom options strategy needs at least one leg."
    );
    // Still savable while being built.
    assert_eq!(draft(&single_action("optionsMultiOrder", data)), vec![]);
}

#[test]
fn v059_the_legacy_strategy_type_is_read() {
    let custom = json!({"strategyType": "custom", "underlying": "NIFTY", "quantity": 1,
        "legs": [{"optionType": "CE", "action": "BUY", "quantity": 1, "offset": "OTM99"}]});
    let errors = draft(&single_action("optionsMultiOrder", custom));
    assert!(has(&errors, "invalid_constant", "/legs/0/offset"));

    let generated = json!({"strategyType": "straddle", "underlying": "NIFTY", "quantity": 1,
        "priceType": "SL", "price": 1, "triggerPrice": 1});
    let errors = draft(&single_action("optionsMultiOrder", generated));
    assert_eq!(
        errors.last().map(|e| e.message.as_str()),
        Some("Generated option strategies support only MARKET and LIMIT price types.")
    );
}

#[test]
fn v059_strategy_wins_over_strategy_type() {
    let data = json!({"strategy": "straddle", "strategyType": "custom", "underlying": "NIFTY",
        "quantity": 1});
    assert_eq!(strict(&single_action("optionsMultiOrder", data)), vec![]);
}

// ---------------------------------------------------------------- V-085

fn gate_workflow(gate: &str, input_count: Value, wires: usize) -> Value {
    let mut nodes = vec![
        node("t", "start", json!({})),
        node("g", gate, json!({"inputCount": input_count})),
    ];
    let mut edges = Vec::new();
    for i in 0..wires {
        let id = format!("c{i}");
        nodes.push(node(
            &id,
            "varCondition",
            json!({"leftValue": "1", "operator": ">"}),
        ));
        edges.push(edge(&format!("in{i}"), "t", &id));
        let mut wire =
            json!({"id": format!("w{i}"), "source": id, "target": "g", "sourceHandle": "true"});
        // AND and OR gates number their input slots; a NOT gate has one.
        if gate != "notGate" {
            wire["targetHandle"] = json!(format!("input-{i}"));
        }
        edges.push(wire);
    }
    if wires == 0 {
        edges.push(edge("e", "t", "g"));
    }
    wf(nodes, edges)
}

#[test]
fn v085_a_gate_wired_to_fewer_inputs_than_it_declares_is_refused() {
    let errors = strict(&gate_workflow("andGate", json!(3), 2));
    assert_eq!(errors.len(), 1, "{errors:?}");
    let error = &errors[0];
    assert_eq!(error.path, "/nodes/1/data/inputCount");
    assert_eq!(error.code, "unwired_gate_input");
    assert_eq!(
        error.message,
        "andGate is configured for 3 inputs but only 2 are wired. Wire the rest, or lower the \
         input count, rather than evaluating on part of the condition."
    );
    assert_eq!(error.expected, Some(json!(3)));
    assert_eq!(error.received, Some(json!(2)));
}

#[test]
fn v085_a_fully_wired_or_unnumbered_gate_passes() {
    assert_eq!(strict(&gate_workflow("andGate", json!(2), 2)), vec![]);
    assert_eq!(strict(&gate_workflow("orGate", json!(2), 2)), vec![]);
    // Only an integer count is compared, as the executor compares it.
    assert_eq!(strict(&gate_workflow("andGate", json!("3"), 2)), vec![]);
    assert_eq!(strict(&gate_workflow("andGate", json!(3.0), 2)), vec![]);
    assert_eq!(strict(&gate_workflow("andGate", Value::Null, 2)), vec![]);
}

#[test]
fn v085_applies_to_every_gate_and_only_when_finished() {
    let errors = strict(&gate_workflow("notGate", json!(2), 1));
    assert_eq!(codes(&errors), vec!["unwired_gate_input"]);
    assert!(errors[0]
        .message
        .starts_with("notGate is configured for 2 inputs but only 1 are wired."));
    // A graph still being wired saves.
    assert_eq!(draft(&gate_workflow("andGate", json!(3), 1)), vec![]);
}

#[test]
fn v085_comes_after_the_webs_graph_errors() {
    let mut payload = gate_workflow("andGate", json!(3), 2);
    payload["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node("orphan", "log", json!({})));
    assert_eq!(
        codes(&strict(&payload)),
        vec!["unreachable", "unwired_gate_input"]
    );
}

// ------------------------------------------------- gate slot list bound

#[test]
fn a_huge_gate_input_count_lists_at_most_max_edges_slots() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("g", "andGate", json!({"inputCount": 100_000_000})),
        ],
        vec![json!({"id": "e", "source": "t", "target": "g", "targetHandle": "input-x"})],
    );
    let errors = draft(&payload);
    assert_eq!(
        errors[0].message,
        "'input-x' is not an input slot on this gate, which has 100000000. The gate would wait \
         for an input that never arrives."
    );
    assert_eq!(
        errors[0]
            .expected
            .as_ref()
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(MAX_EDGES)
    );
}

// ------------------------------------------------------- error shape

#[test]
fn an_error_serializes_with_the_webs_keys_in_the_webs_order() {
    let errors = strict(&json!({"name": "W", "nodes": [], "edges": []}));
    assert_eq!(
        serde_json::to_string(&errors[0]).unwrap(),
        r#"{"path":"/nodes","code":"no_trigger","message":"Workflow has no nodes, so it can never run.","expected":"at least a trigger node","received":0}"#
    );
    let errors = strict(&json!({"name": "W", "nodes": []}));
    assert_eq!(
        serde_json::to_string(&errors[0]).unwrap(),
        r#"{"path":"/edges","code":"required","message":"Workflow needs an edges array","expected":"array"}"#
    );
}

#[test]
fn a_falsy_received_value_is_kept_and_none_is_left_out() {
    let errors = draft(&single_action(
        "placeOrder",
        json!({"exchange": false, "action": null}),
    ));
    let exchange = errors
        .iter()
        .find(|e| e.path.ends_with("/exchange"))
        .unwrap();
    assert_eq!(exchange.received, Some(json!(false)));
    assert_eq!(
        exchange.message,
        "exchange must be one of the exchange constants written as a string; False is a bool."
    );
    let action = errors.iter().find(|e| e.path.ends_with("/action")).unwrap();
    assert_eq!(action.received, None);
    assert_eq!(
        action.message,
        "action must be one of the action constants written as a string; None is a NoneType."
    );
}

#[test]
fn the_default_level_is_the_complete_one() {
    assert_eq!(ValidateOptions::default(), ValidateOptions::COMPLETE);
    let unnamed = json!({"nodes": [], "edges": []});
    assert_eq!(
        validate_workflow(
            &unnamed,
            ValidateOptions {
                require_name: false,
                strict: true
            }
        )[0]
        .code,
        "no_trigger"
    );
    assert_eq!(strict(&unnamed)[0].path, "/name");
}

// ---------------------------------------------- trigger configuration

#[test]
fn trigger_configurations_compare_as_python_does() {
    let a =
        trigger_config(&[json!({"type": "priceAlert", "data": {"price": 100, "message": "a"}})]);
    let b =
        trigger_config(&[json!({"type": "priceAlert", "data": {"price": 100.0, "message": "b"}})]);
    assert_eq!(a, b);
    let c = trigger_config(&[json!({"type": "priceAlert", "data": {"price": 101}})]);
    assert_ne!(a, c);
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        json!({"priceAlert": {"price": 100}})
    );
}

// ----------------------------------------------------- big workflows

#[test]
fn a_workflow_over_the_limits_is_checked_without_running_the_graph() {
    let nodes: Vec<Value> = (0..5000)
        .map(|i| node(&format!("n{i}"), "log", json!({})))
        .collect();
    let mut edges: Vec<Value> = (1..5000)
        .map(|i| edge(&format!("e{i}"), &format!("n{}", i - 1), &format!("n{i}")))
        .collect();
    edges.push(edge("back", "n4999", "n0"));
    let mut payload = wf(nodes, edges);
    payload["nodes"][0]["type"] = json!("start");
    assert_eq!(codes(&strict(&payload)), vec!["too_large", "too_large"]);
}

#[test]
fn a_full_size_chain_with_a_cycle_at_its_end_is_found_without_recursion() {
    let mut nodes = vec![node("t", "start", json!({}))];
    let mut edges = vec![edge("e0", "t", "c0")];
    for i in 0..499 {
        nodes.push(node(&format!("c{i}"), "log", json!({})));
        if i > 0 {
            edges.push(edge(
                &format!("e{i}"),
                &format!("c{}", i - 1),
                &format!("c{i}"),
            ));
        }
    }
    edges.push(edge("back", "c498", "c497"));
    let errors = strict(&wf(nodes, edges));
    assert_eq!(codes(&errors), vec!["cycle"]);
    assert!(
        errors[0].message.ends_with(": c497 -> c498 -> c497"),
        "{}",
        errors[0].message
    );
}
