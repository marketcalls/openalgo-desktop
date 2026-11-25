//! Funds and the margin calculator (web `test/test_pocketful_account.py`,
//! its funds and margin cases), against a local fake Pocketful answering
//! the documentation's own examples.

use super::{capture, success, Fake, Reply, CLIENT};
use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::{AuthToken, Funds, MarginLeg, MarginResult, QuoteKey};
use crate::brokers::Broker;
use crate::error::AppError;
use serde_json::{json, Value};
use std::time::Duration;

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

// --- Funds -----------------------------------------------------------------

const FUNDS: &str = "/api/v2/funds/view";
const POSITIONS: &str = "/api/v1/positions";

/// The docs' funds answer with `amounts` in place of the documented ones.
fn funds_answer(amounts: &[(&str, &str)]) -> Value {
    let mut body = doc(fixture!("funds.json"));
    for pair in body["data"]["values"].as_array_mut().unwrap() {
        if let Some((_, amount)) = amounts.iter().find(|(label, _)| pair[0] == *label) {
            pair[1] = json!(amount);
        }
    }
    body
}

/// One net-wise position row (fetch-position-netwise.md), amounts in one
/// unit (the web test's `netwise`).
fn netwise(
    symbol: &str,
    bought: i64,
    buy_amount: f64,
    sold: i64,
    sell_amount: f64,
    ltp: f64,
) -> Value {
    json!({
        "trading_symbol": symbol,
        "buy_quantity": bought,
        "buy_amount": buy_amount,
        "sell_quantity": sold,
        "sell_amount": sell_amount,
        "net_quantity": bought - sold,
        "net_amount": sell_amount - buy_amount,
        "ltp": ltp,
        "multiplier": 1,
    })
}

/// A fake answering the docs' funds and this position book.
async fn funds_fake(funds: Value, positions: Reply) -> Fake {
    let fake = Fake::start().await;
    fake.on(FUNDS, vec![Reply::Json(200, funds)]);
    fake.on(POSITIONS, vec![positions]);
    fake
}

/// What `/api/v1/funds` answers for these funds (web `get_margin_data`).
fn wire(f: &Funds) -> Value {
    crate::services::account_service::live_funds(f)
}

/// Web ::test_funds_read_the_documented_answer
#[tokio::test]
async fn funds_read_the_documented_answer() {
    let fake = funds_fake(doc(fixture!("funds.json")), success(json!([]))).await;
    let b = fake.broker(SymbolResolver::new());

    let f = b.get_funds(&session()).await.unwrap();

    assert_eq!(
        wire(&f),
        json!({
            "availablecash": "39.99",
            "collateral": "0.00",
            "m2munrealized": "0.00",
            "m2mrealized": "0.00",
            "utiliseddebits": "0.00",
        })
    );
    let calls = fake.to(FUNDS);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(
        calls[0].query_pairs(),
        [("client_id", CLIENT), ("type", "all")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .to_vec()
    );
}

/// Web ::test_funds_take_pledges_as_collateral_and_net_margin_as_used
#[tokio::test]
async fn funds_take_pledges_as_collateral_and_net_margin_as_used() {
    let body = funds_answer(&[
        ("Available", "1,250.50"),
        ("DP Pledge Collateral", "100"),
        ("Pool Pledge Collateral", "50.25"),
        ("DP Collateral Benefit", "70"),
        ("Net Margin", "25.5"),
    ]);
    let fake = funds_fake(body, success(json!([]))).await;
    let b = fake.broker(SymbolResolver::new());

    let funds = wire(&b.get_funds(&session()).await.unwrap());

    assert_eq!(funds["availablecash"], "1250.50");
    // The benefit row is the same pledge seen another way: not added again.
    assert_eq!(funds["collateral"], "150.25");
    assert_eq!(funds["utiliseddebits"], "25.50");
}

/// Web ::test_funds_m2m_comes_from_the_position_book
#[tokio::test]
async fn funds_m2m_comes_from_the_position_book() {
    let book = success(json!([
        // The docs' example: bought and sold one, closed for 18.
        netwise("IDEA-EQ", 1, 40520.0, 1, 40538.0, 40520.0),
        // Bought 100 at 100, sold 50 at 110, 50 still held at an LTP of
        // 100: 500 booked on the 50 sold, nothing open.
        netwise("TCS-EQ", 100, 10000.0, 50, 5500.0, 100.0),
        // Sold 75 at 10, still short at an LTP of 6: 300 open, nothing booked.
        netwise("NIFTY26N2424000CE", 0, 0.0, 75, 750.0, 6.0),
    ]));
    let fake = funds_fake(doc(fixture!("funds.json")), book).await;
    let b = fake.broker(master());

    let funds = wire(&b.get_funds(&session()).await.unwrap());

    assert_eq!(funds["m2mrealized"], "518.00");
    assert_eq!(funds["m2munrealized"], "300.00");
    // The net-wise book, which carries today's and carried positions.
    let read = fake.to(POSITIONS);
    assert_eq!(read.len(), 1);
    assert!(
        read[0]
            .query_pairs()
            .contains(&("type".to_string(), "historical".to_string())),
        "{}",
        read[0].query
    );
}

/// Web ::test_a_short_closed_in_part_books_its_matched_quantity
#[test]
fn a_short_closed_in_part_books_its_matched_quantity() {
    use crate::brokers::pocketful::mapping::{position_pnl, position_pnl_split};
    // Sold 100 at 100, bought 50 back at 90, short 50 at an LTP of 100.
    let row = netwise("SBIN-EQ", 50, 4500.0, 100, 10000.0, 100.0);

    let (realised, unrealised) = position_pnl_split(&row);

    assert_eq!((realised, unrealised), (500.0, 0.0));
    assert_eq!(realised + unrealised, position_pnl(&row));
}

/// Web ::test_funds_stand_when_the_position_book_cannot_be_read
#[tokio::test]
async fn funds_stand_when_the_position_book_cannot_be_read() {
    for book in [
        Reply::Json(200, json!({"status": "error", "message": "unreachable"})),
        Reply::Text(502, "<html>502 Bad Gateway</html>"),
        Reply::Cut,
    ] {
        let fake = funds_fake(doc(fixture!("funds.json")), book.clone()).await;
        let b = fake.broker(SymbolResolver::new());

        let funds = wire(&b.get_funds(&session()).await.unwrap());

        assert_eq!(funds["availablecash"], "39.99", "{:?}", book);
        assert_eq!(funds["m2mrealized"], "0.00", "{:?}", book);
        assert_eq!(funds["m2munrealized"], "0.00", "{:?}", book);
    }
}

/// Review of the funds port: the session resume gives the whole funds call
/// 15 s, and the position book's own request may take 15 s too. A book
/// that does not answer leaves M2M at zero well inside the resume's time,
/// so a session Pocketful accepted is not refused for it. The web waits
/// for the book up to its request timeout.
#[tokio::test]
async fn a_position_book_that_does_not_answer_leaves_the_resume_its_time() {
    let book = success(json!([netwise("IDEA-EQ", 1, 40520.0, 1, 40538.0, 40520.0)]));
    let silent = Reply::Late(Duration::from_secs(60), Box::new(book));
    let fake = funds_fake(doc(fixture!("funds.json")), silent).await;
    let b = fake.broker(master());

    let (log, guard) = capture();
    let answer = tokio::time::timeout(Duration::from_secs(13), b.get_funds(&session())).await;
    drop(guard);

    let funds = wire(
        &answer
            .expect("the funds answer comes back inside 13 s")
            .unwrap(),
    );
    assert_eq!(funds["availablecash"], "39.99");
    assert_eq!(funds["m2mrealized"], "0.00");
    assert_eq!(funds["m2munrealized"], "0.00");
    assert_eq!(fake.to(POSITIONS).len(), 1, "the book was asked for");
    assert!(
        log.text().contains("did not answer in time for M2M"),
        "{}",
        log.text()
    );
}

/// Web ::test_funds_failures_read_as_no_session: funds that cannot be read
/// are an error, never a zero balance, and no position book is read for
/// them. The web answers `{}` for each; the desktop's error says why.
#[tokio::test]
async fn funds_failures_are_errors_never_a_zero_balance() {
    let refused = doc(fixture!("funds_error.json"));
    let mut refused_with_table = refused.clone();
    refused_with_table["data"] = doc(fixture!("funds.json"))["data"].clone();
    for (reply, says) in [
        (Reply::Json(200, refused.clone()), "Request Unauthorised"),
        (Reply::Json(401, refused), "Log in to Pocketful again"),
        // A refusal is never read as funds, even when it carries a table.
        (Reply::Json(200, refused_with_table), "Request Unauthorised"),
        (
            Reply::Json(500, doc(fixture!("funds.json"))),
            "without giving a reason",
        ),
        (
            Reply::Text(502, "<html>502 Bad Gateway</html>"),
            "could not read",
        ),
        (
            Reply::Json(200, json!({"status": "success", "data": {"values": "n/a"}})),
            "could not read your funds",
        ),
        (Reply::Cut, "Could not reach the broker"),
    ] {
        let fake = Fake::start().await;
        fake.on(FUNDS, vec![reply.clone()]);
        fake.on(POSITIONS, vec![success(json!([]))]);
        let b = fake.broker(SymbolResolver::new());

        let e = match b.get_funds(&session()).await {
            Ok(funds) => panic!("{:?} read as funds: {:?}", reply, funds),
            Err(e) => e,
        };

        assert!(
            e.client_message().contains(says),
            "{:?}: {}",
            reply,
            e.client_message()
        );
        assert!(fake.to(POSITIONS).is_empty(), "{:?}", reply);
    }
}

/// Web ::test_funds_without_a_client_id_send_nothing
#[tokio::test]
async fn funds_without_a_client_id_send_nothing() {
    let fake = funds_fake(doc(fixture!("funds.json")), success(json!([]))).await;
    let b = fake.broker(SymbolResolver::new());

    let e = b
        .get_funds(&AuthToken::new("jwt.abc.def"))
        .await
        .unwrap_err();

    assert!(e.client_message().contains("Log in to Pocketful again"));
    assert!(fake.seen().is_empty());
}

/// Web funds.py: rows Pocketful did not send are logged by label only,
/// never with an amount, and nothing the session holds reaches the log.
#[tokio::test]
async fn missing_funds_rows_are_logged_by_label_never_amount() {
    const TOKEN: &str = "ACCESS_SENTINEL_e9a3";
    let body = json!({"status": "success", "message": "", "data": {
        "values": [["Available", "987654.32"], ["Cash Deposit", "123456.78"]],
    }});
    let fake = funds_fake(body, success(json!([]))).await;
    let b = fake.broker(SymbolResolver::new());

    let (log, guard) = capture();
    let f = b
        .get_funds(&AuthToken::new(TOKEN).with_user_id(CLIENT))
        .await
        .unwrap();
    drop(guard);

    assert_eq!(wire(&f)["availablecash"], "987654.32");
    let text = log.text();
    assert!(text.contains("Net Margin"), "the missing label is named");
    for secret in [TOKEN, "987654", "123456"] {
        assert!(!text.contains(secret), "{}", secret);
    }
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
