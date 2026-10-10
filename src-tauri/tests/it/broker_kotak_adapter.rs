//! Kotak Neo adapter against a local fake Neo (ephemeral port): the two-step
//! TOTP and MPIN sign-in, jData orders, books, quotes by neosymbol, history,
//! margin, funds, the scrip master and the feed host lookup. Payloads are the
//! recorded shapes in `tests/fixtures/brokers/kotak/` (no account data).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::kotak::{self, KotakBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// A scriptable fake HTTP server (routes ending in `*` match a prefix)
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
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(k).map(String::as_str)
    }
    /// The JSON inside a `jData=` form body.
    fn jdata(&self) -> Value {
        let enc = self.body.trim_start_matches("jData=");
        serde_json::from_str(&urlencoding::decode(enc).unwrap()).unwrap()
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// Scripted answers (status, body) per `METHOD path`, in registration order.
type Answers = Vec<(String, VecDeque<(u16, String)>)>;

#[derive(Clone, Default)]
struct Mock {
    routes: Arc<Mutex<Answers>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn on(&self, method: &str, path: &str, status: u16, body: impl Into<String>) {
        let key = format!("{} {}", method, path);
        let mut r = self.routes.lock();
        match r.iter_mut().find(|(k, _)| *k == key) {
            Some((_, q)) => q.push_back((status, body.into())),
            None => r.push((key, VecDeque::from(vec![(status, body.into())]))),
        }
    }

    fn calls(&self, method: &str, path_prefix: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.method == method && s.path.starts_with(path_prefix))
            .cloned()
            .collect()
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
    let key = format!("{} {}", method, uri.path());
    let answer = {
        let mut r = m.routes.lock();
        let idx = r.iter().position(|(k, _)| *k == key).or_else(|| {
            r.iter().position(|(k, _)| {
                k.strip_suffix('*')
                    .is_some_and(|prefix| key.starts_with(prefix))
            })
        });
        idx.and_then(|i| {
            let q = &mut r[i].1;
            if q.len() > 1 {
                q.pop_front()
            } else {
                q.front().cloned()
            }
        })
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
        .join("tests/fixtures/brokers/kotak")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}

fn responses() -> Value {
    serde_json::from_str(&fixture("responses.json")).unwrap()
}

fn master() -> SymbolResolver {
    let r = SymbolResolver::new();
    let mut rows = Vec::new();
    for (key, file) in [
        ("NSE_CM", "nse_cm.csv"),
        ("NSE_FO", "nse_fo.csv"),
        ("BSE_CM", "bse_cm.csv"),
        ("CDE_FO", "cde_fo.csv"),
        ("MCX_FO", "mcx_fo.csv"),
        ("BSE_FO", "bse_fo.csv"),
    ] {
        rows.extend(kotak::master_contract::parse_file(key, &fixture(file)).unwrap());
    }
    r.load(rows);
    r
}

fn broker(s: &Server) -> KotakBroker {
    KotakBroker::with_urls(master(), s.url.clone(), format!("{}/5config/config", s.url))
        .with_retry_base(Duration::from_millis(2))
        .with_history_pacing(Duration::ZERO)
        .with_scrip_master_fallbacks(Vec::new(), format!("{}/cdn", s.url))
}

fn auth(s: &Server) -> AuthToken {
    AuthToken::new(format!(
        "trade-tok:::trade-sid:::{}/:::acc-tok:::E43",
        s.url
    ))
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
// Two-step sign-in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn totp_then_mpin_builds_the_composite_session() {
    let m = Mock::default();
    let s = serve(&m).await;
    let r = responses();
    m.on(
        "POST",
        "/login/1.0/tradeApiLogin",
        200,
        r["login_totp"].to_string(),
    );
    let mut mpin = r["login_mpin"].clone();
    mpin["data"]["baseUrl"] = json!(s.url);
    m.on("POST", "/login/1.0/tradeApiValidate", 200, mpin.to_string());
    m.on("GET", "/5config/config", 200, r["feed_config"].to_string());
    let b = broker(&s);
    let creds = BrokerCredentials {
        api_key: "AB1234".into(),
        api_secret: Some("acc-tok".into()),
        client_id: Some("98765 43210".into()),
        totp: Some("123456".into()),
        password: Some("654321".into()),
        ..Default::default()
    };
    let resp = b.authenticate(creds).await.unwrap();
    assert_eq!(
        resp.auth_token,
        format!("<TRADE_TOKEN>:::<TRADE_SID>:::{}:::acc-tok:::E43", s.url)
    );
    assert_eq!(resp.user_id, "AB1234");
    let step1 = &m.calls("POST", "/login/1.0/tradeApiLogin")[0];
    assert_eq!(step1.header("authorization"), Some("acc-tok"));
    assert_eq!(step1.header("neo-fin-key"), Some("neotradeapi"));
    assert_eq!(
        step1.json(),
        json!({"mobileNumber": "+919876543210", "ucc": "AB1234", "totp": "123456"})
    );
    let step2 = &m.calls("POST", "/login/1.0/tradeApiValidate")[0];
    assert_eq!(step2.header("sid"), Some("<VIEW_SID>"));
    assert_eq!(step2.header("auth"), Some("<VIEW_TOKEN>"));
    assert_eq!(step2.json(), json!({"mpin": "654321"}));
    // The data centre's feed host was looked up and is used by the feed.
    let feed = b
        .create_feed(&AuthToken::new(resp.auth_token.clone()))
        .unwrap();
    assert_eq!(
        feed.ws_request().unwrap().uri().to_string(),
        "wss://sfeed-e43.kotaksecurities.com/apifeed"
    );
    // The two steps are callable on their own for a two-form route.
    let view = kotak::auth::totp_login(&b, "AB1234", "acc-tok", "9876543210", "111111")
        .await
        .unwrap();
    let sess = kotak::auth::validate_mpin(&b, "acc-tok", &view, "654321")
        .await
        .unwrap();
    assert_eq!(sess.data_center, "E43");
}

#[tokio::test]
async fn sign_in_refusals() {
    let m = Mock::default();
    m.on(
        "POST",
        "/login/1.0/tradeApiLogin",
        400,
        responses()["login_error"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let mut creds = BrokerCredentials {
        api_key: "AB1234".into(),
        api_secret: Some("acc-tok".into()),
        client_id: Some("9876543210".into()),
        totp: Some("000000".into()),
        password: Some("654321".into()),
        ..Default::default()
    };
    let e = b.authenticate(creds.clone()).await.unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert!(e.client_message().contains("Invalid TOTP"));
    assert!(m.calls("POST", "/login/1.0/tradeApiValidate").is_empty());
    creds.password = None;
    assert_eq!(
        b.authenticate(creds.clone())
            .await
            .unwrap_err()
            .client_message(),
        "Please provide Mobile Number, TOTP, and MPIN"
    );
    creds.api_secret = None;
    creds.password = Some("1".into());
    assert_eq!(
        b.authenticate(creds).await.unwrap_err().code(),
        "VALIDATION_ERROR"
    );
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orders_post_jdata_forms() {
    let m = Mock::default();
    let r = responses();
    m.on(
        "POST",
        "/quick/order/rule/ms/place",
        200,
        r["place_ok"].to_string(),
    );
    m.on(
        "POST",
        "/quick/order/vr/modify",
        200,
        r["modify_ok"].to_string(),
    );
    m.on(
        "POST",
        "/quick/order/cancel",
        200,
        r["cancel_ok"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let o = b
        .place_order(&auth(&s), &resolved("SBIN", "NSE", 10, "MARKET", "BUY"))
        .await
        .unwrap();
    assert_eq!(o.order_id, "261003000201");
    let p = &m.calls("POST", "/quick/order/rule/ms/place")[0];
    assert_eq!(p.header("sid"), Some("trade-sid"));
    assert_eq!(p.header("auth"), Some("trade-tok"));
    assert_eq!(p.header("neo-fin-key"), Some("neotradeapi"));
    assert_eq!(
        p.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert!(p.body.starts_with("jData=%7B"));
    // `ig` is a fresh `openalgo-<uuid4>` per order (web #2177); pin its shape
    // and compare the rest exactly.
    let mut jd = p.jdata();
    let ig = jd["ig"].as_str().unwrap_or_default().to_string();
    assert!(ig.starts_with("openalgo-") && ig.len() == 45, "{ig}");
    jd["ig"] = json!("openalgo");
    assert_eq!(
        jd,
        json!({"am": "NO", "dq": "0", "es": "nse_cm", "mp": "0", "pc": "MIS", "pf": "N",
               "pr": "0", "pt": "MKT", "qt": "10", "rt": "DAY", "tp": "0", "ts": "SBIN-EQ",
               "tt": "B", "ig": "openalgo"})
    );
    let mreq = ModifyOrderRequest {
        symbol: "NIFTY27OCT2625000CE".into(),
        exchange: "NFO".into(),
        action: "SELL".into(),
        product: "NRML".into(),
        pricetype: "SL".into(),
        quantity: 75,
        price: 117.5,
        trigger_price: 118.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("261003000102", &mreq, &master()).unwrap();
    b.modify_order(&auth(&s), &rm).await.unwrap();
    let mo = m.calls("POST", "/quick/order/vr/modify")[0].jdata();
    assert_eq!(
        (
            mo["tk"].as_str(),
            mo["no"].as_str(),
            mo["pt"].as_str(),
            mo["pr"].as_str(),
            mo["tp"].as_str()
        ),
        (
            Some("52011"),
            Some("261003000102"),
            Some("SL"),
            Some("117.5"),
            Some("118.0")
        )
    );
    b.cancel_order(&auth(&s), "261003000201").await.unwrap();
    assert_eq!(
        m.calls("POST", "/quick/order/cancel")[0].jdata(),
        json!({"on": "261003000201", "am": "NO"})
    );
}

#[tokio::test]
async fn refusals_and_expired_sessions() {
    let m = Mock::default();
    let r = responses();
    m.on(
        "POST",
        "/quick/order/rule/ms/place",
        200,
        r["order_error"].to_string(),
    );
    m.on(
        "GET",
        "/quick/user/positions",
        200,
        r["session_error"].to_string(),
    );
    m.on(
        "GET",
        "/quick/user/orders",
        200,
        r["session_error"].to_string(),
    );
    m.on(
        "POST",
        "/quick/user/limits",
        200,
        r["session_error"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let e = b
        .place_order(&auth(&s), &resolved("SBIN", "NSE", 1, "MARKET", "BUY"))
        .await
        .unwrap_err();
    assert_eq!(
        e.client_message(),
        "Kotak: Market order with Algo Id not allowed"
    );
    assert_eq!(
        b.get_positions(&auth(&s)).await.unwrap_err().code(),
        "AUTH_ERROR"
    );
    assert_eq!(
        b.get_order_book(&auth(&s)).await.unwrap_err().code(),
        "AUTH_ERROR"
    );
    assert_eq!(
        b.get_funds(&auth(&s)).await.unwrap_err().code(),
        "AUTH_ERROR"
    );
    // A session token without the base URL is refused before any call.
    let before = m.seen.lock().len();
    let e = b
        .get_holdings(&AuthToken::new("t:::s::::::a"))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "AUTH_ERROR");
    assert_eq!(m.seen.lock().len(), before);
}

#[tokio::test]
async fn cancel_all_and_close_all() {
    let m = Mock::default();
    let r = responses();
    m.on("GET", "/quick/user/orders", 200, fixture("orders.json"));
    m.on(
        "POST",
        "/quick/order/cancel",
        200,
        r["cancel_ok"].to_string(),
    );
    m.on(
        "GET",
        "/quick/user/positions",
        200,
        fixture("positions.json"),
    );
    m.on(
        "POST",
        "/quick/order/rule/ms/place",
        200,
        r["place_ok"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let c = b.cancel_all_orders(&auth(&s)).await.unwrap();
    assert_eq!(c.cancelled.len(), 2);
    let cancelled: Vec<String> = m
        .calls("POST", "/quick/order/cancel")
        .iter()
        .map(|c| c.jdata()["on"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(cancelled, vec!["261003000102", "261003000104"]);
    let r = b.close_all_positions(&auth(&s)).await.unwrap();
    assert_eq!(r.placed.len(), 3);
    let placed: Vec<Value> = m
        .calls("POST", "/quick/order/rule/ms/place")
        .iter()
        .map(|c| c.jdata())
        .collect();
    assert_eq!(
        (
            placed[0]["tt"].as_str(),
            placed[0]["qt"].as_str(),
            placed[0]["pc"].as_str()
        ),
        (Some("S"), Some("10"), Some("MIS"))
    );
    assert_eq!(
        (placed[1]["tt"].as_str(), placed[1]["qt"].as_str()),
        (Some("B"), Some("150"))
    );
    assert_eq!(
        (placed[2]["tt"].as_str(), placed[2]["ts"].as_str()),
        (Some("S"), Some("NIFTY26O2725000CE"))
    );
    let q = b
        .get_open_position(
            &auth(&s),
            "NIFTY27OCT2625000PE",
            Exchange::Nfo,
            Product::Nrml,
        )
        .await
        .unwrap();
    assert_eq!(q, -150);
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

#[tokio::test]
async fn books_are_openalgo_and_positions_are_priced() {
    let m = Mock::default();
    m.on("GET", "/quick/user/orders", 200, fixture("orders.json"));
    m.on("GET", "/quick/user/trades", 200, fixture("trades.json"));
    m.on(
        "GET",
        "/quick/user/positions",
        200,
        fixture("positions.json"),
    );
    m.on(
        "GET",
        "/portfolio/v1/holdings",
        200,
        fixture("holdings.json"),
    );
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/*",
        200,
        json!([
            {"exchange": "nse_cm", "exchange_token": "3045", "display_symbol": "SBIN-EQ", "ltp": "814.10", "ohlc": {}},
            {"exchange": "nse_fo", "exchange_token": "52012", "display_symbol": "NIFTY26O2725000PE", "ltp": "100.00", "ohlc": {}}
        ])
        .to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let o = b.get_order_book(&auth(&s)).await.unwrap();
    assert_eq!(
        (o.len(), o[1].symbol.as_str(), o[1].status.as_str()),
        (6, "NIFTY27OCT2625000CE", "open")
    );
    let t = b.get_trade_book(&auth(&s)).await.unwrap();
    assert_eq!(t[0].symbol, "SBIN");
    let p = b.get_positions(&auth(&s)).await.unwrap();
    assert_eq!((p[0].ltp, p[0].pnl), (814.1, 17.5));
    assert_eq!((p[1].ltp, p[1].pnl), (100.0, 630.0));
    assert_eq!(p[2].pnl, 0.0);
    // One quotes call for the distinct instruments, keys left literal.
    let q = m.calls("GET", "/script-details/1.0/quotes/neosymbol/");
    assert_eq!(q.len(), 1);
    assert_eq!(
        q[0].path,
        "/script-details/1.0/quotes/neosymbol/nse_cm|3045,nse_fo|52012,nse_fo|52011,mcx_fo|437000/all"
    );
    assert_eq!(q[0].header("authorization"), Some("acc-tok"));
    let h = b.get_holdings(&auth(&s)).await.unwrap();
    assert_eq!((h[0].symbol.as_str(), h[0].pnl), ("SBIN", 2279.54));
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn funds_and_per_leg_margin() {
    let m = Mock::default();
    let r = responses();
    m.on("POST", "/quick/user/limits", 200, fixture("limits.json"));
    m.on(
        "POST",
        "/quick/user/check-margin",
        200,
        r["margin_ok"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let f = b.get_funds(&auth(&s)).await.unwrap();
    assert_eq!((f.available_cash, f.collateral), (179542.8, 222565.5));
    assert_eq!(
        m.calls("POST", "/quick/user/limits")[0].body,
        "jData=%7B%22seg%22%3A%22ALL%22%2C%22exch%22%3A%22ALL%22%2C%22prod%22%3A%22ALL%22%7D"
    );
    let leg = |sym: &str| MarginLeg {
        key: QuoteKey::new("NFO", sym),
        action: Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: PriceType::Limit,
        price: 117.5,
        trigger_price: 0.0,
    };
    let res = b
        .calculate_margin(
            &auth(&s),
            &[leg("NIFTY27OCT2625000CE"), leg("NIFTY27OCT2625000PE")],
        )
        .await
        .unwrap();
    assert_eq!(res.total_margin_required, 196469.1);
    assert_eq!((res.span_margin, res.exposure_margin), (0.0, 0.0));
    let sent = m.calls("POST", "/quick/user/check-margin");
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].jdata(),
        json!({"brkName": "KOTAK", "brnchId": "ONLINE", "exSeg": "nse_fo", "prc": "117.5",
               "prcTp": "L", "prod": "NRML", "qty": "75", "tok": "52011", "trnsTp": "S"})
    );
    let m2 = Mock::default();
    m2.on(
        "POST",
        "/quick/user/check-margin",
        200,
        r["margin_error"].to_string(),
    );
    let s2 = serve(&m2).await;
    let e = broker(&s2)
        .calculate_margin(&auth(&s2), &[leg("NIFTY27OCT2625000CE")])
        .await
        .unwrap_err();
    assert!(e
        .client_message()
        .contains("Scrip not allowed for margin calculation"));
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quotes_depth_and_index_candidates() {
    let m = Mock::default();
    let quotes: Vec<Value> = serde_json::from_str(&fixture("quotes.json")).unwrap();
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/nse_cm|3045/all",
        200,
        json!([quotes[0]]).to_string(),
    );
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/nse_cm|Nifty%20Mid%20Select/all",
        200,
        "[]",
    );
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/nse_cm|Nifty%20Midcap%20Sel/all",
        200,
        json!([quotes[2]]).to_string(),
    );
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/nse_cm|11536/all",
        200,
        responses()["quotes_not_ok"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let q = b
        .get_quote(&auth(&s), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.ask), (814.1, 814.05, 814.1));
    let d = b
        .get_market_depth(&auth(&s), &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!((d.bids.len(), d.total_buy_qty), (5, 634710));
    // An index is probed by name until one answers.
    let idx = b
        .get_quote(&auth(&s), &QuoteKey::new("NSE_INDEX", "MIDCPNIFTY"))
        .await
        .unwrap();
    assert_eq!(idx.ltp, 25012.35);
    assert_eq!(
        m.calls(
            "GET",
            "/script-details/1.0/quotes/neosymbol/nse_cm|Nifty%20Mid"
        )
        .len(),
        2
    );
    // Not_Ok on HTTP 200 is a failure.
    assert!(b
        .get_quote(&auth(&s), &QuoteKey::new("NSE", "TCS"))
        .await
        .is_err());
}

#[tokio::test]
async fn multiquotes_batch_by_25() {
    let m = Mock::default();
    let quotes: Vec<Value> = serde_json::from_str(&fixture("quotes.json")).unwrap();
    m.on(
        "GET",
        "/script-details/1.0/quotes/neosymbol/*",
        200,
        json!([quotes[0], quotes[1]]).to_string(),
    );
    let s = serve(&m).await;
    let mut keys: Vec<QuoteKey> = (0..26).map(|_| QuoteKey::new("NSE", "SBIN")).collect();
    keys.push(QuoteKey::new("NFO", "NIFTY27OCT2625000CE"));
    keys.push(QuoteKey::new("NSE", "TCS"));
    keys.push(QuoteKey::new("NSE", "NOPE"));
    let r = broker(&s).get_multiquotes(&auth(&s), &keys).await.unwrap();
    assert_eq!(r.len(), 29);
    assert!(r[..26]
        .iter()
        .all(|x| x.data.as_ref().unwrap().ltp == 814.1));
    assert_eq!(r[26].data.as_ref().unwrap().oi, 5234175);
    assert_eq!(r[27].error.as_deref(), Some("No quote data available"));
    assert!(r[28]
        .error
        .as_deref()
        .unwrap()
        .contains("NOPE was not found"));
    // 28 resolvable keys: one batch of 25 and one of 3.
    let calls = m.calls("GET", "/script-details/1.0/quotes/neosymbol/");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].path.matches(',').count(), 2);
}

fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

#[tokio::test]
async fn history_chunks_repairs_and_no_data_faults() {
    let m = Mock::default();
    let h: Value = serde_json::from_str(&fixture("history.json")).unwrap();
    m.on(
        "GET",
        "/market-data/1.0/historical/details",
        200,
        h["intraday"].to_string(),
    );
    m.on(
        "GET",
        "/market-data/1.0/historical/details",
        400,
        h["no_data_fault"].to_string(),
    );
    let s = serve(&m).await;
    let b = broker(&s);
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "1m".into(),
        start: d(2026, 8, 1),
        end: d(2026, 9, 14),
    };
    let c = b.get_history(&auth(&s), &req).await.unwrap();
    // Four rows: one too short, one duplicate timestamp.
    assert_eq!(c.len(), 2);
    assert_eq!((c[0].timestamp, c[0].low), (1790826300, 1304.1));
    assert_eq!(c[1].volume, 0);
    let calls = m.calls("GET", "/market-data/1.0/historical/details");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[0].query,
        "neosymbol=nse_cm|3045&fromdate=2026-08-01&todate=2026-08-30&interval=1min"
    );
    assert!(calls[1]
        .query
        .contains("fromdate=2026-08-31&todate=2026-09-14"));
    assert_eq!(calls[0].header("authorization"), Some("acc-tok"));
    // CDS and MCX are quote-only on Neo.
    let mcx = HistoryRequest {
        key: QuoteKey::new("MCX", "CRUDEOIL19OCT26FUT"),
        ..req.clone()
    };
    assert!(b
        .get_history(&auth(&s), &mcx)
        .await
        .unwrap_err()
        .client_message()
        .contains("quote-only"));
}

#[tokio::test]
async fn daily_history_and_failed_candidates() {
    let m = Mock::default();
    let h: Value = serde_json::from_str(&fixture("history.json")).unwrap();
    m.on(
        "GET",
        "/market-data/1.0/historical/details",
        200,
        h["daily"].to_string(),
    );
    let s = serve(&m).await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE_INDEX", "NIFTY"),
        interval: "D".into(),
        start: d(2026, 9, 1),
        end: d(2026, 10, 1),
    };
    let c = broker(&s).get_history(&auth(&s), &req).await.unwrap();
    assert_eq!(c.len(), 2);
    // Both land on 00:00 UTC of their IST dates, today's bar included.
    assert_eq!(c[1].timestamp - c[0].timestamp, 86_400);
    assert_eq!(c[1].timestamp, 1790812800);
    let q = &m.calls("GET", "/market-data/1.0/historical/details")[0].query;
    assert!(q.starts_with("neosymbol=nse_cm|99926000&"));
    assert!(q.ends_with("interval=D"));

    let m2 = Mock::default();
    m2.on(
        "GET",
        "/market-data/1.0/historical/details",
        200,
        h["error"].to_string(),
    );
    let s2 = serve(&m2).await;
    let e = broker(&s2).get_history(&auth(&s2), &req).await.unwrap_err();
    assert!(e
        .client_message()
        .starts_with("Kotak did not return history"));
    // The token, then each Neo name and its upper-case form, were tried.
    let tried: Vec<String> = m2
        .calls("GET", "/market-data/1.0/historical/details")
        .iter()
        .map(|c| c.query.split('&').next().unwrap().to_string())
        .collect();
    assert_eq!(
        tried,
        vec![
            "neosymbol=nse_cm|99926000",
            "neosymbol=nse_cm|Nifty%2050",
            "neosymbol=nse_cm|NIFTY%2050"
        ]
    );
}

// ---------------------------------------------------------------------------
// Master contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn master_contract_from_file_paths() {
    let m = Mock::default();
    let s = serve(&m).await;
    let paths = json!({"data": {"filesPaths": [
        format!("{}/files/transformed-v1/nse_cm-v1.csv", s.url),
        format!("{}/files/transformed/nse_fo.csv", s.url),
        format!("{}/files/transformed-v1/bse_cm-v1.csv", s.url),
        format!("{}/files/transformed/bse_fo.csv", s.url),
        format!("{}/files/transformed/cde_fo.csv", s.url),
        format!("{}/files/transformed/mcx_fo.csv", s.url),
        format!("{}/files/transformed/nse_com.csv", s.url)
    ]}});
    m.on(
        "GET",
        "/script-details/1.0/masterscrip/file-paths",
        200,
        paths.to_string(),
    );
    m.on(
        "GET",
        "/files/transformed-v1/nse_cm-v1.csv",
        200,
        fixture("nse_cm.csv"),
    );
    m.on(
        "GET",
        "/files/transformed/nse_fo.csv",
        200,
        fixture("nse_fo.csv"),
    );
    m.on(
        "GET",
        "/files/transformed-v1/bse_cm-v1.csv",
        200,
        fixture("bse_cm.csv"),
    );
    m.on(
        "GET",
        "/files/transformed/bse_fo.csv",
        200,
        fixture("bse_fo.csv"),
    );
    m.on(
        "GET",
        "/files/transformed/cde_fo.csv",
        200,
        fixture("cde_fo.csv"),
    );
    m.on(
        "GET",
        "/files/transformed/mcx_fo.csv",
        200,
        fixture("mcx_fo.csv"),
    );
    let b = broker(&s);
    let rows = b.download_master_contract(&auth(&s)).await.unwrap();
    assert_eq!(rows.len(), 18);
    assert_eq!(
        m.calls("GET", "/script-details/1.0/masterscrip/file-paths")[0].header("authorization"),
        Some("acc-tok")
    );
    // NSE commodity is downloaded by the web but has no processor: not fetched.
    assert!(m.calls("GET", "/files/transformed/nse_com.csv").is_empty());
}

/// The segment files of the fake listing, by key and path.
fn listed_segments(s: &Server) -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "NSE_CM",
            "/files/transformed-v1/nse_cm-v1.csv",
            "nse_cm.csv",
        ),
        ("NSE_FO", "/files/transformed/nse_fo.csv", "nse_fo.csv"),
        (
            "BSE_CM",
            "/files/transformed-v1/bse_cm-v1.csv",
            "bse_cm.csv",
        ),
        ("BSE_FO", "/files/transformed/bse_fo.csv", "bse_fo.csv"),
        ("CDE_FO", "/files/transformed/cde_fo.csv", "cde_fo.csv"),
        ("MCX_FO", "/files/transformed/mcx_fo.csv", "mcx_fo.csv"),
    ]
    .into_iter()
    .map(|(k, p, f)| (k, format!("{}{}", s.url, p), f))
    .collect()
}

/// MC-02: a Kotak master is all or nothing. A listed segment file that is
/// refused, has no instruments or cannot be read fails the download by
/// name, so the stored master is kept; a required segment missing from the
/// listing fails it too; only the currency and commodity files may be
/// absent from the day's listing.
#[tokio::test]
async fn master_contract_is_all_or_nothing() {
    async fn run(
        skip: Option<&str>,
        replace: Option<(&str, u16, &str)>,
    ) -> openalgo_desktop_lib::error::Result<Vec<openalgo_desktop_lib::brokers::types::SymbolData>>
    {
        let m = Mock::default();
        let s = serve(&m).await;
        let files: Vec<_> = listed_segments(&s)
            .into_iter()
            .filter(|(k, _, _)| Some(*k) != skip)
            .collect();
        let paths = json!({"data": {"filesPaths": files.iter().map(|(_, u, _)| u.clone()).collect::<Vec<_>>()}});
        m.on(
            "GET",
            "/script-details/1.0/masterscrip/file-paths",
            200,
            paths.to_string(),
        );
        for (k, url, file) in &files {
            let path = url.strip_prefix(&s.url).unwrap();
            match replace {
                Some((key, status, body)) if key == *k => m.on("GET", path, status, body),
                _ => m.on("GET", path, 200, fixture(file)),
            }
        }
        broker(&s).download_master_contract(&auth(&s)).await
    }
    let msg = run(None, Some(("NSE_FO", 500, "{}")))
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("Kotak's NSE_FO instrument list"), "{}", msg);
    assert!(msg.contains("existing symbols were kept"), "{}", msg);
    let msg = run(None, Some(("BSE_FO", 200, "not,a,master\n1,2,3\n")))
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("BSE_FO"), "{}", msg);
    let msg = run(None, Some(("CDE_FO", 200, "")))
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("CDE_FO"), "{}", msg);
    let msg = run(Some("BSE_CM"), None)
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("BSE_CM"), "{}", msg);
    // No commodity file listed today: the rest is a complete master.
    let rows = run(Some("MCX_FO"), None).await.unwrap();
    assert!(!rows.is_empty());
    assert!(!rows.iter().any(|r| r.exchange == "MCX"));
    assert!(rows.iter().any(|r| r.exchange == "BFO"));
}

#[tokio::test]
async fn master_contract_falls_back_to_dated_cdn() {
    let m = Mock::default();
    m.on(
        "GET",
        "/script-details/1.0/masterscrip/file-paths",
        500,
        "{}",
    );
    let today = (chrono::Utc::now() + chrono::Duration::seconds(19_800))
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    for (dir, name, file) in [
        ("transformed-v1", "nse_cm-v1", "nse_cm.csv"),
        ("transformed-v1", "bse_cm-v1", "bse_cm.csv"),
        ("transformed", "nse_fo", "nse_fo.csv"),
        ("transformed", "bse_fo", "bse_fo.csv"),
        ("transformed", "cde_fo", "cde_fo.csv"),
        ("transformed", "mcx_fo", "mcx_fo.csv"),
    ] {
        m.on(
            "GET",
            &format!("/cdn/{}/{}/{}.csv", today, dir, name),
            200,
            fixture(file),
        );
    }
    let s = serve(&m).await;
    let rows = broker(&s).download_master_contract(&auth(&s)).await;
    let rows = rows.unwrap();
    assert_eq!(rows.len(), 18);
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE" && r.symbol == "SBIN"));
    let probes = m.calls("GET", "/cdn/");
    assert!(probes
        .iter()
        .any(|p| p.header("range") == Some("bytes=0-0")));
}

#[tokio::test]
async fn registry_lists_kotak_with_two_step_login() {
    let reg = openalgo_desktop_lib::brokers::BrokerRegistry::new();
    let b = reg.get("kotak").unwrap();
    assert_eq!(
        b.login_kind(),
        LoginKind::TwoStep {
            step1: &["mobile", "totp"],
            step2: &["mpin"]
        }
    );
    assert_eq!(b.name(), "Kotak Neo");
}
