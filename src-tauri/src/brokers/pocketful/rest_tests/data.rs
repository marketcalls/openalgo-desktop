//! Quotes, depth, multiquotes and chain quotes over REST (web
//! `test/test_pocketful_data.py`, marketcalls/openalgo#2467, a1d539764),
//! against the answers printed on Pocketful's pages (fixtures under
//! `tests/fixtures/brokers/pocketful/`). The master is the web test's token
//! table. The last price is `client.rs`'s. History is in `data/history.rs`;
//! what needs a fixed IST date or a short deadline is in `data/clocked.rs`.

mod clocked;
mod history;

use super::{capture, success, Fake, Reply, CLIENT};
use crate::brokers::common::ratelimit::crowd;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::pocketful::client::{DATA_INTERVAL, RATE_LIMITED, RATE_LIMIT_RETRIES};
use crate::brokers::pocketful::session_expired;
use crate::brokers::types::*;
use crate::brokers::Broker;
use crate::error::AppError;
use crate::services::core::Reply as ServiceReply;
use crate::services::market_data_service::quote_json;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(super) const NSE: &str = "/api/v1/marketdata/NSE/Capital";
const NFO: &str = "/api/v1/marketdata/NFO/FutOpt";
/// The access token of the web test's `DEMO1:::jwt.abc.def`.
pub(super) const ACCESS: &str = "jwt.abc.def";

pub(super) fn session() -> AuthToken {
    AuthToken::new(ACCESS).with_user_id(CLIENT)
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

/// get-market-data.md, verbatim: token 14366 on NSE.
pub(super) fn market_data() -> Value {
    parse(include_str!(
        "../../../../tests/fixtures/brokers/pocketful/market_data.json"
    ))
}

/// get-market-data.md, "Error response".
pub(super) fn error_answer() -> Value {
    parse(include_str!(
        "../../../../tests/fixtures/brokers/pocketful/market_data_error.json"
    ))
}

/// chart-data.md, verbatim.
pub(super) fn chart_answer() -> Value {
    parse(include_str!(
        "../../../../tests/fixtures/brokers/pocketful/chart_data.json"
    ))
}

/// getting-started.md, "Historical Data NFO", verbatim.
pub(super) fn historical_answer() -> Value {
    parse(include_str!(
        "../../../../tests/fixtures/brokers/pocketful/historical_data.json"
    ))
}

/// [`market_data`] with some of its fields replaced.
pub(super) fn market_data_with(fields: Value) -> Value {
    let mut answer = market_data();
    let data = answer["data"].as_object_mut().unwrap();
    for (k, v) in fields.as_object().unwrap() {
        data.insert(k.clone(), v.clone());
    }
    answer
}

pub(super) fn unauthorised() -> Reply {
    Reply::Json(
        401,
        json!({"status": "error", "message": "Request Unauthorised", "data": {}}),
    )
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
        instrument_type: String::new(),
        tick_size: 0.05,
    }
}

/// The web test's token table, two more legs of its option for a chain,
/// an option with a fractional strike (the symbol-format doc's own), and
/// a contract on an exchange Pocketful does not quote.
pub(super) fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(
        [
            ("IDEA", "NSE", "14366"),
            ("RELIANCE", "NSE", "2885"),
            ("SBIN", "BSE", "500112"),
            ("NIFTY", "NSE_INDEX", "26000"),
            ("SENSEX", "BSE_INDEX", "1"),
            ("BANKNIFTY29JAN25FUT", "NFO", "35001"),
            ("BANKNIFTY29JAN2551000CE", "NFO", "35002"),
            ("BANKNIFTY29JAN2551000PE", "NFO", "35003"),
            ("BANKNIFTY29JAN2551100CE", "NFO", "35004"),
            ("VEDL25APR24292.5CE", "NFO", "35005"),
            ("SENSEX24DEC2580000CE", "BFO", "8001"),
            ("CRUDEOIL19DEC25FUT", "MCX", "4001"),
            ("USDINR26DEC25FUT", "CDS", "9001"),
            ("GUARSEED", "NCDEX", "7001"),
        ]
        .iter()
        .map(|(s, e, t)| row(s, e, t))
        .collect(),
    );
    r
}

/// Web `_plain_sentence`: what a trader reads is a sentence, with no
/// status code, path or exception in it.
pub(super) fn plain_sentence(message: &str) {
    assert!(
        message.chars().next().is_some_and(char::is_uppercase) && message.ends_with('.'),
        "{}",
        message
    );
    for jargon in ["HTTP", "/api", "401", "429", "Exception", "Error", "token="] {
        assert!(!message.contains(jargon), "{}", message);
    }
}

fn idea() -> QuoteKey {
    QuoteKey::new("NSE", "IDEA")
}

/// Web test_pocketful_data.py::test_a_cash_quote_reads_the_documented_answer
#[tokio::test]
async fn a_cash_quote_reads_the_documented_answer() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, market_data())]);
    let b = fake.broker(master());

    let q = b.get_quote(&session(), &idea()).await.unwrap();

    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", NSE)
    );
    assert_eq!(seen[0].query, "token=14366");
    assert_eq!(
        seen[0].header("authorization"),
        format!("Bearer {}", ACCESS)
    );
    // The documented book is in paise: 1285 and 1290 for a 12.90 stock.
    assert_eq!(
        quote_json(&q),
        json!({
            "ltp": 12.9, "open": 13.15, "high": 13.3, "low": 12.75, "prev_close": 13.1,
            "volume": 178286763, "bid": 12.85, "ask": 12.9, "bid_qty": 3486991,
            "ask_qty": 1089393, "oi": 0,
        })
    );
}

/// Web test_pocketful_data.py::test_each_exchange_is_asked_on_its_segment
#[tokio::test]
async fn each_exchange_is_asked_on_its_segment() {
    for (symbol, exchange, path, token) in [
        ("SBIN", "BSE", "/api/v1/marketdata/BSE/Capital", "500112"),
        ("NIFTY", "NSE_INDEX", NSE, "26000"),
        ("SENSEX", "BSE_INDEX", "/api/v1/marketdata/BSE/Capital", "1"),
        ("BANKNIFTY29JAN25FUT", "NFO", NFO, "35001"),
        (
            "SENSEX24DEC2580000CE",
            "BFO",
            "/api/v1/marketdata/BFO/FutOpt",
            "8001",
        ),
        (
            "CRUDEOIL19DEC25FUT",
            "MCX",
            "/api/v1/marketdata/MCX/FutOpt",
            "4001",
        ),
        (
            "USDINR26DEC25FUT",
            "CDS",
            "/api/v1/marketdata/CDS/Currency",
            "9001",
        ),
    ] {
        let fake = Fake::start().await;
        fake.on(path, vec![Reply::Json(200, market_data())]);
        let b = fake.broker(master());

        b.get_quote(&session(), &QuoteKey::new(exchange, symbol))
            .await
            .unwrap();

        let seen = fake.seen();
        assert_eq!(seen.len(), 1, "{}", exchange);
        assert_eq!(seen[0].path, path);
        assert_eq!(seen[0].query, format!("token={}", token));
    }
}

/// Web test_pocketful_data.py::test_a_book_sent_in_rupees_is_read_as_rupees
/// and ::test_a_book_without_any_rupee_price_follows_the_documented_paise
#[tokio::test]
async fn the_book_is_read_in_the_unit_nearest_the_rupee_prices() {
    for fields in [
        json!({"bid_price": 12.85, "ask_price": 12.9}),
        json!({"last_trade_price": 0, "close_price": 0, "open_price": 0, "average_trade_price": 0}),
    ] {
        let fake = Fake::start().await;
        fake.on(
            NSE,
            vec![Reply::Json(200, market_data_with(fields.clone()))],
        );
        let b = fake.broker(master());

        let q = b.get_quote(&session(), &idea()).await.unwrap();

        assert_eq!((q.bid, q.ask), (12.85, 12.9), "{}", fields);
    }
}

/// Web test_pocketful_data.py::test_open_interest_is_read_when_the_answer_carries_it
#[tokio::test]
async fn open_interest_is_read_when_the_answer_carries_it() {
    let fake = Fake::start().await;
    fake.on(
        NFO,
        vec![Reply::Json(
            200,
            market_data_with(json!({"current_open_interest": 8805})),
        )],
    );
    let b = fake.broker(master());

    let q = b
        .get_quote(&session(), &QuoteKey::new("NFO", "BANKNIFTY29JAN25FUT"))
        .await
        .unwrap();

    assert_eq!(q.oi, 8805);
}

/// Web test_pocketful_data.py::test_a_symbol_not_in_the_master_contract_sends_nothing
/// and ::test_an_exchange_pocketful_does_not_quote_sends_nothing
#[tokio::test]
async fn what_pocketful_cannot_quote_sends_nothing() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, market_data())]);
    let b = fake.broker(master());

    for key in [
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NCDEX", "GUARSEED"),
    ] {
        let e = b.get_quote(&session(), &key).await.unwrap_err();
        plain_sentence(&e.client_message());
        let d = b.get_market_depth(&session(), &key).await.unwrap_err();
        plain_sentence(&d.client_message());
    }
    let e = b
        .get_quote(&session(), &QuoteKey::new("NSE", "NOPE"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("NOPE on NSE"), "{}", e);
    assert!(fake.seen().is_empty(), "nothing is asked for");
}

/// Web test_pocketful_data.py::test_a_refusal_names_the_symbol_and_gives_the_brokers_words
#[tokio::test]
async fn a_refusal_names_the_symbol_and_gives_the_brokers_words() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, error_answer())]);
    let b = fake.broker(master());

    let message = b
        .get_quote(&session(), &idea())
        .await
        .unwrap_err()
        .client_message();

    assert!(message.contains("IDEA on NSE"), "{}", message);
    assert!(message.contains("Invalid parameters"), "{}", message);
    plain_sentence(&message);
}

/// Web test_pocketful_data.py::test_an_empty_answer_is_not_a_quote_of_zeros
#[tokio::test]
async fn an_empty_answer_is_not_a_quote_of_zeros() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![success(json!({}))]);
    let b = fake.broker(master());

    let e = b.get_quote(&session(), &idea()).await.unwrap_err();

    assert_eq!(fake.seen().len(), 1, "it was asked");
    plain_sentence(&e.client_message());
}

/// Web test_pocketful_data.py::test_an_ended_session_says_to_log_in_again
#[tokio::test]
async fn an_ended_session_says_to_log_in_again() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![unauthorised()]);
    let b = fake.broker(master());

    let e = b.get_quote(&session(), &idea()).await.unwrap_err();

    assert!(matches!(e, AppError::Auth(_)), "{:?}", e);
    assert_eq!(e.client_message(), session_expired().client_message());
    plain_sentence(&e.client_message());
}

/// Web test_pocketful_data.py::test_a_transport_failure_is_a_sentence
#[tokio::test]
async fn a_transport_failure_is_a_sentence() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Cut]);
    let b = fake.broker(master());

    let e = b.get_quote(&session(), &idea()).await.unwrap_err();

    assert!(matches!(e, AppError::Http(_)), "{:?}", e);
    assert_eq!(fake.seen().len(), 1, "it was asked");
    plain_sentence(&e.client_message());
}

/// Web test_pocketful_data.py::test_depth_is_the_best_bid_and_ask_padded_to_five_levels
#[tokio::test]
async fn depth_is_the_best_bid_and_ask_padded_to_five_levels() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, market_data())]);
    let b = fake.broker(master());

    let d = b.get_market_depth(&session(), &idea()).await.unwrap();

    assert_eq!(fake.seen().len(), 1);
    let level = |price, quantity| DepthLevel {
        price,
        quantity,
        orders: 0,
    };
    let mut bids = vec![level(12.85, 3486991)];
    bids.resize(5, level(0.0, 0));
    let mut asks = vec![level(12.9, 1089393)];
    asks.resize(5, level(0.0, 0));
    assert_eq!(d.bids, bids);
    assert_eq!(d.asks, asks);
    assert_eq!(
        (d.high, d.low, d.ltp, d.ltq, d.oi, d.open, d.prev_close),
        (13.3, 12.75, 12.9, 1, 0, 13.15, 13.1)
    );
    assert_eq!(
        (d.total_buy_qty, d.total_sell_qty, d.volume),
        (31748583, 40173028, 178286763)
    );
}

/// Web test_pocketful_data.py::test_multiquotes_quote_each_symbol_once_in_the_order_given
#[tokio::test]
async fn multiquotes_quote_each_symbol_once_in_the_order_given() {
    let fake = Fake::start().await;
    fake.on(
        NSE,
        vec![
            Reply::Json(200, market_data_with(json!({"last_trade_price": 14366.0}))),
            Reply::Json(200, market_data_with(json!({"last_trade_price": 26000.0}))),
        ],
    );
    let b = fake.broker(master());
    let keys = [
        idea(),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
    ];

    let results = b.get_multiquotes(&session(), &keys).await.unwrap();

    let tokens: Vec<String> = fake.seen().iter().map(|s| s.query.clone()).collect();
    assert_eq!(tokens, ["token=14366", "token=26000"]);
    let order: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r.symbol.as_str(), r.exchange.as_str()))
        .collect();
    assert_eq!(
        order,
        [("IDEA", "NSE"), ("NOPE", "NSE"), ("NIFTY", "NSE_INDEX")]
    );
    assert_eq!(results[0].data.as_ref().unwrap().ltp, 14366.0);
    assert_eq!(results[2].data.as_ref().unwrap().ltp, 26000.0);
    assert!(results[1].data.is_none());
    plain_sentence(results[1].error.as_deref().unwrap());
}

/// Web test_pocketful_data.py::test_multiquotes_keep_one_symbols_refusal_and_go_on
#[tokio::test]
async fn multiquotes_keep_one_symbols_refusal_and_go_on() {
    let fake = Fake::start().await;
    fake.on(
        NSE,
        vec![
            Reply::Json(200, error_answer()),
            Reply::Json(200, market_data()),
        ],
    );
    let b = fake.broker(master());

    let results = b
        .get_multiquotes(&session(), &[idea(), QuoteKey::new("NSE", "RELIANCE")])
        .await
        .unwrap();

    assert_eq!(fake.seen().len(), 2);
    assert!(results[0]
        .error
        .as_deref()
        .unwrap()
        .contains("Invalid parameters"));
    assert_eq!(results[1].data.as_ref().unwrap().ltp, 12.9);
}

/// Web test_pocketful_data.py::test_multiquotes_stop_at_the_first_failure_every_symbol_would_share
#[tokio::test]
async fn multiquotes_stop_at_the_first_failure_every_symbol_would_share() {
    for reply in [unauthorised(), Reply::Cut] {
        let fake = Fake::start().await;
        fake.on(NSE, vec![reply.clone()]);
        let b = fake.broker(master());

        let e = b
            .get_multiquotes(&session(), &[idea(), QuoteKey::new("NSE", "RELIANCE")])
            .await
            .unwrap_err();

        assert!(
            matches!(e, AppError::Auth(_) | AppError::Http(_)),
            "{:?}: {:?}",
            reply,
            e
        );
        assert_eq!(fake.seen().len(), 1, "{:?}", reply);
    }
}

/// Web test_pocketful_data.py::test_a_rate_limit_that_holds_is_a_busy_refusal_with_a_sentence
/// (the second symbol is not tried; the retry itself is `client.rs`'s).
#[tokio::test]
async fn a_rate_limit_that_holds_ends_the_multiquotes_as_busy() {
    let fake = Fake::start().await;
    fake.on(
        NSE,
        vec![Reply::Json(
            429,
            json!({"status": "error", "message": "Rate limit Exceeded", "data": {}}),
        )],
    );
    let b = fake.broker(master());

    let e = b
        .get_multiquotes(&session(), &[idea(), QuoteKey::new("NSE", "RELIANCE")])
        .await
        .unwrap_err();

    assert!(
        matches!(&e, AppError::Busy(m) if m == RATE_LIMITED),
        "{:?}",
        e
    );
    assert_eq!(fake.seen().len(), 1 + RATE_LIMIT_RETRIES as usize);
    plain_sentence(&e.client_message());
}

/// Web test_pocketful_data.py::test_a_pacing_refusal_reaches_the_quotes_service_as_busy
/// and ::test_under_gthread_a_turn_too_far_away_is_refused_before_booking:
/// a turn more than ten seconds away is refused before anything is sent,
/// and `/api/v1` answers it 429.
#[tokio::test]
async fn a_turn_too_far_away_is_refused_as_busy_before_sending() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, market_data())]);
    let b = fake.broker(master());
    let _queue = crowd(&b.pacer);

    let quote = b.get_quote(&session(), &idea()).await.unwrap_err();
    let multi = b.get_multiquotes(&session(), &[idea()]).await.unwrap_err();

    for e in [quote, multi] {
        assert!(matches!(e, AppError::Busy(_)), "{:?}", e);
        assert_eq!(ServiceReply::broker_failure(&e, 500).status, 429);
    }
    assert!(fake.seen().is_empty(), "nothing was sent");
}

/// Web test_pocketful_data.py::test_requests_are_paced_from_module_state:
/// one adapter (the registry keeps one per process) paces every data
/// request at the web's 8 a second.
#[tokio::test]
async fn quotes_are_paced() {
    let fake = Fake::start().await;
    fake.on(NSE, vec![Reply::Json(200, market_data())]);
    let b = fake.broker(master());

    let started = std::time::Instant::now();
    for _ in 0..3 {
        b.get_quote(&session(), &idea()).await.unwrap();
    }

    assert_eq!(fake.seen().len(), 3);
    assert!(
        started.elapsed() >= DATA_INTERVAL * 2,
        "{:?}",
        started.elapsed()
    );
}

/// CLAUDE.md "A missing open interest is not a zero" (no web test: the web
/// reads the missing figure as 0): a chain leg whose answer carries no
/// `current_open_interest` is reported, a leg that carries one (0
/// included) is a reading, and the quotes keep the web's 0.
#[tokio::test]
async fn a_chain_leg_without_open_interest_is_reported_not_zero() {
    let fake = Fake::start().await;
    fake.on(
        NFO,
        vec![
            Reply::Json(
                200,
                market_data_with(json!({"current_open_interest": 8805})),
            ),
            Reply::Json(200, market_data()),
            Reply::Json(200, market_data_with(json!({"current_open_interest": 0}))),
        ],
    );
    let b = fake.broker(master());
    let legs = [
        "BANKNIFTY29JAN2551000CE",
        "BANKNIFTY29JAN2551000PE",
        "BANKNIFTY29JAN2551100CE",
    ];
    let req = ChainQuoteRequest {
        underlying: QuoteKey::new("NSE_INDEX", "BANKNIFTY"),
        expiry: chrono::NaiveDate::from_ymd_opt(2025, 1, 29).unwrap(),
        keys: legs.iter().map(|s| QuoteKey::new("NFO", *s)).collect(),
        strikes_each_side: None,
    };

    let chain = b.get_chain_quotes(&session(), &req).await.unwrap();

    let oi: Vec<i64> = chain
        .quotes
        .iter()
        .map(|q| q.data.as_ref().unwrap().oi)
        .collect();
    assert_eq!(oi, [8805, 0, 0]);
    assert_eq!(
        chain.oi_missing,
        HashSet::from(["BANKNIFTY29JAN2551000PE".to_string()])
    );
}

/// Nothing the session carries reaches a log, an error or a trader on the
/// data paths: a refused session, a refusal, an answer that is not JSON
/// and a lost answer, for a quote, a depth and a history.
#[tokio::test]
async fn data_failures_log_no_secret() {
    const TOKEN: &str = "ACCESS_SENTINEL_d4t4";
    let auth = AuthToken::new(TOKEN).with_user_id(CLIENT);
    let history = HistoryRequest {
        key: idea(),
        interval: "1m".into(),
        start: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        end: chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
    };
    let (log, guard) = capture();
    for reply in [
        unauthorised(),
        Reply::Json(200, error_answer()),
        Reply::Text(502, "<html>502 Bad Gateway</html>"),
        Reply::Cut,
    ] {
        let fake = Fake::start().await;
        fake.on(NSE, vec![reply.clone()]);
        fake.on("/api/v1/charts", vec![reply.clone()]);
        let b = fake.broker(master());
        let quote = b.get_quote(&auth, &idea()).await.unwrap_err();
        let depth = b.get_market_depth(&auth, &idea()).await.unwrap_err();
        let bars = b.get_history(&auth, &history).await.unwrap_err();
        for e in [quote, depth, bars] {
            let shown = format!("{} {:?} {}", e, e, e.client_message());
            assert!(!shown.contains(TOKEN), "{:?}: {}", reply, shown);
            plain_sentence(&e.client_message());
        }
    }
    drop(guard);
    let text = log.text();
    assert!(!text.is_empty(), "the capture saw the failures");
    assert!(!text.contains(TOKEN), "{}", text);
}
