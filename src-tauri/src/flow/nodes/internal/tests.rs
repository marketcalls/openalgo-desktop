//! Flow's internal order entries end to end: the run's client
//! (`AppFlowClient`) against a signed-in app with a mock broker, through
//! the real key check, analyzer routing, Semi-Auto and services
//! (WEB-DEFECT CL-09, parity N-optionsMultiOrder-12, D-30).

use super::*;
use crate::agent::tools::testkit::Kit;
use crate::brokers::mock::MockCall;
use crate::brokers::types::Position;
use crate::flow::contract::client::app::AppFlowClient;
use crate::flow::contract::client::{FlowClient, OptionsMultiOrder, PlaceOrder};
use crate::services::apikey_service::ApiKeyService;

fn client(kit: &Kit) -> AppFlowClient {
    AppFlowClient::new(Arc::downgrade(kit.app()), Secret::new(kit.key.clone()))
}

fn position(symbol: &str, quantity: i32) -> Position {
    Position {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        product: "MIS".into(),
        quantity,
        overnight_quantity: 0,
        average_price: 100.0,
        ltp: 101.0,
        pnl: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        buy_quantity: quantity.max(0),
        buy_value: 0.0,
        sell_quantity: 0,
        sell_value: 0.0,
    }
}

/// The orders the mock broker received: (symbol, action, quantity).
fn placed(kit: &Kit) -> Vec<(String, String, i64)> {
    kit.mock
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            MockCall::PlaceOrder(o) => Some((o.symbol, format!("{:?}", o.action), o.quantity)),
            _ => None,
        })
        .collect()
}

/// WEB-DEFECT CL-09: a Close Positions node naming SBIN closes SBIN alone;
/// a second open position survives (the web's live close squared off every
/// position). Fails before the fix: the scoped call reached
/// `/api/v1/closeposition`.
#[tokio::test]
async fn a_scoped_close_closes_only_that_position() {
    let kit = Kit::new();
    *kit.mock.positions.lock() = Some(Ok(vec![position("SBIN", 10), position("INFY", 5)]));
    let out = client(&kit)
        .close_position("SBIN", "NSE", "MIS", "Golden Flow")
        .await;
    assert_eq!(out["status"], "success", "{}", out);
    let calls = kit.mock.calls();
    assert!(
        !calls.iter().any(|c| matches!(c, MockCall::CloseAll)),
        "{:?}",
        calls
    );
    let sent = placed(&kit);
    assert_eq!(sent.len(), 1, "{:?}", sent);
    assert_eq!(sent[0].0, "SBIN");
    assert_eq!(sent[0].2, 10);
    assert!(sent[0].1.to_uppercase().contains("SELL"), "{:?}", sent);
    // Nothing held: nothing is sent, and the trader is told so.
    *kit.mock.positions.lock() = Some(Ok(vec![position("INFY", 5)]));
    let out = client(&kit)
        .close_position("SBIN", "NSE", "MIS", "Golden Flow")
        .await;
    assert_eq!(placed(&kit).len(), 1, "{}", out);
}

/// An unscoped close still squares off everything through
/// `/api/v1/closeposition`.
#[tokio::test]
async fn an_unscoped_close_still_closes_everything() {
    let kit = Kit::new();
    *kit.mock.positions.lock() = Some(Ok(vec![position("SBIN", 10)]));
    client(&kit).close_all_positions().await;
    assert!(kit
        .mock
        .calls()
        .iter()
        .any(|c| matches!(c, MockCall::CloseAll)));
}

/// Semi-Auto refuses a scoped close from Flow as it refuses
/// `closeposition` (the web's sentence), and nothing reaches the broker.
#[tokio::test]
async fn semi_auto_refuses_a_scoped_close() {
    let kit = Kit::new();
    *kit.mock.positions.lock() = Some(Ok(vec![position("SBIN", 10)]));
    ApiKeyService::set_order_mode(kit.app(), "semi_auto").unwrap_or_default();
    let out = client(&kit)
        .close_position("SBIN", "NSE", "MIS", "Golden Flow")
        .await;
    assert_eq!(
        out,
        json!({"status": "error", "error": order_service::CLOSE_IN_SEMI_AUTO, "code": 403})
    );
    assert!(placed(&kit).is_empty());
}

/// A scoped close is checked as the public smart order is: a bad exchange
/// is refused before anything is sent, and an unknown key is refused.
#[tokio::test]
async fn a_scoped_close_is_checked_like_an_api_call() {
    let kit = Kit::new();
    let out = client(&kit).close_position("SBIN", "LSE", "MIS", "s").await;
    assert_eq!(out["status"], "error");
    assert_eq!(out["code"], 400, "{}", out);
    let stranger = AppFlowClient::new(Arc::downgrade(kit.app()), Secret::new("not-a-key"));
    let out = stranger.close_position("SBIN", "NSE", "MIS", "s").await;
    assert_eq!(out["code"], 403, "{}", out);
    assert!(placed(&kit).is_empty());
}

fn strike_order(strike: f64) -> OptionsMultiOrder {
    OptionsMultiOrder {
        underlying: "NIFTY".into(),
        exchange: "NSE_INDEX".into(),
        expiry_date: "27OCT26".into(),
        legs: vec![
            json!({"strike": strike, "option_type": "CE", "action": "BUY",
            "pricetype": "MARKET", "product": "NRML", "price": 0.0, "trigger_price": 0.0,
            "splitsize": 0, "quantity": 75}),
        ],
        strategy: "Golden Flow".into(),
        strike_int: None,
    }
}

/// parity N-optionsMultiOrder-12: a leg naming a strike is placed on that
/// listed contract, live, through Flow's internal entry. Fails before:
/// the public leg schema refused the leg (no `offset`).
#[tokio::test]
async fn a_strike_leg_is_placed_on_its_listed_contract() {
    let kit = Kit::new();
    let out = client(&kit)
        .options_multi_order(&strike_order(24500.0))
        .await;
    assert_eq!(out["status"], "success", "{}", out);
    let leg = &out["results"][0];
    assert_eq!(leg["symbol"], "NIFTY27OCT2624500CE", "{}", out);
    assert_eq!(
        (&leg["strike"], &leg["offset"]),
        (&json!(24500.0), &Value::Null)
    );
    assert_eq!(leg["status"], "success", "{}", out);
    let sent = placed(&kit);
    assert_eq!(
        sent,
        [("NIFTY27OCT2624500CE".to_string(), sent[0].1.clone(), 75)]
    );
}

/// An unlisted strike is refused by name, before anything is sent.
#[tokio::test]
async fn an_unlisted_strike_is_refused_by_name() {
    let kit = Kit::new();
    let out = client(&kit)
        .options_multi_order(&strike_order(24550.0))
        .await;
    assert_eq!(
        out["results"][0]["message"],
        "Option symbol NIFTY27OCT2624550CE not found in NFO. Check that strike 24550.0 is listed for NIFTY 27OCT26, or re-download the master contract."
    );
    assert!(placed(&kit).is_empty());
}

/// The public `/api/v1/optionsmultiorder` is not widened: a strike leg
/// sent by an API client is refused by the leg schema, as on the web.
#[tokio::test]
async fn the_public_api_still_refuses_strike_legs() {
    let kit = Kit::new();
    let order = strike_order(24500.0);
    let payload = json!({"strategy": order.strategy, "underlying": order.underlying,
        "exchange": order.exchange, "expiry_date": order.expiry_date, "legs": order.legs});
    let req = crate::mcp::dispatch::post_request(
        &kit.key,
        "optionsmultiorder",
        payload.as_object().cloned().unwrap_or_default(),
    );
    let Ok(req) = req else {
        panic!("request not built")
    };
    let raw = crate::mcp::dispatch::run_as(kit.app(), FLOW_CALLER, req, CALL_TIMEOUT).await;
    assert_eq!(raw.map(|r| r.status).ok(), Some(400));
    assert!(placed(&kit).is_empty());
}

/// D-30: in analyzer mode a strike leg goes to the sandbox, never to the
/// broker.
#[tokio::test]
async fn a_strike_leg_in_analyzer_mode_never_reaches_the_broker() {
    let kit = Kit::new();
    kit.analyzer(true);
    let out = client(&kit)
        .options_multi_order(&strike_order(24500.0))
        .await;
    assert_eq!(out["mode"], "analyze", "{}", out);
    assert!(placed(&kit).is_empty());
    let out = client(&kit)
        .place_order(&PlaceOrder::new("SBIN", "NSE", "BUY", 1))
        .await;
    assert!(placed(&kit).is_empty(), "{}", out);
}

fn ltp(symbol: &str, price: f64) -> crate::brokers::types::Quote {
    crate::brokers::types::Quote {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        ltp: price,
        bid: price,
        ask: price,
        ..Default::default()
    }
}

/// The open quantity of `symbol` in the sandbox's position book.
async fn sandbox_quantity(c: &AppFlowClient, symbol: &str) -> Value {
    c.get_open_position(symbol, "NSE", Some("MIS")).await["quantity"].clone()
}

/// WEB-DEFECT CL-09 in analyzer mode: a scoped close in the sandbox closes
/// that position alone; the other sandbox position stays open.
#[tokio::test]
async fn a_scoped_close_in_analyzer_mode_closes_only_that_position() {
    let kit = Kit::new();
    kit.analyzer(true);
    kit.mock.set_quote(ltp("SBIN", 800.0));
    kit.mock.set_quote(ltp("INFY", 1500.0));
    let c = client(&kit);
    for (symbol, qty) in [("SBIN", 10), ("INFY", 5)] {
        let out = c
            .place_order(&PlaceOrder::new(symbol, "NSE", "BUY", qty))
            .await;
        assert_eq!(out["status"], "success", "{}", out);
    }
    assert_eq!(sandbox_quantity(&c, "SBIN").await, json!(10));
    let out = c.close_position("SBIN", "NSE", "MIS", "Golden Flow").await;
    assert_eq!(out["status"], "success", "{}", out);
    assert_eq!(sandbox_quantity(&c, "SBIN").await, json!(0));
    assert_eq!(sandbox_quantity(&c, "INFY").await, json!(5));
    assert!(
        placed(&kit).is_empty(),
        "the sandbox never reaches the broker"
    );
}

/// parity N-optionsMultiOrder-12 (web `resolve_leg_symbol_by_strike`): a
/// strike that names no contract (zero, negative, text, a boolean) is
/// refused with the web's sentence, its value printed as Python's repr,
/// and nothing is sent.
#[tokio::test]
async fn a_strike_that_names_no_contract_is_refused_in_the_web_words() {
    let kit = Kit::new();
    for (strike, shown) in [
        (json!(0.0), "0.0"),
        (json!(-24500), "-24500"),
        (json!("abc"), "'abc'"),
        (json!(true), "True"),
    ] {
        let mut order = strike_order(24500.0);
        if let Some(leg) = order.legs[0].as_object_mut() {
            leg.insert("strike".into(), strike.clone());
        }
        let out = client(&kit).options_multi_order(&order).await;
        assert_eq!(
            out["results"][0]["message"],
            format!("Invalid strike: {shown}. Strike must be a positive number."),
            "{strike}: {out}"
        );
        assert_eq!(out["results"][0]["strike"], strike);
    }
    assert!(placed(&kit).is_empty());
}
