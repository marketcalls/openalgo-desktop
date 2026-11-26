//! The shared request helper (web `test/test_pocketful_baseurl.py`) and the
//! data lane's rate-limit retry and last price (`test_pocketful_data.py`).

use super::{success, Fake, Reply, CLIENT};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::pocketful::client::{
    broker_reason, Answer, Lane, Request, RATE_LIMITED, RATE_LIMIT_RETRIES,
};
use crate::brokers::pocketful::market::{last_price, path};
use crate::brokers::types::AuthToken;
use crate::error::AppError;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

fn session() -> AuthToken {
    AuthToken::new("jwt.abc.def").with_user_id(CLIENT)
}

fn answer(status: u16, body: Option<Value>) -> Answer {
    Answer {
        status: StatusCode::from_u16(status).unwrap(),
        body,
    }
}

/// Web test_pocketful_baseurl.py::test_either_success_envelope_is_ok
#[test]
fn either_success_envelope_is_ok() {
    let cases = [
        (
            200,
            Some(json!({"status": "success", "message": "", "data": {}})),
            true,
        ),
        (
            200,
            Some(json!({"status": true, "message": "SUCCESS", "errorcode": "", "data": {}})),
            true,
        ),
        (
            200,
            Some(json!({"status": "error", "message": "Request Unauthorised", "data": {}})),
            false,
        ),
        (
            200,
            Some(json!({"status": false, "message": "Invalid", "data": {}})),
            false,
        ),
        (401, Some(json!({"status": "success"})), false),
        (200, None, false),
    ];
    for (status, body, ok) in cases {
        assert_eq!(
            answer(status, body.clone()).ok(),
            ok,
            "{} {:?}",
            status,
            body
        );
    }
    // Only the explicit failure marker is a refusal; an empty object is not.
    assert!(answer(200, Some(json!({"status": false}))).failed());
    assert!(answer(200, Some(json!({"status": "error"}))).failed());
    assert!(!answer(200, Some(json!({}))).failed());
    assert!(!answer(200, None).failed());
}

/// Web test_pocketful_baseurl.py::test_a_request_goes_to_the_new_host_with_a_timeout
/// and ::test_both_documented_header_forms_carry_the_bare_access_token
#[tokio::test]
async fn a_request_carries_both_header_forms_and_its_own_extras() {
    let fake = Fake::start().await;
    fake.on("/api/v1/orders", vec![success(json!({"x": 1}))]);
    let b = fake.broker(SymbolResolver::new());

    let a = b
        .send(
            Lane::Orders,
            &session(),
            Request::new(Method::POST, "/api/v1/orders")
                .query("a", "1")
                .json(json!({"b": 2}))
                .header("clientid", CLIENT),
        )
        .await
        .unwrap();

    assert!(a.ok());
    assert_eq!(a.payload(), Some(&json!({"x": 1})));
    let seen = fake.to("/api/v1/orders");
    assert_eq!(seen.len(), 1);
    let sent = &seen[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.query, "a=1");
    assert_eq!(sent.json(), json!({"b": 2}));
    assert_eq!(sent.header("authorization"), "Bearer jwt.abc.def");
    assert_eq!(sent.header("x-authorization-token"), "jwt.abc.def");
    assert_eq!(sent.header("x-device-type"), "WEB");
    assert_eq!(sent.header("clientid"), CLIENT);
}

/// Web test_pocketful_data.py::test_a_rate_limited_request_is_retried_with_backoff
#[tokio::test]
async fn a_rate_limited_data_request_is_retried() {
    let fake = Fake::start().await;
    let limited = Reply::Json(
        429,
        json!({"status": "error", "message": "Rate limit Exceeded", "data": {}}),
    );
    fake.on(
        "/api/v1/marketdata/NSE/Capital",
        vec![limited.clone(), limited, success(json!(12.9))],
    );
    let b = fake.broker(SymbolResolver::new());

    let a = b
        .send_data(&session(), Request::get("/api/v1/marketdata/NSE/Capital"))
        .await
        .unwrap();

    assert!(a.ok());
    assert_eq!(fake.seen().len(), 3);
}

/// Web test_pocketful_data.py::test_a_rate_limit_that_holds_is_a_busy_refusal_with_a_sentence
#[tokio::test]
async fn a_rate_limit_that_holds_is_a_busy_refusal() {
    let fake = Fake::start().await;
    fake.on(
        "/api/v1/marketdata/NSE/Capital",
        vec![Reply::Json(
            429,
            json!({"status": "error", "message": "Rate limit Exceeded"}),
        )],
    );
    let b = fake.broker(SymbolResolver::new());

    let e = b
        .send_data(&session(), Request::get("/api/v1/marketdata/NSE/Capital"))
        .await
        .unwrap_err();

    assert!(
        matches!(&e, AppError::Busy(m) if m == RATE_LIMITED),
        "{:?}",
        e
    );
    assert_eq!(fake.seen().len(), 1 + RATE_LIMIT_RETRIES as usize);
}

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.replace("_INDEX", ""),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// Web test_pocketful_data.py::test_each_exchange_is_asked_on_its_segment
#[test]
fn each_exchange_is_asked_on_its_segment() {
    for (exchange, want) in [
        ("NSE", "/api/v1/marketdata/NSE/Capital"),
        ("BSE", "/api/v1/marketdata/BSE/Capital"),
        ("NSE_INDEX", "/api/v1/marketdata/NSE/Capital"),
        ("BSE_INDEX", "/api/v1/marketdata/BSE/Capital"),
        ("NFO", "/api/v1/marketdata/NFO/FutOpt"),
        ("BFO", "/api/v1/marketdata/BFO/FutOpt"),
        ("MCX", "/api/v1/marketdata/MCX/FutOpt"),
        ("CDS", "/api/v1/marketdata/CDS/Currency"),
    ] {
        assert_eq!(path(exchange).as_deref(), Some(want), "{}", exchange);
    }
    assert_eq!(path("NCDEX"), None);
}

/// Web test_pocketful_data.py::test_ltp_is_one_request_with_the_documented_key
#[tokio::test]
async fn the_last_price_is_one_request_with_the_documented_key() {
    let fake = Fake::start().await;
    // get-last-traded-price.md, verbatim.
    fake.on(
        "/api/v1/marketdata/NFO/FutOpt",
        vec![success(json!(446981.3))],
    );
    fake.on(
        "/api/v1/marketdata/NSE/Capital",
        vec![success(json!(446981.3))],
    );
    let b = fake.broker(SymbolResolver::new());

    let index = row("NIFTY", "NSE_INDEX", "26000");
    let option = row("BANKNIFTY29JAN2551000CE", "NFO", "35002");
    assert_eq!(last_price(&b, &session(), &index).await, Some(446981.3));
    assert_eq!(last_price(&b, &session(), &option).await, Some(446981.3));

    let seen = fake.seen();
    assert_eq!(seen[0].path, "/api/v1/marketdata/NSE/Capital");
    assert_eq!(seen[0].query, "token=26000&key=last_trade_price");
    assert_eq!(seen[1].path, "/api/v1/marketdata/NFO/FutOpt");
    assert_eq!(seen[1].query, "token=35002&key=last_trade_price");
}

/// Web test_pocketful_data.py::test_ltp_is_zero_when_there_is_no_price and
/// ::test_ltp_of_something_pocketful_cannot_quote_is_zero_and_sends_nothing
#[tokio::test]
async fn no_price_is_none_never_an_error() {
    let replies = [
        Reply::Json(
            200,
            json!({"data": {}, "message": "Invalid parameters", "status": "error"}),
        ),
        success(json!({})),
        success(json!(0)),
        success(Value::Null),
        success(json!(true)),
        Reply::Json(
            401,
            json!({"status": "error", "message": "Request Unauthorised", "data": {}}),
        ),
        Reply::Text(502, "<html>502 Bad Gateway</html>"),
        Reply::Cut,
    ];
    let idea = row("IDEA", "NSE", "14366");
    for reply in replies {
        let fake = Fake::start().await;
        fake.on("/api/v1/marketdata/NSE/Capital", vec![reply.clone()]);
        let b = fake.broker(SymbolResolver::new());
        assert_eq!(last_price(&b, &session(), &idea).await, None, "{:?}", reply);
    }

    let fake = Fake::start().await;
    let b = fake.broker(SymbolResolver::new());
    assert_eq!(
        last_price(&b, &session(), &row("GUARSEED", "NCDEX", "1")).await,
        None
    );
    assert_eq!(
        last_price(&b, &session(), &row("NOPE", "NSE", "")).await,
        None
    );
    assert!(fake.seen().is_empty(), "nothing is asked for");
}

/// Web gtt_data.broker_reason: Pocketful's backend code goes, and the
/// first letter is capitalised.
#[test]
fn pocketfuls_reason_loses_its_backend_code() {
    for (raw, want) in [
        (
            "Error from backend: (500)-event id not found",
            "Event id not found",
        ),
        (
            "error from backend: (1600)-cannot modify, no data found with this id",
            "Cannot modify, no data found with this id",
        ),
        ("Error from backend: no gtt data found", "No gtt data found"),
        (
            "`order` `product` is invalid",
            "`order` `product` is invalid",
        ),
        ("  ", ""),
        ("", ""),
    ] {
        assert_eq!(broker_reason(raw), want, "{}", raw);
    }
}
