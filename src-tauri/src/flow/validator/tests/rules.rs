//! What the goldens cannot pin: the desktop's fixes of web defects (each
//! answered differently from the web on purpose, parity ids in the names),
//! the serialized error shape, and rules the web's own tests leave open.

use super::*;
use crate::flow::contract::pyfmt;
use crate::flow::validator::{migrate_legacy_node_data, trigger_config, MAX_EDGES};
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

/// The gate's input count with its `inputCount` taken out.
fn without_count(mut payload: Value) -> Value {
    if let Some(data) = payload["nodes"][1]["data"].as_object_mut() {
        data.remove("inputCount");
    }
    payload
}

/// One unwired-input error, with the counts the executor would print.
fn assert_unwired(errors: &[ValidationError], gate: &str, declared: i64, wired: usize) {
    assert_eq!(codes(errors), vec!["unwired_gate_input"], "{errors:?}");
    assert_eq!(
        errors[0].message,
        format!(
            "{gate} is configured for {declared} inputs but only {wired} are wired. Wire the rest, \
             or lower the input count, rather than evaluating on part of the condition."
        )
    );
    assert_eq!(errors[0].expected, Some(json!(declared)));
}

/// The count is the executor's (`declared_inputs`, lead decision 25): an AND
/// or OR gate with no readable `inputCount` has the two slots the editor
/// draws, and text or a fraction is read as a whole number, so whatever the
/// run would refuse is refused here.
#[test]
fn v085_counts_the_inputs_as_the_executor_does() {
    for (gate, count, wires, declared) in [
        ("andGate", json!("3"), 2, 3),
        ("andGate", json!(3.0), 2, 3),
        ("andGate", json!(3.9), 2, 3),
        ("orGate", json!(" 3 "), 1, 3),
        ("andGate", Value::Null, 1, 2),
        ("andGate", json!(""), 1, 2),
        ("orGate", json!("abc"), 1, 2),
        ("orGate", json!([3]), 1, 2),
    ] {
        let errors = strict(&gate_workflow(gate, count.clone(), wires));
        assert_unwired(&errors, gate, declared, wires);
    }
    let absent = strict(&without_count(gate_workflow("andGate", Value::Null, 1)));
    assert_unwired(&absent, "andGate", 2, 1);
}

#[test]
fn v085_a_fully_wired_gate_passes() {
    assert_eq!(strict(&gate_workflow("andGate", json!(2), 2)), vec![]);
    assert_eq!(strict(&gate_workflow("orGate", json!("2"), 2)), vec![]);
    assert_eq!(strict(&gate_workflow("andGate", Value::Null, 2)), vec![]);
    assert_eq!(strict(&gate_workflow("andGate", json!(0), 1)), vec![]);
    assert_eq!(
        strict(&without_count(gate_workflow("orGate", Value::Null, 2))),
        vec![]
    );
    // A NOT gate counts only a whole number, as the executor does.
    assert_eq!(strict(&gate_workflow("notGate", json!("3"), 1)), vec![]);
    assert_eq!(
        strict(&without_count(gate_workflow("notGate", Value::Null, 1))),
        vec![]
    );
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

// ---------------------------------------------------------------- V-113

fn migrated_position_check(data: Value) -> (Value, Vec<String>) {
    let (nodes, notes) = migrate_legacy_node_data(vec![node("p", "positionCheck", data)]);
    (nodes[0]["data"].clone(), notes)
}

/// Lead decision 23: a legacy `>=` keeps `>=`. The node compares a
/// whole-number quantity strictly, so the threshold moves by one and every
/// quantity gets the decision the legacy node gave it.
#[test]
fn v113_an_inclusive_legacy_operator_keeps_its_meaning() {
    for (operator, threshold, condition, moved) in [
        (">=", json!(5), "quantity_above", 4),
        ("gte", json!(5), "quantity_above", 4),
        ("ge", json!("5"), "quantity_above", 4),
        (">=", json!(5.5), "quantity_above", 5),
        (">=", json!(0), "quantity_above", -1),
        ("<=", json!(5), "quantity_below", 6),
        ("lte", json!(5.5), "quantity_below", 6),
        ("le", json!(-2), "quantity_below", -1),
    ] {
        let (data, notes) =
            migrated_position_check(json!({"operator": operator, "threshold": threshold}));
        assert_eq!(
            data,
            json!({"condition": condition, "threshold": moved}),
            "{operator} {threshold}"
        );
        assert_eq!(
            notes,
            vec![format!(
                "positionCheck 'p': operator '{operator}' -> condition '{condition}' (threshold {} \
                 -> {moved}, which keeps '{operator}' on whole-number quantities)",
                pyfmt::str(&threshold)
            )]
        );
        let t = pyfmt::to_float(&threshold).unwrap();
        for qty in -10i64..=10 {
            let legacy = if condition == "quantity_above" {
                qty as f64 >= t
            } else {
                qty as f64 <= t
            };
            let now = if condition == "quantity_above" {
                qty > moved
            } else {
                qty < moved
            };
            assert_eq!(legacy, now, "{operator} {threshold} at quantity {qty}");
        }
    }
}

#[test]
fn v113_a_strict_legacy_operator_keeps_its_threshold() {
    let (data, notes) = migrated_position_check(json!({"operator": ">", "threshold": 5}));
    assert_eq!(data, json!({"condition": "quantity_above", "threshold": 5}));
    assert_eq!(
        notes,
        vec!["positionCheck 'p': operator '>' -> condition 'quantity_above' (threshold kept)"]
    );
}

#[test]
fn v113_an_inclusive_operator_without_a_number_is_left_for_the_trader() {
    for data in [
        json!({"operator": ">=", "threshold": "{{webhook.qty}}"}),
        json!({"operator": "<=", "threshold": "abc"}),
        json!({"operator": ">="}),
    ] {
        let (after, notes) = migrated_position_check(data.clone());
        assert_eq!(after, data);
        assert!(notes[0]
            .ends_with("has no equivalent condition. Left unchanged; set the condition yourself."));
    }
}

// --------------------------------------------------------- V-052, V-055

/// Lead decision 24: JSON written as text is read as Python's json.loads
/// reads it, so `NaN` and `Infinity` parse as on the web and in the run.
#[test]
fn v052_headers_holding_nan_or_infinity_parse_as_python_parses_them() {
    for headers in [r#"{"A": NaN}"#, r#"{"A": Infinity, "B": -Infinity}"#] {
        let errors = draft(&single_action(
            "httpRequest",
            json!({"url": "https://x.com", "headers": headers}),
        ));
        assert!(!has_path(&errors, "/headers"), "{headers}: {errors:?}");
    }
    for headers in ["{not json}", "NaN"] {
        let errors = draft(&single_action(
            "httpRequest",
            json!({"url": "https://x.com", "headers": headers}),
        ));
        assert!(has(&errors, "invalid_headers", "/headers"), "{headers}");
    }
}

#[test]
fn v055_a_margin_basket_holding_nan_parses_as_python_parses_it() {
    let leg = |pricetype: &str| {
        format!(
            r#"[{{"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 1, "product": "MIS", "pricetype": "{pricetype}", "price": NaN}}]"#
        )
    };
    assert_eq!(
        strict(&single_action(
            "margin",
            json!({"positionsJson": leg("MARKET")})
        )),
        vec![]
    );
    // A NaN price is unpriceable, as on the web; the number is kept as its
    // Python text, so the sentence quotes it.
    let errors = strict(&single_action(
        "margin",
        json!({"positionsJson": leg("LIMIT")}),
    ));
    let error = errors.iter().find(|e| e.code == "invalid_price").unwrap();
    assert_eq!(error.path, "/nodes/1/data/positionsJson/0/price");
    assert_eq!(
        error.message,
        "A LIMIT order needs a positive price; 'nan' cannot be priced."
    );
    let errors = strict(&single_action("margin", json!({"positionsJson": "[NaN]"})));
    assert_eq!(
        errors[0].message,
        "Every Margin position must be an object."
    );
}

// ---------------------------------------------------------------- V-057

/// An option leg that names `priceType` and also carries the API spelling
/// `pricetype` has both checked.
#[test]
fn v057_an_option_leg_naming_both_price_type_spellings_has_both_checked() {
    let leg = json!({"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1,
        "priceType": "LIMIT", "price": 1, "pricetype": "BOGUS"});
    let data = json!({"strategy": "custom", "underlying": "NIFTY", "quantity": 1, "legs": [leg]});
    let errors = draft(&single_action("optionsMultiOrder", data));
    let error = errors
        .iter()
        .find(|e| e.path.ends_with("/legs/0/pricetype"))
        .unwrap();
    assert_eq!(error.code, "invalid_constant");
    assert_eq!(error.message, "'BOGUS' is not a valid pricetype constant.");
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
