//! 5paisa XTS reads XTS answers as the XTS v2 docs describe them, asks for
//! history a year at a time, shares one market-data session between its
//! feed, quotes and history, and keeps its subscriptions whole
//! (marketcalls/openalgo#2484, #2490, #2504; web 5749d4126, 19f993c92).
//! Ported from web test/test_fivepaisaxts_xts_docs.py, against a local fake
//! XTS (ephemeral ports). The other XTS members keep the template
//! (`brokers_xts.rs`).
//!
//! Where the desktop keeps a rule of its own the test says so: a position
//! book that cannot be read refuses the smart order (the web reads it as 0
//! and refuses through `refuse_smart_order_on_read_failure`), and a feed
//! the broker refuses is held until the next sign-in (the web retries the
//! same token).

use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::families::xts::streaming::{
    normalise_message, run_command, Command, CommandReport, FeedSource, SubscriptionTarget, XtsFeed,
};
use openalgo_desktop_lib::brokers::families::xts::XtsBroker;
use openalgo_desktop_lib::brokers::fivepaisaxts::CONFIG;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::websocket::{handshake, FeedConfig, WebSocketManager};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

// ---------------------------------------------------------------------------
// A fake XTS
// ---------------------------------------------------------------------------

/// A market-data token as XTS issues it: a JWT whose `userID` claim reads
/// `<user id>_<app id>`, numbered by the login that issued it. Built at
/// run time, so no token-shaped literal is in the source.
fn md_token(n: usize) -> String {
    let part = |s: String| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    format!(
        "{}.{}.sig",
        part("{\"alg\":\"HS256\"}".into()),
        part(format!("{{\"userID\":\"1048131_APP\",\"n\":{n}}}"))
    )
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<String>>,
    logins: AtomicUsize,
    /// Market-data tokens answered "Invalid Token".
    expired: Mutex<Vec<String>>,
    /// While set, a market-data login waits for it before answering.
    hold_login: Mutex<Option<Arc<Notify>>>,
    orders: Mutex<Value>,
    trades: Mutex<Value>,
    positions: Mutex<Value>,
    balance: Mutex<Value>,
    /// The answer to a cancel or a modify.
    answer: Mutex<Value>,
}

type S = State<Arc<Fake>>;

impl Fake {
    fn log(&self, line: String) {
        self.seen.lock().push(line);
    }
    fn lines(&self, prefix: &str) -> Vec<String> {
        self.seen
            .lock()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .cloned()
            .collect()
    }
    fn refuses(&self, token: &str) -> bool {
        self.expired.lock().iter().any(|t| t == token)
    }
}

fn header(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn invalid_token() -> Value {
    json!({"type": "error", "code": "e-session-0007", "description": "Invalid Token"})
}

async fn md_login(State(s): S, OriginalUri(u): OriginalUri) -> Json<Value> {
    let n = s.logins.fetch_add(1, Ordering::SeqCst) + 1;
    s.log(format!("login|{}", u.path()));
    let gate = s.hold_login.lock().clone();
    if let Some(gate) = gate {
        gate.notified().await;
    }
    Json(json!({"type": "success", "result": {"token": md_token(n), "userID": "1048131"}}))
}

fn master_rows(segment: &str) -> &'static str {
    // The XTS v2 docs' example rows: an option (23 fields), a future (21)
    // and a spread (21, InstrumentType 4).
    match segment {
        "NSEFO" => concat!(
            "NSEFO|48225|2|NIFTY|NIFTY2621725700PE|OPTIDX|NIFTY-OPTIDX|2604800048225|588|114.5|1801|0.05|65|1|-1|Nifty 50|2026-02-17T14:30:00|25700|4|NIFTY 17FEB2026 PE 25700|1|1|NIFTY2621725700PE\n",
            "NSEFO|49229|1|NIFTY|NIFTY26JANFUT|FUTIDX|NIFTY-FUTIDX|2602700049229|28369.8|23211.8|1801|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY 27JAN2026|2|1|NIFTY26JANFUT\n",
            "NSEFO|60000|4|NIFTY|NIFTYSPD|FUTIDX|NIFTY-FUTIDX|1|1|1|1|0.1|65|1|-1|Nifty 50|2026-01-27T14:30:00|NIFTY SPD|1|1|NIFTYSPD\n",
        ),
        _ => "",
    }
}

fn md_routes(prefix: &str) -> Router<Arc<Fake>> {
    Router::new()
        .route(&format!("{prefix}/auth/login"), post(md_login))
        .route(
            &format!("{prefix}/instruments/quotes"),
            post(|State(s): S, h: HeaderMap| async move {
                let token = header(&h);
                s.log(format!("quotes|{token}"));
                if s.refuses(&token) {
                    return Json(invalid_token());
                }
                let q = json!({"ExchangeSegment": 1, "ExchangeInstrumentID": 2885,
                    "Touchline": {"LastTradedPrice": 2500.25, "Close": 2490.0}});
                Json(json!({"type": "success", "result": {"listQuotes": [q.to_string()]}}))
            }),
        )
        .route(
            &format!("{prefix}/instruments/ohlc"),
            get(
                |State(s): S, h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    let token = header(&h);
                    s.log(format!(
                        "ohlc|{}|{}|{}|{}",
                        token, q["startTime"], q["endTime"], q["compressionValue"]
                    ));
                    if s.refuses(&token) {
                        return Json(invalid_token());
                    }
                    Json(json!({"type": "success",
                        "result": {"dataReponse": "1737331200|1316|1316|1300.25|1305.45|14040244|0|"}}))
                },
            ),
        )
        .route(
            &format!("{prefix}/instruments/master"),
            post(|Json(b): Json<Value>| async move {
                let seg = b["exchangeSegmentList"][0].as_str().unwrap_or("").to_string();
                Json(json!({"type": "success", "result": master_rows(&seg)}))
            }),
        )
        .route(
            &format!("{prefix}/instruments/indexlist"),
            get(|| async { Json(json!({"type": "success", "result": {"indexList": []}})) }),
        )
        .route(
            &format!("{prefix}/instruments/subscription"),
            post(|| async { Json(json!({"type": "success", "result": {"listQuotes": []}})) }),
        )
}

fn app(state: Arc<Fake>) -> Router {
    Router::new()
        .route(
            "/interactive/user/session",
            post(|| async {
                Json(json!({"type": "success", "result": {"token": "itok", "userID": "1048131"}}))
            }),
        )
        .route(
            "/interactive/orders",
            get(|State(s): S| async move { Json(s.orders.lock().clone()) })
                .put(|State(s): S| async move { Json(s.answer.lock().clone()) })
                .delete(|State(s): S, OriginalUri(u): OriginalUri| async move {
                    s.log(format!("cancel|{}", u.query().unwrap_or("")));
                    Json(s.answer.lock().clone())
                }),
        )
        .route(
            "/interactive/orders/trades",
            get(|State(s): S| async move { Json(s.trades.lock().clone()) }),
        )
        .route(
            "/interactive/portfolio/positions",
            get(|State(s): S| async move { Json(s.positions.lock().clone()) }),
        )
        .route(
            "/interactive/user/balance",
            get(|State(s): S| async move { Json(s.balance.lock().clone()) }),
        )
        .merge(md_routes("/apimarketdata"))
        .merge(md_routes("/apibinarymarketdata"))
        .with_state(state)
}

async fn serve(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn row(symbol: &str, exchange: &str, token: &str, brsymbol: &str, brexchange: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: brexchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        row("RELIANCE", "NSE", "2885", "RELIANCE-EQ", "NSECM"),
        row("NIFTY", "NSE_INDEX", "26000", "NIFTY 50", "NSE_INDEX"),
        row(
            "NIFTY27OCT2622400CE",
            "NFO",
            "48225",
            "NIFTY26OCT22400CE",
            "NSEFO",
        ),
    ]);
    r
}

async fn broker() -> (XtsBroker, Arc<Fake>, String) {
    let fake = Arc::new(Fake::default());
    let base = serve(app(fake.clone())).await;
    (
        XtsBroker::with_base_url(&CONFIG, master(), base.as_str()),
        fake,
        base,
    )
}

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: "ikey".into(),
        api_secret: Some("isecret".into()),
        api_key_market: Some("mkey".into()),
        api_secret_market: Some("msecret".into()),
        ..Default::default()
    }
}

fn token() -> AuthToken {
    AuthToken::new("itok").with_feed(Some(md_token(1)))
}

async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// Open position (web: matched by instrument, segment and product)
// ---------------------------------------------------------------------------

fn position(overrides: Value) -> Value {
    let mut p = json!({
        "TradingSymbol": "NIFTY 27OCT2026 CE 22400",
        "ExchangeSegment": "NSEFO",
        "ExchangeInstrumentId": "48225",
        "ProductType": "NRML",
        "Quantity": "-75",
    });
    if let (Some(p), Some(o)) = (p.as_object_mut(), overrides.as_object()) {
        for (k, v) in o {
            p.insert(k.clone(), v.clone());
        }
    }
    p
}

async fn open_position(book: Value) -> openalgo_desktop_lib::error::Result<i64> {
    let (b, fake, _) = broker().await;
    *fake.positions.lock() = book;
    b.get_open_position(
        &token(),
        "NIFTY27OCT2622400CE",
        Exchange::Nfo,
        Product::Nrml,
    )
    .await
}

fn positions(rows: Vec<Value>) -> Value {
    json!({"type": "success", "result": {"positionList": rows}})
}

/// Web `test_open_position_matches_the_instrument_not_the_name` and
/// `test_open_position_accepts_the_docs_spelling_of_the_instrument_id`.
#[tokio::test]
async fn open_position_matches_the_instrument_not_the_name() {
    assert_eq!(
        open_position(positions(vec![position(json!({}))]))
            .await
            .unwrap(),
        -75
    );
    let mut docs = position(json!({}));
    let id = docs["ExchangeInstrumentId"].take();
    docs.as_object_mut().unwrap().remove("ExchangeInstrumentId");
    docs["ExchangeInstrumentID"] = id;
    assert_eq!(open_position(positions(vec![docs])).await.unwrap(), -75);
}

/// Web `test_open_position_ignores_other_rows`.
#[tokio::test]
async fn open_position_ignores_other_rows() {
    for other in [
        json!({"ProductType": "MIS"}),
        json!({"ExchangeSegment": "BSEFO"}),
        json!({"ExchangeInstrumentId": "99999"}),
    ] {
        let book = positions(vec![position(other.clone())]);
        assert_eq!(open_position(book).await.unwrap(), 0, "{other}");
    }
}

/// Web `test_open_position_reads_zero_from_an_error_answer`: the web reads
/// 0 there and refuses the smart order on the failed read; the desktop's
/// read refuses it itself (a book that cannot be read is never flat).
#[tokio::test]
async fn open_position_refuses_an_error_answer() {
    let read = open_position(json!({"type": "error", "description": "x"})).await;
    assert!(
        read.is_err(),
        "an error answer is not a flat position: {read:?}"
    );
}

// ---------------------------------------------------------------------------
// Trade book: each row is one fill
// ---------------------------------------------------------------------------

async fn trades(rows: Vec<Value>) -> Vec<Trade> {
    let (b, fake, _) = broker().await;
    *fake.trades.lock() = json!({"type": "success", "result": rows});
    b.get_trade_book(&token()).await.unwrap()
}

/// Web `test_trade_book_row_is_one_fill`.
#[tokio::test]
async fn trade_book_row_is_one_fill() {
    let fill = json!({
        "TradingSymbol": "SBIN",
        "ExchangeSegment": "NSECM",
        "OrderSide": "BUY",
        "ProductType": "MIS",
        "AppOrderID": "1100126316",
        "OrderQuantity": 100,
        "OrderAverageTradedPrice": 801.0,
        "LastTradedQuantity": 40,
        "LastTradedPrice": 800.5,
        "LastExecutionTransactTime": "17-11-2026 10:00:05",
        "OrderGeneratedDateTime": "17-11-2026 10:00:00",
    });
    let t = trades(vec![fill]).await;
    assert_eq!(t[0].quantity, 40);
    assert_eq!(t[0].average_price, 800.5);
    assert_eq!(t[0].trade_value, 40.0 * 800.5);
    assert_eq!(t[0].timestamp, "17-11-2026 10:00:05");
}

/// Web `test_trade_book_row_without_fill_fields_keeps_order_fields`.
#[tokio::test]
async fn trade_book_row_without_fill_fields_keeps_order_fields() {
    let order = json!({
        "ExchangeSegment": "NSECM",
        "AppOrderID": "1",
        "OrderQuantity": 10,
        "OrderAverageTradedPrice": 5.0,
        "OrderGeneratedDateTime": "t",
    });
    let t = trades(vec![order]).await;
    assert_eq!(
        (t[0].quantity, t[0].average_price, t[0].timestamp.as_str()),
        (10, 5.0, "t")
    );
}

// ---------------------------------------------------------------------------
// Order status
// ---------------------------------------------------------------------------

/// Web `test_order_status_maps_every_documented_state` and
/// `test_order_statistics_count_part_filled_and_modified_orders_as_open`
/// (the desktop's statistics count the book's `open` rows).
#[tokio::test]
async fn order_status_maps_every_documented_state() {
    let (b, fake, _) = broker().await;
    let cases = [
        ("New", "open"),
        ("PartiallyFilled", "open"),
        ("Replaced", "open"),
        ("PendingNew", "open"),
        ("PendingReplace", "open"),
        ("PendingCancel", "open"),
        ("Filled", "complete"),
        ("Rejected", "rejected"),
        ("Cancelled", "cancelled"),
    ];
    let rows: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (status, _))| json!({"OrderStatus": status, "AppOrderID": i + 1}))
        .collect();
    *fake.orders.lock() = json!({"type": "success", "result": rows});
    let book = b.get_order_book(&token()).await.unwrap();
    for ((status, expected), order) in cases.iter().zip(&book) {
        assert_eq!(order.status, *expected, "{status}");
    }
    let open = book.iter().filter(|o| o.status == "open").count();
    assert_eq!(open, 6);
}

/// Web `test_cancel_all_cancels_modified_and_part_filled_orders`.
#[tokio::test]
async fn cancel_all_cancels_modified_and_part_filled_orders() {
    let (b, fake, _) = broker().await;
    *fake.orders.lock() = json!({"type": "success", "result": [
        {"AppOrderID": 1, "OrderStatus": "New"},
        {"AppOrderID": 2, "OrderStatus": "Replaced"},
        {"AppOrderID": 3, "OrderStatus": "PartiallyFilled"},
        {"AppOrderID": 4, "OrderStatus": "PendingCancel"},
        {"AppOrderID": 5, "OrderStatus": "Filled"},
    ]});
    *fake.answer.lock() = json!({"type": "success", "result": {}});
    let r = b.cancel_all_orders(&token()).await.unwrap();
    assert_eq!(r.cancelled, ["1", "2", "3"]);
    assert!(r.failed.is_empty());
}

// ---------------------------------------------------------------------------
// Cancel and modify read XTS's {"type": ...} answer
// ---------------------------------------------------------------------------

/// Web `test_cancel_reads_the_xts_success_answer`.
#[tokio::test]
async fn cancel_reads_the_xts_success_answer() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() = json!({"type": "success", "result": [{"AppOrderID": 7}]});
    let r = b.cancel_order(&token(), "7").await.unwrap();
    assert_eq!(r.order_id, "7");
    assert_eq!(fake.lines("cancel|"), ["cancel|appOrderID=7"]);
}

/// Web `test_cancel_reports_the_xts_reason`.
#[tokio::test]
async fn cancel_reports_the_xts_reason() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() = json!({"type": "error", "description": "Order is already cancelled"});
    let e = b.cancel_order(&token(), "7").await.unwrap_err();
    assert_eq!(e.client_message(), "Order is already cancelled");
}

/// Web `test_modify_reads_the_xts_success_answer`: a 20-digit order id
/// comes back digit for digit.
#[tokio::test]
async fn modify_reads_the_xts_success_answer() {
    let (b, fake, _) = broker().await;
    *fake.answer.lock() =
        json!({"type": "success", "result": {"AppOrderID": 10001253450000041478u64}});
    let m = ResolvedModify::resolve(
        "10001253450000041478",
        &ModifyOrderRequest {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "LIMIT".into(),
            quantity: 1,
            price: 2500.0,
            trigger_price: 0.0,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    let r = b.modify_order(&token(), &m).await.unwrap();
    assert_eq!(r.order_id, "10001253450000041478");
}

// ---------------------------------------------------------------------------
// Funds: the ALL|ALL|ALL entry
// ---------------------------------------------------------------------------

fn balance_entry(header: &str, cash: f64) -> Value {
    json!({
        "limitHeader": header,
        "limitObject": {"RMSSubLimits": {
            "netMarginAvailable": cash,
            "collateral": 0,
            "UnrealizedMTM": 0,
            "RealizedMTM": 0,
            "marginUtilized": 0,
        }},
    })
}

/// Web `test_funds_use_the_consolidated_balance_entry`, and a `BalanceList`
/// sent as one entry (web funds.py).
#[tokio::test]
async fn funds_use_the_consolidated_balance_entry() {
    let (b, fake, _) = broker().await;
    *fake.balance.lock() = json!({"type": "success", "result": {"BalanceList": [
        balance_entry("CASH|NSE|MTF", 0.0),
        balance_entry("ALL|ALL|ALL", 12500.5),
    ]}});
    assert_eq!(b.get_funds(&token()).await.unwrap().available_cash, 12500.5);
    *fake.balance.lock() = json!({"type": "success",
        "result": {"BalanceList": balance_entry("ALL|ALL|ALL", 300.25)}});
    assert_eq!(b.get_funds(&token()).await.unwrap().available_cash, 300.25);
}

// ---------------------------------------------------------------------------
// Master contract: futures and spreads in their own layout
// ---------------------------------------------------------------------------

/// Web `test_futures_rows_are_read_in_their_own_layout`: a future is FUT at
/// strike 0 (it was a put at strike 1 before, typed from the
/// PriceNumerator column), the spread is not a contract.
#[tokio::test]
async fn futures_rows_are_read_in_their_own_layout() {
    let (b, _, _) = broker().await;
    let rows = b.download_master_contract(&token()).await.unwrap();
    let nfo: HashMap<&str, &SymToken> = rows
        .iter()
        .filter(|r| r.exchange == "NFO")
        .map(|r| (r.token.as_str(), r))
        .collect();
    let mut tokens: Vec<&str> = nfo.keys().copied().collect();
    tokens.sort_unstable();
    assert_eq!(tokens, ["48225", "49229"], "the spread is not a contract");
    let future = nfo["49229"];
    assert_eq!(
        (
            future.symbol.as_str(),
            future.instrument_type.as_str(),
            future.strike
        ),
        ("NIFTY27JAN26FUT", "FUT", 0.0)
    );
    assert_eq!(future.lot_size, 65);
    let option = nfo["48225"];
    assert_eq!(
        (
            option.symbol.as_str(),
            option.instrument_type.as_str(),
            option.strike
        ),
        ("NIFTY17FEB2625700PE", "PE", 25700.0)
    );
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn history(interval: &str, start: (i32, u32, u32), end: (i32, u32, u32)) -> HistoryRequest {
    let day = |(y, m, d): (i32, u32, u32)| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: interval.into(),
        start: day(start),
        end: day(end),
    }
}

/// Web 19f993c92 (#2490): a year per call, whole days, the next window one
/// second after the last; no day between two windows is lost.
#[tokio::test]
async fn history_is_asked_a_year_at_a_time() {
    let (b, fake, _) = broker().await;
    let t = token();
    b.get_history(&t, &history("1m", (2024, 1, 1), (2025, 2, 4)))
        .await
        .unwrap();
    let windows: Vec<String> = fake
        .lines("ohlc|")
        .iter()
        .map(|l| l.splitn(3, '|').nth(2).unwrap_or("").to_string())
        .collect();
    assert_eq!(
        windows,
        [
            "Jan 01 2024 000000|Dec 30 2024 235959|60",
            "Dec 31 2024 000000|Feb 04 2025 235959|60",
        ]
    );
}

/// Web #2504: XTS's smallest OHLC compression is 60, so 5paisa XTS offers no
/// `1s` and refuses it.
#[tokio::test]
async fn there_is_no_one_second_interval() {
    let (b, fake, _) = broker().await;
    assert!(b.timeframe_map().iter().all(|(k, _)| *k != "1s"));
    assert_eq!(b.timeframe_map().first(), Some(&("1m", "60")));
    let e = b
        .get_history(&token(), &history("1s", (2024, 1, 1), (2024, 1, 2)))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
    assert!(fake.lines("ohlc|").is_empty());
}

/// Web `test_history_logs_in_again_once_on_an_invalid_token` (#2484).
#[tokio::test]
async fn history_logs_in_again_once_on_an_invalid_token() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    let old = md_token(1);
    fake.expired.lock().push(old.clone());
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        ..history("D", (2025, 1, 20), (2025, 1, 20))
    };
    let candles = b.get_history(&token(), &req).await.unwrap();
    assert_eq!(candles.len(), 1);
    let used: Vec<String> = fake
        .lines("ohlc|")
        .iter()
        .map(|l| l.split('|').nth(1).unwrap_or("").to_string())
        .collect();
    assert_eq!(used, [old, md_token(2)]);
}

// ---------------------------------------------------------------------------
// One market-data session for feed, quotes and history (#2484)
// ---------------------------------------------------------------------------

/// A fake market-data socket: records each connection's path and query,
/// opens Engine.IO, and accepts (or refuses) the Socket.IO connect.
async fn socket(log: Arc<Mutex<Vec<String>>>, refuse: bool) -> String {
    use tokio_tungstenite::tungstenite::Message as Frame;
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let seen = log.clone();
                let cb = handshake::check(move |req: &Request, resp: Response| {
                    seen.lock().push(format!(
                        "ws|{}|{}",
                        req.uri().path(),
                        req.uri().query().unwrap_or("")
                    ));
                    Ok(resp)
                });
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let open = "0{\"sid\":\"s1\",\"upgrades\":[],\"pingInterval\":25000,\"pingTimeout\":20000,\"maxPayload\":1000000}";
                let _ = ws.send(Frame::Text(open.into())).await;
                loop {
                    match ws.next().await {
                        Some(Ok(Frame::Text(t))) if t == "40" => break,
                        Some(Ok(_)) => continue,
                        _ => return,
                    }
                }
                let answer = if refuse {
                    "44{\"message\":\"Invalid Token\"}"
                } else {
                    "40{\"sid\":\"n1\"}"
                };
                let _ = ws.send(Frame::Text(answer.into())).await;
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    url
}

fn manager() -> WebSocketManager {
    WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(10),
        backoff_max: Duration::from_millis(50),
        ..FeedConfig::default()
    })
}

fn connections(log: &Mutex<Vec<String>>) -> Vec<String> {
    log.lock()
        .iter()
        .filter(|l| l.starts_with("ws|"))
        .cloned()
        .collect()
}

/// Web `test_feed_connects_with_the_stored_token_and_never_logs_in` and
/// `test_market_data_user_id_is_the_claim_before_the_underscore`: the
/// address carries the token stored at sign-in and the user id its claim
/// names; the only market-data login is the sign-in's.
#[tokio::test]
async fn feed_connects_with_the_stored_token_and_never_logs_in() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    let signed_in = b.authenticate(creds()).await.unwrap();
    let stored = signed_in.feed_token.clone().unwrap();
    let auth = AuthToken::new(signed_in.auth_token.as_str())
        .with_feed(Some(stored.as_str()))
        .with_user_id(signed_in.user_id.as_str());
    let m = manager();
    m.connect(b.create_feed(&auth).unwrap()).await.unwrap();
    until("the socket connect", || !connections(&log).is_empty()).await;
    let first = &connections(&log)[0];
    assert!(
        first.starts_with("ws|/apimarketdata/socket.io/|"),
        "{first}"
    );
    assert!(first.contains(&format!("token={stored}&")), "{first}");
    assert!(first.contains("userID=1048131&"), "{first}");
    assert_eq!(fake.lines("login|"), ["login|/apimarketdata/auth/login"]);
    m.disconnect().await.unwrap();
}

/// Web `test_reconnect_reads_the_stored_token_again`: once quotes renewed
/// the market-data session, the feed's next connect uses it, and logs in
/// for nothing of its own.
#[tokio::test]
async fn reconnect_takes_the_session_quotes_renewed() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    m.connect(b.create_feed(&token()).unwrap()).await.unwrap();
    until("the first connect", || connections(&log).len() == 1).await;
    assert!(connections(&log)[0].contains(&format!("token={}&", md_token(1))));

    fake.expired.lock().push(md_token(1));
    b.get_quote(&token(), &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    m.reconnect();
    until("the second connect", || connections(&log).len() >= 2).await;
    assert!(
        connections(&log)[1].contains(&format!("token={}&", md_token(2))),
        "{:?}",
        connections(&log)
    );
    assert_eq!(fake.logins.load(Ordering::SeqCst), 2, "sign-in and renewal");
    m.disconnect().await.unwrap();
}

/// Web `test_without_a_stored_token_the_feed_logs_in_where_history_does`.
#[tokio::test]
async fn without_a_stored_token_the_feed_logs_in_where_history_does() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), false).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    m.connect(b.create_feed(&AuthToken::new("itok")).unwrap())
        .await
        .unwrap();
    until("the socket connect", || !connections(&log).is_empty()).await;
    assert_eq!(
        fake.lines("login|"),
        [
            "login|/apimarketdata/auth/login",
            "login|/apimarketdata/auth/login"
        ]
    );
    assert!(connections(&log)[0].contains(&format!("token={}&", md_token(2))));
    m.disconnect().await.unwrap();
}

/// Web `test_a_failed_connect_with_a_stored_token_raises_instead_of_logging_in`:
/// a refused stored session logs in for nothing. The desktop then holds
/// the feed until the next sign-in, as it does every refused feed, where
/// the web retries the same token.
#[tokio::test]
async fn a_refused_stored_session_is_not_followed_by_a_login() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sock = socket(log.clone(), true).await;
    let (b, fake, _) = broker().await;
    let b = b.with_socket_base(sock.as_str());
    b.authenticate(creds()).await.unwrap();
    let m = manager();
    let status = m.watch_status();
    m.connect(b.create_feed(&token()).unwrap()).await.unwrap();
    until("the refusal", || {
        format!("{:?}", *status.borrow()).contains("AuthFailed")
    })
    .await;
    assert_eq!(fake.lines("login|"), ["login|/apimarketdata/auth/login"]);
    m.disconnect().await.unwrap();
}

/// The renewed session is a cache under the app's rules: a renewal still
/// logging in when the trader signs out answers its own call but is not
/// kept, so nothing after the sign-out uses it (CLAUDE.md, TtlCache).
#[tokio::test]
async fn a_renewal_across_a_sign_out_is_not_kept() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    let old = md_token(1);
    fake.expired.lock().push(old.clone());
    let gate = Arc::new(Notify::new());
    *fake.hold_login.lock() = Some(gate.clone());
    let key = QuoteKey::new("NSE", "RELIANCE");
    let t = token();
    let (quote, ()) = tokio::join!(b.get_quote(&t, &key), async {
        until("the renewal's login", || fake.lines("login|").len() == 2).await;
        b.on_logout().await;
        gate.notify_one();
    });
    assert_eq!(quote.unwrap().ltp, 2500.25, "its own call is answered");
    *fake.hold_login.lock() = None;
    fake.expired.lock().clear();
    b.get_quote(&t, &key).await.unwrap();
    let last = fake.lines("quotes|").pop().unwrap_or_default();
    assert_eq!(last, format!("quotes|{old}"), "the renewal was not kept");
}

/// A sign-out forgets a renewed session, and the next session starts from
/// its own stored token.
#[tokio::test]
async fn a_sign_out_forgets_the_renewed_session() {
    let (b, fake, _) = broker().await;
    b.authenticate(creds()).await.unwrap();
    fake.expired.lock().push(md_token(1));
    let key = QuoteKey::new("NSE", "RELIANCE");
    b.get_quote(&token(), &key).await.unwrap();
    b.on_logout().await;
    b.restore_session(&creds());
    fake.expired.lock().clear();
    b.get_quote(&token(), &key).await.unwrap();
    let last = fake.lines("quotes|").pop().unwrap_or_default();
    assert_eq!(last, format!("quotes|{}", md_token(1)));
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

fn sub(token: &str) -> FeedSubscription {
    FeedSubscription {
        symbol: format!("S{token}"),
        exchange: "NSE".into(),
        token: token.into(),
        brsymbol: format!("S{token}"),
        brexchange: "NSECM".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

/// Web `test_streaming_quote_reads_touchline_for_1501`.
#[test]
fn streaming_quote_reads_touchline_for_1501() {
    let message = json!({
        "MessageCode": 1501,
        "Touchline": {"LastTradedPrice": 1723.0, "Open": 1696.8, "TotalTradedQuantity": 189238},
    });
    let events = normalise_message(&message, &sub("2885"));
    let Some(FeedEvent::Tick(t)) = events.first() else {
        panic!("no tick: {events:?}");
    };
    assert_eq!((t.ltp, t.open, t.volume), (1723.0, 1696.8, 189238));
}

fn ticks_for(feed: &mut XtsFeed, packet: &str) -> Vec<String> {
    let frame = format!("42[\"1105-json-partial\",\"{packet}\"]");
    feed.parse(&Message::Text(frame))
        .into_iter()
        .filter_map(|e| match e {
            FeedEvent::Tick(t) => Some(t.symbol),
            _ => None,
        })
        .collect()
}

/// Web `test_replaying_the_book_on_every_reconnect_does_not_grow_it`,
/// `test_unsubscribe_removes_the_instrument_from_the_book` and
/// `test_1105_ticks_pass_only_for_confirmed_instruments`: the feed keeps one
/// entry per instrument however often it is subscribed again, one
/// unsubscribe removes it, and a 1105 tick passes only for an instrument
/// it holds.
#[tokio::test]
async fn the_book_keeps_one_entry_per_instrument() {
    // A closed loopback port: the REST subscriptions fail and are retried
    // in the feed's own task, which goes with the feed.
    let mut feed = XtsFeed::new(
        &CONFIG,
        openalgo_desktop_lib::brokers::common::http::client(),
        "http://127.0.0.1:1".into(),
        None,
        FeedSource::stored("tok", "U1"),
    );
    for _ in 0..20 {
        feed.subscribe_frames(&[sub("1140025"), sub("2885")]);
    }
    feed.unsubscribe_frames(&[sub("2885")]);
    assert_eq!(
        ticks_for(&mut feed, "t:1_1140025,110:2067.75"),
        ["S1140025"]
    );
    assert!(ticks_for(&mut feed, "t:1_2885,110:2501.5").is_empty());
    assert!(ticks_for(&mut feed, "t:1_999,110:1.0").is_empty());
}

// ---------------------------------------------------------------------------
// Subscriptions: a refused batch goes again one instrument at a time
// ---------------------------------------------------------------------------

const DUPLICATE: &str =
    "{\"type\":\"error\",\"code\":\"e-session-0002\",\"description\":\"Instrument Already Subscribed !\"}";
const REFUSED: &str =
    "{\"type\":\"error\",\"code\":\"e-x\",\"description\":\"Invalid instrument\"}";

/// A subscription endpoint that answers a request of several instruments
/// with `batch`, and one instrument by `single(id)`; it records each
/// request's instrument ids.
async fn endpoint(
    batch: (u16, &'static str),
    single: fn(i64) -> (u16, &'static str),
) -> (String, Arc<Mutex<Vec<Vec<i64>>>>) {
    let seen: Arc<Mutex<Vec<Vec<i64>>>> = Arc::default();
    let s2 = seen.clone();
    let app = Router::new().route(
        "/sub",
        post(move |Json(b): Json<Value>| {
            let seen = s2.clone();
            async move {
                let ids: Vec<i64> = b["instruments"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|i| i["exchangeInstrumentID"].as_i64())
                            .collect()
                    })
                    .unwrap_or_default();
                seen.lock().push(ids.clone());
                let ok = "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}";
                let (status, body) = match ids.as_slice() {
                    [one] => single(*one),
                    [] => (200, ok),
                    _ => batch,
                };
                (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body)
            }
        }),
    );
    let url = serve(app).await;
    (format!("{url}/sub"), seen)
}

fn subscribe(ids: &[i64]) -> Command {
    Command {
        subscribe: true,
        code: 1501,
        instruments: ids
            .iter()
            .map(|i| json!({"exchangeSegment": 1, "exchangeInstrumentID": i}))
            .collect(),
        waited: None,
        then_release: None,
    }
}

async fn run(url: &str, cmd: &Command) -> CommandReport {
    let http = openalgo_desktop_lib::brokers::common::http::client();
    let target = SubscriptionTarget {
        broker: CONFIG.id,
        http: &http,
        url,
        token: "tok",
        gap: Duration::ZERO,
        split: CONFIG.hooks.split_refused_batch,
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    run_command(&target, cmd, &tx, &mut true).await
}

fn refused_ids(report: &CommandReport) -> Vec<i64> {
    report
        .refused
        .iter()
        .filter_map(|(i, _)| i["exchangeInstrumentID"].as_i64())
        .collect()
}

/// Web `test_a_refused_batch_is_sent_one_at_a_time`: one instrument the
/// server will not take no longer costs the others; "already subscribed"
/// counts as subscribed.
#[tokio::test]
async fn a_refused_batch_is_sent_one_at_a_time() {
    let (url, seen) = endpoint((400, DUPLICATE), |id| match id {
        1 => (200, "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}"),
        2 => (400, DUPLICATE),
        _ => (400, REFUSED),
    })
    .await;
    let report = run(&url, &subscribe(&[1, 2, 3])).await;
    assert_eq!(*seen.lock(), [vec![1, 2, 3], vec![1], vec![2], vec![3]]);
    assert_eq!(refused_ids(&report), [3]);

    // Refused for any other reason: the same.
    let (url, seen) = endpoint((400, REFUSED), |id| match id {
        2 => (400, REFUSED),
        _ => (200, "{\"type\":\"success\",\"result\":{\"listQuotes\":[]}}"),
    })
    .await;
    let report = run(&url, &subscribe(&[1, 2, 3])).await;
    assert_eq!(*seen.lock(), [vec![1, 2, 3], vec![1], vec![2], vec![3]]);
    assert_eq!(refused_ids(&report), [2]);
}

/// Web `test_a_single_already_subscribed_instrument_counts_as_subscribed`.
#[tokio::test]
async fn a_single_already_subscribed_instrument_counts_as_subscribed() {
    let (url, seen) = endpoint((400, DUPLICATE), |_| (400, DUPLICATE)).await;
    let report = run(&url, &subscribe(&[7])).await;
    assert_eq!(*seen.lock(), [vec![7]]);
    assert!(report.refused.is_empty() && report.retry.is_empty());
}

/// Web `test_a_network_error_reaches_the_adapter_so_it_queues_the_batch_again`:
/// a call with no answer goes back to be sent again (BF-03), never taken
/// for subscribed.
#[tokio::test]
async fn a_failed_call_is_sent_again() {
    let report = run("http://127.0.0.1:1/sub", &subscribe(&[1])).await;
    assert_eq!(report.retry.len(), 1);
    assert!(report.refused.is_empty());
}
