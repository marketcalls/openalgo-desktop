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

    // A failing chunk is retried three times then skipped (web).
    let fake = Fake::default();
    for _ in 0..4 {
        fake.once(
            "GET",
            "/data/history",
            (
                200,
                vec![],
                json!({"s": "error", "code": -300, "message": "no data"}).to_string(),
            ),
        );
    }
    let b = broker(&fake.serve().await);
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2025, 10, 3).unwrap(),
    };
    assert!(b.get_history(&auth(), &req).await.unwrap().is_empty());
    assert_eq!(fake.requests("GET", "/data/history").len(), 4);
    let bad = HistoryRequest {
        interval: "W".into(),
        ..req
    };
    assert_eq!(
        b.get_history(&auth(), &bad).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
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
    assert_eq!(rows.len(), 28);
    let nifty = rows
        .iter()
        .find(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX")
        .unwrap();
    assert_eq!(nifty.name, "Nifty 50");
    assert_eq!(b.tbt_socket_url(&auth()).await, "wss://tbt.example/versova");
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
    // TBT address lookup falls back to the documented default.
    assert_eq!(
        b.tbt_socket_url(&auth()).await,
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
