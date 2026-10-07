//! Paytm Money against a local fake: token exchange, order bodies (with
//! the book lookups modify and cancel need), cancel-all, close-all, open
//! position, normalised books, funds with P&L from positions, quotes and
//! multiquotes via `pref` strings, depth, read retries, history empty,
//! and the security master download.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::paytm::master_contract::parse_security_master;
use openalgo_desktop_lib::brokers::paytm::PaytmBroker;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../fixtures/brokers/paytm/", $name))
    };
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    jwt: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    /// Next N live-price reads answer 503.
    live_failures: AtomicUsize,
    /// Every order-book read answers 401.
    expired: AtomicUsize,
}

impl Fake {
    fn calls(&self, method: &str, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path == path)
            .cloned()
            .collect()
    }
}

fn ok(v: impl ToString) -> Response {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn status(code: StatusCode, v: Value) -> Response {
    (code, [("content-type", "application/json")], v.to_string()).into_response()
}

fn live(query: &str) -> Response {
    let pref = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("pref="))
        .map(|p| urlencoding::decode(p).unwrap().into_owned())
        .unwrap_or_default();
    let tpl: Value = serde_json::from_str(fixture!("live_price.json")).unwrap();
    let base = tpl["data"][0].clone();
    let rows: Vec<Value> = pref
        .split(',')
        .filter_map(|p| p.split(':').nth(1))
        .map(|id| {
            let mut r = base.clone();
            r["security_id"] = json!(id.parse::<u64>().unwrap_or(0));
            r
        })
        .collect();
    ok(json!({"status": "success", "data": rows}))
}

fn route(fake: &Fake, method: &str, path: &str, query: &str, body: &Value) -> Response {
    match (method, path) {
        ("POST", "/accounts/v2/gettoken") => match body["request_token"].as_str() {
            Some("bad") => status(
                StatusCode::UNAUTHORIZED,
                json!({"errors": [{"message": "Invalid request token"}]}),
            ),
            Some("nopub") => ok(json!({"access_token": "acc-only"})),
            _ => ok(json!({
                "access_token": "acc-1",
                "public_access_token": "pub-1",
                "read_access_token": "read-1"
            })),
        },
        ("GET", "/orders/v1/user/orders") => {
            if fake.expired.load(Ordering::SeqCst) > 0 {
                return status(
                    StatusCode::UNAUTHORIZED,
                    json!({"status": "error", "message": "Session expired"}),
                );
            }
            ok(fixture!("orders.json"))
        }
        ("GET", "/orders/v1/position") => ok(fixture!("positions.json")),
        ("GET", "/holdings/v1/get-user-holdings-data") => ok(fixture!("holdings.json")),
        ("GET", "/accounts/v1/funds/summary") => ok(fixture!("funds.json")),
        ("GET", "/data/v1/price/live") => {
            if fake
                .live_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return status(StatusCode::SERVICE_UNAVAILABLE, json!({"message": "busy"}));
            }
            live(query)
        }
        ("POST", "/orders/v1/place/regular") => {
            if body["security_id"] == "880001" {
                return status(
                    StatusCode::BAD_REQUEST,
                    json!({"status": "error", "message": "Margin exceeds available funds"}),
                );
            }
            if body["security_id"] == "500112" {
                return status(StatusCode::BAD_GATEWAY, json!({}));
            }
            ok(json!({"status": "success", "data": [{"order_no": "261003000009999"}]}))
        }
        ("POST", "/orders/v1/modify/regular") => {
            ok(json!({"status": "success", "data": [{"order_no": body["order_no"]}]}))
        }
        ("POST", "/orders/v1/cancel/regular") => {
            ok(json!({"status": "success", "data": [{"order_no": body["order_no"]}]}))
        }
        ("GET", "/master.csv") => ok(fixture!("security_master.csv")),
        _ => status(
            StatusCode::NOT_FOUND,
            json!({"message": "no such endpoint"}),
        ),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let seen = Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or("").to_string(),
                    jwt: headers
                        .get("x-jwt-token")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string(),
                    body: body.clone(),
                };
                fake.seen.lock().push(seen.clone());
                route(&fake, &seen.method, &seen.path, &seen.query, &body)
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(parse_security_master(fixture!("security_master.csv")).unwrap());
    r
}

async fn setup() -> (PaytmBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = PaytmBroker::with_base_url(
        master(),
        host.clone(),
        format!("{}/master.csv", host),
        "ws://127.0.0.1:1",
    )
    .with_retry_delay(Duration::from_millis(5));
    (b, fake, AuthToken::new("acc-1").with_feed(Some("pub-1")))
}

fn order(
    symbol: &str,
    exchange: &str,
    side: &str,
    pricetype: &str,
    product: &str,
) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: side.into(),
        quantity: 10,
        price: if pricetype == "MARKET" { 0.0 } else { 800.0 },
        order_type: pricetype.into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: Some(799.0),
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

fn creds(token: &str) -> BrokerCredentials {
    BrokerCredentials {
        api_key: "<APIKEY>".into(),
        api_secret: Some("secret".into()),
        request_token: Some(token.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn paytm_login_exchanges_the_request_token() {
    let (b, fake, _) = setup().await;
    let r = b.authenticate(creds("rt-1")).await.unwrap();
    assert_eq!(r.auth_token, "acc-1");
    assert_eq!(r.feed_token.as_deref(), Some("pub-1"));
    let call = &fake.calls("POST", "/accounts/v2/gettoken")[0];
    assert_eq!(
        call.body,
        json!({"api_key": "<APIKEY>", "api_secret_key": "secret", "request_token": "rt-1"})
    );
    // No public token: the access token serves the feed too.
    let r = b.authenticate(creds("nopub")).await.unwrap();
    assert_eq!(r.feed_token.as_deref(), Some("acc-only"));
    let e = b.authenticate(creds("bad")).await.unwrap_err();
    assert!(matches!(e, AppError::Auth(_)));
    assert!(e.client_message().contains("did not accept the login"));
    let mut missing = creds("x");
    missing.api_secret = None;
    assert!(matches!(
        b.authenticate(missing).await.unwrap_err(),
        AppError::Validation(_)
    ));
}

#[tokio::test]
async fn paytm_place_orders_send_the_transform_data_body() {
    let (b, fake, auth) = setup().await;
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "BUY", "MARKET", "MIS"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "261003000009999");
    let r = b
        .place_order(
            &auth,
            &order("NIFTY06OCT2624000CE", "NFO", "SELL", "SL", "NRML"),
        )
        .await
        .unwrap();
    assert_eq!(r.order_id, "261003000009999");
    let calls = fake.calls("POST", "/orders/v1/place/regular");
    assert_eq!(calls[0].jwt, "acc-1");
    assert_eq!(
        calls[0].body,
        json!({
            "security_id": "3045", "exchange": "NSE", "txn_type": "B", "order_type": "MKT",
            "quantity": 10, "product": "I", "price": 0.0, "validity": "DAY",
            "segment": "E", "source": "M"
        })
    );
    let sl = &calls[1].body;
    assert_eq!(
        (
            &sl["exchange"],
            &sl["segment"],
            &sl["order_type"],
            &sl["product"]
        ),
        (&json!("NSE"), &json!("D"), &json!("SL"), &json!("M"))
    );
    assert_eq!(sl["trigger_price"], 799.0);
    // Broker refusal carries the broker's reason.
    let e = b
        .place_order(
            &auth,
            &order("SENSEX29OCT26FUT", "BFO", "BUY", "LIMIT", "NRML"),
        )
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Margin exceeds available funds");
    // A server error on a write is not retried (no duplicate orders).
    let before = fake.calls("POST", "/orders/v1/place/regular").len();
    assert!(b
        .place_order(&auth, &order("SBIN", "BSE", "BUY", "LIMIT", "CNC"))
        .await
        .is_err());
    assert_eq!(
        fake.calls("POST", "/orders/v1/place/regular").len(),
        before + 1
    );
}

fn modify_req(order_id: &str, pricetype: &str) -> ResolvedModify {
    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: pricetype.into(),
        quantity: 20,
        price: 801.5,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    ResolvedModify::resolve(order_id, &m, &master()).unwrap()
}

#[tokio::test]
async fn paytm_modify_and_cancel_look_up_the_book_row() {
    let (b, fake, auth) = setup().await;
    let r = b
        .modify_order(&auth, &modify_req("261003000000101", "LIMIT"))
        .await
        .unwrap();
    assert_eq!(r.order_id, "261003000000101");
    let body = &fake.calls("POST", "/orders/v1/modify/regular")[0].body;
    assert_eq!(body["serial_no"], 1);
    assert_eq!(body["group_id"], 8);
    assert_eq!(body["security_id"], "3045");
    assert_eq!(body["quantity"], 20);
    assert_eq!(body["price"], 801.5);
    assert_eq!(body["source"], "N");
    // A rejected order cannot be modified; an unknown one is not found.
    let e = b
        .modify_order(&auth, &modify_req("261003000000103", "LIMIT"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("cannot be modified"));
    assert!(matches!(
        b.modify_order(&auth, &modify_req("nope", "LIMIT"))
            .await
            .unwrap_err(),
        AppError::NotFound(_)
    ));

    let r = b.cancel_order(&auth, "261003000000101").await.unwrap();
    assert_eq!(r.order_id, "261003000000101");
    let c = &fake.calls("POST", "/orders/v1/cancel/regular")[0].body;
    assert_eq!(c["order_no"], "261003000000101");
    assert_eq!(c["mkt_type"], "NL");
    assert_eq!(c["txn_type"], "B");
    // Only `Pending` orders are cancellable.
    assert!(b.cancel_order(&auth, "261003000000102").await.is_err());
    let all = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(all.cancelled, ["261003000000101"]);
    assert!(all.failed.is_empty());
}

#[tokio::test]
async fn paytm_close_all_and_open_position() {
    let (b, fake, auth) = setup().await;
    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert!(r.failed.is_empty());
    let calls = fake.calls("POST", "/orders/v1/place/regular");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].body["txn_type"], "S");
    assert_eq!(calls[0].body["quantity"], 10);
    assert_eq!(calls[1].body["txn_type"], "B");
    assert_eq!(calls[1].body["quantity"], 75);
    assert_eq!(calls[1].body["segment"], "D");
    assert_eq!(calls[1].body["product"], "M");

    let q = |s: &'static str, e, p| {
        let b = &b;
        let auth = &auth;
        async move { b.get_open_position(auth, s, e, p).await.unwrap() }
    };
    assert_eq!(q("SBIN", Exchange::Nse, Product::Mis).await, 10);
    assert_eq!(q("SBIN", Exchange::Nse, Product::Cnc).await, 0);
    assert_eq!(
        q("NIFTY06OCT2624000CE", Exchange::Nfo, Product::Nrml).await,
        -75
    );
    assert_eq!(q("SENSEX29OCT26FUT", Exchange::Bfo, Product::Nrml).await, 0);
}

#[tokio::test]
async fn paytm_books_and_funds_are_normalised() {
    let (b, _fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders.len(), 5);
    assert_eq!(orders[1].symbol, "NIFTY06OCT2624000CE");
    assert_eq!(orders[1].exchange, "NFO");
    assert!(orders.iter().all(|o| o.status == o.status.to_lowercase()));
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades.len(), 1);
    let positions = b.get_positions(&auth).await.unwrap();
    assert_eq!(positions[2].exchange, "BFO");
    assert_eq!(positions[2].symbol, "SENSEX29OCT26FUT");
    let holdings = b.get_holdings(&auth).await.unwrap();
    assert_eq!(holdings[0].symbol, "RELIANCE");
    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 98500.76);
    assert_eq!((f.m2m_realized, f.m2m_unrealized), (1250.0, 285.0));
    assert_eq!(f.collateral, 2500.0);
}

#[tokio::test]
async fn paytm_quotes_depth_and_multiquotes() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 810.25);
    let call = &fake.calls("GET", "/data/v1/price/live")[0];
    assert_eq!(call.query, "mode=FULL&pref=NSE%3A13%3AINDEX");
    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NFO", "NIFTY06OCT2624000CE"))
        .await
        .unwrap();
    assert_eq!(d.bids[0].price, 810.2);
    assert_eq!(
        fake.calls("GET", "/data/v1/price/live")[1].query,
        "mode=FULL&pref=NSE%3A43210%3AOPTION"
    );

    let before = fake.calls("GET", "/data/v1/price/live").len();
    let mut keys: Vec<QuoteKey> = (0..100).map(|_| QuoteKey::new("NSE", "SBIN")).collect();
    keys.push(QuoteKey::new("BFO", "SENSEX29OCT26FUT"));
    keys.push(QuoteKey::new("NSE", "NOPE"));
    let res = b.get_multiquotes(&auth, &keys).await.unwrap();
    assert_eq!(res.len(), 102);
    // 101 resolved keys -> two batches of at most 100 prefs.
    let calls = fake.calls("GET", "/data/v1/price/live");
    assert_eq!(calls.len() - before, 2);
    assert!(calls
        .last()
        .unwrap()
        .query
        .ends_with("BSE%3A880001%3AFUTURE"));
    assert!(res[..101].iter().all(|r| r.data.is_some()));
    assert_eq!(res[101].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(res[100].exchange, "BFO");
}

#[tokio::test]
async fn paytm_reads_are_retried_on_server_errors() {
    let (b, fake, auth) = setup().await;
    fake.live_failures.store(2, Ordering::SeqCst);
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 810.25);
    assert_eq!(fake.calls("GET", "/data/v1/price/live").len(), 3);
    fake.live_failures.store(3, Ordering::SeqCst);
    let e = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("not responding normally"));
    // An expired session is reported as such, not retried.
    fake.expired.store(1, Ordering::SeqCst);
    let before = fake.calls("GET", "/orders/v1/user/orders").len();
    let e = b.get_order_book(&auth).await.unwrap_err();
    assert!(matches!(e, AppError::Auth(_)));
    assert_eq!(
        fake.calls("GET", "/orders/v1/user/orders").len(),
        before + 1
    );
}

#[tokio::test]
async fn paytm_history_is_empty_and_master_downloads() {
    let (b, _fake, auth) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "D".into(),
        start: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
    };
    // web: an empty frame, not an error.
    assert!(b.get_history(&auth, &req).await.unwrap().is_empty());
    assert!(!b.capabilities().history);
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(rows.len(), 17);
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY" && r.exchange == "NSE_INDEX"));
}
