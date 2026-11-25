//! Funds and the margin calculator (web `test/test_pocketful_account.py`,
//! its funds and margin cases), against a local fake Pocketful answering
//! the documentation's own examples.

use super::{capture, Fake, Reply, CLIENT};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::{AuthToken, MarginLeg, MarginResult, QuoteKey};
use crate::brokers::Broker;
use crate::error::AppError;
use serde_json::{json, Value};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            "../../../../tests/fixtures/brokers/pocketful/",
            $name
        ))
    };
}

const MARGIN: &str = "/api/space/v1/orderapis/marginCalculations";

fn doc(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn session() -> AuthToken {
    AuthToken::new("jwt.abc.def").with_user_id(CLIENT)
}

// --- Master contract stand-in (the web test's SYMBOLS) ---------------------

fn row(
    symbol: &str,
    exchange: &str,
    token: &str,
    brsymbol: &str,
    name: &str,
    instrument_type: &str,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.replace("_INDEX", ""),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: instrument_type.into(),
        tick_size: 0.05,
    }
}

fn master() -> SymbolResolver {
    let s = SymbolResolver::new();
    s.load(vec![
        row("ADANIENT", "NSE", "25", "ADANIENT-EQ", "ADANIENT", "EQ"),
        row(
            "ADANIENT29SEP26FUT",
            "NFO",
            "68417",
            "ADANIENT26SEPFUT",
            "ADANIENT",
            "FUT",
        ),
        row(
            "ADANIENT27OCT26FUT",
            "NFO",
            "48711",
            "ADANIENT26OCTFUT",
            "ADANIENT",
            "FUT",
        ),
        row("NIFTY", "NSE_INDEX", "26000", "Nifty 50", "NIFTY", "EQ"),
        row(
            "NIFTY24NOV2624000CE",
            "NFO",
            "40001",
            "NIFTY26N2424000CE",
            "NIFTY",
            "CE",
        ),
        row(
            "NIFTY24NOV2624000PE",
            "NFO",
            "40002",
            "NIFTY26N2424000PE",
            "NIFTY",
            "PE",
        ),
        row(
            "CRUDEOIL18DEC26FUT",
            "MCX",
            "445001",
            "CRUDEOIL26DECFUT",
            "CRUDEOIL",
            "FUT",
        ),
        row("IDEA", "NSE", "14366", "IDEA-EQ", "IDEA", "EQ"),
        row("TCS", "NSE", "11536", "TCS-EQ", "TCS", "EQ"),
    ]);
    s
}

// --- Margin calculator -----------------------------------------------------

/// An NRML limit leg; the cases change what they need.
fn leg(symbol: &str, exchange: &str, action: Action, quantity: i64, price: f64) -> MarginLeg {
    MarginLeg {
        key: QuoteKey::new(exchange, symbol),
        action,
        quantity,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price,
        trigger_price: 0.0,
    }
}

/// The docs' own request: a calendar spread on ADANIENT futures.
fn calendar() -> Vec<MarginLeg> {
    vec![
        leg("ADANIENT29SEP26FUT", "NFO", Action::Buy, 309, 2939.7),
        leg("ADANIENT27OCT26FUT", "NFO", Action::Sell, 309, 2954.0),
    ]
}

/// The docs' answer with these `combinedMargin` members set.
fn margin_answer(combined: &[(&str, f64)]) -> Reply {
    let mut body = doc(fixture!("margin.json"));
    for (key, value) in combined {
        body["data"]["combinedMargin"][*key] = json!(value);
    }
    Reply::Json(200, body)
}

fn refusal_text(e: AppError) -> String {
    match e {
        AppError::Broker(m) | AppError::Validation(m) => m,
        other => panic!("not a refusal /api/v1/margin answers with 400: {:?}", other),
    }
}

/// Web ::test_a_basket_goes_in_one_request_shaped_as_the_docs
#[tokio::test]
async fn a_basket_goes_in_one_request_shaped_as_the_docs() {
    let fake = Fake::start().await;
    fake.on(
        MARGIN,
        vec![margin_answer(&[
            ("span", 100000.0),
            ("exposureMargin", 20000.0),
            ("spanSpreadMargin", 5000.0),
        ])],
    );
    let b = fake.broker(master());

    let m = b.calculate_margin(&session(), &calendar()).await.unwrap();

    assert_eq!(
        m,
        MarginResult {
            total_margin_required: 125000.0,
            span_margin: 100000.0,
            exposure_margin: 20000.0,
        }
    );
    assert_eq!(fake.seen().len(), 1, "one request for the whole basket");
    let sent = &fake.to(MARGIN)[0];
    assert_eq!(sent.method, "POST");
    // margin-calculator.md lists a clientid header beside the session's own.
    assert_eq!(sent.header("clientid"), CLIENT);
    assert_eq!(sent.header("authorization"), "Bearer jwt.abc.def");
    // The docs' own request, with the product sent by name.
    assert_eq!(
        sent.json(),
        json!({"data": [
            {
                "exchange": "NFO",
                "mode": "NEW",
                "price": "2939.7",
                "product": "NRML",
                "quantity": "309",
                "segment": "FutOpt",
                "series": "FUTSTK",
                "side": "BUY",
                "symbol": "ADANIENT26SEPFUT",
                "token": "68417",
                "underlying": "25",
            },
            {
                "exchange": "NFO",
                "mode": "NEW",
                "price": "2954",
                "product": "NRML",
                "quantity": "309",
                "segment": "FutOpt",
                "series": "FUTSTK",
                "side": "SELL",
                "symbol": "ADANIENT26OCTFUT",
                "token": "48711",
                "underlying": "25",
            },
        ]})
    );
}

/// Web ::test_index_options_and_cash_legs_take_their_series_and_underlying
#[tokio::test]
async fn index_options_and_cash_legs_take_their_series_and_underlying() {
    let fake = Fake::start().await;
    fake.on(
        MARGIN,
        vec![margin_answer(&[
            ("premiumMargin", 3000.0),
            ("premiumBenefit", 1000.0),
            ("varMargin", 50.0),
        ])],
    );
    let b = fake.broker(master());
    let legs = [
        MarginLeg {
            pricetype: PriceType::Market,
            ..leg("NIFTY24NOV2624000CE", "NFO", Action::Sell, 75, 0.0)
        },
        MarginLeg {
            product: Product::Cnc,
            ..leg("ADANIENT", "NSE", Action::Buy, 1, 2500.0)
        },
    ];

    let m = b.calculate_margin(&session(), &legs).await.unwrap();

    assert_eq!(m.total_margin_required, 2050.0);
    let body = fake.to(MARGIN)[0].json();
    let (option, cash) = (&body["data"][0], &body["data"][1]);
    assert_eq!(
        (&option["series"], &option["underlying"], &option["price"]),
        (&json!("OPTIDX"), &json!("26000"), &json!("0"))
    );
    assert_eq!(
        (&cash["segment"], &cash["series"], &cash["underlying"]),
        (&json!("Capital"), &json!("EQ"), &json!("25"))
    );
}

/// Web ::test_one_position_uses_the_same_calculator
#[tokio::test]
async fn one_position_uses_the_same_calculator() {
    let fake = Fake::start().await;
    fake.on(
        MARGIN,
        vec![margin_answer(&[("span", 1.0), ("exposureMargin", 1.0)])],
    );
    let b = fake.broker(master());

    b.calculate_margin(&session(), &calendar()[..1])
        .await
        .unwrap();

    assert_eq!(
        fake.to(MARGIN)[0].json()["data"].as_array().unwrap().len(),
        1
    );
}

/// Web ::test_an_empty_combined_block_falls_back_to_the_legs
#[tokio::test]
async fn an_empty_combined_block_falls_back_to_the_legs() {
    let fake = Fake::start().await;
    let mut body = doc(fixture!("margin.json"));
    body["data"]["individualMarginValues"] = json!([
        {"span": 400, "exposureMargin": 100},
        {"span": 300, "exposureMargin": 50},
    ]);
    fake.on(MARGIN, vec![Reply::Json(200, body)]);
    let b = fake.broker(master());

    let m = b.calculate_margin(&session(), &calendar()).await.unwrap();

    assert_eq!(
        m,
        MarginResult {
            total_margin_required: 850.0,
            span_margin: 700.0,
            exposure_margin: 150.0,
        }
    );
}

/// Web ::test_a_refusal_sent_with_http_200_is_not_a_success and
/// ::test_the_margin_service_reports_an_http_200_refusal_as_a_failure: the
/// adapter answers a broker refusal, which `/api/v1/margin` answers with
/// 400 (`market_data_service::margin`), as the web's service does.
#[tokio::test]
async fn a_refusal_sent_with_http_200_is_not_a_success() {
    let fake = Fake::start().await;
    fake.on(
        MARGIN,
        vec![Reply::Json(200, doc(fixture!("margin_error.json")))],
    );
    let b = fake.broker(master());

    let e = b
        .calculate_margin(&session(), &calendar())
        .await
        .unwrap_err();

    assert!(matches!(&e, AppError::Broker(_)), "{:?}", e);
    assert!(
        refusal_text(e).contains("Invalid request parameters or unauthorized"),
        "Pocketful's own reason reaches the trader"
    );
}

/// Web ::test_an_unreadable_or_missing_answer_is_a_502. The desktop answers
/// these with 400 like every margin refusal; the sentences are the web's.
#[tokio::test]
async fn an_unreadable_or_missing_answer_is_refused_with_the_webs_sentence() {
    const UNREADABLE: &str = "Pocketful's margin calculator sent an answer OpenAlgo could not read. Try again in a moment.";
    const NO_ANSWER: &str =
        "Pocketful's margin calculator did not answer. Check your connection and try again.";
    for (reply, want) in [
        (Reply::Text(200, "<html>gateway</html>"), UNREADABLE),
        (
            Reply::Json(200, json!({"status": true, "data": {}})),
            UNREADABLE,
        ),
        (Reply::Cut, NO_ANSWER),
    ] {
        let fake = Fake::start().await;
        fake.on(MARGIN, vec![reply.clone()]);
        let b = fake.broker(master());

        let e = b
            .calculate_margin(&session(), &calendar())
            .await
            .unwrap_err();

        assert!(matches!(&e, AppError::Broker(_)), "{:?}: {:?}", reply, e);
        assert_eq!(refusal_text(e), want, "{:?}", reply);
    }
}

/// Web ::test_a_leg_that_cannot_be_priced_refuses_the_basket_unsent
#[tokio::test]
async fn a_leg_that_cannot_be_priced_refuses_the_basket_unsent() {
    for (symbol, exchange, words) in [
        ("NOSUCH", "NSE", "not in the Pocketful master contract"),
        ("CRUDEOIL18DEC26FUT", "MCX", "MCX"),
        ("NIFTY", "NSE_INDEX", "NSE_INDEX"),
    ] {
        let fake = Fake::start().await;
        let b = fake.broker(master());
        let legs = [
            calendar().remove(0),
            MarginLeg {
                pricetype: PriceType::Market,
                ..leg(symbol, exchange, Action::Buy, 1, 0.0)
            },
        ];

        let e = b.calculate_margin(&session(), &legs).await.unwrap_err();

        let text = refusal_text(e);
        assert!(text.contains(words), "{}: {}", symbol, text);
        assert!(fake.seen().is_empty(), "{}: nothing is sent", symbol);
    }
}

/// Web margin_api: a refusal reads as Pocketful's own reason without its
/// backend code, or as a plain sentence when Pocketful gave none.
#[tokio::test]
async fn a_refusal_reads_as_pocketfuls_reason() {
    for (reply, want) in [
        (
            Reply::Json(
                400,
                json!({"status": "error", "message": "Error from backend: (500)-margin data not found", "data": {}}),
            ),
            "Pocketful could not calculate the margin: Margin data not found",
        ),
        (
            Reply::Json(
                401,
                json!({"status": "error", "message": "Request Unauthorised", "error_code": 40000, "data": {}}),
            ),
            "Pocketful could not calculate the margin: Request Unauthorised",
        ),
        (
            Reply::Json(200, json!({"status": false, "message": "", "data": {}})),
            "Pocketful could not calculate the margin. Check the positions and try again.",
        ),
    ] {
        let fake = Fake::start().await;
        fake.on(MARGIN, vec![reply.clone()]);
        let b = fake.broker(master());

        let e = b
            .calculate_margin(&session(), &calendar())
            .await
            .unwrap_err();

        assert_eq!(refusal_text(e), want, "{:?}", reply);
    }
}

/// Web margin_api: a session without its trading account sends nothing.
#[tokio::test]
async fn a_margin_without_a_client_id_sends_nothing() {
    let fake = Fake::start().await;
    let b = fake.broker(master());

    let e = b
        .calculate_margin(&AuthToken::new("jwt.abc.def"), &calendar())
        .await
        .unwrap_err();

    assert_eq!(
        refusal_text(e),
        "Your Pocketful session carries no client id. Log in to Pocketful again and retry."
    );
    assert!(fake.seen().is_empty());
}

/// Nothing the session holds reaches a log, on success or refusal.
#[tokio::test]
async fn a_margin_request_logs_no_session_secret() {
    const TOKEN: &str = "ACCESS_SENTINEL_4b1d";
    let fake = Fake::start().await;
    fake.on(
        MARGIN,
        vec![
            margin_answer(&[("span", 10.0)]),
            Reply::Json(200, doc(fixture!("margin_error.json"))),
            Reply::Cut,
        ],
    );
    let b = fake.broker(master());
    let auth = AuthToken::new(TOKEN).with_user_id(CLIENT);

    let (log, guard) = capture();
    for _ in 0..3 {
        let _ = b.calculate_margin(&auth, &calendar()).await;
    }
    drop(guard);

    let text = log.text();
    assert!(!text.is_empty(), "the margin path logs what it did");
    assert!(!text.contains(TOKEN));
    assert_eq!(fake.to(MARGIN).len(), 3);
}
