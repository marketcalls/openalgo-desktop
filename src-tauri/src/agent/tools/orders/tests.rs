//! The order toolkit's contract with the web: names, order, descriptions,
//! argument schemas, the approval gate, surfaces and capability.

use super::*;
use crate::agent::contract::tool::{Capabilities, Surface};
use crate::agent::tool_guard::{missing_arguments, validate};
use crate::agent::tools::testing::ctx;
use crate::agent::tools::{build_tools, TOOLKITS};
use serde_json::json;

fn spec(name: &str) -> &'static ToolSpec {
    specs().iter().find(|s| s.name == name).expect("an order tool")
}

fn args(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// T-150, T-151, T-152: seven tools, in the web's order, every one of them
/// paused for approval; the approval list and the tool list are one.
#[test]
fn seven_tools_in_the_webs_order_all_needing_approval() {
    let names: Vec<&str> = specs().iter().map(|s| s.name).collect();
    assert_eq!(names, MUTATING_TOOLS);
    assert!(specs().iter().all(|s| s.needs_approval));
    let recorded: Vec<Value> = serde_json::from_str(SPECS_JSON).unwrap();
    assert_eq!(recorded.len(), 7);
    for r in &recorded {
        assert_eq!(r["requires_confirmation"], true, "{}", r["name"]);
    }
}

/// The descriptions are the web's docstrings as agno hands them over: the
/// first sentence of each, and the warnings the model must read.
#[test]
fn the_descriptions_are_the_webs() {
    for (name, first) in [
        ("place_order", "Place a real order on the operator's live broker account."),
        ("place_smart_order", "Place a real order that moves a position to a target size."),
        ("modify_order", "Modify a real pending order on the broker."),
        ("cancel_order", "Cancel one real pending order on the broker."),
        ("cancel_all_orders", "Cancel every open order on the account."),
        ("close_position", "Close one real open position by squaring it off at market."),
        ("close_all_positions", "Square off every open position on the account."),
    ] {
        assert!(spec(name).description.starts_with(first), "{}: {}", name, spec(name).description);
    }
    let place = &spec("place_order").description;
    assert!(place.contains("THIS PLACES A REAL ORDER WITH REAL MONEY"));
    assert!(place.contains("If the platform is in analyzer mode the order goes to the\nsandbox instead, and the result says so."));
    assert!(spec("cancel_all_orders").description.contains("THIS CANCELS EVERY PENDING ORDER ON THE WHOLE ACCOUNT"));
    assert!(spec("close_all_positions").description.contains("THIS CLOSES EVERY POSITION ON THE WHOLE ACCOUNT WITH REAL MONEY"));
}

/// Q-529, Q-530, Q-532, Q-533 and the argument shapes: product has no
/// default, prices and a smart order's quantity may be null, an order id
/// may be a number, and no tool takes a credential (X-050).
#[test]
fn the_argument_schemas_are_the_webs() {
    let required = |n: &str| spec(n).required();
    assert_eq!(required("place_order"), ["symbol", "exchange", "action", "quantity", "product"]);
    assert_eq!(required("place_smart_order"), ["symbol", "exchange", "action", "quantity", "position_size", "product"]);
    assert_eq!(required("modify_order"), ["order_id", "symbol", "exchange", "action", "quantity", "product", "price_type"]);
    assert_eq!(required("cancel_order"), ["order_id"]);
    assert_eq!(required("close_position"), ["symbol", "exchange", "product"]);
    assert!(required("cancel_all_orders").is_empty() && required("close_all_positions").is_empty());
    let p = &spec("place_order").parameters["properties"];
    assert_eq!(p["price"]["anyOf"], json!([{"type": "number"}, {"type": "null"}]));
    assert_eq!(spec("modify_order").parameters["properties"]["order_id"]["anyOf"], json!([{"type": "string"}, {"type": "integer"}]));
    assert_eq!(spec("place_smart_order").parameters["properties"]["quantity"]["anyOf"], json!([{"type": "integer"}, {"type": "null"}]));
    for s in specs() {
        let text = s.parameters.to_string().to_lowercase();
        for word in ["apikey", "api_key", "token", "password", "strategy"] {
            assert!(!s.parameters["properties"].as_object().unwrap().contains_key(word), "{} takes {}", s.name, word);
        }
        assert!(!text.contains("\"apikey\""), "{}", s.name);
    }
    let null_prices = validate(&spec("place_order").parameters, &args(json!({"symbol": "SBIN", "exchange": "NSE",
        "action": "BUY", "quantity": 1, "product": "MIS", "price": null, "trigger_price": null}))).unwrap();
    assert_eq!(null_prices["price"], Value::Null);
    assert!(validate(&spec("place_smart_order").parameters, &args(json!({"symbol": "SBIN", "exchange": "NSE",
        "action": "BUY", "quantity": null, "position_size": 10, "product": "MIS"}))).is_ok());
    for name in ["cancel_order", "modify_order"] {
        let mut a = json!({"order_id": 240307000616990_i64, "symbol": "SBIN", "exchange": "NSE", "action": "BUY",
            "quantity": 1, "product": "MIS", "price_type": "LIMIT", "price": 100.0});
        if name == "cancel_order" {
            a = json!({"order_id": 240307000616990_i64});
        }
        assert!(validate(&spec(name).parameters, &args(a)).is_ok(), "{}", name);
    }
}

/// Q-707, Q-708, Q-709: the approval card's missing list, from the tool's
/// own schema; the two tools with no arguments are never incomplete.
#[test]
fn the_missing_list_disables_approval_only_when_something_is_missing() {
    let missing = missing_arguments(&spec("place_order").parameters, &Map::new());
    for name in ["symbol", "exchange", "action", "quantity", "product"] {
        assert!(missing.contains(&name.to_string()), "{:?}", missing);
    }
    assert_eq!(
        missing_arguments(&spec("place_order").parameters, &args(json!({"symbol": "", "exchange": null, "action": "BUY",
            "quantity": 1, "product": "MIS"}))),
        ["exchange", "symbol"]
    );
    assert!(missing_arguments(&spec("cancel_all_orders").parameters, &Map::new()).is_empty());
    assert!(missing_arguments(&spec("close_all_positions").parameters, &Map::new()).is_empty());
}

/// T-149, Q-342, Q-343, Q-344, Q-108: the toolkit is on every surface,
/// needs the trading capability, and is offered with all seven tools
/// paused for approval, or not at all.
#[tokio::test]
async fn orders_are_offered_only_with_trading_and_always_gated() {
    assert_eq!(GROUP.key, "orders");
    assert_eq!(GROUP.order, 50);
    assert_eq!(GROUP.requires, [Capability::Trading]);
    assert_eq!(GROUP.description, "Place, modify, cancel and close real orders and positions. Every tool requires human approval and runs the risk guard.");
    assert!(TOOLKITS.iter().any(|g| g.key == "orders"));
    let t = crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
    let on = Capabilities { trading_enabled: true, ..Default::default() };
    for surface in [Surface::Chat, Surface::Chart, Surface::Voice] {
        let off = build_tools(&[GROUP], &ctx(t.ctx.clone(), surface, Capabilities::default()));
        assert!(off.is_empty(), "{:?} offered orders without trading", surface);
        let tools = build_tools(&[GROUP], &ctx(t.ctx.clone(), surface, on));
        let names: Vec<&str> = tools.iter().map(|t| t.spec().name).collect();
        assert_eq!(names, MUTATING_TOOLS, "{:?}", surface);
        assert!(tools.iter().all(|t| t.spec().needs_approval), "{:?}", surface);
    }
}
