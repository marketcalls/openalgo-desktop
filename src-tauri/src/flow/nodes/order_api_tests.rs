//! What the order nodes rely on from the desktop's `/api/v1`, reached in
//! process as a run reaches it (parity X-078, X-091, X-137..X-139, D-30,
//! D-31): the web's tests of its own services, run against the desktop's.

use crate::agent::tools::testkit::{master_rows, row, Kit};
use crate::brokers::mock::MockCall;
use crate::brokers::types::{Quote, QuoteKey};
use crate::flow::contract::client::app::{AppFlowClient, CALL_TIMEOUT, FLOW_CALLER};
use crate::flow::contract::client::{FlowClient, PlaceOrder};
use crate::flow::nodes::order_testing::WORKFLOW;
use crate::mcp::dispatch::{post_request, run_as};
use crate::security::Secret;
use crate::services::apikey_service::ApiKeyService;
use serde_json::{json, Value};
use std::sync::Arc;

/// POST `endpoint` with the account's key as Flow does; (status, body).
async fn post(kit: &Kit, endpoint: &str, payload: Value) -> (u16, Value) {
    let Ok(req) = post_request(
        &kit.key,
        endpoint,
        payload.as_object().cloned().unwrap_or_default(),
    ) else {
        return (0, Value::Null);
    };
    match run_as(kit.app(), FLOW_CALLER, req, CALL_TIMEOUT).await {
        Ok(raw) => (
            raw.status,
            serde_json::from_str(&raw.text).unwrap_or(Value::Null),
        ),
        Err(t) => (0, json!(format!("{:?}", t))),
    }
}

fn orders_placed(kit: &Kit) -> usize {
    kit.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::PlaceOrder(_)))
        .count()
}

fn quoted(kit: &Kit) -> Vec<(String, String)> {
    kit.mock
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::Quote(QuoteKey { exchange, symbol }) => Some((symbol, exchange)),
            _ => None,
        })
        .collect()
}

fn quote(symbol: &str, exchange: &str, ltp: f64) -> Quote {
    Quote {
        symbol: symbol.into(),
        exchange: exchange.into(),
        ltp,
        ..Default::default()
    }
}

/// X-091: the smart-order endpoint Flow reaches refuses a bad price type
/// or product with 400 before the broker.
#[tokio::test]
async fn the_smart_order_endpoint_refuses_bad_constants_before_the_broker() {
    let kit = Kit::new();
    for (key, value, word) in [
        ("pricetype", "NOT-A-TYPE", "price"),
        ("product", "NOT-A-PRODUCT", "product"),
    ] {
        let mut body = json!({"strategy": "s", "symbol": "SBIN", "exchange": "NSE",
            "action": "BUY", "quantity": 1, "position_size": 1});
        if let Some(m) = body.as_object_mut() {
            m.insert(key.into(), json!(value));
        }
        let (status, reply) = post(&kit, "placesmartorder", body).await;
        assert_eq!(status, 400, "{reply}");
        assert_eq!(reply["status"], "error");
        assert!(
            reply["message"].to_string().to_lowercase().contains(word),
            "{reply}"
        );
    }
    assert_eq!(orders_placed(&kit), 0);
}

/// The MCX contracts of the commodity cases: a future and a strike ladder.
fn mcx_rows(with_future: bool) -> Vec<crate::brokers::common::symbols::SymToken> {
    let mut rows = master_rows();
    if with_future {
        rows.push(row(
            "CRUDEOIL19OCT26FUT",
            "MCX",
            "FUT",
            "19-OCT-26",
            0.0,
            100,
        ));
    }
    for strike in (5800..=6200).step_by(100) {
        rows.push(row(
            &format!("CRUDEOIL17NOV26{}CE", strike),
            "MCX",
            "CE",
            "17-NOV-26",
            f64::from(strike),
            100,
        ));
    }
    rows
}

/// X-137, X-139: a bare commodity is priced against its near-month
/// future; with no live future it is refused, not guessed, and nothing is
/// quoted.
#[tokio::test]
async fn a_commodity_is_priced_against_its_near_month_future() {
    let kit = Kit::new();
    kit.app().symbols.load(mcx_rows(true));
    kit.mock
        .set_quote(quote("CRUDEOIL19OCT26FUT", "MCX", 6010.0));
    let body = json!({"underlying": "CRUDEOIL", "exchange": "MCX", "expiry_date": "17NOV26",
        "offset": "ATM", "option_type": "CE"});
    let (status, reply) = post(&kit, "optionsymbol", body.clone()).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(
        (&reply["symbol"], &reply["exchange"]),
        (&json!("CRUDEOIL17NOV266000CE"), &json!("MCX"))
    );
    assert_eq!(
        quoted(&kit),
        [("CRUDEOIL19OCT26FUT".to_string(), "MCX".to_string())]
    );
    let kit = Kit::new();
    kit.app().symbols.load(mcx_rows(false));
    let (status, reply) = post(&kit, "optionsymbol", body).await;
    assert_eq!(status, 404, "{reply}");
    assert!(reply["message"].to_string().contains("futures"), "{reply}");
    assert!(quoted(&kit).is_empty());
}

/// X-138: an index underlying is still quoted directly.
#[tokio::test]
async fn an_index_is_quoted_directly() {
    let kit = Kit::new();
    kit.mock.set_quote(quote("NIFTY", "NSE_INDEX", 24510.0));
    let body = json!({"underlying": "NIFTY", "exchange": "NSE_INDEX", "expiry_date": "27OCT26",
        "offset": "ATM", "option_type": "CE"});
    let (status, reply) = post(&kit, "optionsymbol", body).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["symbol"], "NIFTY27OCT2624500CE");
    assert_eq!(
        quoted(&kit),
        [("NIFTY".to_string(), "NSE_INDEX".to_string())]
    );
}

/// X-078: the desktop's public leg schema gives a product-less leg MIS, as
/// the web's `OptionsMultiOrderLegSchema` does; the web service's NRML
/// default only serves in-process callers, and every leg a Flow node
/// sends names its product (custom legs inherit the node's, NRML by
/// default; generated legs carry it), so Flow never relies on either.
#[test]
fn a_product_less_leg_is_mis_at_the_public_schema() {
    let body = json!({"apikey": "k", "strategy": "s", "underlying": "NIFTY", "exchange": "NSE_INDEX",
        "legs": [{"offset": "ATM", "option_type": "CE", "action": "BUY", "quantity": 75}]});
    let loaded = crate::services::schemas::options_multi_order()
        .load(body.as_object().unwrap_or(&serde_json::Map::new()));
    assert_eq!(
        loaded.ok().map(|m| m["legs"][0]["product"].clone()),
        Some(json!("MIS"))
    );
}

/// D-30: in Semi-Auto a Flow order goes to the Action Center; the node's
/// result is the queue reply and nothing reaches the broker. D-31: the
/// order carries the workflow's tag.
#[tokio::test]
async fn a_flow_order_in_semi_auto_is_queued() {
    let kit = Kit::new();
    ApiKeyService::set_order_mode(kit.app(), "semi_auto").unwrap_or_default();
    let client = AppFlowClient::new(Arc::downgrade(kit.app()), Secret::new(kit.key.clone()));
    let mut order = PlaceOrder::new("SBIN", "NSE", "BUY", 1);
    order.strategy = WORKFLOW.into();
    let out = client.place_order(&order).await;
    assert_eq!(
        (&out["status"], &out["mode"]),
        (&json!("success"), &json!("semi_auto")),
        "{out}"
    );
    assert!(out["pending_order_id"].is_number(), "{out}");
    assert_eq!(orders_placed(&kit), 0);
    let pending = kit
        .app()
        .sqlite
        .conn()
        .ok()
        .and_then(|c| {
            c.query_row("SELECT order_data FROM pending_orders", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
        .unwrap_or_default();
    assert!(pending.contains(WORKFLOW), "{pending}");
}
