//! Delta Exchange adapter against a local fake Delta (ephemeral port):
//! signed requests verified server-side, sign-in, orders with exact
//! (fractional) sizes, leverage, books, funds, margin, quotes, depth,
//! history, the product master, 429 handling and the weighted quota; then
//! the `/api/v1` order path end to end with a crypto order, and the
//! CRYPTO always-open behaviour of the sandbox and the market-hours check.
//! Payloads are the recorded shapes in `tests/fixtures/brokers/deltaexchange/`
//! (no account data).

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use http_body_util::BodyExt;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::deltaexchange::ratelimit::{Bucket, Quota};
use openalgo_desktop_lib::brokers::deltaexchange::{auth, master_contract, DeltaBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials, BrokerRegistry};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// A scriptable fake Delta that checks every signature
// ---------------------------------------------------------------------------

const KEY: &str = "test-key-abcdef";
const SECRET: &str = "test-secret-0123456789";
/// 2026-10-03 11:30 IST.
const NOW: i64 = 1_791_007_200;

fn clock() -> i64 {
    NOW
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(k).map(String::as_str)
    }
    /// The signature the exchange would expect for this request.
    fn expected_signature(&self) -> String {
        let q = if self.query.is_empty() {
            String::new()
        } else {
            format!("?{}", self.query)
        };
        auth::signature(
            SECRET,
            &self.method,
            self.header("timestamp").unwrap_or(""),
            &self.path,
            &q,
            &self.body,
        )
    }
    fn signed_ok(&self) -> bool {
        self.header("api-key") == Some(KEY)
            && self.header("timestamp") == Some(&NOW.to_string())
            && self.header("signature") == Some(self.expected_signature().as_str())
    }
}

type Answers = HashMap<String, VecDeque<(u16, Vec<(String, String)>, String)>>;

#[derive(Clone, Default)]
struct Mock {
    routes: Arc<Mutex<Answers>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn on(&self, method: &str, path: &str, status: u16, body: impl Into<String>) {
        self.on_with(method, path, status, &[], body);
    }

    fn on_with(
        &self,
        method: &str,
        path: &str,
        status: u16,
        headers: &[(&str, &str)],
        body: impl Into<String>,
    ) {
        self.routes
            .lock()
            .entry(format!("{} {}", method, path))
            .or_default()
            .push_back((
                status,
                headers
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
                body.into(),
            ));
    }

    fn calls(&self, method: &str, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Seen> {
        self.seen.lock().clone()
    }
}

async fn handle(
    State(m): State<Mock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    m.seen.lock().push(Seen {
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or("").to_string(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect(),
        body: String::from_utf8_lossy(&body).to_string(),
    });
    let answer = {
        let mut r = m.routes.lock();
        match r.get_mut(&format!("{} {}", method, uri.path())) {
            Some(q) if q.len() > 1 => q.pop_front(),
            Some(q) => q.front().cloned(),
            None => None,
        }
    };
    match answer {
        Some((status, hs, body)) => {
            let mut resp = (
                StatusCode::from_u16(status).unwrap(),
                [("content-type", "application/json")],
                body,
            )
                .into_response();
            for (k, v) in hs {
                resp.headers_mut().insert(
                    axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                );
            }
            resp
        }
        None => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
}

struct Server {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(m: &Mock) -> Server {
    let app = Router::new().fallback(handle).with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Server {
        url: format!("http://{}", addr),
        task,
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/brokers/deltaexchange")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn error_body(kind: &str) -> String {
    fixture_json("errors.json")[kind].to_string()
}

fn master_contract() -> MasterContract {
    let mut all: Vec<Value> = Vec::new();
    for page in ["products_page1.json", "products_page2.json"] {
        all.extend(
            fixture_json(page)["result"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
    }
    master_contract::parse_products(&all)
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load_master(master_contract());
    r
}

fn broker(server: &Server) -> DeltaBroker {
    broker_with(server, master())
}

fn broker_with(server: &Server, symbols: SymbolResolver) -> DeltaBroker {
    DeltaBroker::with_urls(symbols, server.url.clone(), "ws://127.0.0.1:9/unused")
        .with_clock(clock)
        .without_delays()
}

fn auth() -> AuthToken {
    AuthToken::new(format!("{}:{}", KEY, SECRET))
}

fn resolved(
    symbols: &SymbolResolver,
    symbol: &str,
    action: &str,
    pt: &str,
    price: f64,
) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: "CRYPTO".into(),
        side: action.into(),
        quantity: 1,
        price,
        order_type: pt.into(),
        product: "NRML".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, symbols).unwrap()
}

fn exact(v: Value) -> CryptoQuantity {
    CryptoQuantity::parse(Exchange::Crypto, &v).unwrap()
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sign_in_is_a_signed_profile_probe() {
    let m = Mock::default();
    m.on("GET", "/v2/profile", 200, fixture("profile.json"));
    let s = serve(&m).await;
    let b = broker(&s);
    let r = b
        .authenticate(BrokerCredentials {
            api_key: format!(" {} ", KEY),
            api_secret: Some(SECRET.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.auth_token, format!("{}:{}", KEY, SECRET));
    assert_eq!(r.user_id, "<USER_ID>");
    assert_eq!(r.user_name.as_deref(), Some("Trader"));
    assert!(r.feed_token.is_none());
    let call = &m.calls("GET", "/v2/profile")[0];
    assert!(call.signed_ok(), "{:?}", call);
    assert_eq!(call.header("accept"), Some("application/json"));
    // The secret itself never goes over the wire.
    assert!(!format!("{:?}", call).contains(SECRET));
    assert!(!format!("{:?}", r).contains(SECRET));
}

#[tokio::test]
async fn sign_in_failures_name_the_cause() {
    let m = Mock::default();
    m.on("GET", "/v2/profile", 401, error_body("unauthorized"));
    let s = serve(&m).await;
    let b = broker(&s);
    let creds = BrokerCredentials {
        api_key: KEY.into(),
        api_secret: Some(SECRET.into()),
        ..Default::default()
    };
    let e = b.authenticate(creds.clone()).await.unwrap_err();
    assert!(matches!(e, openalgo_desktop_lib::error::AppError::Auth(_)));
    assert!(e.client_message().contains("API key or signature"));

    let m2 = Mock::default();
    m2.on("GET", "/v2/profile", 403, "<html>Forbidden</html>");
    let s2 = serve(&m2).await;
    let e = broker(&s2).authenticate(creds).await.unwrap_err();
    assert!(
        e.client_message().contains("Whitelist"),
        "{}",
        e.client_message()
    );

    // Missing secret: refused before any call.
    let e = b
        .authenticate(BrokerCredentials {
            api_key: KEY.into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(e.client_message().contains("API secret"));
    assert_eq!(m.all().len(), 1);
}

// ---------------------------------------------------------------------------
// Orders and leverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn place_limit_order_signs_the_exact_body_and_returns_a_composite_id() {
    let m = Mock::default();
    m.on("POST", "/v2/orders", 200, fixture("place_order.json"));
    let s = serve(&m).await;
    let b = broker(&s);
    let o = resolved(b.symbols().unwrap(), "BTCUSDFUT", "BUY", "LIMIT", 60000.0);
    let r = b.place_order(&auth(), &o).await.unwrap();
    assert_eq!(r.order_id, "27:7101");
    let call = &m.calls("POST", "/v2/orders")[0];
    assert!(call.signed_ok());
    assert_eq!(call.header("content-type"), Some("application/json"));
    assert_eq!(
        call.json(),
        json!({"product_id": 27, "product_symbol": "BTCUSD", "size": 1, "side": "buy",
               "order_type": "limit_order", "time_in_force": "gtc", "limit_price": "60000.0"})
    );
}

#[tokio::test]
async fn fractional_spot_sizes_reach_delta_exactly() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":7200,"product_id":1600}}"#,
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let o = resolved(b.symbols().unwrap(), "BTCINR", "SELL", "MARKET", 0.0);
    for (input, wire) in [
        (json!(0.0005), "0.0005"),
        (json!("0.123456789"), "0.123456789"),
    ] {
        let r = b
            .place_order_exact(&auth(), &o, &exact(input))
            .await
            .unwrap();
        assert_eq!(r.order_id, "1600:7200");
        let call = m.calls("POST", "/v2/orders").pop().unwrap();
        assert!(call.signed_ok());
        assert!(
            call.body.contains(&format!("\"size\":{}", wire)),
            "{}",
            call.body
        );
        assert_eq!(call.json()["side"], "sell");
    }
    // Derivatives refuse a fraction before anything is sent.
    let perp = resolved(b.symbols().unwrap(), "BTCUSDFUT", "BUY", "MARKET", 0.0);
    let before = m.all().len();
    let e = b
        .place_order_exact(&auth(), &perp, &exact(json!(0.5)))
        .await
        .unwrap_err();
    assert!(e
        .client_message()
        .contains("Use whole numbers for BTCUSDFUT"));
    assert_eq!(m.all().len(), before);
}

#[tokio::test]
async fn order_errors_carry_deltas_reason() {
    let m = Mock::default();
    m.on("POST", "/v2/orders", 400, error_body("insufficient_margin"));
    m.on("PUT", "/v2/orders", 400, error_body("with_message"));
    m.on("DELETE", "/v2/orders", 400, error_body("signature_expired"));
    let s = serve(&m).await;
    let b = broker(&s);
    let o = resolved(b.symbols().unwrap(), "BTCUSDFUT", "BUY", "MARKET", 0.0);
    let e = b.place_order(&auth(), &o).await.unwrap_err();
    assert_eq!(e.client_message(), "insufficient margin");
    let mo = ResolvedModify {
        order_id: "27:7001".into(),
        symbol: "BTCUSDFUT".into(),
        exchange: Exchange::Crypto,
        action: Action::Buy,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        quantity: 1,
        price: 59000.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
        instrument: o.instrument.clone(),
    };
    let e = b.modify_order(&auth(), &mo).await.unwrap_err();
    assert_eq!(e.client_message(), "size must be a positive integer");
    let e = b.cancel_order(&auth(), "27:7001").await.unwrap_err();
    assert!(
        e.client_message().contains("clock"),
        "{}",
        e.client_message()
    );
}

#[tokio::test]
async fn modify_and_cancel_use_the_composite_id() {
    let m = Mock::default();
    m.on(
        "PUT",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":7004}}"#,
    );
    m.on(
        "DELETE",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":7001}}"#,
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let symbols = b.symbols().unwrap().clone();
    let m_req = ModifyOrderRequest {
        symbol: "BTCINR".into(),
        exchange: "CRYPTO".into(),
        action: "SELL".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 0,
        price: 5_950_000.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("1600:7004", &m_req, &symbols).unwrap();
    let r = b
        .modify_order_exact(&auth(), &rm, &exact(json!("0.25")))
        .await
        .unwrap();
    assert_eq!(r.order_id, "1600:7004");
    let put = &m.calls("PUT", "/v2/orders")[0];
    assert!(put.signed_ok());
    assert_eq!(
        put.json(),
        json!({"id": 7004, "product_id": 1600, "size": 0.25, "limit_price": "5950000.0"})
    );
    let r = b.cancel_order(&auth(), "27:7001").await.unwrap();
    assert_eq!(r.order_id, "27:7001");
    let del = &m.calls("DELETE", "/v2/orders")[0];
    assert!(del.signed_ok());
    assert_eq!(del.json(), json!({"id": 7001, "product_id": 27}));
    assert!(b.cancel_order(&auth(), "not-an-id").await.is_err());
}

#[tokio::test]
async fn cancel_all_uses_the_bulk_endpoint_then_falls_back() {
    let m = Mock::default();
    m.on(
        "DELETE",
        "/v2/orders/all",
        200,
        r#"{"success":true,"result":{}}"#,
    );
    let s = serve(&m).await;
    let r = broker(&s).cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, ["all"]);
    let bulk = &m.calls("DELETE", "/v2/orders/all")[0];
    assert!(bulk.signed_ok());
    assert_eq!(
        bulk.json(),
        json!({"cancel_limit_orders": true, "cancel_stop_orders": true, "cancel_reduce_only_orders": true})
    );

    let m = Mock::default();
    m.on("DELETE", "/v2/orders/all", 400, error_body("with_message"));
    m.on("GET", "/v2/orders", 200, fixture("orders_open.json"));
    m.on(
        "DELETE",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{}}"#,
    );
    m.on(
        "DELETE",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{}}"#,
    );
    m.on("DELETE", "/v2/orders", 400, error_body("with_message"));
    let s = serve(&m).await;
    let r = broker(&s).cancel_all_orders(&auth()).await.unwrap();
    // Three open/pending orders: two cancelled, the last refused.
    assert_eq!(r.cancelled, ["27:7001", "3136:7002"]);
    assert_eq!(r.failed, ["27:6999"]);
    let list = &m.calls("GET", "/v2/orders")[0];
    assert_eq!(list.query, "state=open");
    assert!(list.signed_ok());
}

#[tokio::test]
async fn leverage_is_set_and_read_per_product() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/products/27/orders/leverage",
        200,
        fixture("leverage.json"),
    );
    m.on(
        "GET",
        "/v2/products/27/orders/leverage",
        200,
        fixture("leverage.json"),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let row = b
        .symbols()
        .unwrap()
        .by_symbol("CRYPTO", "BTCUSDFUT")
        .unwrap();
    b.set_leverage(&auth(), &row, 10).await.unwrap();
    let post = &m.calls("POST", "/v2/products/27/orders/leverage")[0];
    assert!(post.signed_ok());
    assert_eq!(post.json(), json!({"leverage": "10"}));
    assert_eq!(b.get_leverage(&auth(), &row).await.unwrap(), 10.0);
    assert!(m.calls("GET", "/v2/products/27/orders/leverage")[0].signed_ok());
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

fn books_mock() -> Mock {
    let m = Mock::default();
    m.on("GET", "/v2/orders", 200, fixture("orders_open.json"));
    m.on(
        "GET",
        "/v2/orders/history",
        200,
        fixture("orders_history.json"),
    );
    m.on("GET", "/v2/fills", 200, fixture("fills.json"));
    m.on(
        "GET",
        "/v2/positions/margined",
        200,
        fixture("positions_margined.json"),
    );
    m.on(
        "GET",
        "/v2/wallet/balances",
        200,
        fixture("wallet_balances.json"),
    );
    m
}

#[tokio::test]
async fn order_and_trade_books_show_today_in_ist_with_openalgo_symbols() {
    let m = books_mock();
    let s = serve(&m).await;
    let b = broker(&s);
    let book = b.get_order_book(&auth()).await.unwrap();
    let ids: Vec<&str> = book.iter().map(|o| o.order_id.as_str()).collect();
    // 6999 is from 1 Oct; 7001 appears in both lists once.
    assert_eq!(
        ids,
        [
            "27:7001",
            "3136:7002",
            "95011:7003",
            "1600:7004",
            "3136:7005",
            "99999:7006"
        ]
    );
    let syms: Vec<&str> = book.iter().map(|o| o.symbol.as_str()).collect();
    assert_eq!(
        syms,
        [
            "BTCUSDFUT",
            "ETHUSDFUT",
            "BTC27NOV2662000CE",
            "BTCINR",
            "ETHUSDFUT",
            "DOGEUSD"
        ]
    );
    assert!(book.iter().all(|o| o.exchange == "CRYPTO"));
    for c in m.all() {
        assert!(c.signed_ok(), "{:?}", c);
    }
    let trades = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(trades.len(), 2);
    assert_eq!(trades[0].symbol, "BTCUSDFUT");
    assert_eq!(trades[1].symbol, "BTCINR");
    assert!(b.get_holdings(&auth()).await.unwrap().is_empty());
}

#[tokio::test]
async fn positions_open_position_and_close_all_use_exact_sizes() {
    let m = books_mock();
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":9001,"product_id":27}}"#,
    );
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":9002,"product_id":3136}}"#,
    );
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":9003,"product_id":1600}}"#,
    );
    m.on("POST", "/v2/orders", 400, error_body("insufficient_margin"));
    let s = serve(&m).await;
    let b = broker(&s);
    let pos = b.get_positions(&auth()).await.unwrap();
    let rows: Vec<(&str, &str, i32)> = pos
        .iter()
        .map(|p| (p.symbol.as_str(), p.product.as_str(), p.quantity))
        .collect();
    assert_eq!(
        rows,
        [
            ("BTCUSDFUT", "NRML", 6),
            ("ETHUSDFUT", "NRML", -3),
            ("ETHINR", "CNC", 2)
        ]
    );
    // Matched on the broker symbol whatever the product (web).
    for product in [Product::Mis, Product::Nrml, Product::Cnc] {
        assert_eq!(
            b.get_open_position(&auth(), "ETHUSDFUT", Exchange::Crypto, product)
                .await
                .unwrap(),
            -3
        );
    }
    assert_eq!(
        b.get_open_position(&auth(), "BTC27NOV26FUT", Exchange::Crypto, Product::Nrml)
            .await
            .unwrap(),
        0
    );

    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed, ["27:9001", "3136:9002", "1600:9003"]);
    assert_eq!(r.failed, ["ETHINR (CRYPTO): insufficient margin"]);
    let orders: Vec<Value> = m
        .calls("POST", "/v2/orders")
        .iter()
        .map(Seen::json)
        .collect();
    assert_eq!(
        (
            orders[0]["product_symbol"].as_str(),
            orders[0]["side"].as_str(),
            &orders[0]["size"]
        ),
        (Some("BTCUSD"), Some("sell"), &json!(6))
    );
    assert_eq!(
        (orders[1]["side"].as_str(), &orders[1]["size"]),
        (Some("buy"), &json!(3))
    );
    // The 0.01 BTC wallet balance is sold exactly.
    assert_eq!(
        (orders[2]["product_symbol"].as_str(), &orders[2]["size"]),
        (Some("BTC_INR"), &json!(0.01))
    );
    assert_eq!(orders[3]["product_id"], 1601);
    assert!(orders.iter().all(|o| o["order_type"] == "market_order"));
}

#[tokio::test]
async fn strict_position_read_fails_a_smart_order_instead_of_reading_flat() {
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/positions/margined",
        200,
        fixture("positions_margined.json"),
    );
    m.on("GET", "/v2/wallet/balances", 500, "oops");
    let s = serve(&m).await;
    let b = broker(&s);
    // The book shows the half it could read.
    assert_eq!(b.get_positions(&auth()).await.unwrap().len(), 2);
    // A smart order must not size itself against a half book.
    assert!(b
        .get_open_position(&auth(), "BTCUSDFUT", Exchange::Crypto, Product::Nrml)
        .await
        .is_err());
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn funds_and_margin() {
    let m = books_mock();
    m.on(
        "GET",
        "/v2/products/27/margin_required",
        200,
        fixture("margin_required.json"),
    );
    m.on(
        "GET",
        "/v2/products/95011/margin_required",
        200,
        r#"{"success":true,"result":{"initial_margin":"1.5"}}"#,
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let f = b.get_funds(&auth()).await.unwrap();
    assert_eq!(
        (f.available_cash, f.utilised_debits, f.m2m_unrealized),
        (155544.27, 43.05, 1.5)
    );
    let leg = |sym: &str, pt: PriceType, price: f64| MarginLeg {
        key: QuoteKey::new("CRYPTO", sym),
        action: Action::Buy,
        quantity: 2,
        product: Product::Nrml,
        pricetype: pt,
        price,
        trigger_price: 0.0,
    };
    let r = b
        .calculate_margin(
            &auth(),
            &[
                leg("BTCUSDFUT", PriceType::Limit, 60000.0),
                leg("BTC27NOV2662000CE", PriceType::Limit, 0.0),
                leg("UNKNOWN", PriceType::Market, 0.0),
            ],
        )
        .await
        .unwrap();
    assert!((r.total_margin_required - 7.5125).abs() < 1e-9);
    assert_eq!(r.span_margin, r.total_margin_required);
    assert_eq!(r.exposure_margin, 0.0);
    let c = &m.calls("GET", "/v2/products/27/margin_required")[0];
    assert!(c.signed_ok(), "{:?}", c);
    assert_eq!(
        c.query,
        "limit_price=60000.0&order_type=limit_order&side=buy&size=2"
    );
    // A limit leg without a price is priced as market.
    let c = &m.calls("GET", "/v2/products/95011/margin_required")[0];
    assert_eq!(c.query, "order_type=market_order&side=buy&size=2");
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quotes_depth_and_multiquotes_are_public() {
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/tickers/BTCUSD",
        200,
        fixture("ticker_btcusd.json"),
    );
    m.on(
        "GET",
        "/v2/l2orderbook/27",
        200,
        fixture("l2orderbook.json"),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let key = QuoteKey::new("CRYPTO", "BTCUSDFUT");
    let q = b.get_quote(&auth(), &key).await.unwrap();
    assert_eq!((q.ltp, q.bid, q.ask), (59876.12, 59875.5, 59877.0));
    let d = b.get_market_depth(&auth(), &key).await.unwrap();
    assert_eq!(
        (d.bids[0].price, d.asks[0].quantity, d.total_buy_qty),
        (59875.5, 1200, 1700)
    );
    let mq = b
        .get_multiquotes(
            &auth(),
            &[key.clone(), QuoteKey::new("CRYPTO", "ETHUSDFUT")],
        )
        .await
        .unwrap();
    assert!(mq[0].data.is_some());
    assert!(mq[1].error.is_some());
    // Public calls carry no credentials.
    for c in m.all() {
        assert!(c.header("api-key").is_none() && c.header("signature").is_none());
    }
}

#[tokio::test]
async fn history_chunks_ist_days_and_stops_at_now() {
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/history/candles",
        200,
        fixture("candles_1h.json"),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let d = |y, mo, da| chrono::NaiveDate::from_ymd_opt(y, mo, da).unwrap();
    let req = HistoryRequest {
        key: QuoteKey::new("CRYPTO", "BTCUSDFUT"),
        interval: "1h".into(),
        start: d(2026, 10, 3),
        end: d(2026, 10, 3),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    let ts: Vec<i64> = c.iter().map(|c| c.timestamp).collect();
    // Sorted, de-duplicated, and the padded bar after NOW dropped.
    assert_eq!(ts, [1790998200, 1791001800, 1791005400]);
    let call = &m.calls("GET", "/v2/history/candles")[0];
    assert_eq!(
        call.query,
        format!("end={}&resolution=1h&start=1790965800&symbol=BTCUSD", NOW)
    );

    // 1m over 3 days is three one-day requests; a future day sends nothing.
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/history/candles",
        200,
        r#"{"success":true,"result":[]}"#,
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let req = HistoryRequest {
        interval: "1m".into(),
        start: d(2026, 10, 1),
        end: d(2026, 10, 5),
        ..req
    };
    assert!(b.get_history(&auth(), &req).await.unwrap().is_empty());
    assert_eq!(m.calls("GET", "/v2/history/candles").len(), 3);

    // Daily: one request, aliases accepted, unknown interval refused.
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/history/candles",
        200,
        fixture("candles_1d.json"),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let req = HistoryRequest {
        interval: "D".into(),
        start: d(2026, 9, 1),
        end: d(2026, 10, 3),
        ..req
    };
    assert_eq!(b.get_history(&auth(), &req).await.unwrap().len(), 2);
    assert_eq!(m.calls("GET", "/v2/history/candles").len(), 1);
    assert!(m.calls("GET", "/v2/history/candles")[0]
        .query
        .contains("resolution=1d"));
    let bad = HistoryRequest {
        interval: "10m".into(),
        ..req
    };
    assert!(b
        .get_history(&auth(), &bad)
        .await
        .unwrap_err()
        .client_message()
        .contains("Unsupported interval '10m'"));
}

// ---------------------------------------------------------------------------
// Product master
// ---------------------------------------------------------------------------

#[tokio::test]
async fn master_download_follows_the_cursor_and_keeps_contract_values() {
    let m = Mock::default();
    m.on("GET", "/v2/products", 200, fixture("products_page1.json"));
    m.on("GET", "/v2/products", 200, fixture("products_page2.json"));
    let s = serve(&m).await;
    let b = broker_with(&s, SymbolResolver::new());
    let mc = b.download_master(&auth()).await.unwrap();
    assert_eq!(mc.rows.len(), 9);
    assert_eq!(mc.contract_values.get("27"), Some(&0.001));
    let calls = m.calls("GET", "/v2/products");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].query, "page_size=500&states=live");
    assert_eq!(
        calls[1].query,
        "after=g3QAAAACZAACaWRhCmQABHR5cGVkAAhwcm9kdWN0&page_size=500&states=live"
    );
    // The resolver takes the multipliers with the rows.
    let r = SymbolResolver::new();
    r.load_master(mc);
    assert_eq!(r.contract_value("ETHUSDFUT", "CRYPTO"), Some(0.01));

    // A failed page keeps the stored master: the download errors out.
    let m = Mock::default();
    m.on("GET", "/v2/products", 200, fixture("products_page1.json"));
    m.on("GET", "/v2/products", 500, "upstream error");
    let s = serve(&m).await;
    assert!(broker_with(&s, SymbolResolver::new())
        .download_master_contract(&auth())
        .await
        .is_err());
}

// ---------------------------------------------------------------------------
// Rate limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_429_is_retried_on_the_reset_and_re_signed() {
    let m = Mock::default();
    m.on_with(
        "GET",
        "/v2/fills",
        429,
        &[("x-rate-limit-reset", "20")],
        "{}",
    );
    m.on("GET", "/v2/fills", 200, fixture("fills.json"));
    let s = serve(&m).await;
    let b = broker(&s);
    assert_eq!(b.get_trade_book(&auth()).await.unwrap().len(), 2);
    let calls = m.calls("GET", "/v2/fills");
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(Seen::signed_ok));

    // Retries are bounded.
    let m = Mock::default();
    m.on_with(
        "GET",
        "/v2/fills",
        429,
        &[("x-rate-limit-reset", "10")],
        "{}",
    );
    let s = serve(&m).await;
    let b = broker(&s).with_quota(Quota::new(1_000_000, Duration::from_secs(300)));
    let e = b.get_trade_book(&auth()).await.unwrap_err();
    assert!(
        e.client_message().contains("allowance"),
        "{}",
        e.client_message()
    );
    assert_eq!(m.calls("GET", "/v2/fills").len(), 4);

    // Public data does not wait out a reset longer than the ceiling.
    let m = Mock::default();
    m.on_with(
        "GET",
        "/v2/tickers/BTCUSD",
        429,
        &[("x-rate-limit-reset", "120000")],
        "{}",
    );
    let s = serve(&m).await;
    let b = broker(&s).with_quota(Quota::new(1_000_000, Duration::from_secs(300)));
    assert!(b
        .get_quote(&auth(), &QuoteKey::new("CRYPTO", "BTCUSDFUT"))
        .await
        .is_err());
    assert_eq!(m.calls("GET", "/v2/tickers/BTCUSD").len(), 1);
}

#[tokio::test]
async fn the_weighted_quota_paces_buckets_independently() {
    let m = books_mock();
    m.on(
        "GET",
        "/v2/tickers/BTCUSD",
        200,
        fixture("ticker_btcusd.json"),
    );
    // The server reports a spent IP allowance with a long reset.
    m.on(
        "GET",
        "/v2/rate_limits/quota",
        200,
        r#"{"current_quota": 9999, "remaining_time_in_milliseconds": 200000}"#,
    );
    let s = serve(&m).await;
    // Budget 13: one order book (open 3 + history 10) fits, nothing more.
    let b = broker(&s).with_quota(Quota::new(13, Duration::from_secs(300)));
    b.get_order_book(&auth()).await.unwrap();
    assert_eq!(b.quota().snapshot(Bucket::Private).0, 13);
    let calls_before = m.all().len();
    let e = b.get_trade_book(&auth()).await.unwrap_err();
    assert!(e.client_message().contains("allowance"));
    // Refused locally: no request reached Delta.
    assert_eq!(m.all().len(), calls_before);
    // The public bucket is separate: a quote still goes out.
    b.get_quote(&auth(), &QuoteKey::new("CRYPTO", "BTCUSDFUT"))
        .await
        .unwrap();
    // Once the public budget is spent, the exchange is asked before giving up.
    let b = broker(&s).with_quota(Quota::new(3, Duration::from_secs(300)));
    b.get_quote(&auth(), &QuoteKey::new("CRYPTO", "BTCUSDFUT"))
        .await
        .unwrap();
    assert!(b
        .get_quote(&auth(), &QuoteKey::new("CRYPTO", "BTCUSDFUT"))
        .await
        .is_err());
    assert_eq!(m.calls("GET", "/v2/rate_limits/quota").len(), 1);
}

// ---------------------------------------------------------------------------
// The /api/v1 order path with a crypto order
// ---------------------------------------------------------------------------

mod app {
    use super::*;
    use openalgo_desktop_lib::clock::ManualClock;
    use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
    use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
    use openalgo_desktop_lib::services::auth_service::AuthService;
    use openalgo_desktop_lib::services::broker_auth_service::BrokerAuthService;
    use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
    use tower::ServiceExt;

    pub struct App {
        pub ctx: Arc<AppState>,
        pub key: String,
        pub clock: Arc<ManualClock>,
        _dir: tempfile::TempDir,
    }

    impl App {
        pub async fn new(server: &Server) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let symbols = SymbolResolver::new();
            let delta = Arc::new(broker_with(server, symbols.clone()));
            let clock = ManualClock::new(chrono::DateTime::from_timestamp(NOW, 0).unwrap());
            let ctx = AppState::open(
                dir.path(),
                OpenOptions {
                    keystore: Arc::new(MemoryKeyStore::new()),
                    clock: clock.clone(),
                    brokers: Arc::new(BrokerRegistry::with_symbols(
                        symbols.clone(),
                        vec![delta as Arc<dyn Broker>],
                    )),
                },
            )
            .unwrap();
            symbols.load_master(master_contract());
            AuthService::setup(&ctx, "trader", "trader@example.com", "Secret@123").unwrap();
            let key = ApiKeyService::current(&ctx)
                .unwrap()
                .unwrap()
                .expose()
                .to_string();
            BrokerAuthService::persist(
                &ctx,
                &BrokerSession {
                    broker_id: "deltaexchange".into(),
                    auth_token: format!("{}:{}", KEY, SECRET).into(),
                    feed_token: None,
                    user_id: "<USER_ID>".into(),
                    user_name: None,
                    authenticated_at: ctx.now(),
                },
            )
            .unwrap();
            App {
                ctx,
                key,
                clock,
                _dir: dir,
            }
        }

        pub async fn post(&self, path: &str, mut body: Value) -> (StatusCode, Value) {
            body["apikey"] = json!(self.key);
            let mut req = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(std::net::SocketAddr::from((
                    [10, 0, 0, 7],
                    40000,
                ))));
            crate::with_host(&mut req, &self.ctx);
            let resp = openalgo_desktop_lib::server::app(self.ctx.clone())
                .oneshot(req)
                .await
                .unwrap();
            let status = resp.status();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        pub fn set_leverage(&self, v: i64) {
            let conn = self.ctx.sqlite.conn().unwrap();
            openalgo_desktop_lib::db::sqlite::webui::set_leverage(&conn, v).unwrap();
        }

        pub async fn shutdown(self) {
            self.ctx.shutdown().await;
        }
    }
}

fn place_body(symbol: &str, quantity: Value) -> Value {
    json!({
        "strategy": "crypto-test",
        "exchange": "CRYPTO",
        "symbol": symbol,
        "action": "BUY",
        "quantity": quantity,
        "pricetype": "MARKET",
        "product": "NRML",
    })
}

#[tokio::test]
async fn api_places_a_fractional_crypto_order_with_the_saved_leverage() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/products/1600/orders/leverage",
        200,
        fixture("leverage.json"),
    );
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":7300,"product_id":1600}}"#,
    );
    let s = serve(&m).await;
    let app = app::App::new(&s).await;
    app.set_leverage(10);
    let (status, body) = app
        .post("/api/v1/placeorder", place_body("BTCINR", json!(0.0005)))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(body, json!({"status": "success", "orderid": "1600:7300"}));
    // Leverage first, then the order, both signed; the size is exact.
    let seen: Vec<String> = m
        .all()
        .iter()
        .map(|c| format!("{} {}", c.method, c.path))
        .collect();
    assert_eq!(
        seen,
        ["POST /v2/products/1600/orders/leverage", "POST /v2/orders"]
    );
    assert!(m.all().iter().all(Seen::signed_ok));
    assert_eq!(
        m.calls("POST", "/v2/products/1600/orders/leverage")[0].json(),
        json!({"leverage": "10"})
    );
    assert!(m.calls("POST", "/v2/orders")[0]
        .body
        .contains("\"size\":0.0005"));

    // Leverage 0 (broker default) sends no leverage call.
    app.set_leverage(0);
    let (status, _) = app
        .post("/api/v1/placeorder", place_body("BTCINR", json!("0.25")))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        m.calls("POST", "/v2/products/1600/orders/leverage").len(),
        1
    );
    assert!(m.calls("POST", "/v2/orders")[1]
        .body
        .contains("\"size\":0.25"));

    // A derivative refuses a fraction with Delta's message; nothing is sent.
    let before = m.all().len();
    let (status, body) = app
        .post("/api/v1/placeorder", place_body("BTCUSDFUT", json!(1.5)))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    assert!(body["message"].as_str().unwrap().contains("whole numbers"));
    assert_eq!(m.all().len(), before);

    // A whole derivative order goes through as an integer size.
    m.on(
        "POST",
        "/v2/orders",
        200,
        r#"{"success":true,"result":{"id":7301,"product_id":27}}"#,
    );
    let (status, body) = app
        .post("/api/v1/placeorder", place_body("BTCUSDFUT", json!(2)))
        .await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    assert_eq!(
        m.calls("POST", "/v2/orders").pop().unwrap().json()["size"],
        json!(2)
    );
    app.shutdown().await;
}

#[tokio::test]
async fn api_books_report_exact_crypto_sizes_like_the_web() {
    let m = books_mock();
    let s = serve(&m).await;
    let app = app::App::new(&s).await;

    // Positions: web `float(size)`, the fractional BTC balance included,
    // with the contract multiplier as `lot_size`.
    let (status, body) = app.post("/api/v1/positionbook", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{}", body);
    let rows: Vec<(String, String, Value, Value)> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["symbol"].as_str().unwrap().to_string(),
                p["product"].as_str().unwrap().to_string(),
                p["quantity"].clone(),
                p["lot_size"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("BTCUSDFUT".into(), "NRML".into(), json!(6.0), json!(0.001)),
            ("ETHUSDFUT".into(), "NRML".into(), json!(-3.0), json!(0.01)),
            ("BTCINR".into(), "CNC".into(), json!(0.01), json!(1.0)),
            ("ETHINR".into(), "CNC".into(), json!(2.0), json!(1.0)),
        ]
    );

    // Open position: the position book's float, or 0.
    let (_, body) = app
        .post(
            "/api/v1/openposition",
            json!({"strategy": "t", "symbol": "BTCINR", "exchange": "CRYPTO", "product": "CNC"}),
        )
        .await;
    assert_eq!(body, json!({"quantity": 0.01, "status": "success"}));
    let (_, body) = app
        .post(
            "/api/v1/openposition",
            json!({"strategy": "t", "symbol": "BTC27NOV26FUT", "exchange": "CRYPTO", "product": "NRML"}),
        )
        .await;
    assert_eq!(body, json!({"quantity": 0, "status": "success"}));

    // Trades: float sizes; the 0.0005 BTC spot fill is exact.
    let (_, body) = app.post("/api/v1/tradebook", json!({})).await;
    let q: Vec<&Value> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["quantity"])
        .collect();
    assert_eq!(q, [&json!(6.0), &json!(0.0005)]);

    // Orders: the raw size (whole contracts stay integers).
    let (_, body) = app.post("/api/v1/orderbook", json!({})).await;
    let orders = body["data"]["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 6);
    let spot = orders
        .iter()
        .find(|o| o["symbol"] == "BTCINR")
        .expect("spot order");
    assert!(spot["quantity"].is_number(), "{}", spot);
    let fut = orders.iter().find(|o| o["orderid"] == "27:7001").unwrap();
    assert!(fut["quantity"].is_i64(), "{}", fut);
    app.shutdown().await;
}

#[test]
fn closing_a_fractional_crypto_position_sells_its_exact_size() {
    use openalgo_desktop_lib::services::ui_order_service::crypto_close_decision;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    let pos = |sym: &str, product: &str, q: &str| ExactRow {
        row: Position {
            symbol: sym.into(),
            exchange: "CRYPTO".into(),
            product: product.into(),
            quantity: 0,
            overnight_quantity: 0,
            average_price: 0.0,
            ltp: 0.0,
            pnl: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            buy_quantity: 0,
            buy_value: 0.0,
            sell_quantity: 0,
            sell_value: 0.0,
        },
        quantity: Decimal::from_str(q).unwrap(),
    };
    let rows = [
        pos("BTCINR", "CNC", "0.0105"),
        pos("ETHUSDFUT", "NRML", "-3"),
    ];
    assert_eq!(
        crypto_close_decision(&rows, "BTCINR", "CRYPTO", "CNC").unwrap(),
        ("SELL".to_string(), json!("0.0105"))
    );
    assert_eq!(
        crypto_close_decision(&rows, "ETHUSDFUT", "CRYPTO", "NRML").unwrap(),
        ("BUY".to_string(), json!("3"))
    );
    assert!(crypto_close_decision(&rows, "BTCINR", "CRYPTO", "NRML").is_err());
}

#[tokio::test]
async fn api_keeps_every_other_exchange_whole_and_the_sandbox_whole() {
    let m = Mock::default();
    let s = serve(&m).await;
    let app = app::App::new(&s).await;
    // Non-crypto exchanges refuse a fraction at validation, as before.
    let mut nse = place_body("SBIN", json!(1.5));
    nse["exchange"] = json!("NSE");
    let (status, body) = app.post("/api/v1/placeorder", nse).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.to_string()
            .contains("Fractional quantity (1.5) is not allowed for non-crypto exchanges."),
        "{}",
        body
    );
    // The sandbox carries whole units: a fractional crypto order is refused.
    app.ctx.sqlite.set_analyze_mode(true).unwrap();
    let (status, body) = app
        .post("/api/v1/placeorder", place_body("BTCINR", json!(0.0005)))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", body);
    assert!(body["message"].as_str().unwrap().contains("whole-number"));
    assert!(m.all().is_empty());
    app.shutdown().await;
}

#[tokio::test]
async fn capabilities_advertise_crypto_and_leverage() {
    let m = Mock::default();
    let s = serve(&m).await;
    let app = app::App::new(&s).await;
    let b = app.ctx.brokers.get("deltaexchange").unwrap();
    assert_eq!(b.broker_type(), "crypto");
    assert!(b.leverage_config());
    assert_eq!(b.supported_exchanges(), &[Exchange::Crypto]);
    // Every other compiled adapter keeps the defaults.
    let reg = BrokerRegistry::new();
    for id in reg.ids() {
        let a = reg.get(&id).unwrap();
        if id == "deltaexchange" {
            assert!(a.leverage_config());
        } else {
            assert!(!a.leverage_config(), "{}", id);
            assert_eq!(a.broker_type(), "IN_stock", "{}", id);
        }
    }
    app.shutdown().await;
}

// ---------------------------------------------------------------------------
// Crypto trades 24x7: sandbox square-off and the market-hours check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn crypto_is_always_open_for_the_sandbox_and_market_hours() {
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/tickers/BTCUSD",
        200,
        fixture("ticker_btcusd.json"),
    );
    let s = serve(&m).await;
    let app = app::App::new(&s).await;
    assert!(app.ctx.sqlite.is_market_open("CRYPTO").unwrap());
    let cfg = app.ctx.sandbox.config().await.unwrap();
    assert_eq!(cfg.square_off_time("CRYPTO"), None);
    assert!(cfg.square_off_time("NSE").is_some());

    app.ctx.sqlite.set_analyze_mode(true).unwrap();
    // 23:50 IST, well past every Indian square-off time.
    let late = chrono::DateTime::from_timestamp(NOW + 12 * 3600 + 20 * 60, 0).unwrap();
    app.clock.set(late);
    let mut body = place_body("BTCUSDFUT", json!(2));
    body["product"] = json!("MIS");
    let (status, reply) = app.post("/api/v1/placeorder", body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{}", reply);
    assert_eq!(reply["mode"], "analyze");
    let qty = |r: openalgo_desktop_lib::sandbox::replies::OpenPositionReply| r.quantity;
    assert_eq!(
        qty(app
            .ctx
            .sandbox
            .open_position("BTCUSDFUT", "CRYPTO", "MIS")
            .await
            .unwrap()),
        2
    );
    // A sweep at this hour leaves the crypto MIS position open.
    let report = app.ctx.sandbox.square_off_now().await.unwrap();
    assert_eq!(report.closed_positions, 0);
    assert_eq!(
        qty(app
            .ctx
            .sandbox
            .open_position("BTCUSDFUT", "CRYPTO", "MIS")
            .await
            .unwrap()),
        2
    );
    // And new MIS orders are still accepted.
    let (status, reply) = app.post("/api/v1/placeorder", body).await;
    assert_eq!(status, StatusCode::OK, "{}", reply);
    // No live order reached Delta; only the quote was read.
    assert!(m.calls("POST", "/v2/orders").is_empty());
    app.shutdown().await;
}

// ---------------------------------------------------------------------------
// The default exact-size path of whole-unit brokers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn whole_unit_brokers_refuse_fractional_exact_sizes() {
    use openalgo_desktop_lib::brokers::mock::MockBroker;
    let symbols = master();
    let mock = MockBroker::with_symbols("zerodha", symbols.clone());
    let o = resolved(&symbols, "BTCINR", "BUY", "MARKET", 0.0);
    let e = mock
        .place_order_exact(&AuthToken::new("t"), &o, &exact(json!(0.5)))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("whole-number"));
    let r = mock
        .place_order_exact(&AuthToken::new("t"), &o, &exact(json!(3)))
        .await
        .unwrap();
    assert!(!r.order_id.is_empty());
    assert!(mock
        .set_leverage(&AuthToken::new("t"), &o.instrument, 5)
        .await
        .is_err());
}
