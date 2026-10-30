//! `test/test_flow_workflow_validator.py`, ported test for test. The
//! frontend parity tests read the desktop's carried-over editor sources, and
//! the prompt-contract test reads the web's import format, recorded under
//! `tests/fixtures/flow/`.

use super::*;
use crate::flow::node_contracts::parse_underlying_symbol;
use crate::flow::validator::{
    migrate_legacy_node_data, BRANCHING_NODE_TYPES, TRIGGER_NODE_TYPES, VALID_NODE_TYPES,
};
use serde_json::json;

fn priced_order_nodes() -> Vec<(&'static str, Value)> {
    vec![
        (
            "placeOrder",
            json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1}),
        ),
        (
            "smartOrder",
            json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1}),
        ),
        (
            "optionsOrder",
            json!({"underlying": "NIFTY", "action": "BUY", "quantity": 1}),
        ),
        (
            "optionsMultiOrder",
            json!({"strategy": "straddle", "underlying": "NIFTY", "quantity": 1}),
        ),
        ("basketOrder", json!({"orders": "RELIANCE,NSE,BUY,1"})),
        (
            "splitOrder",
            json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1, "splitSize": 1}),
        ),
    ]
}

const PRICE_CASES: [(&str, &str); 4] = [
    ("LIMIT", "price"),
    ("SL", "price"),
    ("SL", "triggerPrice"),
    ("SL-M", "triggerPrice"),
];

#[test]
fn priced_order_requires_each_static_price() {
    for (node_type, minimal) in priced_order_nodes() {
        for (price_type, field) in PRICE_CASES {
            let data = with(&minimal, json!({"priceType": price_type}));
            let suffix = format!("/{field}");
            assert!(
                has(
                    &strict_node(node_type, data.clone()),
                    "missing_price",
                    &suffix
                ),
                "{node_type} {price_type}"
            );
            assert!(
                !has_path(&draft_node(node_type, data), &suffix),
                "{node_type} {price_type}"
            );
        }
    }
}

#[test]
fn priced_order_price_values_follow_draft_and_runtime_contract() {
    let values: [(Value, Option<&str>, Option<&str>); 5] = [
        (json!(""), Some("missing_price"), None),
        (json!(0), Some("invalid_price"), Some("invalid_price")),
        (json!(-1), Some("invalid_price"), Some("invalid_price")),
        (json!(1), None, None),
        (json!("{{webhook.price}}"), None, None),
    ];
    for (node_type, minimal) in priced_order_nodes() {
        for (price_type, field) in PRICE_CASES {
            for (value, strict_code, draft_code) in &values {
                let data = with(&minimal, json!({"priceType": price_type, field: value}));
                let suffix = format!("/{field}");
                let label = format!("{node_type} {price_type} {field} {value}");
                let strict_errors = strict_node(node_type, data.clone());
                let draft_errors = draft_node(node_type, data);
                match strict_code {
                    Some(code) => assert!(has(&strict_errors, code, &suffix), "{label}"),
                    None => assert!(!has_path(&strict_errors, &suffix), "{label}"),
                }
                match draft_code {
                    Some(code) => assert!(has(&draft_errors, code, &suffix), "{label}"),
                    None => assert!(!has_path(&draft_errors, &suffix), "{label}"),
                }
            }
        }
    }
}

fn margin_leg_json(overrides: Value) -> String {
    let leg = with(
        &json!({
            "symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": "1",
            "product": "MIS", "pricetype": "MARKET", "price": "0"
        }),
        overrides,
    );
    json!([leg]).to_string()
}

/// `(data, strict code, draft code, path suffix)`. Where the web expects no
/// code its assertion is vacuous (`error["code"] == None` never holds); here
/// it asserts that nothing is reported at that path.
fn contract_cases_hold(node_type: &str, cases: Vec<(Value, Option<&str>, Option<&str>, &str)>) {
    for (data, strict_code, draft_code, path) in cases {
        let suffix = format!("/{path}");
        let strict_errors = strict_node(node_type, data.clone());
        let draft_errors = draft_node(node_type, data.clone());
        let check = |errors: &[ValidationError], code: Option<&str>| match code {
            Some(code) => has(errors, code, &suffix),
            None => !has_path(errors, &suffix),
        };
        assert!(
            check(&strict_errors, strict_code),
            "strict {data} -> {strict_errors:?}"
        );
        assert!(
            check(&draft_errors, draft_code),
            "draft {data} -> {draft_errors:?}"
        );
    }
}

#[test]
fn margin_contract() {
    contract_cases_hold(
        "margin",
        vec![
            (
                json!({}),
                Some("missing_alternative"),
                None,
                "positionsJson",
            ),
            (
                json!({"positionsJson": ""}),
                Some("missing_alternative"),
                None,
                "positionsJson",
            ),
            (
                json!({"positionsJson": "not json"}),
                Some("invalid_positions"),
                Some("invalid_positions"),
                "positionsJson",
            ),
            (
                json!({"positionsJson": "[]"}),
                Some("invalid_positions"),
                Some("invalid_positions"),
                "positionsJson",
            ),
            (
                json!({"positionsJson": "{}"}),
                Some("invalid_positions"),
                Some("invalid_positions"),
                "positionsJson",
            ),
            (
                json!({"positionsJson": "[\"not a leg\"]"}),
                Some("invalid_positions"),
                Some("invalid_positions"),
                "positionsJson/0",
            ),
            (
                json!({"positionsJson": "[{\"symbol\": \"RELIANCE\"}]"}),
                Some("missing_required_field"),
                None,
                "positionsJson/0/exchange",
            ),
            (
                json!({"positionsJson": margin_leg_json(json!({"exchange": "NOPE"}))}),
                Some("invalid_constant"),
                Some("invalid_constant"),
                "positionsJson/0/exchange",
            ),
            (
                json!({"positionsJson": margin_leg_json(json!({"pricetype": "LIMIT"}))}),
                Some("invalid_price"),
                Some("invalid_price"),
                "positionsJson/0/price",
            ),
            (
                json!({"positionsJson": "{{webhook.positions}}"}),
                None,
                None,
                "positionsJson",
            ),
            (
                json!({"positionsJson": margin_leg_json(json!({
                    "symbol": "{{symbol}}", "quantity": "{{quantity}}", "pricetype": "LIMIT", "price": "{{price}}"
                }))}),
                None,
                None,
                "positionsJson",
            ),
            (
                json!({"positionsJson": margin_leg_json(json!({}))}),
                None,
                None,
                "positionsJson",
            ),
            (
                json!({"positions": [{"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": "1",
                    "product": "MIS", "pricetype": "MARKET", "price": "0"}]}),
                None,
                None,
                "positions",
            ),
            (json!({"symbol": "RELIANCE"}), None, None, "symbol"),
        ],
    );
}

fn options_multi_base() -> Value {
    json!({"underlying": "NIFTY", "quantity": 1})
}

fn custom_option_leg() -> Value {
    json!({"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1})
}

#[test]
fn options_multi_contract() {
    let base = options_multi_base();
    let custom = |extra: Value| with(&with(&base, json!({"strategy": "custom"})), extra);
    contract_cases_hold(
        "optionsMultiOrder",
        vec![
            (
                custom(json!({})),
                Some("missing_required_field"),
                None,
                "legs",
            ),
            (
                custom(json!({"legs": []})),
                Some("invalid_legs"),
                Some("invalid_legs"),
                "legs",
            ),
            (
                custom(json!({"legs": {}})),
                Some("invalid_legs"),
                Some("invalid_legs"),
                "legs",
            ),
            (
                custom(json!({"legs": [{"offset": "ATM"}]})),
                Some("missing_required_field"),
                None,
                "legs/0/optionType",
            ),
            (
                custom(json!({"legs": [with(&custom_option_leg(), json!({"product": "NOPE"}))]})),
                Some("invalid_constant"),
                Some("invalid_constant"),
                "legs/0/product",
            ),
            (
                custom(json!({"legs": [with(&custom_option_leg(), json!({
                    "priceType": "SL", "price": 100, "triggerPrice": 99, "expiryDate": "27AUG26"
                }))]})),
                None,
                None,
                "legs",
            ),
            (
                custom(json!({"legs": [{
                    "offset": "{{offset}}", "optionType": "{{optionType}}", "action": "{{action}}",
                    "quantity": "{{quantity}}", "priceType": "{{priceType}}", "price": "{{price}}"
                }]})),
                None,
                None,
                "legs",
            ),
            (
                custom(json!({"legs": "{{webhook.legs}}"})),
                None,
                None,
                "legs",
            ),
            (
                with(
                    &base,
                    json!({"strategy": "straddle", "priceType": "SL", "price": 100}),
                ),
                Some("invalid_constant"),
                Some("invalid_constant"),
                "priceType",
            ),
            (
                with(
                    &base,
                    json!({"strategy": "straddle", "priceType": "MARKET"}),
                ),
                None,
                None,
                "price",
            ),
            (
                with(
                    &base,
                    json!({"strategy": "straddle", "priceType": "LIMIT", "price": 100}),
                ),
                None,
                None,
                "price",
            ),
        ],
    );
}

const VARIABLE_OPERATIONS: [(&str, &str, i32); 11] = [
    ("set", "", 0),
    ("get", "sourceVariable", 0),
    ("add", "value", 1),
    ("subtract", "value", 1),
    ("multiply", "value", 2),
    ("divide", "value", 2),
    ("increment", "", 0),
    ("decrement", "", 0),
    ("parse_json", "value", 0),
    ("stringify", "sourceVariable", 0),
    ("append", "", 0),
];

#[test]
fn variable_contract_accepts_supported_operations() {
    for (operation, field, number) in VARIABLE_OPERATIONS {
        let mut data = json!({"variableName": "target", "operation": operation});
        if field == "sourceVariable" {
            data = with(&data, json!({"sourceVariable": "source"}));
        } else if operation == "parse_json" {
            data = with(&data, json!({"value": "{\"key\": \"value\"}"}));
        } else if field == "value" {
            data = with(&data, json!({"value": number}));
        }
        for errors in [
            strict_node("variable", data.clone()),
            draft_node("variable", data.clone()),
        ] {
            for suffix in ["/operation", "/sourceVariable", "/value"] {
                assert!(!has_path(&errors, suffix), "{operation}: {errors:?}");
            }
        }
    }
}

#[test]
fn variable_contract_rejects_unknown_operations_even_in_drafts() {
    let data = json!({"variableName": "target", "operation": "merge"});
    for errors in [
        strict_node("variable", data.clone()),
        draft_node("variable", data),
    ] {
        assert!(has(&errors, "invalid_constant", "/operation"));
    }
}

#[test]
fn variable_contract_requires_a_source_variable_in_strict_mode() {
    for operation in ["get", "stringify"] {
        let data = json!({"variableName": "target", "operation": operation});
        assert!(has(
            &strict_node("variable", data.clone()),
            "missing_required_field",
            "/sourceVariable"
        ));
        assert!(!has_path(&draft_node("variable", data), "/sourceVariable"));
    }
}

#[test]
fn variable_contract_requires_a_value_in_strict_mode() {
    for operation in ["add", "subtract", "multiply", "divide", "parse_json"] {
        let data = json!({"variableName": "target", "operation": operation});
        assert!(has(
            &strict_node("variable", data.clone()),
            "missing_required_field",
            "/value"
        ));
        assert!(!has_path(&draft_node("variable", data), "/value"));
    }
}

#[test]
fn variable_contract_defers_templated_conditional_values() {
    for (operation, field) in [("get", "sourceVariable"), ("add", "value")] {
        let data =
            json!({"variableName": "target", "operation": operation, field: "{{webhook.value}}"});
        assert!(!has_path(
            &strict_node("variable", data),
            &format!("/{field}")
        ));
    }
}

#[test]
fn variable_contract_defaults_a_missing_operation_to_set() {
    assert!(!has_path(
        &strict_node("variable", json!({"variableName": "target"})),
        "/operation"
    ));
}

#[test]
fn variable_contract_rejects_noncanonical_operation_spellings() {
    for operation in ["", "SET", " set "] {
        let data = json!({"variableName": "target", "operation": operation});
        for errors in [
            strict_node("variable", data.clone()),
            draft_node("variable", data),
        ] {
            assert!(
                has(&errors, "invalid_constant", "/operation"),
                "{operation:?}"
            );
        }
    }
}

/// `float("inf")` cannot travel in JSON, so the text forms stand for it.
const NON_FINITE: [&str; 3] = ["Infinity", "-Infinity", "NaN"];

#[test]
fn executable_price_rejects_boolean_and_nonfinite_values() {
    let place = &priced_order_nodes()[0].1;
    let mut values = vec![json!(true)];
    values.extend(NON_FINITE.iter().map(|v| json!(v)));
    for value in values {
        let data = with(place, json!({"priceType": "LIMIT", "price": value}));
        for errors in [
            strict_node("placeOrder", data.clone()),
            draft_node("placeOrder", data),
        ] {
            assert!(has(&errors, "invalid_price", "/price"), "{value}");
        }
    }
}

#[test]
fn custom_option_leg_quantity_rejects_boolean_and_nonfinite_values() {
    let mut values = vec![json!(true)];
    values.extend(NON_FINITE.iter().map(|v| json!(v)));
    for value in values {
        let leg = with(&custom_option_leg(), json!({"quantity": value}));
        let data = with(
            &options_multi_base(),
            json!({"strategy": "custom", "legs": [leg]}),
        );
        for errors in [
            strict_node("optionsMultiOrder", data.clone()),
            draft_node("optionsMultiOrder", data),
        ] {
            assert!(
                has(&errors, "invalid_quantity", "/legs/0/quantity"),
                "{value}"
            );
        }
    }
}

#[test]
fn executable_price_and_custom_leg_quantity_keep_finite_values() {
    let place = &priced_order_nodes()[0].1;
    for value in [json!(1), json!("1.25")] {
        let price_data = with(place, json!({"priceType": "LIMIT", "price": value}));
        let leg = with(&custom_option_leg(), json!({"quantity": value}));
        let leg_data = with(
            &options_multi_base(),
            json!({"strategy": "custom", "legs": [leg]}),
        );
        for errors in [
            strict_node("placeOrder", price_data.clone()),
            draft_node("placeOrder", price_data),
        ] {
            assert!(!has_path(&errors, "/price"), "{value}");
        }
        for errors in [
            strict_node("optionsMultiOrder", leg_data.clone()),
            draft_node("optionsMultiOrder", leg_data),
        ] {
            assert!(!has_path(&errors, "/legs/0/quantity"), "{value}");
        }
    }
}

#[test]
fn margin_contract_defers_any_template_containing_positions_json() {
    for positions in ["{{webhook.positions}}", "[{{webhook.positions}}]"] {
        let data = json!({"positionsJson": positions});
        for errors in [
            strict_node("margin", data.clone()),
            draft_node("margin", data),
        ] {
            assert!(!has_path(&errors, "/positionsJson"));
        }
    }
}

#[test]
fn indicator_requirements_follow_its_data_source() {
    let cases: [(Value, &[&str]); 3] = [
        (
            json!({"indicatorName": "RSI", "sourceSeries": "{{bars}}"}),
            &[],
        ),
        (json!({"indicatorName": "RSI"}), &["symbol", "exchange"]),
        (json!({"sourceSeries": "{{bars}}"}), &["indicatorName"]),
    ];
    for (data, missing) in cases {
        let errors = strict(&single_action("indicator", data));
        let mut found: Vec<&str> = errors
            .iter()
            .filter(|e| e.code == "missing_required_field")
            .filter_map(|e| e.path.rsplit('/').next())
            .collect();
        found.sort_unstable();
        let mut want = missing.to_vec();
        want.sort_unstable();
        assert_eq!(found, want);
    }
}

#[test]
fn smart_order_quantity_allows_zero_but_rejects_negative_values() {
    for (quantity, invalid) in [
        (json!(0), false),
        (json!("0"), false),
        (json!(-1), true),
        (json!(1), false),
    ] {
        let data =
            json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": quantity});
        let errors = strict(&single_action("smartOrder", data));
        assert_eq!(
            codes(&errors).contains(&"invalid_quantity"),
            invalid,
            "{quantity}"
        );
    }
}

#[test]
fn place_order_quantity_must_stay_positive() {
    let data = json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 0});
    assert!(codes(&strict(&single_action("placeOrder", data))).contains(&"invalid_quantity"));
}

#[test]
fn order_update_trigger_accepts_watchable_filters_and_normalized_status() {
    for data in [
        json!({"orderId": "241001000000001", "status": "complete"}),
        json!({"symbol": "RELIANCE", "status": " TRIGGER_PENDING "}),
    ] {
        let errors = strict_node("orderUpdateTrigger", data);
        for suffix in ["/orderId", "/symbol", "/status"] {
            assert!(!has_path(&errors, suffix), "{errors:?}");
        }
    }
}

#[test]
fn order_update_trigger_rejects_unwatchable_filters() {
    for extra in [json!({}), json!({"orderId": "{{previous.orderid}}"})] {
        let errors = strict_node(
            "orderUpdateTrigger",
            with(&json!({"status": "complete"}), extra),
        );
        assert!(errors
            .iter()
            .any(|e| e.code == "missing_alternative" || e.code == "invalid_trigger_filter"));
    }
}

#[test]
fn order_update_trigger_rejects_unknown_status() {
    let errors = strict_node(
        "orderUpdateTrigger",
        json!({"orderId": "241001000000001", "status": "filled"}),
    );
    assert!(has(&errors, "invalid_status", "/status"));
}

#[test]
fn order_update_trigger_rejects_invalid_statuses_when_drafts_are_saved() {
    for status in [json!("filled"), json!(123)] {
        let errors = draft_node(
            "orderUpdateTrigger",
            json!({"orderId": "241001000000001", "status": status}),
        );
        assert!(has(&errors, "invalid_status", "/status"), "{status}");
    }
}

#[test]
fn order_update_trigger_rejects_non_string_filters_when_drafts_are_saved() {
    for (field, value, other) in [
        ("orderId", json!(123), json!({"symbol": "RELIANCE"})),
        (
            "symbol",
            json!(["RELIANCE"]),
            json!({"orderId": "241001000000001"}),
        ),
    ] {
        let data = with(
            &with(&json!({"status": "complete"}), json!({field: value})),
            other,
        );
        assert!(has(
            &draft_node("orderUpdateTrigger", data),
            "invalid_type",
            &format!("/{field}")
        ));
    }
}

#[test]
fn expiry_requirement_follows_node_and_underlying() {
    assert_eq!(
        parse_underlying_symbol("NIFTY27AUG26").1.as_deref(),
        Some("27AUG26")
    );
    for (node_type, data, requires_expiry) in [
        (
            "optionSymbol",
            json!({"underlying": "NIFTY", "optionType": "CE", "expiryDate": "27AUG26"}),
            false,
        ),
        (
            "optionSymbol",
            json!({"underlying": "NIFTY27AUG26", "optionType": "CE"}),
            false,
        ),
        (
            "optionSymbol",
            json!({"underlying": "NIFTY", "optionType": "CE"}),
            true,
        ),
        (
            "optionChain",
            json!({"underlying": "NIFTY", "expiryDate": "27AUG26"}),
            false,
        ),
        ("optionChain", json!({"underlying": "NIFTY27AUG26"}), false),
        ("optionChain", json!({"underlying": "NIFTY"}), true),
        (
            "syntheticFuture",
            json!({"underlying": "NIFTY", "expiryDate": "27AUG26"}),
            false,
        ),
        ("syntheticFuture", json!({"underlying": "NIFTY"}), true),
        (
            "optionChain",
            json!({"underlying": "{{previous.underlying}}"}),
            false,
        ),
    ] {
        let errors = strict(&single_action(node_type, data.clone()));
        assert_eq!(
            has(&errors, "missing_required_field", "/expiryDate"),
            requires_expiry,
            "{node_type} {data}"
        );
    }
}

#[test]
fn valid_workflow_passes() {
    let payload = wf(
        vec![
            node("n1", "start", json!({})),
            node("n2", "log", json!({"message": "hi"})),
        ],
        vec![edge("e1", "n1", "n2")],
    );
    assert_eq!(strict(&payload), vec![]);
}

#[test]
fn whatsapp_alert_allows_self_send_without_recipient() {
    for alert in [
        json!({"message": "Workflow executed successfully"}),
        json!({"message": "Workflow executed successfully", "to": ""}),
    ] {
        let payload = wf(
            vec![
                node("t", "start", json!({})),
                node("wa", "whatsappAlert", alert),
            ],
            vec![edge("e1", "t", "wa")],
        );
        assert_eq!(strict(&payload), vec![]);
    }
}

#[test]
fn whatsapp_alert_still_requires_a_message() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("wa", "whatsappAlert", json!({"to": ""})),
        ],
        vec![edge("e1", "t", "wa")],
    );
    assert!(strict(&payload)
        .iter()
        .any(|e| e.path == "/nodes/1/data/message"));
}

#[test]
fn rejects_malformed_workflows() {
    let at = json!({"x": 0, "y": 0});
    let cases = [
        (json!({"nodes": [], "edges": []}), "required"),
        (json!({"name": "W", "edges": []}), "required"),
        (json!({"name": "W", "nodes": []}), "required"),
        (
            wf(
                vec![json!({"id": "a", "type": "NotARealNode", "position": at, "data": {}})],
                vec![],
            ),
            "unknown_node_type",
        ),
        (
            wf(
                vec![json!({"type": "start", "position": at, "data": {}})],
                vec![],
            ),
            "required",
        ),
        (
            wf(
                vec![json!({"id": "a", "type": "start", "position": at})],
                vec![],
            ),
            "required",
        ),
        (
            wf(
                vec![json!({"id": "a", "type": "start", "data": {}})],
                vec![],
            ),
            "required",
        ),
        (
            wf(
                vec![node("a", "start", json!({})), node("a", "log", json!({}))],
                vec![],
            ),
            "duplicate",
        ),
        (
            wf(
                vec![node("n1", "start", json!({}))],
                vec![edge("e1", "n1", "ghost")],
            ),
            "dangling_edge",
        ),
        (
            wf(
                vec![node("a", "start", json!({})), node("b", "start", json!({}))],
                vec![],
            ),
            "multiple_triggers",
        ),
        (wf(vec![node("a", "log", json!({}))], vec![]), "no_trigger"),
        (json!(["not", "an", "object"]), "invalid_type"),
    ];
    for (payload, code) in cases {
        assert!(codes(&strict(&payload)).contains(&code), "{payload} {code}");
    }
}

#[test]
fn rejects_the_invented_schema_shape() {
    let payload = json!({"strategy": "Gap fill", "settings": {}, "variables": {}, "flow": []});
    let paths: Vec<String> = strict(&payload).into_iter().map(|e| e.path).collect();
    for path in ["/name", "/nodes", "/edges"] {
        assert!(paths.iter().any(|p| p == path), "{path}");
    }
}

#[test]
fn errors_carry_a_path_and_a_code() {
    let errors = strict(&json!({"name": "W", "nodes": [{"type": "start"}], "edges": []}));
    assert!(!errors.is_empty());
    for error in errors {
        assert!(error.path.starts_with('/'));
        assert!(!error.code.is_empty() && !error.message.is_empty());
    }
}

#[test]
fn trigger_types_are_registered_node_types() {
    for trigger in TRIGGER_NODE_TYPES {
        assert!(VALID_NODE_TYPES.contains(&trigger));
    }
}

#[test]
fn detects_a_cycle() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("a", "log", json!({})),
            node("b", "log", json!({})),
        ],
        vec![
            edge("e1", "t", "a"),
            edge("e2", "a", "b"),
            edge("e3", "b", "a"),
        ],
    );
    assert!(codes(&strict(&payload)).contains(&"cycle"));
}

#[test]
fn detects_unreachable_nodes() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("a", "log", json!({})),
            node("orphan", "log", json!({})),
        ],
        vec![edge("e1", "t", "a")],
    );
    assert!(codes(&strict(&payload)).contains(&"unreachable"));
}

#[test]
fn validates_source_handles() {
    for (source_type, handle, expected_error) in [
        ("varCondition", "maybe", true),
        ("varCondition", "true", false),
        ("varCondition", "no", false),
        ("getQuote", "true", true),
    ] {
        let condition =
            json!({"symbol": "X", "exchange": "NSE", "leftValue": "{{x}}", "operator": ">"});
        let payload = wf(
            vec![
                node("t", "start", json!({})),
                node("c", source_type, condition),
                node("a", "log", json!({})),
            ],
            vec![
                edge("e1", "t", "c"),
                json!({"id": "e2", "source": "c", "target": "a", "sourceHandle": handle}),
            ],
        );
        assert_eq!(
            codes(&strict(&payload)).contains(&"invalid_source_handle"),
            expected_error,
            "{source_type} {handle}"
        );
    }
}

#[test]
fn requires_order_fields() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node(
                "o",
                "placeOrder",
                json!({"exchange": "NSE", "action": "BUY"}),
            ),
        ],
        vec![edge("e1", "t", "o")],
    );
    assert!(codes(&strict(&payload)).contains(&"missing_required_field"));
}

#[test]
fn rejects_a_workflow_with_no_nodes() {
    assert!(codes(&strict(&wf(vec![], vec![]))).contains(&"no_trigger"));
}

#[test]
fn partial_graphs_stay_savable() {
    let half = wf(
        vec![
            node("t", "start", json!({})),
            node("o", "placeOrder", json!({})),
        ],
        vec![],
    );
    assert_eq!(draft(&half), vec![]);
    assert_ne!(strict(&half), vec![]);
}

#[test]
fn corrupt_graphs_are_rejected_even_when_saving() {
    let corrupt = wf(
        vec![json!({"id": "a", "type": "NotReal", "position": {"x": 0, "y": 0}, "data": {}})],
        vec![],
    );
    assert!(codes(&draft(&corrupt)).contains(&"unknown_node_type"));
}

#[test]
fn malformed_gate_input_count_does_not_crash() {
    let payload = wf(
        vec![node("g", "andGate", json!({"inputCount": "abc"}))],
        vec![],
    );
    strict(&payload);
}

#[test]
fn empty_split_order_is_rejected() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("s", "splitOrder", json!({})),
        ],
        vec![edge("e1", "t", "s")],
    );
    assert!(codes(&strict(&payload)).contains(&"missing_required_field"));
}

#[test]
fn phantom_handle_on_a_non_branching_node_is_rejected() {
    let payload = wf(
        vec![
            node("t", "start", json!({})),
            node("q", "getQuote", json!({"symbol": "X", "exchange": "NSE"})),
            node("a", "log", json!({})),
        ],
        vec![
            edge("e1", "t", "q"),
            json!({"id": "e2", "source": "q", "target": "a", "sourceHandle": "output-9"}),
        ],
    );
    assert!(codes(&strict(&payload)).contains(&"invalid_source_handle"));
}

#[test]
fn price_alert_may_use_its_own_branch_handles() {
    let payload = wf(
        vec![
            node(
                "t",
                "priceAlert",
                json!({"symbol": "X", "exchange": "NSE", "condition": "above", "price": 100}),
            ),
            node("a", "log", json!({})),
        ],
        vec![json!({"id": "e1", "source": "t", "target": "a", "sourceHandle": "true"})],
    );
    assert!(!codes(&strict(&payload)).contains(&"invalid_source_handle"));
}

#[test]
fn position_check_migration_preserves_the_comparison() {
    for (operator, expected) in [
        ("gt", Some("quantity_above")),
        ("lt", Some("quantity_below")),
        ("weird", None),
    ] {
        let nodes = vec![node(
            "p",
            "positionCheck",
            json!({"operator": operator, "threshold": 5}),
        )];
        let (migrated, _) = migrate_legacy_node_data(nodes);
        assert_eq!(
            migrated[0]["data"].get("condition").and_then(Value::as_str),
            expected
        );
        assert_eq!(migrated[0]["data"]["threshold"], json!(5));
    }
}

#[test]
fn fund_check_migration_never_reverses_the_guard() {
    let (migrated, _) = migrate_legacy_node_data(vec![node(
        "f",
        "fundCheck",
        json!({"operator": "gt", "threshold": 10000}),
    )]);
    assert_eq!(migrated[0]["data"], json!({"minAvailable": 10000}));

    let (migrated, notes) = migrate_legacy_node_data(vec![node(
        "f",
        "fundCheck",
        json!({"operator": "lt", "threshold": 10000}),
    )]);
    assert!(migrated[0]["data"].get("minAvailable").is_none());
    assert!(notes.iter().any(|n| n.contains("no equivalent")));
}

#[test]
fn price_alert_requirements_follow_the_condition() {
    for (condition, extra, expect_error) in [
        ("above", json!({"price": 100}), false),
        ("above", json!({}), true),
        (
            "entering_channel",
            json!({"priceLower": 90, "priceUpper": 110}),
            false,
        ),
        ("entering_channel", json!({"price": 100}), true),
        ("moving_up_percent", json!({"percentage": 2}), false),
    ] {
        let alert = with(
            &json!({"symbol": "X", "exchange": "NSE", "condition": condition}),
            extra,
        );
        let payload = wf(
            vec![
                node("t", "priceAlert", alert),
                node("a", "log", json!({"message": "hi"})),
            ],
            vec![edge("e1", "t", "a")],
        );
        assert_eq!(
            codes(&strict(&payload)).contains(&"missing_required_field"),
            expect_error,
            "{condition}"
        );
    }
}

#[test]
fn every_level_alias_requires_a_price() {
    for condition in [
        "above",
        "price_above",
        "crosses_above",
        "cross_above",
        "crosses",
        "ABOVE",
        " above ",
    ] {
        let payload = wf(
            vec![
                node(
                    "t",
                    "priceAlert",
                    json!({"symbol": "X", "exchange": "NSE", "condition": condition}),
                ),
                node("a", "log", json!({"message": "hi"})),
            ],
            vec![edge("e1", "t", "a")],
        );
        assert!(
            codes(&strict(&payload)).contains(&"missing_required_field"),
            "{condition}"
        );
    }
}

#[test]
fn unknown_alert_condition_is_rejected() {
    let payload = wf(
        vec![
            node(
                "t",
                "priceAlert",
                json!({"symbol": "X", "exchange": "NSE", "condition": "abov", "price": 100}),
            ),
            node("a", "log", json!({"message": "hi"})),
        ],
        vec![edge("e1", "t", "a")],
    );
    assert!(codes(&strict(&payload)).contains(&"unknown_condition"));
}

/// `blueprints.flow.create_workflow`: only a graph that was sent is checked.
fn create_route(data: &Value) -> Vec<ValidationError> {
    let sent_graph = data.get("nodes").is_some() || data.get("edges").is_some();
    if !sent_graph {
        return Vec::new();
    }
    let or_empty = |key: &str| {
        data.get(key)
            .filter(|v| crate::flow::validator::py::truthy(v))
            .cloned()
    };
    draft(&json!({
        "name": or_empty("name").unwrap_or(json!("")),
        "nodes": or_empty("nodes").unwrap_or(json!([])),
        "edges": or_empty("edges").unwrap_or(json!([])),
    }))
}

#[test]
fn creating_a_new_workflow_needs_no_graph() {
    assert_eq!(create_route(&json!({"name": "My Strategy"})), vec![]);
    assert_eq!(
        create_route(&json!({"name": "My Strategy", "description": "x"})),
        vec![]
    );
    assert_eq!(
        create_route(&json!({"name": "My Strategy", "nodes": [], "edges": []})),
        vec![]
    );
    let corrupt = json!({
        "name": "S",
        "nodes": [{"id": "a", "type": "Nope", "position": {"x": 0, "y": 0}, "data": {}}],
        "edges": [],
    });
    assert!(codes(&create_route(&corrupt)).contains(&"unknown_node_type"));
}

#[test]
fn a_complete_editor_built_strategy_activates() {
    let nodes = vec![
        node(
            "n1",
            "start",
            json!({"scheduleType": "interval", "intervalValue": 1, "intervalUnit": "minutes"}),
        ),
        node(
            "n2",
            "getQuote",
            json!({"symbol": "RELIANCE", "exchange": "NSE", "outputVariable": "q"}),
        ),
        node(
            "n3",
            "varCondition",
            json!({"leftValue": "{{q.data.ltp}}", "operator": ">", "rightValue": "1000"}),
        ),
        node(
            "n4",
            "placeOrder",
            json!({"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 1}),
        ),
    ];
    let edges = vec![
        edge("e1", "n1", "n2"),
        edge("e2", "n2", "n3"),
        json!({"id": "e3", "source": "n3", "target": "n4", "sourceHandle": "true"}),
    ];
    assert_eq!(strict(&wf(nodes, edges)), vec![]);
}

#[test]
fn branching_set_is_the_web_set() {
    let mut mine = BRANCHING_NODE_TYPES.to_vec();
    mine.sort_unstable();
    assert_eq!(
        mine,
        vec![
            "andGate",
            "fundCheck",
            "notGate",
            "orGate",
            "positionCheck",
            "priceAlert",
            "priceCondition",
            "timeCondition",
            "timeWindow",
            "varCondition"
        ]
    );
}
