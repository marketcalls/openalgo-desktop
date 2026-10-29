//! The validator parts of the web's other Flow tests:
//! `test/test_flow_custom_legs.py` (custom legs), `test/test_flow_qa_regressions.py`
//! (order constants, quantities, prices, httpRequest, indicator params, the
//! fields the guards read) and `test/test_flow_final_fix.py` (canonical
//! constants, inherited leg prices).

use super::*;
use crate::flow::node_contracts::{VALID_EXPIRY_TYPES, VALID_LEG_STRIKE_MODES};
use crate::flow::validator::{EITHER_REQUIRED_FIELDS, REQUIRED_NODE_FIELDS};
use serde_json::json;

// ---------------------------------------------------- test_flow_custom_legs

fn base_leg() -> Value {
    json!({"optionType": "CE", "action": "BUY", "quantity": 1})
}

/// The web's `validate_legs`: codes for a custom multi-leg node.
fn validate_legs(legs: Vec<Value>, strict_level: bool) -> Vec<&'static str> {
    let data = json!({"strategy": "custom", "underlying": "NIFTY", "quantity": 1, "legs": legs});
    let payload = json!({
        "name": "t",
        "nodes": [
            {"id": "n1", "type": "start", "position": {"x": 0, "y": 0}, "data": {}},
            {"id": "n2", "type": "optionsMultiOrder", "position": {"x": 0, "y": 0}, "data": data},
        ],
        "edges": [{"id": "e1", "source": "n1", "target": "n2"}],
    });
    let options = ValidateOptions {
        require_name: false,
        strict: strict_level,
    };
    codes(&validate_workflow(&payload, options))
}

fn leg(extra: Value) -> Vec<Value> {
    vec![with(&base_leg(), extra)]
}

#[test]
fn an_offset_leg_is_accepted() {
    assert!(validate_legs(leg(json!({"offset": "ATM"})), true).is_empty());
}

#[test]
fn a_leg_naming_an_absolute_strike_is_accepted() {
    let strike = json!({"strikeMode": "STRIKE", "strike": 24500});
    assert!(validate_legs(leg(strike.clone()), true).is_empty());
    assert!(!validate_legs(leg(strike), true).contains(&"missing_required_field"));
}

#[test]
fn a_leg_naming_neither_cannot_execute_but_still_saves() {
    assert!(validate_legs(vec![base_leg()], true).contains(&"missing_required_field"));
    assert!(validate_legs(vec![base_leg()], false).is_empty());
}

#[test]
fn a_strike_that_names_no_contract_is_refused() {
    for strike in [json!(0), json!(-100), json!("0"), json!("abc"), json!(true)] {
        let codes = validate_legs(leg(json!({"strikeMode": "STRIKE", "strike": strike})), true);
        assert!(codes.contains(&"invalid_strike"), "{strike}");
    }
}

#[test]
fn a_usable_strike_is_accepted_in_either_type() {
    for strike in [json!(24500), json!(24500.5), json!("24500")] {
        assert!(
            validate_legs(leg(json!({"strikeMode": "STRIKE", "strike": strike})), true).is_empty()
        );
    }
}

#[test]
fn the_offset_range_the_executor_accepts() {
    for offset in ["ATM", "ITM1", "ITM50", "OTM1", "OTM50"] {
        assert!(
            validate_legs(leg(json!({"offset": offset})), true).is_empty(),
            "{offset}"
        );
    }
    for offset in ["OTM99", "ATM1", "DEEP", "ITM0"] {
        assert!(
            validate_legs(leg(json!({"offset": offset})), true).contains(&"invalid_constant"),
            "{offset}"
        );
    }
}

#[test]
fn both_strike_modes_are_known_and_others_refused() {
    assert_eq!(VALID_LEG_STRIKE_MODES, ["OFFSET", "STRIKE"]);
    let codes = validate_legs(leg(json!({"strikeMode": "NEAREST", "strike": 100})), true);
    assert!(codes.contains(&"invalid_constant"));
}

#[test]
fn a_leg_may_override_the_node_expiry() {
    assert!(validate_legs(leg(json!({"offset": "ATM", "expiry": "28OCT25"})), true).is_empty());
    for expiry_type in VALID_EXPIRY_TYPES {
        assert!(validate_legs(
            leg(json!({"offset": "ATM", "expiryType": expiry_type})),
            true
        )
        .is_empty());
    }
    for expiry in ["2025-10-28", "28-OCT-25", "OCT2825", "28OCTOBER25"] {
        let codes = validate_legs(leg(json!({"offset": "ATM", "expiry": expiry})), true);
        assert!(codes.contains(&"invalid_expiry"), "{expiry}");
    }
    let codes = validate_legs(
        leg(json!({"offset": "ATM", "expiryType": "next_year"})),
        true,
    );
    assert!(codes.contains(&"invalid_constant"));
    assert!(validate_legs(leg(json!({"offset": "ATM"})), true).is_empty());
}

#[test]
fn a_calendar_spread_validates() {
    let legs = vec![
        with(
            &base_leg(),
            json!({"strikeMode": "STRIKE", "strike": 24500, "expiry": "28OCT25"}),
        ),
        json!({"optionType": "CE", "action": "SELL", "quantity": 1, "strikeMode": "STRIKE",
            "strike": 24500, "expiry": "25NOV25"}),
    ];
    assert!(validate_legs(legs, true).is_empty());
}

#[test]
fn a_variable_reference_is_not_checked_for_shape() {
    for (field, value) in [
        ("strike", "{{webhook.strike}}"),
        ("offset", "{{webhook.offset}}"),
        ("expiry", "{{webhook.expiry}}"),
        ("expiryType", "{{webhook.expiryType}}"),
        ("strikeMode", "{{webhook.mode}}"),
    ] {
        let extra = with(&json!({"offset": "ATM"}), json!({field: value}));
        assert!(validate_legs(leg(extra), true).is_empty(), "{field}");
    }
}

// ------------------------------------------------- test_flow_qa_regressions

fn valid_order() -> Value {
    json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10, "product": "MIS",
        "priceType": "MARKET"})
}

/// The web's `_validate`: codes for start -> node, strict.
fn validate(node_type: &str, data: Value) -> Vec<&'static str> {
    codes(&strict(&wf(
        vec![node("n1", "start", json!({})), node("n2", node_type, data)],
        vec![edge("e1", "n1", "n2")],
    )))
}

/// The web's `_validate_lenient`: the ordinary editor save.
fn validate_lenient(node_type: &str, data: Value) -> Vec<&'static str> {
    codes(&draft(&wf(
        vec![node("n1", "start", json!({})), node("n2", node_type, data)],
        vec![edge("e1", "n1", "n2")],
    )))
}

#[test]
fn the_guards_need_the_fields_they_read() {
    let fund_check = EITHER_REQUIRED_FIELDS
        .iter()
        .find(|(t, _)| *t == "fundCheck")
        .unwrap();
    assert!(fund_check.1.contains(&&["minAvailable", "threshold"][..]));
    let position_check = REQUIRED_NODE_FIELDS
        .iter()
        .find(|(t, _)| *t == "positionCheck")
        .unwrap();
    assert!(position_check.1.contains(&"symbol") && position_check.1.contains(&"condition"));
}

#[test]
fn order_constants_are_checked() {
    assert!(validate("placeOrder", valid_order()).is_empty());
    for (field, value) in [
        ("exchange", "NSEE"),
        ("action", "BUYY"),
        ("product", "INTRADAY"),
        ("priceType", "LIMT"),
    ] {
        assert!(
            validate("placeOrder", with(&valid_order(), json!({field: value})))
                .contains(&"invalid_constant")
        );
    }
    for quantity in [json!(0), json!(-5), json!("abc")] {
        let codes = validate(
            "placeOrder",
            with(&valid_order(), json!({"quantity": quantity})),
        );
        assert!(codes.contains(&"invalid_quantity"), "{quantity}");
    }
    for field in ["exchange", "action", "quantity"] {
        assert!(validate(
            "placeOrder",
            with(&valid_order(), json!({field: "{{webhook.value}}"}))
        )
        .is_empty());
    }
    assert!(validate(
        "placeOrder",
        with(&valid_order(), json!({"exchange": "nse", "action": "buy"}))
    )
    .is_empty());
    for value in [json!(123), json!(true), json!(4.5)] {
        let codes = validate(
            "placeOrder",
            with(&valid_order(), json!({"exchange": value})),
        );
        assert!(codes.contains(&"invalid_constant"), "{value}");
    }
}

#[test]
fn an_ordinary_save_checks_values_but_not_presence() {
    let bad = with(&valid_order(), json!({"exchange": "NSEE"}));
    assert!(validate_lenient("placeOrder", bad).contains(&"invalid_constant"));
    assert!(validate_lenient("placeOrder", json!({})).is_empty());
}

#[test]
fn a_priced_order_needs_a_price() {
    for (price_type, field, value) in [
        ("LIMIT", "price", 0),
        ("LIMIT", "price", -10),
        ("SL", "price", 0),
        ("SL-M", "triggerPrice", 0),
    ] {
        let data = with(
            &valid_order(),
            json!({"priceType": price_type, field: value}),
        );
        assert!(
            validate("placeOrder", data).contains(&"invalid_price"),
            "{price_type} {field}"
        );
    }
    assert!(validate(
        "placeOrder",
        with(&valid_order(), json!({"priceType": "MARKET", "price": 0}))
    )
    .is_empty());
    let templated = with(
        &valid_order(),
        json!({"priceType": "LIMIT", "price": "{{webhook.px}}"}),
    );
    assert!(validate("placeOrder", templated).is_empty());
}

#[test]
fn http_request_shape_is_checked_before_run_time() {
    for (data, code) in [
        (
            json!({"url": "https://x.com", "headers": "{not json}"}),
            "invalid_headers",
        ),
        (
            json!({"url": "https://x.com", "headers": "[\"a\"]"}),
            "invalid_headers",
        ),
        (
            json!({"url": "https://x.com", "timeout": 999_999}),
            "invalid_timeout",
        ),
        (
            json!({"url": "https://x.com", "timeout": 0}),
            "invalid_timeout",
        ),
    ] {
        assert!(
            validate("httpRequest", data.clone()).contains(&code),
            "{data}"
        );
    }
    let valid = json!({"url": "https://x.com", "headers": "{\"A\": \"b\"}", "timeout": 30000});
    assert!(validate("httpRequest", valid).is_empty());
    for method in ["TRACE", "CONNECT", "OPTIONS"] {
        let codes = validate(
            "httpRequest",
            json!({"url": "https://x.com", "method": method}),
        );
        assert!(codes.contains(&"invalid_method"), "{method}");
    }
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
        assert!(validate(
            "httpRequest",
            json!({"url": "https://x.com", "method": method})
        )
        .is_empty());
    }
    for timeout in [1, 10, 999] {
        let codes = validate(
            "httpRequest",
            json!({"url": "https://x.com", "timeout": timeout}),
        );
        assert!(codes.contains(&"invalid_timeout"), "{timeout}");
    }
}

#[test]
fn indicator_params_are_a_json_string() {
    let indicator = json!({"symbol": "SBIN", "exchange": "NSE", "indicatorName": "rsi"});
    let object_form = with(&indicator, json!({"params": {"period": 14}}));
    assert!(validate("indicator", object_form).contains(&"invalid_params"));
    let string_form = with(&indicator, json!({"params": "{\"period\": 14}"}));
    assert!(validate("indicator", string_form).is_empty());
}

// ------------------------------------------------------ test_flow_final_fix

fn strict_action_workflow(node_type: &str, data: Value) -> Value {
    json!({
        "name": format!("strict-{node_type}"),
        "nodes": [
            {"id": "trigger", "type": "webhookTrigger", "position": {"x": 0, "y": 0}, "data": {}},
            {"id": "action", "type": node_type, "position": {"x": 0, "y": 100}, "data": data},
        ],
        "edges": [{"id": "edge", "source": "trigger", "target": "action"}],
    })
}

#[test]
fn trimmed_lowercase_constants_are_accepted_on_import() {
    let data = json!({"symbol": "SBIN", "exchange": " nse ", "action": " buy ", "quantity": 1,
        "positionSize": 0, "product": " mis ", "priceType": " limit ", "price": 100});
    assert_eq!(strict(&strict_action_workflow("smartOrder", data)), vec![]);
}

#[test]
fn custom_legs_inherit_common_prices_under_both_spellings() {
    for legs_key in ["legs", "orderLegs"] {
        let leg = json!({"offset": "ATM", "optionType": "CE", "action": "BUY", "quantity": 1,
            "pricetype": " sl "});
        let data = json!({"underlying": "NIFTY", "expiryType": "current_week", "strategy": "custom",
            "quantity": 1, "action": "SELL", "product": " mis ", "priceType": " sl ", "price": 110,
            "triggerPrice": 105, legs_key: [leg]});
        assert_eq!(
            strict(&strict_action_workflow("optionsMultiOrder", data)),
            vec![],
            "{legs_key}"
        );
    }
}
