//! Dhan and Dhan sandbox adapters against a local fake Dhan (ephemeral port):
//! sign-in (consent flow and pasted token), orders, books, quotes, history,
//! margin, funds, Forever Orders, close-all and the scrip master. Payloads are
//! the recorded shapes in `tests/fixtures/brokers/dhan/` (no account data).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::dhan::{self, DhanBroker, Variant};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// A scriptable fake HTTP server
// ---------------------------------------------------------------------------

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
}

/// Scripted answers (status, body) per `METHOD path`.
type Answers = HashMap<String, VecDeque<(u16, String)>>;

#[derive(Clone, Default)]
struct Mock {
    routes: Arc<Mutex<Answers>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    /// Answer `METHOD path` with `body`. Several answers for one route are
    /// served in order; the last one repeats.
    fn on(&self, method: &str, path: &str, status: u16, body: impl Into<String>) {
        self.routes
            .lock()
            .entry(format!("{} {}", method, path))
            .or_default()
            .push_back((status, body.into()));
    }

    fn calls(&self, method: &str, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }

    fn count(&self) -> usize {
        self.seen.lock().len()
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
        Some((status, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [("content-type", "application/json")],
            body,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
}

/// The server task, aborted when the test drops it.
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
        .join("tests/fixtures/brokers/dhan")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(
        dhan::master_contract::parse_scrip_master(&fixture("api-scrip-master.csv"), Variant::Live)
            .unwrap(),
    );
    r
}

fn broker(server: &Server) -> DhanBroker {
    DhanBroker::with_urls(
        Variant::Live,
        master(),
        server.url.clone(),
        format!("{}/auth", server.url),
        format!("{}/api-data/api-scrip-master.csv", server.url),
    )
    .with_retry_base(Duration::from_millis(1))
    .with_pacing(Duration::ZERO, Duration::ZERO)
}

const TOKEN: &str = "1100012345:::eyJhbGciOiJIUzUxMiJ9.test.sig";

fn auth() -> AuthToken {
    AuthToken::new(TOKEN)
}

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: "1100012345:::app-id-1".into(),
        api_secret: Some("app-secret-1".into()),
        ..Default::default()
    }
}

fn resolved(symbol: &str, exchange: &str, qty: i32, pricetype: &str, side: &str) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: qty,
        price: 0.0,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn consent_flow_generates_consumes_and_validates() {
    let m = Mock::default();
    m.on(
        "POST",
        "/auth/app/generate-consent",
        200,
        json!({"consentAppId": "c-123", "consentAppStatus": "GENERATED", "status": "success"})
            .to_string(),
    );
    m.on(
        "POST",
        "/auth/app/consumeApp-consent",
        200,
        json!({"dhanClientId": "1100012345", "dhanClientName": "TRADER", "dhanClientUcc": "U1",
               "givenPowerOfAttorney": true, "accessToken": "eyJ.consent.token",
               "expiryTime": "2026-10-04T03:00:00"})
        .to_string(),
    );
    m.on("GET", "/v2/fundlimit", 200, fixture("fundlimit.json"));
    let s = serve(&m).await;
    let b = broker(&s);

    let url = dhan::auth::login_url(&b, &creds()).await.unwrap();
    assert_eq!(
        url,
        format!("{}/auth/login/consentApp-login?consentAppId=c-123", s.url)
    );
    let gen = &m.calls("POST", "/auth/app/generate-consent")[0];
    assert_eq!(gen.query, "client_id=1100012345");
    assert_eq!(gen.header("app_id"), Some("app-id-1"));
    assert_eq!(gen.header("app_secret"), Some("app-secret-1"));

    // The callback's tokenId is consumed, then the token is checked.
    let mut c = creds();
    c.request_token = Some("token-id-9".into());
    let resp = b.authenticate(c).await.unwrap();
    assert_eq!(resp.auth_token, "1100012345:::eyJ.consent.token");
    assert_eq!(resp.user_id, "1100012345");
    assert_eq!(resp.user_name.as_deref(), Some("TRADER"));
    let consume = &m.calls("POST", "/auth/app/consumeApp-consent")[0];
    assert_eq!(consume.query, "tokenId=token-id-9");
    assert_eq!(consume.header("app_id"), Some("app-id-1"));
    let check = &m.calls("GET", "/v2/fundlimit")[0];
    assert_eq!(check.header("access-token"), Some("eyJ.consent.token"));
    assert!(!format!("{:?}", resp).contains("consent.token"));
}

#[tokio::test]
async fn consent_refusals_are_trader_facing() {
    let m = Mock::default();
    m.on(
        "POST",
        "/auth/app/generate-consent",
        200,
        json!({"status": "failed"}).to_string(),
    );
    m.on("POST", "/auth/app/consumeApp-consent", 400, "{}");
    let s = serve(&m).await;
    let b = broker(&s);
    let e = dhan::auth::login_url(&b, &creds()).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    let mut c = creds();
    c.request_token = Some("expired".into());
    let e = b.authenticate(c).await.unwrap_err();
    assert!(e.client_message().contains("start the Dhan login again"));
    // No app id in the API key.
    let bad = BrokerCredentials {
        api_key: "1100012345".into(),
        api_secret: Some("s".into()),
        ..Default::default()
    };
    assert_eq!(
        dhan::auth::login_url(&b, &bad).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn pasted_access_token_is_validated_with_fundlimit() {
    let m = Mock::default();
    m.on("GET", "/v2/fundlimit", 200, fixture("fundlimit.json"));
    let s = serve(&m).await;
    let b = broker(&s);
    let jwt = format!("eyJ{}", "a".repeat(120));
    let mut c = creds();
    c.password = Some(jwt.clone());
    let resp = b.authenticate(c).await.unwrap();
    assert_eq!(resp.auth_token, format!("1100012345:::{}", jwt));
    // No consent call for a pasted token.
    assert!(m.calls("POST", "/auth/app/consumeApp-consent").is_empty());

    // A token Dhan refuses.
    let m2 = Mock::default();
    m2.on(
        "GET",
        "/v2/fundlimit",
        401,
        fixture_json("errors.json")["invalid_token"].to_string(),
    );
    let s2 = serve(&m2).await;
    let mut c = creds();
    c.password = Some(jwt);
    let e = broker(&s2).authenticate(c).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn place_modify_cancel_send_the_web_shapes() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/orders",
        200,
        json!({"orderId": "52261003099", "orderStatus": "TRANSIT"}).to_string(),
    );
    m.on(
        "PUT",
        "/v2/orders/52261003099",
        200,
        json!({"orderId": "52261003099", "orderStatus": "TRANSIT"}).to_string(),
    );
    m.on(
        "DELETE",
        "/v2/orders/52261003099",
        202,
        json!({"orderId": "52261003099", "orderStatus": "CANCELLED"}).to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let r = b
        .place_order(&auth(), &resolved("SBIN", "NSE", 10, "MARKET", "BUY"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "52261003099");
    let p = &m.calls("POST", "/v2/orders")[0];
    assert_eq!(
        p.header("access-token"),
        Some("eyJhbGciOiJIUzUxMiJ9.test.sig")
    );
    assert_eq!(p.header("client-id"), Some("1100012345"));
    assert_eq!(p.header("content-type"), Some("application/json"));
    assert_eq!(
        p.json(),
        json!({"dhanClientId": "1100012345", "transactionType": "BUY", "exchangeSegment": "NSE_EQ",
               "productType": "INTRADAY", "orderType": "MARKET", "validity": "DAY",
               "securityId": "3045", "quantity": 10})
    );

    let mreq = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 10,
        price: 811.5,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("52261003099", &mreq, &master()).unwrap();
    assert_eq!(
        b.modify_order(&auth(), &rm).await.unwrap().order_id,
        "52261003099"
    );
    let put = &m.calls("PUT", "/v2/orders/52261003099")[0];
    assert_eq!(put.json()["price"], json!(811.5));
    assert_eq!(put.json()["dhanClientId"], "1100012345");
    assert_eq!(put.header("client-id"), None);
    assert_eq!(
        b.cancel_order(&auth(), "52261003099")
            .await
            .unwrap()
            .order_id,
        "52261003099"
    );
}

#[tokio::test]
async fn refused_orders_carry_dhans_reason() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/orders",
        400,
        fixture_json("errors.json")["order_error"].to_string(),
    );
    m.on(
        "DELETE",
        "/v2/orders/1",
        200,
        fixture_json("errors.json")["invalid_token"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let e = b
        .place_order(&auth(), &resolved("SBIN", "NSE", 1, "MARKET", "BUY"))
        .await
        .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Dhan: Trigger Price should be greater than Price"
    );
    let e = b.cancel_order(&auth(), "1").await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    // Orders need the client id in the session.
    let e = b
        .place_order(
            &AuthToken::new("bare-token"),
            &resolved("SBIN", "NSE", 1, "MARKET", "BUY"),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn cancel_all_cancels_pending_only() {
    let m = Mock::default();
    m.on("GET", "/v2/orders", 200, fixture("orders.json"));
    m.on(
        "DELETE",
        "/v2/orders/52261003002",
        200,
        json!({"orderId": "52261003002"}).to_string(),
    );
    m.on("DELETE", "/v2/orders/52261003006", 500, "{}");
    let s = serve(&m).await;
    let r = broker(&s).cancel_all_orders(&auth()).await.unwrap();
    assert_eq!(r.cancelled, vec!["52261003002".to_string()]);
    assert_eq!(r.failed, vec!["52261003006".to_string()]);
}

#[tokio::test]
async fn close_all_squares_off_open_rows() {
    let m = Mock::default();
    m.on("GET", "/v2/positions", 200, fixture("positions.json"));
    m.on(
        "POST",
        "/v2/orders",
        200,
        json!({"orderId": "X1", "orderStatus": "TRANSIT"}).to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert!(r.failed.is_empty());
    let orders: Vec<Value> = m
        .calls("POST", "/v2/orders")
        .iter()
        .map(Seen::json)
        .collect();
    assert_eq!(orders[0]["transactionType"], "SELL");
    assert_eq!(orders[0]["productType"], "INTRADAY");
    assert_eq!(orders[0]["quantity"], 10);
    assert_eq!(orders[1]["transactionType"], "BUY");
    assert_eq!(orders[1]["securityId"], "35004");
    assert_eq!(orders[1]["productType"], "MARGIN");
    assert_eq!(orders[1]["quantity"], 150);
    // Open position matches segment, product and security id.
    let q = b
        .get_open_position(&auth(), "NIFTY27OCT2625000PE", Exchange::Nfo, Product::Nrml)
        .await
        .unwrap();
    assert_eq!(q, -150);
    let q = b
        .get_open_position(&auth(), "SBIN", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(q, 0);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[tokio::test]
async fn books_are_openalgo_and_priced() {
    let m = Mock::default();
    m.on("GET", "/v2/orders", 200, fixture("orders.json"));
    m.on("GET", "/v2/trades", 200, fixture("trades.json"));
    m.on("GET", "/v2/positions", 200, fixture("positions.json"));
    m.on("GET", "/v2/holdings", 200, fixture("holdings.json"));
    m.on(
        "POST",
        "/v2/marketfeed/quote",
        200,
        json!({"status": "success", "data": {
            "NSE_EQ": {"3045": {"last_price": 814.1}, "1333": {"last_price": 1700.25}},
            "NSE_FNO": {"35004": {"last_price": 100.0}}
        }})
        .to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let orders = b.get_order_book(&auth()).await.unwrap();
    assert_eq!(orders[1].symbol, "NIFTY27OCT2625000CE");
    // Reads send no client-id (web), data calls do.
    assert_eq!(m.calls("GET", "/v2/orders")[0].header("client-id"), None);
    let trades = b.get_trade_book(&auth()).await.unwrap();
    assert_eq!(trades[1].symbol, "NIFTY27OCT2625000PE");
    let pos = b.get_positions(&auth()).await.unwrap();
    assert_eq!((pos[0].ltp, pos[1].ltp, pos[2].ltp), (814.1, 100.0, 0.0));
    let quote_call = &m.calls("POST", "/v2/marketfeed/quote")[0];
    assert_eq!(quote_call.header("client-id"), Some("1100012345"));
    let body = quote_call.json();
    assert!(body["NSE_EQ"].as_array().unwrap().contains(&json!(3045)));
    let h = b.get_holdings(&auth()).await.unwrap();
    assert_eq!(
        (h[0].symbol.as_str(), h[0].ltp, h[0].pnl),
        ("HDFCBANK", 1700.25, 1000.0)
    );
    assert_eq!(h[1].exchange, "BSE");
}

#[tokio::test]
async fn empty_holdings_and_expired_session() {
    let m = Mock::default();
    m.on(
        "GET",
        "/v2/holdings",
        500,
        fixture_json("errors.json")["no_holdings"].to_string(),
    );
    m.on(
        "GET",
        "/v2/orders",
        401,
        fixture_json("errors.json")["invalid_token"].to_string(),
    );
    m.on("GET", "/v2/positions", 200, "[]");
    let s = serve(&m).await;
    let b = broker(&s);
    assert!(b.get_holdings(&auth()).await.unwrap().is_empty());
    assert_eq!(
        b.get_order_book(&auth()).await.unwrap_err().code(),
        "AUTH_ERROR"
    );
    assert!(b.get_positions(&auth()).await.unwrap().is_empty());
    // A close-all on an empty book places nothing.
    let r = b.close_all_positions(&auth()).await.unwrap();
    assert_eq!(r.message(), "No Open Positions Found");
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn funds_from_fundlimit_and_positions() {
    let m = Mock::default();
    m.on("GET", "/v2/fundlimit", 200, fixture("fundlimit.json"));
    m.on("GET", "/v2/positions", 200, fixture("positions.json"));
    let s = serve(&m).await;
    let f = broker(&s).get_funds(&auth()).await.unwrap();
    assert_eq!(
        (f.available_cash, f.collateral, f.utilised_debits),
        (132340.75, 20000.0, 12500.5)
    );
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (-330.0, -312.0));
}

fn leg(symbol: &str, exchange: &str, action: Action) -> MarginLeg {
    MarginLeg {
        key: QuoteKey::new(exchange, symbol),
        action,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Market,
        price: 0.0,
        trigger_price: 0.0,
    }
}

#[tokio::test]
async fn margin_routes_single_and_basket() {
    let m = Mock::default();
    let margin = fixture_json("margin.json");
    m.on(
        "POST",
        "/v2/margincalculator",
        200,
        margin["single"].to_string(),
    );
    m.on(
        "POST",
        "/v2/margincalculator/multi",
        200,
        margin["multi_snake"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let one = b
        .calculate_margin(&auth(), &[leg("NIFTY27OCT2625000CE", "NFO", Action::Sell)])
        .await
        .unwrap();
    assert_eq!(one.total_margin_required, 152340.5);
    let sent = m.calls("POST", "/v2/margincalculator")[0].json();
    assert_eq!(
        sent,
        json!({"dhanClientId": "1100012345", "exchangeSegment": "NSE_FNO", "transactionType": "SELL",
               "quantity": 75, "productType": "MARGIN", "securityId": "35003", "price": 0.0})
    );
    // Two legs (one unknown symbol is skipped, one more valid): basket.
    let two = b
        .calculate_margin(
            &auth(),
            &[
                leg("NIFTY27OCT2625000CE", "NFO", Action::Sell),
                leg("NOPE", "NFO", Action::Buy),
                leg("NIFTY27OCT2625000PE", "NFO", Action::Sell),
            ],
        )
        .await
        .unwrap();
    assert_eq!(two.span_margin, 45000.5);
    let basket = m.calls("POST", "/v2/margincalculator/multi")[0].json();
    assert_eq!(basket["includePosition"], true);
    assert_eq!(basket["includeOrder"], true);
    assert_eq!(basket["scripList"].as_array().unwrap().len(), 2);
    let none = b
        .calculate_margin(&auth(), &[leg("NOPE", "NFO", Action::Buy)])
        .await;
    assert!(none
        .unwrap_err()
        .client_message()
        .starts_with("No valid positions to calculate margin"));
}

#[tokio::test]
async fn margin_errors_with_http_200_and_bad_json() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/margincalculator",
        200,
        fixture_json("errors.json")["margin_error_200"].to_string(),
    );
    m.on(
        "POST",
        "/v2/margincalculator/multi",
        200,
        "<html>gateway</html>",
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let e = b
        .calculate_margin(&auth(), &[leg("NIFTY27OCT2625000CE", "NFO", Action::Sell)])
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Invalid securityId"));
    let e = b
        .calculate_margin(
            &auth(),
            &[
                leg("NIFTY27OCT2625000CE", "NFO", Action::Sell),
                leg("NIFTY27OCT2625000PE", "NFO", Action::Sell),
            ],
        )
        .await
        .unwrap_err();
    assert!(e
        .client_message()
        .starts_with("Invalid response from broker API"));
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quotes_multiquotes_and_depth() {
    let m = Mock::default();
    m.on("POST", "/v2/marketfeed/quote", 200, fixture("quote.json"));
    let s = serve(&m).await;
    let b = broker(&s);
    let q = b
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.ask), (814.1, 814.05, 814.1));
    assert_eq!(
        m.calls("POST", "/v2/marketfeed/quote")[0].json(),
        json!({"NSE_EQ": [3045]})
    );
    let d = b
        .get_market_depth(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((d.bids.len(), d.total_sell_qty), (5, 1290));
    // An instrument Dhan has nothing for: an all-zero quote, as on the web.
    let z = b
        .get_quote(&auth(), &QuoteKey::new("NFO", "NIFTY27OCT2625000PE"))
        .await
        .unwrap();
    assert_eq!(z.ltp, 0.0);
    let keys = vec![
        QuoteKey::new("NSE", "SBIN"),
        QuoteKey::new("NSE_INDEX", "NIFTY"),
        QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        QuoteKey::new("NFO", "NIFTY27OCT2625000PE"),
        QuoteKey::new("NSE", "NOPE"),
    ];
    let r = b.get_multiquotes(&auth(), &keys).await.unwrap();
    assert_eq!(r.len(), 5);
    assert_eq!(r[0].data.as_ref().unwrap().ltp, 814.1);
    assert_eq!(r[1].data.as_ref().unwrap().ltp, 25012.35);
    assert_eq!(r[2].data.as_ref().unwrap().oi, 5234100);
    assert_eq!(r[3].error.as_deref(), Some("No quote data available"));
    assert_eq!(
        r[4].error.as_deref(),
        Some("Could not resolve broker symbol")
    );
    let last = m
        .calls("POST", "/v2/marketfeed/quote")
        .last()
        .unwrap()
        .json();
    assert_eq!(last["IDX_I"], json!([13]));
    assert_eq!(last["NSE_FNO"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn rate_limited_data_calls_are_retried() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/marketfeed/quote",
        200,
        fixture_json("errors.json")["data_rate_limit"].to_string(),
    );
    m.on("POST", "/v2/marketfeed/quote", 200, fixture("quote.json"));
    let s = serve(&m).await;
    let q = broker(&s)
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 814.1);
    assert_eq!(m.calls("POST", "/v2/marketfeed/quote").len(), 2);

    let m2 = Mock::default();
    m2.on(
        "POST",
        "/v2/marketfeed/quote",
        200,
        fixture_json("errors.json")["data_not_subscribed"].to_string(),
    );
    let s2 = serve(&m2).await;
    let e = broker(&s2)
        .get_quote(&auth(), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Data APIs"));
    assert_eq!(m2.count(), 1);
}

fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[tokio::test]
async fn daily_history_uses_historical_endpoint() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/charts/historical",
        200,
        fixture("charts_historical.json"),
    );
    let s = serve(&m).await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "D".into(),
        start: d(2026, 9, 28),
        end: d(2026, 10, 2),
    };
    let c = broker(&s).get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1790899200);
    let sent = m.calls("POST", "/v2/charts/historical")[0].json();
    assert_eq!(
        sent,
        json!({"securityId": "3045", "exchangeSegment": "NSE_EQ", "instrument": "EQUITY",
               "fromDate": "2026-09-28", "toDate": "2026-10-03", "oi": true, "expiryCode": 0})
    );
}

#[tokio::test]
async fn intraday_history_chunks_and_single_day() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/charts/intraday",
        200,
        fixture("charts_intraday.json"),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    // One session: one request, start to start + 1.
    let req = HistoryRequest {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        interval: "5m".into(),
        start: d(2026, 10, 1),
        end: d(2026, 10, 1),
    };
    let c = b.get_history(&auth(), &req).await.unwrap();
    assert_eq!(c.len(), 3);
    let sent = m.calls("POST", "/v2/charts/intraday")[0].json();
    assert_eq!(sent["instrument"], "OPTIDX");
    assert_eq!(sent["interval"], "5");
    assert_eq!(
        (sent["fromDate"].as_str(), sent["toDate"].as_str()),
        (Some("2026-10-01"), Some("2026-10-02"))
    );
    // 200 days: three 90-day chunks sharing their boundary days.
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1h".into(),
        start: d(2026, 1, 5),
        end: d(2026, 7, 24),
    };
    let before = m.calls("POST", "/v2/charts/intraday").len();
    let c = b.get_history(&auth(), &req).await.unwrap();
    // Same three candles from every chunk: deduped.
    assert_eq!(c.len(), 3);
    let calls = m.calls("POST", "/v2/charts/intraday");
    let chunks: Vec<(String, String)> = calls[before..]
        .iter()
        .map(|s| {
            let v = s.json();
            (
                v["fromDate"].as_str().unwrap().to_string(),
                v["toDate"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        chunks,
        vec![
            ("2026-01-05".to_string(), "2026-04-05".to_string()),
            ("2026-04-05".to_string(), "2026-07-04".to_string()),
            ("2026-07-04".to_string(), "2026-07-24".to_string()),
        ]
    );
    let bad = HistoryRequest {
        interval: "3m".into(),
        ..req.clone()
    };
    assert!(b
        .get_history(&auth(), &bad)
        .await
        .unwrap_err()
        .client_message()
        .starts_with("Unsupported interval '3m'"));
}

#[tokio::test]
async fn interior_empty_chunk_is_an_error_not_a_gap() {
    let m = Mock::default();
    let data = fixture("charts_intraday.json");
    let empty =
        json!({"open": [], "high": [], "low": [], "close": [], "volume": [], "timestamp": []})
            .to_string();
    // Chunk 1 has data; chunk 2 is empty on all three attempts; chunk 3 has data.
    m.on("POST", "/v2/charts/intraday", 200, data.clone());
    m.on("POST", "/v2/charts/intraday", 200, empty.clone());
    m.on("POST", "/v2/charts/intraday", 200, empty.clone());
    m.on("POST", "/v2/charts/intraday", 200, empty);
    m.on("POST", "/v2/charts/intraday", 200, data);
    let s = serve(&m).await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: d(2026, 1, 5),
        end: d(2026, 7, 24),
    };
    let e = broker(&s).get_history(&auth(), &req).await.unwrap_err();
    assert!(e.client_message().contains("2026-04-05 to 2026-07-04"));
    assert_eq!(m.calls("POST", "/v2/charts/intraday").len(), 5);
}

// ---------------------------------------------------------------------------
// Forever Orders
// ---------------------------------------------------------------------------

fn gtt_req(kind: GttTriggerType) -> GttRequest {
    GttRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        trigger_type: kind,
        action: Action::Buy,
        product: Product::Cnc,
        quantity: 10,
        pricetype: PriceType::Limit,
        price: 0.0,
        trigger_price: 791.0,
        triggerprice_sl: 0.0,
        stoploss: 0.0,
        triggerprice_tg: 0.0,
        target: 0.0,
        last_price: None,
    }
}

#[tokio::test]
async fn forever_orders_place_modify_cancel_and_book() {
    let m = Mock::default();
    m.on(
        "POST",
        "/v2/forever/orders",
        201,
        json!({"orderId": "5132208051112", "orderStatus": "PENDING"}).to_string(),
    );
    m.on(
        "GET",
        "/v2/forever/orders",
        200,
        fixture("forever_orders.json"),
    );
    m.on(
        "PUT",
        "/v2/forever/orders/5132208051112",
        200,
        json!({"orderId": "5132208051112", "orderStatus": "PENDING"}).to_string(),
    );
    m.on(
        "DELETE",
        "/v2/forever/orders/5132208051112",
        200,
        json!({"orderId": "5132208051112", "orderStatus": "CANCELLED"}).to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let mut req = gtt_req(GttTriggerType::Single);
    req.price = 790.0;
    let r = b.place_gtt(&auth(), &req).await.unwrap();
    assert_eq!(r.trigger_id, "5132208051112");
    let sent = m.calls("POST", "/v2/forever/orders")[0].clone();
    assert_eq!(sent.header("client-id"), Some("1100012345"));
    assert_eq!(sent.json()["orderFlag"], "SINGLE");
    assert_eq!(sent.json()["triggerPrice"], json!(791.0));
    // SINGLE modify reads the stored leg name; LIMIT at price 0 goes as MARKET.
    let r = b
        .modify_gtt(&auth(), "5132208051112", &gtt_req(GttTriggerType::Single))
        .await
        .unwrap();
    assert_eq!(r.trigger_id, "5132208051112");
    let put = m.calls("PUT", "/v2/forever/orders/5132208051112")[0].json();
    assert_eq!(put["legName"], "STOP_LOSS_LEG");
    assert_eq!(put["orderType"], "MARKET");
    // OCO modify is two PUTs, stop-loss leg first.
    let mut oco = gtt_req(GttTriggerType::Oco);
    oco.triggerprice_sl = 780.0;
    oco.stoploss = 779.0;
    oco.triggerprice_tg = 850.0;
    oco.target = 851.0;
    b.modify_gtt(&auth(), "5132208051112", &oco).await.unwrap();
    let puts = m.calls("PUT", "/v2/forever/orders/5132208051112");
    assert_eq!(puts.len(), 3);
    assert_eq!(puts[1].json()["legName"], "STOP_LOSS_LEG");
    assert_eq!(puts[2].json()["legName"], "TARGET_LEG");
    assert_eq!(puts[2].json()["price"], json!(851.0));
    assert_eq!(
        b.cancel_gtt(&auth(), "5132208051112")
            .await
            .unwrap()
            .trigger_id,
        "5132208051112"
    );
    let book = b.get_gtt_book(&auth(), false).await.unwrap();
    assert_eq!(book.len(), 2);
    assert_eq!(book[1].trigger_type, "two-leg");
}

// ---------------------------------------------------------------------------
// Master contract and sandbox
// ---------------------------------------------------------------------------

#[tokio::test]
async fn master_contract_downloads_and_parses() {
    let m = Mock::default();
    m.on(
        "GET",
        "/api-data/api-scrip-master.csv",
        200,
        fixture("api-scrip-master.csv"),
    );
    let s = serve(&m).await;
    let rows = broker(&s).download_master_contract(&auth()).await.unwrap();
    assert_eq!(rows.len(), 24);
    let m2 = Mock::default();
    let s2 = serve(&m2).await;
    assert!(broker(&s2).download_master_contract(&auth()).await.is_err());
}

#[tokio::test]
async fn sandbox_uses_its_host_token_and_chart_quotes() {
    let m = Mock::default();
    m.on("GET", "/v2/fundlimit", 200, fixture("fundlimit.json"));
    m.on("GET", "/v2/orders", 429, "{}");
    m.on("GET", "/v2/orders", 200, fixture("orders.json"));
    m.on(
        "POST",
        "/v2/charts/intraday",
        200,
        fixture("charts_intraday.json"),
    );
    m.on(
        "POST",
        "/v2/orders",
        200,
        json!({"orderId": "S1", "orderStatus": "TRANSIT"}).to_string(),
    );
    let s = serve(&m).await;
    let b = DhanBroker::with_urls(
        Variant::Sandbox,
        master(),
        s.url.clone(),
        format!("{}/auth", s.url),
        format!("{}/master.csv", s.url),
    )
    .with_retry_base(Duration::from_millis(1));
    assert_eq!(b.id(), "dhan_sandbox");
    // The API secret is the access token.
    let token = format!("eyJ{}.x", "b".repeat(60));
    let resp = b
        .authenticate(BrokerCredentials {
            api_key: "2200012345".into(),
            api_secret: Some(token.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(resp.auth_token, format!("2200012345:::{}", token));
    let a = AuthToken::new(resp.auth_token);
    // Reads carry client-id on the sandbox; a 429 is retried.
    let orders = b.get_order_book(&a).await.unwrap();
    assert_eq!(orders.len(), 6);
    let reads = m.calls("GET", "/v2/orders");
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[1].header("client-id"), Some("2200012345"));
    // Quotes come from today's 1-minute candles.
    let q = b
        .get_quote(&a, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (q.ltp, q.open, q.high, q.low, q.volume),
        (814.1, 812.0, 814.2, 811.5, 31301)
    );
    let chart = m.calls("POST", "/v2/charts/intraday")[0].json();
    assert_eq!(chart["interval"], "1");
    // SL-M goes as a bare STOP_LOSS_MARKET on the sandbox.
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "SELL".into(),
        quantity: 1,
        price: 0.0,
        order_type: "SL-M".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(800.0),
        disclosed_quantity: None,
        amo: false,
    };
    b.place_order(&a, &ResolvedOrder::resolve(&req, &master()).unwrap())
        .await
        .unwrap();
    let sent = m.calls("POST", "/v2/orders")[0].json();
    assert_eq!(sent["orderType"], "STOP_LOSS_MARKET");
    assert!(b
        .place_gtt(&a, &gtt_req(GttTriggerType::Single))
        .await
        .is_err());
    assert!(b.create_feed(&a).is_err());
}

#[tokio::test]
async fn registry_lists_dhan_and_dhan_sandbox() {
    let reg = openalgo_desktop_lib::brokers::BrokerRegistry::new();
    let ids = reg.ids();
    assert!(ids.contains(&"dhan".to_string()));
    assert!(ids.contains(&"dhan_sandbox".to_string()));
    let b = reg.get("dhan").unwrap();
    assert_eq!(b.login_kind(), LoginKind::Redirect { param: "tokenId" });
    assert_eq!(b.capabilities().depth_levels, &[5, 20]);
}
