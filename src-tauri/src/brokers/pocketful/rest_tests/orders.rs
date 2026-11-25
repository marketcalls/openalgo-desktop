//! Orders, books, positions and holdings (web `test/test_pocketful_orders.py`),
//! against the examples of Pocketful's API documentation
//! (`tests/fixtures/brokers/pocketful/`).

use super::{success, Fake, Reply, CLIENT};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::outcome::{classify, placing, PlaceOutcome};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::pocketful::pricing::{market_limit, stop_market, Ask};
use crate::brokers::types::*;
use crate::brokers::Broker;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../../tests/fixtures/brokers/pocketful/",
            $name
        ))
    };
}

const OPTION: &str = "NIFTY25NOV2524000CE";
const ORDERS: &str = "/api/v1/orders";
const POSITIONS: &str = "/api/v1/positions";
const NSE_QUOTES: &str = "/api/v1/marketdata/NSE/Capital";

/// OpenAlgo symbol, exchange, broker symbol, token, tick size (web `MASTER`).
const MASTER: &[(&str, &str, &str, &str, f64)] = &[
    ("SBIN", "NSE", "SBIN-EQ", "3045", 0.05),
    ("YESBANK", "NSE", "YESBANK-EQ", "11915", 0.01),
    ("IDEA", "NSE", "IDEA-EQ", "14366", 0.01),
    ("PENNY", "NSE", "PENNY-EQ", "777", 0.01),
    (OPTION, "NFO", "NIFTY25NOV24000CE", "45000", 0.05),
    (
        "COFORGE24MAR5700PE",
        "NFO",
        "COFORGE24MAR5700PE",
        "100803",
        0.05,
    ),
    ("PNB", "BSE", "PNB-A", "532461", 0.05),
    (
        "GOLDGUINEA20NOVFUT",
        "MCX",
        "GOLDGUINEA20NOVFUT",
        "222895",
        1.0,
    ),
    ("UNTICKED", "NSE", "UNTICKED-EQ", "999", 0.0),
];

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(
        MASTER
            .iter()
            .map(|(symbol, exchange, brsymbol, token, tick)| SymToken {
                symbol: symbol.to_string(),
                brsymbol: brsymbol.to_string(),
                name: symbol.to_string(),
                exchange: exchange.to_string(),
                brexchange: exchange.to_string(),
                token: token.to_string(),
                expiry: String::new(),
                strike: 0.0,
                lot_size: 1,
                instrument_type: String::new(),
                tick_size: *tick,
            })
            .collect(),
    );
    r
}

fn session() -> AuthToken {
    AuthToken::new("jwt.access.token").with_user_id(CLIENT)
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn rows(fixture: &str, key: Option<&str>) -> Vec<Value> {
    let v = json(fixture);
    let list = match key {
        Some(k) => &v["data"][k],
        None => &v["data"],
    };
    list.as_array().unwrap().clone()
}

fn pending_order() -> Value {
    rows(fixture!("orders_pending.json"), Some("orders")).remove(0)
}

fn completed_order() -> Value {
    rows(fixture!("orders_completed.json"), Some("orders")).remove(0)
}

fn with(mut row: Value, changes: Value) -> Value {
    for (k, v) in changes.as_object().unwrap() {
        row[k] = v.clone();
    }
    row
}

fn orders_of(list: Vec<Value>) -> Reply {
    success(json!({ "orders": list }))
}

fn request(symbol: &str, exchange: &str, side: &str, pricetype: &str) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 1,
        price: 0.0,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    }
}

fn order(symbol: &str, exchange: &str, side: &str, pricetype: &str, trigger: f64) -> ResolvedOrder {
    let req = OrderRequest {
        trigger_price: Some(trigger),
        ..request(symbol, exchange, side, pricetype)
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

const PLACE_OK: &str = r#"{"data": {"oms_order_id": "200018000000003"}, "message": "Order place successfully", "status": "success"}"#;
const FORBIDDEN: &str =
    r#"{"status": "error", "message": "Request forbidden", "error_code": 40000, "data": {}}"#;
const UNAUTHORISED: &str =
    r#"{"status": "error", "message": "Request Unauthorised", "error_code": 40000, "data": {}}"#;

// ---------------------------------------------------------------------------
// MARKET -> protected LIMIT off the LTP; SL-M -> SL beyond its own trigger
// ---------------------------------------------------------------------------

/// Web ::test_a_market_order_goes_out_as_a_protected_limit
#[test]
fn a_market_order_goes_out_as_a_protected_limit() {
    for (symbol, exchange, ltp, side, limit) in [
        // Equity above 500: 0.5%. 800.5 x 1.005 = 804.5025, up to 804.55.
        ("SBIN", "NSE", 800.5, "BUY", 804.55),
        ("SBIN", "NSE", 800.5, "SELL", 796.45),
        // Options below 10: 5%, wider than equity.
        (OPTION, "NFO", 5.0, "BUY", 5.25),
        (OPTION, "NFO", 5.0, "SELL", 4.75),
        // 5% of 0.05 is less than a tick: at least one tick past the LTP.
        (OPTION, "NFO", 0.05, "BUY", 0.1),
        // A SELL cannot go below the smallest price there is.
        (OPTION, "NFO", 0.05, "SELL", 0.05),
    ] {
        let o = order(symbol, exchange, side, "MARKET", 0.0);
        let p = market_limit(&Ask::order(&o), Some(ltp)).unwrap();
        assert_eq!(
            (p.order_type, p.price, p.trigger_price),
            ("LIMIT", limit, 0.0),
            "{} {} {}",
            symbol,
            side,
            ltp
        );
    }
}

/// Web ::test_an_sl_m_order_goes_out_as_an_sl_beyond_its_trigger
#[test]
fn an_sl_m_order_goes_out_as_an_sl_beyond_its_trigger() {
    for (symbol, exchange, trigger, side, limit) in [
        ("SBIN", "NSE", 800.0, "BUY", 804.0),
        ("SBIN", "NSE", 800.0, "SELL", 796.0),
        // Equity below 100: 2%.
        ("PENNY", "NSE", 50.0, "BUY", 51.0),
        ("PENNY", "NSE", 50.0, "SELL", 49.0),
        (OPTION, "NFO", 5.0, "SELL", 4.75),
        // 5% of 0.10 is less than a tick: one tick past the trigger.
        (OPTION, "NFO", 0.10, "BUY", 0.15),
        (OPTION, "NFO", 0.10, "SELL", 0.05),
        // A trigger off the tick grid still gets a full tick of room.
        (OPTION, "NFO", 0.06, "BUY", 0.15),
        (OPTION, "NFO", 0.14, "SELL", 0.05),
    ] {
        let o = order(symbol, exchange, side, "SL-M", trigger);
        let p = stop_market(&Ask::order(&o)).unwrap();
        assert_eq!(
            (p.order_type, p.price, p.trigger_price),
            ("SL", limit, trigger),
            "{} {} {}",
            symbol,
            side,
            trigger
        );
    }
}

/// Web ::test_an_sl_m_sell_too_low_to_protect_is_refused, ::test_an_sl_m_without_a_tick_size_is_refused,
/// ::test_an_sl_m_without_a_trigger_is_refused, ::test_a_market_order_without_an_ltp_is_refused_not_sent_as_market,
/// ::test_a_market_order_without_a_tick_size_is_refused_without_a_quote
#[test]
fn an_order_that_cannot_be_priced_is_refused() {
    let cases = [
        (
            stop_market(&Ask::order(&order(OPTION, "NFO", "SELL", "SL-M", 0.05))),
            "too low",
        ),
        (
            stop_market(&Ask::order(&order("UNTICKED", "NSE", "BUY", "SL-M", 100.0))),
            "tick size",
        ),
        (
            stop_market(&Ask::order(&order("SBIN", "NSE", "BUY", "SL-M", 0.0))),
            "trigger price",
        ),
        (
            market_limit(
                &Ask::order(&order("SBIN", "NSE", "BUY", "MARKET", 0.0)),
                None,
            ),
            "live price",
        ),
        (
            market_limit(
                &Ask::order(&order("UNTICKED", "NSE", "BUY", "MARKET", 0.0)),
                Some(100.0),
            ),
            "tick size",
        ),
    ];
    for (result, words) in cases {
        let e = result.unwrap_err();
        assert!(e.client_message().contains(words), "{}", e.client_message());
    }
}

// ---------------------------------------------------------------------------
// Place, modify, cancel
// ---------------------------------------------------------------------------

/// Web ::test_place_sends_the_documented_body_with_the_tokens_client_id
#[tokio::test]
async fn place_sends_the_documented_body() {
    let fake = Fake::start().await;
    fake.on(NSE_QUOTES, vec![success(json!(13.0))]);
    fake.on(ORDERS, vec![Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());
    let o = ResolvedOrder::resolve(
        &OrderRequest {
            product: "CNC".into(),
            ..request("IDEA", "NSE", "BUY", "MARKET")
        },
        &master(),
    )
    .unwrap();

    let r = b.place_order(&session(), &o).await.unwrap();

    assert_eq!(r.order_id, "200018000000003");
    let ltp = fake.to(NSE_QUOTES);
    assert_eq!(ltp.len(), 1);
    assert_eq!(ltp[0].query, "token=14366&key=last_trade_price");
    let sent = fake.to(ORDERS);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].header("authorization"), "Bearer jwt.access.token");
    // 13.0 x 1.02 = 13.26 on a 0.01 tick; place-regular-order.md, field for field.
    assert_eq!(
        sent[0].json(),
        json!({
            "exchange": "NSE",
            "instrument_token": "14366",
            "client_id": CLIENT,
            "order_type": "LIMIT",
            "amo": false,
            "price": 13.26,
            "quantity": 1,
            "disclosed_quantity": 0,
            "validity": "DAY",
            "product": "CNC",
            "order_side": "BUY",
            "device": "WEB",
            "user_order_id": 1,
            "trigger_price": 0.0,
            "execution_type": "REGULAR",
        })
    );
}

/// Web ::test_limit_and_sl_orders_keep_their_own_prices
#[tokio::test]
async fn limit_and_sl_orders_keep_their_own_prices() {
    let fake = Fake::start().await;
    fake.on(ORDERS, vec![Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());
    let limit = ResolvedOrder::resolve(
        &OrderRequest {
            price: 801.5,
            ..request("SBIN", "NSE", "BUY", "LIMIT")
        },
        &master(),
    )
    .unwrap();
    let sl = ResolvedOrder::resolve(
        &OrderRequest {
            price: 801.5,
            trigger_price: Some(800.0),
            ..request("SBIN", "NSE", "BUY", "SL")
        },
        &master(),
    )
    .unwrap();

    b.place_order(&session(), &limit).await.unwrap();
    b.place_order(&session(), &sl).await.unwrap();

    let sent = fake.to(ORDERS);
    let shape = |v: Value| {
        (
            v["order_type"].clone(),
            v["price"].clone(),
            v["trigger_price"].clone(),
        )
    };
    assert_eq!(
        shape(sent[0].json()),
        (json!("LIMIT"), json!(801.5), json!(0.0))
    );
    assert_eq!(
        shape(sent[1].json()),
        (json!("SL"), json!(801.5), json!(800.0))
    );
    assert!(fake.to(NSE_QUOTES).is_empty(), "no price was asked for");
}

/// Web ::test_an_order_without_a_client_id_is_refused_before_sending and
/// ::test_place_refuses_an_order_it_cannot_price_before_sending
#[tokio::test]
async fn an_order_that_cannot_go_is_refused_before_sending() {
    let fake = Fake::start().await;
    fake.on(ORDERS, vec![Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());
    let limit = order("SBIN", "NSE", "BUY", "LIMIT", 0.0);
    let e = b
        .place_order(&AuthToken::new("bare-token"), &limit)
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));

    let unpriced = order("UNTICKED", "NSE", "SELL", "SL-M", 100.0);
    let r = placing(b.place_order(&session(), &unpriced)).await;
    match classify(&r) {
        PlaceOutcome::Refused { reason } => assert!(reason.contains("tick size"), "{}", reason),
        other => panic!("{:?}", other),
    }
    assert!(fake.seen().is_empty(), "nothing reached Pocketful");
}

/// One placement against `reply`, classified as the order service does.
async fn placed(reply: Reply) -> (PlaceOutcome, Fake) {
    let fake = Fake::start().await;
    fake.on(ORDERS, vec![reply]);
    let b = fake.broker(master());
    let o = ResolvedOrder::resolve(
        &OrderRequest {
            price: 801.5,
            ..request("SBIN", "NSE", "BUY", "LIMIT")
        },
        &master(),
    )
    .unwrap();
    let outcome = classify(&placing(b.place_order(&session(), &o)).await);
    (outcome, fake)
}

/// Web ::test_an_error_answered_with_a_200_is_a_refusal,
/// ::test_a_session_refusal_tells_the_trader_to_log_in_again,
/// ::test_an_unreadable_answer_to_an_order_says_to_check_the_order_book and
/// ::test_a_transport_error_on_place_reaches_the_service. A lost or
/// unreadable answer is uncertain (LOG-08), never "not placed".
#[tokio::test]
async fn each_answer_to_an_order_has_its_outcome() {
    let place_error = json!({"data": {}, "error_code": 44000, "message": "`product` is invalid", "status": "error"});
    match placed(Reply::Json(200, place_error)).await.0 {
        PlaceOutcome::Refused { reason } => {
            assert_eq!(reason, "Pocketful refused the order: `product` is invalid")
        }
        other => panic!("{:?}", other),
    }
    for status in [401, 403] {
        match placed(Reply::Json(status, json(UNAUTHORISED))).await.0 {
            PlaceOutcome::Refused { reason } => {
                assert!(reason.contains("Log in to Pocketful again"), "{}", reason);
                assert!(!reason.contains(&status.to_string()), "{}", reason);
            }
            other => panic!("{:?}", other),
        }
    }
    match placed(Reply::Text(502, "<html>502 Bad Gateway</html>"))
        .await
        .0
    {
        PlaceOutcome::Uncertain { reason, .. } => {
            assert!(reason.contains("Check the order book"), "{}", reason)
        }
        other => panic!("{:?}", other),
    }
    let (outcome, fake) = placed(Reply::Cut).await;
    assert!(outcome.is_uncertain(), "{:?}", outcome);
    assert_eq!(fake.to(ORDERS).len(), 1, "an order is never sent twice");
    let no_id = json!({"data": {}, "message": "", "status": "success"});
    assert!(placed(Reply::Json(200, no_id)).await.0.is_uncertain());
}

fn modify(pricetype: &str, symbol: &str, trigger: f64) -> ResolvedModify {
    let m = ModifyOrderRequest {
        symbol: symbol.into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: pricetype.into(),
        quantity: 1,
        price: 801.0,
        trigger_price: trigger,
        disclosed_quantity: 0,
    };
    ResolvedModify::resolve("202403111778", &m, &master()).unwrap()
}

/// Web ::test_a_modify_to_market_is_priced_by_the_openalgo_symbol
#[tokio::test]
async fn a_modify_to_market_is_priced_like_a_new_order() {
    let fake = Fake::start().await;
    fake.on(NSE_QUOTES, vec![success(json!(800.5))]);
    fake.on(
        ORDERS,
        vec![success(json!({"oms_order_id": "202403111778"}))],
    );
    let b = fake.broker(master());

    let r = b
        .modify_order(&session(), &modify("MARKET", "SBIN", 0.0))
        .await
        .unwrap();

    assert_eq!(r.order_id, "202403111778");
    let sent = fake.to(ORDERS);
    assert_eq!(sent[0].method, "PUT");
    let body = sent[0].json();
    assert_eq!(body["oms_order_id"], "202403111778");
    assert_eq!(body["client_id"], CLIENT);
    assert_eq!(
        (body["order_type"].clone(), body["price"].clone()),
        (json!("LIMIT"), json!(804.55))
    );
    assert_eq!(body["execution_type"], "REGULAR");
    assert!(body.get("amo").is_none());
    assert_eq!(
        fake.to(NSE_QUOTES)[0].query,
        "token=3045&key=last_trade_price"
    );
}

/// Web ::test_a_modify_to_an_sl_m_it_cannot_price_is_refused and
/// ::test_a_modify_pocketful_refuses_reports_its_reason
#[tokio::test]
async fn a_modify_that_cannot_go_or_is_refused_says_why() {
    let fake = Fake::start().await;
    fake.on(
        ORDERS,
        vec![Reply::Json(
            200,
            json!({"data": {}, "error_code": 45010, "message": "Something went wrong", "status": "error"}),
        )],
    );
    let b = fake.broker(master());

    let e = b
        .modify_order(&session(), &modify("SL-M", "UNTICKED", 100.0))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("tick size"));
    assert!(fake.seen().is_empty());

    let e = b
        .modify_order(&session(), &modify("LIMIT", "SBIN", 0.0))
        .await
        .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Pocketful refused the change to the order: Something went wrong"
    );
}

/// Web ::test_cancel_sends_the_documented_body_and_the_sdks_query,
/// ::test_a_cancel_pocketful_refuses_is_an_error and
/// ::test_an_order_id_cannot_leave_the_orders_path
#[tokio::test]
async fn cancel_sends_the_documented_body_and_query() {
    let fake = Fake::start().await;
    fake.on(
        "/api/v1/orders/202403111778",
        vec![success(json!({"oms_order_id": "202403111778"}))],
    );
    fake.on(
        "/api/v1/orders/1",
        vec![Reply::Json(200, json(UNAUTHORISED))],
    );
    let b = fake.broker(master());

    let r = b.cancel_order(&session(), "202403111778").await.unwrap();
    assert_eq!(r.order_id, "202403111778");
    let sent = fake.to("/api/v1/orders/202403111778");
    assert_eq!(sent[0].method, "DELETE");
    assert_eq!(
        sent[0].json(),
        json!({"oms_order_id": "202403111778", "client_id": CLIENT, "execution_type": "REGULAR"})
    );
    assert_eq!(
        sent[0].query_pairs(),
        vec![
            ("client_id".to_string(), CLIENT.to_string()),
            ("execution_type".to_string(), "REGULAR".to_string()),
        ]
    );

    let e = b.cancel_order(&session(), "1").await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Pocketful refused the cancel: Request Unauthorised"
    );

    let _ = b.cancel_order(&session(), "../holdings?x=1").await;
    assert!(fake
        .seen()
        .iter()
        .any(|s| s.path == "/api/v1/orders/..%2Fholdings%3Fx%3D1"));
}

/// Web ::test_cancel_all_cancels_every_pending_order_with_its_execution_type
#[tokio::test]
async fn cancel_all_cancels_every_pending_order_with_its_execution_type() {
    let fake = Fake::start().await;
    let regular = with(
        pending_order(),
        json!({"oms_order_id": "R1", "execution_type": "REGULAR"}),
    );
    let cover = with(
        pending_order(),
        json!({"oms_order_id": "C1", "execution_type": "CO"}),
    );
    fake.on_query(
        ORDERS,
        "type=pending",
        vec![orders_of(vec![pending_order(), regular, cover])],
    );
    let cancelled = success(json!({"oms_order_id": "x"}));
    fake.on("/api/v1/orders/2024030943", vec![cancelled.clone()]);
    fake.on("/api/v1/orders/R1", vec![cancelled]);
    let b = fake.broker(master());

    let r = b.cancel_all_orders(&session()).await.unwrap();

    assert_eq!(r.cancelled, ["2024030943", "R1"]);
    assert_eq!(r.failed, ["C1"]);
    let read = fake.to(ORDERS);
    assert_eq!(read.len(), 1);
    let params = read[0].query_pairs();
    assert!(params.contains(&("type".into(), "pending".into())));
    assert!(params.contains(&("client_id".into(), CLIENT.into())));
    assert_eq!(
        fake.to("/api/v1/orders/2024030943")[0].json()["execution_type"],
        "AMO"
    );
    assert_eq!(
        fake.to("/api/v1/orders/R1")[0].json()["execution_type"],
        "REGULAR"
    );
}

/// Web ::test_cancel_all_fails_loudly_when_the_pending_list_cannot_be_read
#[tokio::test]
async fn cancel_all_fails_when_the_pending_list_cannot_be_read() {
    let fake = Fake::start().await;
    fake.on(ORDERS, vec![Reply::Json(200, json(FORBIDDEN))]);
    let b = fake.broker(master());

    let e = b.cancel_all_orders(&session()).await.unwrap_err();

    assert!(e
        .client_message()
        .contains("Pocketful refused the order book"));
    assert!(fake.seen().iter().all(|s| s.method == "GET"));
}

// ---------------------------------------------------------------------------
// The order book and the trade book
// ---------------------------------------------------------------------------

async fn book(pending: Reply, completed: Reply) -> (crate::error::Result<Vec<Order>>, Fake) {
    let fake = Fake::start().await;
    fake.on_query(ORDERS, "type=pending", vec![pending]);
    fake.on_query(ORDERS, "type=completed", vec![completed]);
    let b = fake.broker(master());
    let r = b.get_order_book(&session()).await;
    (r, fake)
}

/// Web ::test_the_order_book_is_the_pending_and_completed_lists
#[tokio::test]
async fn the_order_book_is_the_pending_and_completed_lists() {
    let (r, fake) = book(
        orders_of(vec![pending_order()]),
        orders_of(vec![completed_order()]),
    )
    .await;
    let ids: Vec<String> = r.unwrap().into_iter().map(|o| o.order_id).collect();
    assert_eq!(ids, ["2024030943", "2024030935"]);
    let kinds: Vec<Vec<(String, String)>> =
        fake.to(ORDERS).iter().map(|s| s.query_pairs()).collect();
    assert_eq!(kinds[0][0], ("type".to_string(), "pending".to_string()));
    assert_eq!(kinds[1][0], ("type".to_string(), "completed".to_string()));
    assert!(kinds
        .iter()
        .all(|q| q.contains(&("client_id".to_string(), CLIENT.to_string()))));
}

/// Web ::test_an_order_in_both_lists_is_shown_once_as_finished and
/// ::test_a_half_of_the_order_book_that_fails_fails_the_book
#[tokio::test]
async fn an_order_in_both_lists_is_shown_once_and_a_failed_half_fails_the_book() {
    let working = with(
        pending_order(),
        json!({"oms_order_id": "X1", "order_status": "PENDING"}),
    );
    let done = with(
        completed_order(),
        json!({"oms_order_id": "X1", "order_status": "COMPLETE"}),
    );
    let (r, _) = book(orders_of(vec![working]), orders_of(vec![done])).await;
    let rows: Vec<(String, String)> = r
        .unwrap()
        .into_iter()
        .map(|o| (o.order_id, o.status))
        .collect();
    assert_eq!(rows, [("X1".to_string(), "complete".to_string())]);

    for failing_pending in [true, false] {
        let forbidden = Reply::Json(200, json(FORBIDDEN));
        let fine = orders_of(vec![pending_order()]);
        let (pending, completed) = if failing_pending {
            (forbidden, fine)
        } else {
            (fine, forbidden)
        };
        let e = book(pending, completed).await.0.unwrap_err();
        assert_eq!(
            e.client_message(),
            "Pocketful refused the order book: Request forbidden"
        );
    }
}

/// Web ::test_the_order_book_in_openalgos_shape
#[tokio::test]
async fn the_order_book_in_openalgos_shape() {
    let trigger_wait = with(
        pending_order(),
        json!({"oms_order_id": "T1", "order_status": "TRIGGER_PENDING", "order_type": "SL", "trigger_price": 30.0}),
    );
    let filled = with(
        completed_order(),
        json!({"oms_order_id": "F1", "order_status": "COMPLETE", "order_type": "SLM", "order_side": "SELL",
               "filled_quantity": 1, "average_trade_price": 124.5}),
    );
    let rejected = with(
        completed_order(),
        json!({"oms_order_id": "J1", "order_status": "REJECTED"}),
    );
    let (r, _) = book(
        orders_of(vec![pending_order(), trigger_wait]),
        orders_of(vec![completed_order(), filled, rejected]),
    )
    .await;
    let orders = r.unwrap();
    let by_id = |id: &str| orders.iter().find(|o| o.order_id == id).unwrap().clone();

    let o = by_id("2024030943");
    assert_eq!(
        (o.symbol.as_str(), o.exchange.as_str(), o.side.as_str()),
        ("COFORGE24MAR5700PE", "NFO", "BUY")
    );
    assert_eq!((o.quantity, o.price, o.trigger_price), (1500, 30.7, 0.0));
    assert_eq!(
        (o.order_type.as_str(), o.product.as_str()),
        ("LIMIT", "NRML")
    );
    assert_eq!(o.status, "open");
    // 1709995984 is 2024-03-09 14:53:04 UTC.
    assert_eq!(o.order_timestamp, "09-03-2024 20:23:04");
    assert_eq!(
        (o.filled_quantity, o.pending_quantity, o.average_price),
        (0, 1500, 0.0)
    );
    assert_eq!(by_id("T1").status, "trigger pending");
    let c = by_id("2024030935");
    assert_eq!(
        (c.status.as_str(), c.symbol.as_str(), c.product.as_str()),
        ("cancelled", "PNB", "CNC")
    );
    let f = by_id("F1");
    assert_eq!(
        (
            f.status.as_str(),
            f.order_type.as_str(),
            f.side.as_str(),
            f.average_price
        ),
        ("complete", "SL-M", "SELL", 124.5)
    );
    assert_eq!(by_id("J1").status, "rejected");
    let count = |pred: &dyn Fn(&Order) -> bool| orders.iter().filter(|&o| pred(o)).count();
    assert_eq!(count(&|o| o.side == "BUY"), 4);
    assert_eq!(count(&|o| o.side == "SELL"), 1);
    assert_eq!(count(&|o| o.status == "complete"), 1);
    assert_eq!(
        count(&|o| o.status == "open" || o.status == "trigger pending"),
        2
    );
    assert_eq!(count(&|o| o.status == "rejected"), 1);
}

/// Web ::test_an_order_still_in_the_pending_list_is_working_whatever_its_word
/// and ::test_a_finished_order_with_a_word_the_docs_do_not_name
#[tokio::test]
async fn the_list_decides_what_the_word_cannot() {
    for word in ["CANCEL_REJECTED", "MODIFY_REJECTED", "PARTIALLY_COMPLETE"] {
        let (r, _) = book(
            orders_of(vec![with(pending_order(), json!({"order_status": word}))]),
            orders_of(vec![]),
        )
        .await;
        assert_eq!(r.unwrap()[0].status, "open", "{}", word);
    }
    let executed = with(
        completed_order(),
        json!({"order_status": "EXECUTED", "filled_quantity": 1}),
    );
    let lapsed = with(
        completed_order(),
        json!({"oms_order_id": "L1", "order_status": "EXPIRED"}),
    );
    let (r, _) = book(orders_of(vec![]), orders_of(vec![executed, lapsed])).await;
    let statuses: Vec<String> = r.unwrap().into_iter().map(|o| o.status).collect();
    assert_eq!(statuses, ["complete", "expired"]);
}

/// Web ::test_the_trade_book_reads_data_orders_and_comes_out_in_openalgos_shape
#[tokio::test]
async fn the_trade_book_reads_data_orders() {
    let trade = rows(fixture!("trades.json"), Some("orders")).remove(0);
    let fill = with(
        trade.clone(),
        json!({"oms_order_id": "202403152099", "order_side": "SELL", "filled_quantity": 5, "trade_price": 12.05}),
    );
    let fake = Fake::start().await;
    fake.on(
        "/api/v1/trades",
        vec![success(json!({"orders": [trade, fill]}))],
    );
    let b = fake.broker(master());

    let trades = b.get_trade_book(&session()).await.unwrap();

    assert_eq!(
        fake.to("/api/v1/trades")[0].query_pairs(),
        vec![("client_id".to_string(), CLIENT.to_string())]
    );
    let t = &trades[1];
    assert_eq!(
        (
            t.symbol.as_str(),
            t.exchange.as_str(),
            t.product.as_str(),
            t.side.as_str()
        ),
        ("IDEA", "NSE", "MIS", "SELL")
    );
    assert_eq!(
        (t.quantity, t.average_price, t.trade_value),
        (5, 12.05, 60.25)
    );
    assert_eq!(t.order_id, "202403152099");
    // 1710485885 is 2024-03-15 06:58:05 UTC.
    assert_eq!(t.timestamp, "15-03-2024 12:28:05");
    // The docs' own row: no fill quantity or trade price yet.
    assert_eq!((trades[0].quantity, trades[0].average_price), (1, 0.0));
}

/// Web ::test_a_book_in_an_unknown_shape_is_an_error_not_an_empty_book and
/// ::test_a_book_that_cannot_be_reached_is_an_error. A success without its
/// `data` is unread too (BR-03); a null or empty list is an empty book.
#[tokio::test]
async fn a_book_that_cannot_be_read_is_an_error_not_an_empty_book() {
    let fake = Fake::start().await;
    fake.on(
        "/api/v1/trades",
        vec![
            success(json!({"orders": {"unexpected": true}})),
            Reply::Json(200, json!({"status": "success"})),
            success(json!({"orders": null})),
            success(json!({})),
        ],
    );
    fake.on("/api/v1/holdings", vec![Reply::Cut]);
    let b = fake.broker(master());

    let e = b.get_trade_book(&session()).await.unwrap_err();
    assert!(e.client_message().contains("does not recognise"));
    assert!(b.get_trade_book(&session()).await.is_err());
    assert!(b.get_trade_book(&session()).await.unwrap().is_empty());
    assert!(b.get_trade_book(&session()).await.unwrap().is_empty());

    let e = b.get_holdings(&session()).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "OpenAlgo could not reach Pocketful for the holdings. Check your connection and try again."
    );
}

// ---------------------------------------------------------------------------
// Positions and holdings
// ---------------------------------------------------------------------------

fn daywise() -> Value {
    rows(fixture!("positions_daywise.json"), None).remove(0)
}

fn netwise() -> Value {
    rows(fixture!("positions.json"), None).remove(0)
}

/// A daywise row for one of `MASTER`'s symbols (web `_position`).
fn position(symbol: &str, exchange: &str, net: i64, product: &str) -> Value {
    let (_, _, brsymbol, token, _) = MASTER
        .iter()
        .find(|(s, e, ..)| *s == symbol && *e == exchange)
        .unwrap();
    let token: i64 = token.parse().unwrap();
    with(
        daywise(),
        json!({"exchange": exchange, "trading_symbol": brsymbol, "instrument_token": token,
               "token": token, "product": product, "net_quantity": net}),
    )
}

async fn positions_fake(rows: Vec<Value>) -> Fake {
    let fake = Fake::start().await;
    fake.on(POSITIONS, vec![success(Value::Array(rows))]);
    fake
}

/// Web ::test_positions_are_read_netwise and ::test_the_position_book_in_openalgos_shape
#[tokio::test]
async fn the_position_book_is_read_netwise_in_openalgos_shape() {
    let short = with(
        position("SBIN", "NSE", -10, "NRML"),
        json!({"average_sell_price": 805.0, "average_buy_price": 0, "ltp": 800.0, "net_amount": 8050.0}),
    );
    let fake = positions_fake(vec![daywise(), netwise(), short]).await;
    let b = fake.broker(master());

    let rows = b.get_positions(&session()).await.unwrap();

    let params = fake.to(POSITIONS)[0].query_pairs();
    assert!(params.contains(&("type".into(), "historical".into())));
    assert!(params.contains(&("client_id".into(), CLIENT.into())));
    let r = &rows[0];
    assert_eq!(
        (
            r.symbol.as_str(),
            r.exchange.as_str(),
            r.product.as_str(),
            r.quantity
        ),
        ("YESBANK", "NSE", "MIS", 1)
    );
    // Bought at 14.70, now 14.60.
    assert_eq!((r.average_price, r.ltp, r.pnl), (14.7, 14.6, -0.1));
    let r = &rows[1];
    assert_eq!(
        (
            r.symbol.as_str(),
            r.exchange.as_str(),
            r.product.as_str(),
            r.quantity
        ),
        ("GOLDGUINEA20NOVFUT", "MCX", "NRML", 0)
    );
    // Bought at 40520, sold at 40538: closed for 18, all of it realised.
    assert_eq!((r.average_price, r.ltp, r.pnl), (0.0, 40520.0, 18.0));
    assert_eq!((r.realized_pnl, r.unrealized_pnl), (18.0, 0.0));
    // Sold 10 at 805, now 800.
    assert_eq!(
        (rows[2].quantity, rows[2].average_price, rows[2].pnl),
        (-10, 805.0, 50.0)
    );
}

/// Web ::test_the_annexures_product_names_read_back_as_openalgos
#[tokio::test]
async fn the_annexures_product_names_read_back_as_openalgos() {
    let fake = positions_fake(vec![
        position("SBIN", "NSE", 1, "INTRADAY"),
        position("SBIN", "NSE", 2, "MARGIN"),
    ])
    .await;
    let rows = fake
        .broker(master())
        .get_positions(&session())
        .await
        .unwrap();
    let products: Vec<&str> = rows.iter().map(|r| r.product.as_str()).collect();
    assert_eq!(products, ["MIS", "NRML"]);
}

/// BR-03: a row without its net quantity hides a position, so the position
/// book is not read (the web reads such a row as flat).
#[tokio::test]
async fn a_position_book_row_without_its_quantity_is_not_flat() {
    let mut row = position("SBIN", "NSE", 10, "MIS");
    row.as_object_mut().unwrap().remove("net_quantity");
    let fake = positions_fake(vec![row]).await;
    assert!(fake
        .broker(master())
        .get_positions(&session())
        .await
        .is_err());
}

/// Web ::test_holdings_in_openalgos_shape and
/// ::test_a_holding_missing_from_the_master_contract_keeps_its_own_name
#[tokio::test]
async fn holdings_in_openalgos_shape() {
    let holding = rows(fixture!("holdings.json"), Some("holdings")).remove(0);
    let stranger = with(
        holding.clone(),
        json!({"instrument_details": {"trading_symbol": "ZZZ-EQ", "instrument_token": 1},
               "token": 1, "trading_symbol": "ZZZ"}),
    );
    let fake = Fake::start().await;
    fake.on(
        "/api/v1/holdings",
        vec![success(json!({"holdings": [holding, stranger]}))],
    );
    let b = fake.broker(master());

    let h = b.get_holdings(&session()).await.unwrap();

    assert_eq!(
        fake.to("/api/v1/holdings")[0].query_pairs(),
        vec![("client_id".to_string(), CLIENT.to_string())]
    );
    let s = &h[0];
    assert_eq!(
        (
            s.symbol.as_str(),
            s.exchange.as_str(),
            s.product.as_str(),
            s.quantity
        ),
        ("SBIN", "NSE", "CNC", 55)
    );
    assert_eq!(
        (s.average_price, s.ltp, s.pnl, s.pnl_percentage),
        (240.2, 245.25, 277.75, 2.1)
    );
    let stats = PortfolioStats::from_holdings(&h[..1]);
    assert!((stats.totalholdingvalue - 13488.75).abs() < 1e-6);
    assert!((stats.totalinvvalue - 13211.0).abs() < 1e-6);
    assert!((stats.totalprofitandloss - 277.75).abs() < 1e-6);
    assert!((stats.totalpnlpercentage - 2.1).abs() < 0.01);
    assert_eq!(h[1].symbol, "ZZZ-EQ");
}

// ---------------------------------------------------------------------------
// The open position a smart order sizes against
// ---------------------------------------------------------------------------

async fn open_position(rows: Reply) -> crate::error::Result<i64> {
    let fake = Fake::start().await;
    fake.on(POSITIONS, vec![rows]);
    fake.broker(master())
        .get_open_position(&session(), "SBIN", Exchange::Nse, Product::Mis)
        .await
}

/// Web ::test_a_position_is_matched_by_token_when_its_symbol_differs,
/// ::test_another_products_position_is_not_this_one and
/// ::test_an_empty_position_book_is_flat
#[tokio::test]
async fn the_open_position_is_matched_by_instrument_and_product() {
    let mut held = position("SBIN", "NSE", 10, "MIS");
    held["trading_symbol"] = json!("SBIN"); // not the master's SBIN-EQ
    assert_eq!(open_position(success(json!([held]))).await.unwrap(), 10);
    let nrml = position("SBIN", "NSE", 10, "NRML");
    assert_eq!(open_position(success(json!([nrml]))).await.unwrap(), 0);
    let both = json!([
        position("SBIN", "NSE", 10, "MIS"),
        position("SBIN", "NSE", 99, "NRML")
    ]);
    assert_eq!(open_position(success(both)).await.unwrap(), 10);
    assert_eq!(open_position(success(json!([]))).await.unwrap(), 0);
}

/// Web ::test_a_smart_order_never_sizes_itself_against_an_unread_position
/// and ::test_a_position_row_without_a_net_quantity_refuses_the_smart_order
#[tokio::test]
async fn an_unread_position_never_sizes_an_order() {
    let mut no_quantity = position("SBIN", "NSE", 10, "MIS");
    no_quantity.as_object_mut().unwrap().remove("net_quantity");
    for reply in [
        Reply::Json(200, json(FORBIDDEN)),
        Reply::Json(403, json(FORBIDDEN)),
        Reply::Text(502, "<html>502</html>"),
        success(json!({"positions": "unexpected"})),
        success(json!([no_quantity])),
    ] {
        let e = open_position(reply.clone()).await.unwrap_err();
        assert!(
            e.client_message()
                .contains("could not read your open position"),
            "{:?}: {}",
            reply,
            e.client_message()
        );
    }
}

// ---------------------------------------------------------------------------
// Close all
// ---------------------------------------------------------------------------

/// Web ::test_close_all_squares_off_with_protected_limits and
/// ::test_close_all_with_nothing_open
#[tokio::test]
async fn close_all_squares_off_with_protected_limits() {
    let fake = positions_fake(vec![
        netwise(),
        position("SBIN", "NSE", 10, "MIS"),
        position("YESBANK", "NSE", -5, "NRML"),
    ])
    .await;
    fake.on_query(NSE_QUOTES, "token=3045", vec![success(json!(800.5))]);
    fake.on_query(NSE_QUOTES, "token=11915", vec![success(json!(14.6))]);
    fake.on(ORDERS, vec![Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());

    let r = b.close_all_positions(&session()).await.unwrap();

    assert_eq!(r.placed.len(), 2);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    let sent: Vec<Value> = fake.to(ORDERS).iter().map(|s| s.json()).collect();
    let shape = |v: &Value| {
        (
            v["order_side"].clone(),
            v["quantity"].clone(),
            v["product"].clone(),
            v["order_type"].clone(),
            v["price"].clone(),
        )
    };
    assert_eq!(
        shape(&sent[0]),
        (
            json!("SELL"),
            json!(10),
            json!("MIS"),
            json!("LIMIT"),
            json!(796.45)
        )
    );
    // 14.6 x 1.02 = 14.892, up to 14.90 on a 0.01 tick.
    assert_eq!(
        shape(&sent[1]),
        (
            json!("BUY"),
            json!(5),
            json!("NRML"),
            json!("LIMIT"),
            json!(14.9)
        )
    );

    let flat = positions_fake(vec![netwise()]).await;
    let r = flat
        .broker(master())
        .close_all_positions(&session())
        .await
        .unwrap();
    assert!(r.placed.is_empty() && r.failed.is_empty());
    assert!(flat.to(ORDERS).is_empty());
}

/// Web ::test_a_square_off_that_cannot_be_priced_does_not_stop_the_others
#[tokio::test]
async fn a_square_off_that_cannot_be_priced_does_not_stop_the_others() {
    let fake = positions_fake(vec![
        position("YESBANK", "NSE", 5, "MIS"),
        position("SBIN", "NSE", 10, "MIS"),
    ])
    .await;
    fake.on_query(NSE_QUOTES, "token=3045", vec![success(json!(800.5))]);
    fake.on(ORDERS, vec![Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());

    let r = b.close_all_positions(&session()).await.unwrap();

    let sent = fake.to(ORDERS);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].json()["instrument_token"], "3045");
    assert_eq!(r.placed.len(), 1);
    assert_eq!(r.failed.len(), 1);
    assert!(r.failed[0].starts_with("YESBANK (NSE)"), "{:?}", r.failed);
    assert!(r.failed[0].contains("live price"), "{:?}", r.failed);
    assert!(r.message().contains("still open"));
}

/// Web ::test_a_square_off_whose_order_gets_no_answer_does_not_stop_the_others
#[tokio::test]
async fn a_square_off_with_no_answer_does_not_stop_the_others() {
    let fake = positions_fake(vec![
        position("YESBANK", "NSE", 5, "MIS"),
        position("SBIN", "NSE", 10, "MIS"),
    ])
    .await;
    fake.on_query(NSE_QUOTES, "token=3045", vec![success(json!(800.5))]);
    fake.on_query(NSE_QUOTES, "token=11915", vec![success(json!(14.6))]);
    fake.on(ORDERS, vec![Reply::Cut, Reply::Json(200, json(PLACE_OK))]);
    let b = fake.broker(master());

    let r = b.close_all_positions(&session()).await.unwrap();

    assert_eq!(fake.to(ORDERS).len(), 2);
    assert_eq!(r.placed.len(), 1);
    assert_eq!(r.uncertain, ["YESBANK (NSE)"]);
    assert!(
        r.failed[0].contains("may have been placed"),
        "{:?}",
        r.failed
    );
}

/// Web ::test_close_all_sends_nothing_when_the_positions_cannot_be_read
#[tokio::test]
async fn close_all_sends_nothing_when_the_positions_cannot_be_read() {
    let fake = Fake::start().await;
    fake.on(POSITIONS, vec![Reply::Json(200, json(FORBIDDEN))]);
    let b = fake.broker(master());

    let e = b.close_all_positions(&session()).await.unwrap_err();

    assert!(e
        .client_message()
        .contains("could not read your open position"));
    assert!(fake.to(ORDERS).is_empty());
}

/// A position under a product OpenAlgo does not trade, or whose quantity
/// cannot be read, is reported; the others are still closed.
#[tokio::test]
async fn a_position_openalgo_cannot_close_is_reported() {
    let mut unread_row = position("YESBANK", "NSE", 5, "MIS");
    unread_row["net_quantity"] = json!("n/a");
    let fake = positions_fake(vec![position("SBIN", "NSE", 10, "CO"), unread_row]).await;
    let b = fake.broker(master());

    let r = b.close_all_positions(&session()).await.unwrap();

    assert!(fake.to(ORDERS).is_empty());
    assert_eq!(r.failed.len(), 2, "{:?}", r.failed);
    assert!(r.failed[0].contains("product CO"));
    assert!(r.failed[1].contains("could not read its quantity"));
}
