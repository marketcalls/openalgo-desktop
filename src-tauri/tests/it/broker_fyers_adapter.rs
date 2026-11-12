//! Fyers adapter end to end against a local fake Fyers (ephemeral port):
//! auth, orders, books, funds, quotes, history, margin, GTT, master
//! contract, 429 retry. Every request the adapter sends is recorded and
//! checked against the web's wire format (`broker/fyers/api/*.py`).

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::Engine;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::fyers::master_contract::parse_all;
use openalgo_desktop_lib::brokers::fyers::{FyersBroker, FyersUrls};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

macro_rules! fx {
    ($name:literal) => {
        include_str!(concat!("../fixtures/brokers/fyers/", $name))
    };
}

fn master() -> SymbolResolver {
    let mut files: HashMap<&str, String> = HashMap::new();
    files.insert("NSE_CM", fx!("NSE_CM.csv").into());
    files.insert("BSE_CM", fx!("BSE_CM.csv").into());
    files.insert("NSE_FO", fx!("NSE_FO.csv").into());
    files.insert("BSE_FO", fx!("BSE_FO.csv").into());
    files.insert("NSE_CD", fx!("NSE_CD_sym_master.json").into());
    files.insert("MCX_COM", fx!("MCX_COM_sym_master.json").into());
    let names: HashMap<String, String> =
        serde_json::from_str(fx!("index_hsm_mapping.json")).unwrap();
    let r = SymbolResolver::new();
    r.load(parse_all(&files, &names).unwrap());
    r
}

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    query: String,
    auth: Option<String>,
    body: Value,
}

type Resp = (u16, Vec<(&'static str, String)>, String);

#[derive(Clone, Default)]
struct Fake {
    log: Arc<Mutex<Vec<Req>>>,
    queued: Arc<Mutex<HashMap<String, VecDeque<Resp>>>>,
    defaults: Arc<Mutex<HashMap<String, Resp>>>,
}

impl Fake {
    fn on(&self, method: &str, path: &str, body: &str) -> &Self {
        self.defaults.lock().insert(
            format!("{} {}", method, path),
            (200, Vec::new(), body.to_string()),
        );
        self
    }

    fn once(&self, method: &str, path: &str, resp: Resp) -> &Self {
        self.queued
            .lock()
            .entry(format!("{} {}", method, path))
            .or_default()
            .push_back(resp);
        self
    }

    fn requests(&self, method: &str, path: &str) -> Vec<Req> {
        self.log
            .lock()
            .iter()
            .filter(|r| r.method == method && r.path == path)
            .cloned()
            .collect()
    }

    async fn serve(&self) -> String {
        let me = self.clone();
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let me = me.clone();
                async move { me.handle(method, uri, headers, body) }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{}", addr)
    }

    fn handle(&self, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
        let req = Req {
            method: method.to_string(),
            path: uri.path().to_string(),
            query: uri.query().unwrap_or("").to_string(),
            auth: headers
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_string()),
            body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        };
        let key = format!("{} {}", req.method, req.path);
        self.log.lock().push(req);
        let resp = self
            .queued
            .lock()
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
            .or_else(|| self.defaults.lock().get(&key).cloned());
        match resp {
            Some((status, hdrs, body)) => {
                let mut r = (StatusCode::from_u16(status).unwrap(), body).into_response();
                r.headers_mut()
                    .insert("content-type", "application/json".parse().unwrap());
                for (k, v) in hdrs {
                    r.headers_mut().insert(k, v.parse().unwrap());
                }
                r
            }
            None => (StatusCode::NOT_FOUND, "{}").into_response(),
        }
    }
}

fn broker(base: &str) -> FyersBroker {
    FyersBroker::with_urls(
        master(),
        FyersUrls {
            api: base.to_string(),
            public: format!("{}/sym_details", base),
            ..FyersUrls::default()
        },
    )
    .with_retry_base(Duration::from_millis(1))
}

fn jwt() -> String {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(br#"{"hsm_key":"hsmkey123","exp":4102444800,"fy_id":"<USER_ID>"}"#);
    format!("eyJhbGciOiJIUzI1NiJ9.{}.c2ln", payload)
}

fn auth() -> AuthToken {
    AuthToken::new("APPID-100:access-token")
}

fn order(symbol: &str, exchange: &str, pricetype: &str) -> ResolvedOrder {
    ResolvedOrder::resolve(
        &OrderRequest {
            symbol: symbol.into(),
            exchange: exchange.into(),
            side: "BUY".into(),
            quantity: 10,
            price: 950.0,
            order_type: pricetype.into(),
            product: "MIS".into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        },
        &master(),
    )
    .unwrap()
}

#[tokio::test]
async fn authenticate_exchanges_code_for_app_id_pair() {
    let fake = Fake::default();
    let ok = fx!("auth_ok.json").replace("<ACCESS_TOKEN>", &jwt());
    fake.once("POST", "/api/v3/validate-authcode", (200, vec![], ok));
    fake.once(
        "POST",
        "/api/v3/validate-authcode",
        (
            400,
            vec![],
            json!({"s": "error", "code": -413, "message": "Invalid auth code"}).to_string(),
        ),
    );
    let b = broker(&fake.serve().await);
    let creds = BrokerCredentials {
        api_key: "APPID-100".into(),
        api_secret: Some("secret".into()),
        auth_code: Some("code-123".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, format!("APPID-100:{}", jwt()));
    assert_eq!(r.user_id, "<USER_ID>");
    let sent = &fake.requests("POST", "/api/v3/validate-authcode")[0];
    assert_eq!(sent.body["grant_type"], "authorization_code");
    assert_eq!(sent.body["code"], "code-123");
    assert_eq!(
        sent.body["appIdHash"],
        openalgo_desktop_lib::brokers::fyers::app_id_hash("APPID-100", "secret")
    );
    let e = b.authenticate(creds).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("Invalid auth code"));
    let e = b
        .authenticate(BrokerCredentials {
            api_key: "APPID-100".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn place_modify_cancel_send_web_bodies() {
    let fake = Fake::default();
    fake.once(
        "POST",
        "/api/v3/orders/sync",
        (
            200,
            vec![],
            json!({"s": "ok", "code": 1101, "message": "Order submitted", "id": "26100300000009"})
                .to_string(),
        ),
    );
    fake.once(
        "POST",
        "/api/v3/orders/sync",
        (
            400,
            vec![],
            json!(JsonFix::errors()["order_rejected"]).to_string(),
        ),
    );
    fake.on(
        "PATCH",
        "/api/v3/orders/sync",
        &json!({"s": "OK", "code": 1102, "id": "26100300000009"}).to_string(),
    );
    fake.on(
        "DELETE",
        "/api/v3/orders/sync",
        &json!({"s": "ok", "code": 1103, "id": "26100300000009"}).to_string(),
    );
    let b = broker(&fake.serve().await);
    let r = b
        .place_order(&auth(), &order("SBIN", "NSE", "LIMIT"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "26100300000009");
    let sent = &fake.requests("POST", "/api/v3/orders/sync")[0];
    assert_eq!(sent.auth.as_deref(), Some("APPID-100:access-token"));
    assert_eq!(
        sent.body,
        json!({"symbol": "NSE:SBIN-EQ", "qty": 10, "type": 1, "side": 1, "productType": "INTRADAY",
               "limitPrice": 950.0, "stopPrice": 0.0, "validity": "DAY", "disclosedQty": 0,
               "offlineOrder": false, "stopLoss": 0, "takeProfit": 0, "orderTag": "openalgo"})
    );
    let e = b
        .place_order(&auth(), &order("SBIN", "NSE", "MARKET"))
        .await
        .unwrap_err();
    assert!(e.client_message().starts_with("Insufficient fund"));

    let m = ResolvedModify::resolve(
        "26100300000009",
        &ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "SL".into(),
            quantity: 5,
            price: 951.0,
            trigger_price: 950.5,
            disclosed_quantity: 0,
        },
        &master(),
    )
    .unwrap();
    assert_eq!(
        b.modify_order(&auth(), &m).await.unwrap().order_id,
        "26100300000009"
    );
    assert_eq!(
        fake.requests("PATCH", "/api/v3/orders/sync")[0].body,
        json!({"id": "26100300000009", "qty": 5, "type": 4, "limitPrice": 951.0, "stopPrice": 950.5})
    );
    b.cancel_order(&auth(), "26100300000009").await.unwrap();
    assert_eq!(
        fake.requests("DELETE", "/api/v3/orders/sync")[0].body,
        json!({"id": "26100300000009"})
    );
    // A malformed stored token never reaches the network.
    let e = b
        .get_order_book(&AuthToken::new("garbage"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
}

struct JsonFix;
impl JsonFix {
    fn errors() -> Value {
        serde_json::from_str(fx!("errors.json")).unwrap()
    }
}

#[tokio::test]
async fn books_cancel_all_close_all_and_open_position() {
    let fake = Fake::default();
    fake.on("GET", "/api/v3/orders", fx!("orders.json"))
        .on("GET", "/api/v3/tradebook", fx!("trades.json"))
        .on("GET", "/api/v3/positions", fx!("positions.json"))
        .on("GET", "/api/v3/holdings", fx!("holdings.json"))
        .on(
            "DELETE",
            "/api/v3/orders/sync",
            &json!({"s": "ok", "id": "x"}).to_string(),
        )
        .on(
            "DELETE",
            "/api/v3/positions",
            &json!({"s": "ok", "code": 200, "message": "All positions are closed"}).to_string(),
        );
    let b = broker(&fake.serve().await);
    let a = auth();
    let orders = b.get_order_book(&a).await.unwrap();
    assert_eq!(orders[1].symbol, "NIFTY06OCT2622400PE");
    assert_eq!(orders[1].status, "trigger pending");
    assert_eq!(b.get_trade_book(&a).await.unwrap()[0].symbol, "SBIN");
    let p = b.get_positions(&a).await.unwrap();
    assert_eq!(
        (p[1].symbol.as_str(), p[1].exchange.as_str()),
        ("NIFTY06OCT2622400PE", "NFO")
    );
    assert_eq!(b.get_holdings(&a).await.unwrap()[0].product, "CNC");

    // web: cancel statuses 4 and 6 only.
    let c = b.cancel_all_orders(&a).await.unwrap();
    assert_eq!(
        c.cancelled,
        ["26100300000002", "26100300000003", "26100300000006"]
    );
    assert!(c.failed.is_empty());

    // exit-all is one DELETE with {"exit_all": 1}.
    let r = b.close_all_positions(&a).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert_eq!(r.message(), "All Open Positions SquaredOff");
    assert_eq!(
        fake.requests("DELETE", "/api/v3/positions")[0].body,
        json!({"exit_all": 1})
    );

    assert_eq!(
        b.get_open_position(&a, "SBIN", Exchange::Nse, Product::Mis)
            .await
            .unwrap(),
        10
    );
    assert_eq!(
        b.get_open_position(&a, "SBIN", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        b.get_open_position(&a, "NIFTY06OCT2622400PE", Exchange::Nfo, Product::Nrml)
            .await
            .unwrap(),
        -65
    );
}

#[tokio::test]
async fn funds_are_cached_and_errors_are_never_zeros() {
    let fake = Fake::default();
    fake.on("GET", "/api/v3/funds", fx!("funds.json")).on(
        "GET",
        "/api/v3/positions",
        fx!("positions.json"),
    );
    let b = broker(&fake.serve().await);
    let f = b.get_funds(&auth()).await.unwrap();
    assert!((f.available_cash - 209_293.45).abs() < 1e-6);
    assert!((f.m2m_unrealized - 451.41).abs() < 1e-9);
    b.get_funds(&auth()).await.unwrap();
    // Second read within 60 s comes from the cache.
    assert_eq!(fake.requests("GET", "/api/v3/funds").len(), 1);

    let fake = Fake::default();
    fake.on(
        "GET",
        "/api/v3/funds",
        &JsonFix::errors()["funds_unauth"].to_string(),
    );
    let b = broker(&fake.serve().await);
    assert_eq!(b.get_funds(&auth()).await.unwrap_err().code(), "AUTH_ERROR");
}

#[tokio::test]
async fn rate_limited_calls_are_retried_with_the_brokers_delay() {
    let fake = Fake::default();
    fake.once(
        "GET",
        "/api/v3/orders",
        (429, vec![("x-retry-after-ms", "5".into())], "{}".into()),
    )
    .once(
        "GET",
        "/api/v3/orders",
        (429, vec![("retry-after", "0.01".into())], "{}".into()),
    )
    .on("GET", "/api/v3/orders", fx!("orders.json"));
    let b = broker(&fake.serve().await);
    assert_eq!(b.get_order_book(&auth()).await.unwrap().len(), 7);
    assert_eq!(fake.requests("GET", "/api/v3/orders").len(), 3);

    // Four 429s in a row: three retries, then a trader-facing error.
    let fake = Fake::default();
    for _ in 0..4 {
        fake.once("GET", "/api/v3/orders", (429, vec![], "{}".into()));
    }
    let b = broker(&fake.serve().await);
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("limiting requests"));
    assert_eq!(fake.requests("GET", "/api/v3/orders").len(), 4);
}

#[tokio::test]
async fn quotes_depth_and_batched_multiquotes() {
    let fake = Fake::default();
    fake.on("GET", "/data/depth", fx!("depth.json"))
        .on("GET", "/data/quotes", fx!("quotes.json"));
    let b = broker(&fake.serve().await);
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.ask), (954.1, 954.05, 954.1));
    assert_eq!(
        fake.requests("GET", "/data/depth")[0].query,
        "symbol=NSE%3ASBIN-EQ&ohlcv_flag=1"
    );
    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(d.asks.len(), 5);
    assert_eq!(d.total_sell_qty, 689_415);
    let e = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "NOPE"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");

    // <= 100 symbols: OI for derivatives via /data/depth.
    let keys = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NSE", "UNKNOWN"),
        QuoteKey::new("NSE", "RELIANCE"),
    ];
    let depth_before = fake.requests("GET", "/data/depth").len();
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 5);
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 954.1);
    assert_eq!(r[1].symbol, "NIFTY27OCT26FUT");
    assert!(r[1].data.is_some());
    assert_eq!(r[2].data.as_ref().unwrap().ltp, 22512.35);
    assert_eq!(
        r[3].error.as_deref(),
        Some("Could not resolve broker symbol")
    );
    assert_eq!(r[4].error.as_deref(), Some("No quote data available"));
    assert_eq!(
        fake.requests("GET", "/data/depth").len(),
        depth_before + 1,
        "one OI lookup for the one derivative"
    );
    let q = &fake.requests("GET", "/data/quotes")[0].query;
    assert_eq!(
        urlencoding::decode(q).unwrap(),
        "symbols=NSE:SBIN-EQ,NSE:NIFTY26OCTFUT,NSE:NIFTY50-INDEX,NSE:RELIANCE-EQ"
    );

    // 120 symbols: three batches of <= 50, no OI lookups.
    let many: Vec<QuoteKey> = (0..120)
        .map(|i| {
            if i % 2 == 0 {
                QuoteKey::new("NSE", "SBIN")
            } else {
                QuoteKey::new("NFO", "NIFTY27OCT26FUT")
            }
        })
        .collect();
    let depth_before = fake.requests("GET", "/data/depth").len();
    let quotes_before = fake.requests("GET", "/data/quotes").len();
    let r = b.get_multiquotes(&auth(), &many).await.unwrap();
    assert_eq!(r.len(), 120);
    assert!(r.iter().all(|x| x.data.as_ref().map(|d| d.oi) == Some(0)));
    assert_eq!(
        fake.requests("GET", "/data/quotes").len(),
        quotes_before + 3
    );
    assert_eq!(fake.requests("GET", "/data/depth").len(), depth_before);
}

#[tokio::test]
async fn history_is_chunked_with_epoch_dates_and_oi_for_derivatives() {
    let fake = Fake::default();
    fake.on("GET", "/data/history", fx!("history_day.json"));
    let b = broker(&fake.serve().await);
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    // Two 300-day chunks, deduplicated and sorted.
    assert_eq!(c.len(), 2);
    assert!(c[0].timestamp < c[1].timestamp);
    let calls = fake.requests("GET", "/data/history");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].query,
        "symbol=NSE%3ASBIN-EQ&resolution=1D&date_format=1&range_from=2025-01-01&range_to=2025-10-27&cont_flag=1"
    );
    assert!(calls[1]
        .query
        .contains("range_from=2025-10-28&range_to=2025-12-31"));

    let fake = Fake::default();
    fake.on("GET", "/data/history", fx!("history_oi.json"));
    let b = broker(&fake.serve().await);
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT26FUT"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2025, 10, 3).unwrap(),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c[1].oi, 13_031_500);
    let q = &fake.requests("GET", "/data/history")[0].query;
    assert!(q.contains("resolution=1&") && q.ends_with("&oi_flag=1"));

    // A range Fyers has no candles for is an empty answer, sent once.
    let fake = Fake::default();
    fake.once(
        "GET",
        "/data/history",
        (
            200,
            vec![],
            json!({"s": "no_data", "code": 200, "candles": []}).to_string(),
        ),
    );
    let b = broker(&fake.serve().await);
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2025, 10, 3).unwrap(),
    };
    assert!(b.get_history(&auth(), &req).await.unwrap().is_empty());
    assert_eq!(fake.requests("GET", "/data/history").len(), 1);
    let bad = HistoryRequest {
        interval: "W".into(),
        ..req
    };
    assert_eq!(
        b.get_history(&auth(), &bad).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

fn history(interval: &str, start: (i32, u32, u32), end: (i32, u32, u32)) -> HistoryRequest {
    HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: interval.into(),
        start: NaiveDate::from_ymd_opt(start.0, start.1, start.2).unwrap(),
        end: NaiveDate::from_ymd_opt(end.0, end.1, end.2).unwrap(),
    }
}

/// BRL-04: a chunk that keeps failing is retried within the request's one
/// budget (three retries in all, not three per send and three per chunk)
/// and then fails the history: a chart never takes a gap for a quiet
/// market. The web skips the chunk and answers with the rest.
#[tokio::test]
async fn a_history_chunk_that_keeps_failing_fails_the_history() {
    let fake = Fake::default();
    // Two daily chunks: the first reads, the second answers a server
    // error every time.
    fake.once(
        "GET",
        "/data/history",
        (200, vec![], fx!("history_day.json").into()),
    );
    for _ in 0..10 {
        fake.once(
            "GET",
            "/data/history",
            (
                502,
                vec![],
                json!({"s": "error", "code": 502, "message": "Bad gateway"}).to_string(),
            ),
        );
    }
    let b = broker(&fake.serve().await);
    let e = b
        .get_history(&auth(), &history("D", (2025, 1, 1), (2025, 12, 31)))
        .await
        .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Fyers did not return the history for 28 Oct 2025 to 31 Dec 2025 (Bad gateway). Nothing is shown rather than a history with a gap; try again shortly."
    );
    // One send for the first chunk, the second's first try and three
    // retries: five, where the nested retries allowed up to sixteen per
    // chunk.
    assert_eq!(fake.requests("GET", "/data/history").len(), 5);

    // An explicit refusal (an unknown symbol) is not retried at all.
    let fake = Fake::default();
    fake.on(
        "GET",
        "/data/history",
        &json!({"s": "error", "code": -300, "message": "Invalid symbol"}).to_string(),
    );
    let b = broker(&fake.serve().await);
    let e = b
        .get_history(&auth(), &history("5m", (2025, 10, 1), (2025, 10, 3)))
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("(Invalid symbol)"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.requests("GET", "/data/history").len(), 1);
}

/// A 200 the fake answers to the next history chunk.
fn chunk_answer(fake: &Fake, body: Value) {
    fake.once("GET", "/data/history", (200, vec![], body.to_string()));
}

/// Web test/test_fyers_history_chunks.py (web #2422, #2438): each chunk is
/// asked for once. An empty chunk (`no_data`: a weekend, a holiday, dates
/// before listing) is skipped at once and the data around it returned; a
/// refusal, with or without a reason, fails the history after one send,
/// and a refusal after data is never returned as a partial success. The
/// desktop had this from BRL-04; these are the web's cases.
#[tokio::test]
async fn each_history_chunk_is_asked_for_once() {
    let no_data = json!({"s": "no_data", "code": 200, "message": "", "candles": []});
    let bad_symbol = json!({"s": "error", "code": -300, "message": "Invalid symbol"});
    let candle = |ts: i64| json!({"s": "ok", "code": 200, "message": "", "candles": [[ts, 1, 2, 0.5, 1.5, 100]]});
    // Two 60-day chunks of 5-minute data.
    let range = history("5m", (2025, 8, 1), (2025, 11, 8));

    // test_empty_chunk_then_data_returns_the_data
    let fake = Fake::default();
    chunk_answer(&fake, no_data.clone());
    chunk_answer(&fake, candle(1_762_000_000));
    let b = broker(&fake.serve().await);
    let c = b.get_history(&auth(), &range).await.unwrap();
    assert_eq!(
        c.iter().map(|c| c.timestamp).collect::<Vec<_>>(),
        [1_762_000_000]
    );
    assert_eq!(fake.requests("GET", "/data/history").len(), 2);

    // test_failure_after_data_is_not_returned_as_partial_success
    let fake = Fake::default();
    chunk_answer(&fake, candle(1_756_000_000));
    chunk_answer(&fake, bad_symbol);
    let b = broker(&fake.serve().await);
    let e = b.get_history(&auth(), &range).await.unwrap_err();
    assert!(
        e.client_message().contains("(Invalid symbol)"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.requests("GET", "/data/history").len(), 2);

    // test_failed_chunk_raises_once_with_reason, the blank reason
    let fake = Fake::default();
    chunk_answer(&fake, json!({"s": "error", "message": ""}));
    let b = broker(&fake.serve().await);
    let e = b
        .get_history(&auth(), &history("5m", (2025, 11, 7), (2025, 11, 8)))
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("(Fyers refused the request)"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.requests("GET", "/data/history").len(), 1);
}

/// BRL-03: Fyers' Retry-After is honoured in full. A wait the request can
/// hold is slept once and the request repeated; one that outlasts the
/// request's deadline is refused at once, unretried, where the old code cut
/// it to 10 s and sent again early.
#[tokio::test]
async fn a_retry_after_is_honoured_or_refused_never_shortened() {
    let fake = Fake::default();
    fake.once(
        "GET",
        "/api/v3/orders",
        (429, vec![("retry-after", "60".into())], "{}".into()),
    )
    .on("GET", "/api/v3/orders", fx!("orders.json"));
    let b = broker(&fake.serve().await);
    let started = std::time::Instant::now();
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Fyers asked OpenAlgo to slow down for longer than this request can be held. It was not retried. Try again in a minute."
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused at once"
    );
    assert_eq!(fake.requests("GET", "/api/v3/orders").len(), 1);
    // The pause holds the category: the next order-book read is refused
    // before sending rather than sent into the throttle.
    let e = b.get_order_book(&auth()).await.unwrap_err();
    assert!(
        e.client_message().contains("would have had to wait"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.requests("GET", "/api/v3/orders").len(), 1);
    // Market data has its own budget and is not held by it.
    fake.on("GET", "/data/depth", fx!("depth.json"));
    b.get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();

    // A throttle answer in the body (HTTP 200, code 429) is one too.
    let fake = Fake::default();
    fake.once(
        "GET",
        "/api/v3/orders",
        (
            200,
            vec![("x-retry-after-ms", "20".into())],
            json!({"s": "error", "code": 429, "message": "request limit reached"}).to_string(),
        ),
    )
    .on("GET", "/api/v3/orders", fx!("orders.json"));
    let b = broker(&fake.serve().await);
    assert_eq!(b.get_order_book(&auth()).await.unwrap().len(), 7);
    assert_eq!(fake.requests("GET", "/api/v3/orders").len(), 2);
}

/// BRL-01: each API category has its own budget with a daily cap. Once
/// today's market-data cap is used, data requests are refused before
/// anything is sent, and orders still go.
#[tokio::test]
async fn the_daily_data_cap_refuses_before_sending_and_spares_orders() {
    use openalgo_desktop_lib::brokers::common::ratelimit::{Budget, Limits, Rolling};
    use openalgo_desktop_lib::brokers::fyers::ratelimit::{app_ceiling, FyersLimiter};
    let fake = Fake::default();
    fake.on("GET", "/data/depth", fx!("depth.json")).on(
        "GET",
        "/api/v3/orders",
        fx!("orders.json"),
    );
    let roomy = || {
        Budget::new(Limits {
            windows: vec![Rolling::per_second(100)],
            per_day: None,
        })
    };
    let data = Budget::new(Limits {
        windows: vec![Rolling::per_second(100)],
        per_day: Some(2),
    });
    let b = broker(&fake.serve().await).with_limiter(FyersLimiter::with_limits(
        data,
        roomy(),
        roomy(),
        Budget::new(app_ceiling()),
    ));
    let key = QuoteKey::new("NSE", "SBIN");
    b.get_quote(&auth(), &key).await.unwrap();
    b.get_quote(&auth(), &key).await.unwrap();
    let e = b.get_quote(&auth(), &key).await.unwrap_err();
    assert_eq!(
        e.client_message(),
        "Fyers' daily limit of 2 market data requests on the Standard plan is used up for today, so OpenAlgo has stopped sending them. It resets at midnight."
    );
    assert_eq!(fake.requests("GET", "/data/depth").len(), 2);
    assert_eq!(b.get_order_book(&auth()).await.unwrap().len(), 7);
}

/// A throttled order placement is repeated only after Fyers' explicit 429
/// (the order was not taken); a pause it cannot wait for refuses the
/// placement as not sent, never as uncertain.
#[tokio::test]
async fn a_throttled_placement_is_retried_or_refused_never_uncertain() {
    use openalgo_desktop_lib::brokers::common::outcome::{classify, placing, PlaceOutcome};
    let fake = Fake::default();
    fake.once(
        "POST",
        "/api/v3/orders/sync",
        (429, vec![("x-retry-after-ms", "10".into())], "{}".into()),
    )
    .on(
        "POST",
        "/api/v3/orders/sync",
        &json!({"s": "ok", "code": 1101, "message": "Order submitted", "id": "26100300000011"})
            .to_string(),
    );
    let b = broker(&fake.serve().await);
    let o = order("SBIN", "NSE", "LIMIT");
    let r = placing(b.place_order(&auth(), &o)).await.unwrap();
    assert_eq!(r.order_id, "26100300000011");
    assert_eq!(fake.requests("POST", "/api/v3/orders/sync").len(), 2);

    let fake = Fake::default();
    fake.once(
        "POST",
        "/api/v3/orders/sync",
        (429, vec![("retry-after", "120".into())], "{}".into()),
    );
    let b = broker(&fake.serve().await);
    let r = placing(b.place_order(&auth(), &o)).await;
    assert!(
        matches!(classify(&r), PlaceOutcome::Refused { .. }),
        "{:?}",
        r
    );
    assert_eq!(fake.requests("POST", "/api/v3/orders/sync").len(), 1);
}

/// A NIFTY master with 91 strikes a side of CE and PE for two weekly
/// expiries, Fyers-style broker symbols.
fn chain_master() -> SymbolResolver {
    use openalgo_desktop_lib::brokers::common::symbols::SymToken;
    let row =
        |symbol: String, brsymbol: String, exchange: &str, expiry: &str, strike: f64, it: &str| {
            SymToken {
                symbol,
                brsymbol,
                name: "NIFTY".into(),
                exchange: exchange.into(),
                brexchange: "NSE".into(),
                token: String::new(),
                expiry: expiry.into(),
                strike,
                lot_size: 65,
                instrument_type: it.into(),
                tick_size: 0.05,
            }
        };
    let mut rows = vec![row(
        "NIFTY".into(),
        "NSE:NIFTY50-INDEX".into(),
        "NSE_INDEX",
        "",
        0.0,
        "INDEX",
    )];
    for (code, br, expiry) in [
        ("13OCT26", "26O13", "13-OCT-26"),
        ("20OCT26", "26O20", "20-OCT-26"),
    ] {
        for i in 0..91 {
            let k = 22_750.0 + 50.0 * i as f64;
            for t in ["CE", "PE"] {
                rows.push(row(
                    format!("NIFTY{}{}{}", code, k as i64, t),
                    format!("NSE:NIFTY{}{}{}", br, k as i64, t),
                    "NFO",
                    expiry,
                    k,
                    t,
                ));
            }
        }
    }
    // Each contract has its own token: the master keeps one row per
    // (exchange, token).
    for (i, r) in rows.iter_mut().enumerate() {
        r.token = format!("{}", 100_000 + i);
    }
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

/// The native chain answer for one expiry: every leg with OI = its index.
fn native_chain(br: &str) -> String {
    let mut legs = vec![
        json!({"symbol": "NSE:NIFTY50-INDEX", "option_type": "", "strike_price": -1, "ltp": 25010}),
    ];
    for i in 0..91 {
        let k = 22_750 + 50 * i;
        for (j, t) in ["CE", "PE"].iter().enumerate() {
            legs.push(json!({
                "symbol": format!("NSE:NIFTY{}{}{}", br, k, t),
                "option_type": t, "strike_price": k,
                "ltp": 100.0, "ltpch": 5.0, "ltpchp": 5.26, "bid": 99.5, "ask": 100.5,
                "volume": 10, "oi": 1000 + 2 * i + j,
            }));
        }
    }
    json!({
        "s": "ok", "code": 200, "message": "",
        "data": {
            "expiryData": [
                {"date": "13-10-2026", "expiry": "1760349600"},
                {"date": "20-10-2026", "expiry": "1760954400"},
            ],
            "optionsChain": legs,
        }
    })
    .to_string()
}

fn chain_request(code: &str, expiry: (i32, u32, u32)) -> ChainQuoteRequest {
    let mut keys = Vec::new();
    for i in 0..91 {
        let k = 22_750 + 50 * i;
        for t in ["CE", "PE"] {
            keys.push(QuoteKey::new("NFO", format!("NIFTY{}{}{}", code, k, t)));
        }
    }
    ChainQuoteRequest {
        underlying: QuoteKey::new("NSE_INDEX", "NIFTY"),
        expiry: NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
        keys,
        strikes_each_side: Some(45),
    }
}

fn chain_broker(base: &str) -> FyersBroker {
    FyersBroker::with_urls(
        chain_master(),
        FyersUrls {
            api: base.to_string(),
            public: format!("{}/sym_details", base),
            ..FyersUrls::default()
        },
    )
    .with_retry_base(Duration::from_millis(1))
}

/// TOOLS-04, BRL-02: a 182-option chain (GEX's 45 strikes a side) gets
/// every leg's LTP and open interest in one native request, with no
/// per-option depth call; it used to read OI 0 for every leg above 100
/// options and send one depth call per option below.
/// TOOLS-05: a later expiry is asked for with its timestamp, and an
/// answer whose contracts belong to another expiry is not used: those
/// legs come from the batch quotes, reported without open interest.
#[tokio::test]
async fn the_native_chain_gives_every_legs_oi_for_the_requested_expiry() {
    let fake = Fake::default();
    fake.once(
        "GET",
        "/data/options-chain-v3",
        (200, vec![], native_chain("26O13")),
    );
    fake.once(
        "GET",
        "/data/options-chain-v3",
        (200, vec![], native_chain("26O20")),
    );
    let b = chain_broker(&fake.serve().await);
    let req = chain_request("13OCT26", (2026, 10, 13));
    let got = b.get_chain_quotes(&auth(), &req).await.unwrap();
    assert!(got.oi_missing.is_empty());
    assert_eq!(got.quotes.len(), 182);
    let first = got.quotes[0].data.as_ref().unwrap();
    assert_eq!(
        (first.symbol.as_str(), first.oi, first.ltp, first.close),
        ("NIFTY13OCT2622750CE", 1000, 100.0, 95.0)
    );
    assert_eq!(got.quotes[181].data.as_ref().unwrap().oi, 1000 + 2 * 90 + 1);
    let sent = fake.requests("GET", "/data/options-chain-v3");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].query, "symbol=NSE%3ANIFTY50-INDEX&strikecount=47");
    assert!(fake.requests("GET", "/data/depth").is_empty());
    assert!(fake.requests("GET", "/data/quotes").is_empty());

    // The next expiry is asked for directly with the timestamp the first
    // answer listed: one request again.
    let got = b
        .get_chain_quotes(&auth(), &chain_request("20OCT26", (2026, 10, 20)))
        .await
        .unwrap();
    assert!(got.oi_missing.is_empty());
    assert_eq!(
        got.quotes[0].data.as_ref().unwrap().symbol,
        "NIFTY20OCT2622750CE"
    );
    let sent = fake.requests("GET", "/data/options-chain-v3");
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1].query,
        "symbol=NSE%3ANIFTY50-INDEX&strikecount=47&timestamp=1760954400"
    );

    // A broker that answers the nearest expiry whatever is asked: the
    // answer is not mapped onto the later expiry's contracts.
    let fake = Fake::default();
    fake.on("GET", "/data/options-chain-v3", &native_chain("26O13"))
        .on(
            "GET",
            "/data/quotes",
            &json!({"s": "ok", "d": []}).to_string(),
        );
    let b = chain_broker(&fake.serve().await);
    let req = chain_request("20OCT26", (2026, 10, 20));
    let got = b.get_chain_quotes(&auth(), &req).await.unwrap();
    assert_eq!(got.oi_missing.len(), 182);
    assert!(got.quotes.iter().all(|q| q.data.is_none()));
    let sent = fake.requests("GET", "/data/options-chain-v3");
    assert_eq!(sent.len(), 2, "asked again with the expiry's timestamp");
    assert!(sent[1].query.ends_with("&timestamp=1760954400"));
    assert_eq!(
        fake.requests("GET", "/data/quotes").len(),
        4,
        "182 legs in batches of 50"
    );
    assert!(fake.requests("GET", "/data/depth").is_empty());
}

/// NEW-PAR-06: a native leg whose open interest Fyers did not send, or sent
/// as `null` or text that is not a number, is reported without OI (so the
/// OI tools refuse a figure over it); a reported zero is a measured zero.
#[tokio::test]
async fn native_legs_without_a_readable_oi_are_reported_without_oi() {
    let mut chain: serde_json::Value = serde_json::from_str(&native_chain("26O13")).unwrap();
    let legs = chain["data"]["optionsChain"].as_array_mut().unwrap();
    // legs[0] is the underlying; then CE, PE per strike from 22750.
    legs[1].as_object_mut().unwrap().remove("oi");
    legs[2]["oi"] = json!(null);
    legs[3]["oi"] = json!("n/a");
    legs[4]["oi"] = json!(0);
    let fake = Fake::default();
    fake.once(
        "GET",
        "/data/options-chain-v3",
        (200, vec![], chain.to_string()),
    );
    let b = chain_broker(&fake.serve().await);
    let got = b
        .get_chain_quotes(&auth(), &chain_request("13OCT26", (2026, 10, 13)))
        .await
        .unwrap();
    let mut missing: Vec<&str> = got.oi_missing.iter().map(String::as_str).collect();
    missing.sort_unstable();
    assert_eq!(
        missing,
        [
            "NIFTY13OCT2622750CE",
            "NIFTY13OCT2622750PE",
            "NIFTY13OCT2622800CE"
        ]
    );
    let zero = got.quotes[3].data.as_ref().unwrap();
    assert_eq!((zero.symbol.as_str(), zero.oi), ("NIFTY13OCT2622800PE", 0));
    assert!(fake.requests("GET", "/data/quotes").is_empty());
}

#[tokio::test]
async fn margin_uses_multiorder_endpoint() {
    let fake = Fake::default();
    fake.on("POST", "/api/v3/multiorder/margin", fx!("margin.json"));
    let b = broker(&fake.serve().await);
    let leg = |s: &str, ex: &str, a| MarginLeg {
        key: QuoteKey::new(ex, s),
        action: a,
        quantity: 65,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    };
    let r = b
        .calculate_margin(
            &auth(),
            &[
                leg("NIFTY27OCT26FUT", "NFO", Action::Buy),
                leg("NIFTY06OCT2622400PE", "NFO", Action::Sell),
                leg("NOPE", "NFO", Action::Buy),
            ],
        )
        .await
        .unwrap();
    assert_eq!(r.total_margin_required, 147_738.056_3);
    let body = &fake.requests("POST", "/api/v3/multiorder/margin")[0].body;
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[1]["symbol"], "NSE:NIFTY26O0622400PE");
    assert_eq!(data[1]["side"], -1);
    assert_eq!(data[0]["type"], 2);
    let e = b
        .calculate_margin(&auth(), &[leg("NOPE", "NFO", Action::Buy)])
        .await
        .unwrap_err();
    assert!(e.client_message().starts_with("No valid positions"));
}

#[tokio::test]
async fn gtt_place_modify_cancel_and_book() {
    let fake = Fake::default();
    fake.on(
        "POST",
        "/api/v3/gtt/orders/sync",
        &json!({"s": "ok", "code": 1101, "message": "GTT placed", "id": "25100300000009"})
            .to_string(),
    )
    .on(
        "PATCH",
        "/api/v3/gtt/orders/sync",
        &json!({"code": 1102, "message": "modified", "id": "25100300000009"}).to_string(),
    )
    .on(
        "DELETE",
        "/api/v3/gtt/orders/sync",
        &json!({"s": "ok", "code": 1103, "id": "25100300000009"}).to_string(),
    )
    .on("GET", "/api/v3/gtt/orders", fx!("gtt_book.json"))
    .on("GET", "/data/depth", fx!("depth.json"));
    let b = broker(&fake.serve().await);
    let req = GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type: GttTriggerType::Single,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 1,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 900.5,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 0.0,
        target: 0.0,
        last_price: None,
    };
    let r = b.place_gtt(&auth(), &req).await.unwrap();
    assert_eq!(r.trigger_id, "25100300000009");
    // MARKET: LTP fetched (depth), protected LIMIT price sent.
    let body = &fake.requests("POST", "/api/v3/gtt/orders/sync")[0].body;
    assert_eq!(body["orderInfo"]["leg1"]["price"], 958.85);
    assert_eq!(body["orderInfo"]["leg1"]["triggerPrice"], 900.5);
    assert_eq!(body["symbol"], "NSE:SBIN-EQ");
    assert_eq!(body["productType"], "CNC");
    b.modify_gtt(&auth(), "25100300000009", &req).await.unwrap();
    let body = &fake.requests("PATCH", "/api/v3/gtt/orders/sync")[0].body;
    assert_eq!(body["id"], "25100300000009");
    assert!(body.get("symbol").is_none());
    b.cancel_gtt(&auth(), "25100300000009").await.unwrap();
    assert_eq!(
        fake.requests("DELETE", "/api/v3/gtt/orders/sync")[0].body,
        json!({"id": "25100300000009"})
    );
    let book = b.get_gtt_book(&auth(), true).await.unwrap();
    assert_eq!(book.len(), 2);
    assert_eq!(book[1].trigger_type, "two-leg");
    assert_eq!(
        b.cancel_gtt(&auth(), "").await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn master_contract_download_and_tbt_address() {
    let fake = Fake::default();
    fake.on("GET", "/sym_details/NSE_CM.csv", fx!("NSE_CM.csv"))
        .on("GET", "/sym_details/BSE_CM.csv", fx!("BSE_CM.csv"))
        .on("GET", "/sym_details/NSE_FO.csv", fx!("NSE_FO.csv"))
        .on("GET", "/sym_details/BSE_FO.csv", fx!("BSE_FO.csv"))
        .on(
            "GET",
            "/sym_details/NSE_CD_sym_master.json",
            fx!("NSE_CD_sym_master.json"),
        )
        .on(
            "GET",
            "/sym_details/MCX_COM_sym_master.json",
            fx!("MCX_COM_sym_master.json"),
        )
        .on(
            "GET",
            "/sym_details/index_hsm_mapping.json",
            fx!("index_hsm_mapping.json"),
        )
        .on(
            "GET",
            "/indus/home/tbtws",
            &json!({"s": "ok", "data": {"socket_url": "wss://tbt.example/versova"}}).to_string(),
        );
    let b = broker(&fake.serve().await);
    let rows = b.download_master_contract(&auth()).await.unwrap();
    assert_eq!(rows.len(), 39);
    let nifty = rows
        .iter()
        .find(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX")
        .unwrap();
    assert_eq!(nifty.name, "Nifty 50");
    // The 50-level depth socket looks its address up before each connect.
    {
        let mut f = b.create_depth_feed(&auth(), 50).unwrap();
        assert_eq!(
            f.ws_request().unwrap().uri().to_string(),
            "wss://rtsocket-api.fyers.in/versova"
        );
        f.prepare().await.unwrap();
        assert_eq!(
            f.ws_request().unwrap().uri().to_string(),
            "wss://tbt.example/versova"
        );
        assert_eq!(f.supported_depth_levels(), &[50]);
    }

    // One missing file keeps the existing master (error, no partial list).
    let fake = Fake::default();
    fake.on("GET", "/sym_details/NSE_CM.csv", fx!("NSE_CM.csv"));
    let b = broker(&fake.serve().await);
    assert!(b.download_master_contract(&auth()).await.is_err());
    // A failed depth address lookup keeps the documented default.
    let mut f = b.create_depth_feed(&auth(), 50).unwrap();
    f.prepare().await.unwrap();
    assert_eq!(
        f.ws_request().unwrap().uri().to_string(),
        "wss://rtsocket-api.fyers.in/versova"
    );
}

#[test]
fn identity_and_capabilities() {
    let b = FyersBroker::new(master());
    assert_eq!(b.id(), "fyers");
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "auth_code" });
    let c = b.capabilities();
    assert!(c.history && c.margin && c.gtt && c.streaming && c.multiquotes_batch);
    assert_eq!(c.depth_levels, &[5, 50]);
    assert!(c.order_feed);
    // 50 levels on NSE and NFO only, through the TBT socket.
    assert_eq!(b.feed_depth_levels("NSE"), vec![5, 50]);
    assert_eq!(b.feed_depth_levels("NFO"), vec![5, 50]);
    assert_eq!(b.feed_depth_levels("MCX"), vec![5]);
    assert!(b.create_depth_feed(&auth(), 20).is_err());
    assert!(matches!(
        b.create_order_feed(&auth()).unwrap(),
        openalgo_desktop_lib::brokers::common::streaming::OrderFeed::Socket(_)
    ));
    let keys: Vec<&str> = b.timeframe_map().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys.first(), Some(&"5s"));
    assert_eq!(keys.last(), Some(&"D"));
    assert_eq!(b.supported_exchanges().len(), 8);
}
