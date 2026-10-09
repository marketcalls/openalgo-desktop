//! Tradejini adapter suite against a local fake broker: an axum REST server
//! (sign-in, OMS forms, books, funds, history, symbol store) and a
//! NxtradStream WebSocket server that answers L1/L5 subscriptions with
//! binary frames, on ephemeral ports.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::tradejini::streaming::build::{self, V};
use openalgo_desktop_lib::brokers::tradejini::{TradejiniBroker, WsTimings};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/tradejini/", $name))
    };
}

const KEY: &str = "APPKEY0123456789";
const ACCESS: &str = "access-xyz";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    authorization: String,
    form: Vec<(String, String)>,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    ws_open: AtomicUsize,
    ws_total: AtomicUsize,
    ws_frames: Mutex<Vec<Value>>,
    /// Symbol-store groups that answer 500.
    down_groups: Mutex<Vec<&'static str>>,
}

impl Fake {
    fn calls(&self, path_end: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path.ends_with(path_end))
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

fn form(body: &[u8]) -> Vec<(String, String)> {
    serde_urlencoded::from_bytes(body).unwrap_or_default()
}

fn route(fake: &Fake, s: &Seen) -> Response {
    let get = |k: &str| {
        s.form
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let path = s.path.trim_start_matches("/v2");
    if path == "/api-gw/oauth/individual-token-v2" {
        if s.authorization != format!("Bearer {}", KEY) {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
        if get("password") == "1234" && get("twoFa") == "123456" && get("twoFaTyp") == "totp" {
            return ok(
                json!({"scope":"general","access_token":ACCESS,"token_type":"bearer","expires_in":86400}),
            );
        }
        return (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json")],
            r#"{"s":"error","msg":"Unauthorized"}"#,
        )
            .into_response();
    }
    if path.starts_with("/api/mkt-data/scrips/symbol-store") {
        let group = path.rsplit('/').next().unwrap_or("");
        if fake.down_groups.lock().contains(&group) {
            return (StatusCode::INTERNAL_SERVER_ERROR, "down").into_response();
        }
        return match group {
            "symbol-store" => ok(fixture!("symbol_store.json")),
            // A group with nothing in it today (header only): skipped.
            "CommodityOptions" => {
                "id,dispName,excToken,lot,tick,symbol,expiry,strike,optType,asset\n".into_response()
            }
            "Securities" => fixture!("Securities.csv").into_response(),
            "FutureContracts" => fixture!("FutureContracts.csv").into_response(),
            "NSEOptions" => fixture!("NSEOptions.csv").into_response(),
            "Index" => fixture!("Index.csv").into_response(),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "down").into_response(),
        };
    }
    if s.authorization != format!("Bearer {}:{}", KEY, ACCESS) {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    match (s.method.as_str(), path) {
        ("POST", "/api/oms/place-order") => {
            if get("side") == "sell" && get("symId") == "EQT_RELIANCE_EQ_NSE" {
                (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    r#"{"s":"error","msg":"RMS: insufficient holdings"}"#,
                )
                    .into_response()
            } else {
                ok(json!({"s":"ok","d":{"orderId":"9001","msg":"Order placed"}}))
            }
        }
        ("PUT", "/api/oms/modify-order") => ok(json!({"s":"ok","d":{"orderId": get("orderId")}})),
        ("DELETE", "/api/oms/cancel-order") => {
            if s.query.contains("26100300002") {
                ok(json!({"s":"error","msg":"Order already in final state"}))
            } else {
                ok(json!({"s":"ok","d":{"orderId": s.query.trim_start_matches("orderId=")}}))
            }
        }
        ("GET", "/api/oms/orders") => ok(fixture!("orders.json")),
        ("GET", "/api/oms/trades") => ok(fixture!("trades.json")),
        ("GET", "/api/oms/positions") => ok(fixture!("positions.json")),
        ("GET", "/api/oms/holdings") => ok(fixture!("holdings.json")),
        ("GET", "/api/oms/limits") => ok(fixture!("limits.json")),
        ("GET", "/api/mkt-data/chart/interval-data") => ok(fixture!("history.json")),
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn serve_rest(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let s = Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or("").to_string(),
                    authorization: headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string(),
                    form: form(&body),
                };
                fake.seen.lock().push(s.clone());
                route(&fake, &s)
            }
        },
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    });
    format!("http://{}/v2", addr)
}

fn seg_of(exch: &str) -> u8 {
    match exch {
        "NSE" => 1,
        "BSE" => 2,
        "NFO" => 3,
        _ => 1,
    }
}

/// Binary answer to one subscription request.
fn answer(req: &Value) -> Option<Vec<u8>> {
    let feed = req["type"].as_str()?;
    let mut packets = vec![build::auth(1)];
    for t in req["tokens"].as_array()? {
        let (tok, exch) = t["t"].as_str()?.split_once('_')?;
        let tok: i32 = tok.parse().ok()?;
        let seg = seg_of(exch);
        match (feed, tok) {
            // Never answered: multiquote reports "No data received".
            (_, 99999) => {}
            // Index: no bid/ask, so the quote is never "complete".
            ("L1", 26000) => packets.push(build::packet(
                10,
                &[
                    (26, V::U8(seg)),
                    (27, V::I32(tok)),
                    (29, V::I32(2_501_235)),
                    (33, V::I32(2_490_000)),
                ],
            )),
            ("L1", _) => packets.push(build::l1_full(
                seg,
                tok,
                80_150,
                80_000,
                80_500,
                79_900,
                79_800,
                123_456,
                80_145,
                80_155,
                if exch == "NFO" { Some(1500) } else { None },
            )),
            ("L5", _) => packets.push(build::l5(
                seg,
                tok,
                &[(80_145, 100, 2), (80_140, 50, 1)],
                &[(80_155, 70, 3)],
            )),
            _ => {}
        }
    }
    Some(build::frame(&packets, true))
}

async fn serve_ws(fake: Arc<Fake>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let fake = fake.clone();
            tokio::spawn(async move {
                let check = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                             resp| {
                    let q = req.uri().query().unwrap_or("");
                    if q == format!("token={}:{}&version=3.1", KEY, ACCESS) {
                        Ok(resp)
                    } else {
                        let mut r =
                            tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(
                                None,
                            );
                        *r.status_mut() =
                            tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                        Err(r)
                    }
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, check).await else {
                    return;
                };
                fake.ws_open.fetch_add(1, Ordering::SeqCst);
                fake.ws_total.fetch_add(1, Ordering::SeqCst);
                while let Some(Ok(m)) = ws.next().await {
                    match m {
                        Message::Text(t) => {
                            assert!(t.ends_with('\n'), "requests end with a newline");
                            let v: Value = serde_json::from_str(t.trim_end()).unwrap();
                            fake.ws_frames.lock().push(v.clone());
                            if let Some(b) = answer(&v) {
                                if ws.send(Message::Binary(b)).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                fake.ws_open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    format!("ws://{}/v2.1/stream", addr)
}

fn timings() -> WsTimings {
    WsTimings {
        connect: Duration::from_secs(3),
        quote_settle: Duration::from_millis(10),
        multi_settle: Duration::from_millis(10),
        quote_step: Duration::from_millis(200),
        quote_steps: 3,
        depth_wait: Duration::from_millis(800),
        multi_per_symbol: Duration::from_millis(10),
        multi_min: Duration::from_millis(150),
        multi_max: Duration::from_millis(600),
    }
}

struct Env {
    fake: Arc<Fake>,
    broker: TradejiniBroker,
    auth: AuthToken,
}

async fn env() -> Env {
    let fake = Arc::new(Fake::default());
    let base = serve_rest(fake.clone()).await;
    let ws = serve_ws(fake.clone()).await;
    let symbols = SymbolResolver::new();
    let broker = TradejiniBroker::with_urls(symbols.clone(), base, ws).with_timings(timings());
    let auth = AuthToken::new(format!("{}:{}", KEY, ACCESS));
    let rows = broker.download_master_contract(&auth).await.unwrap();
    symbols.load(rows);
    Env { fake, broker, auth }
}

async fn sockets_closed(fake: &Fake) {
    for _ in 0..100 {
        if fake.ws_open.load(Ordering::SeqCst) == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("quote socket left open");
}

#[tokio::test]
async fn sign_in_with_pin_and_totp() {
    let e = env().await;
    let creds = BrokerCredentials {
        api_key: KEY.into(),
        password: Some("1234".into()),
        totp: Some("123456".into()),
        ..Default::default()
    };
    let r = e.broker.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, format!("{}:{}", KEY, ACCESS));
    assert!(r.feed_token.is_none());
    let call = &e.fake.calls("individual-token-v2")[0];
    assert_eq!(call.authorization, format!("Bearer {}", KEY));
    // The API secret is never sent.
    assert!(!call.form.iter().any(|(k, _)| k.contains("secret")));

    let mut bad = creds.clone();
    bad.totp = Some("000000".into());
    let err = e.broker.authenticate(bad).await.unwrap_err();
    assert!(err.client_message().contains("CubePlus login PIN"));

    let mut missing = creds;
    missing.password = None;
    assert!(e.broker.authenticate(missing).await.is_err());
}

#[tokio::test]
async fn master_contract_download() {
    let e = env().await;
    let s = e.broker.symbols().unwrap();
    assert!(s.by_symbol("NSE", "SBIN").is_some());
    assert_eq!(
        s.by_symbol("NFO", "NIFTY27OCT26FUT").unwrap().expiry,
        "27-OCT-26"
    );
    assert!(s.by_symbol("NSE_INDEX", "INDIAVIX").is_some());
    // The groups are unauthenticated and asked for version 0.
    let g = e.fake.calls("/Securities");
    assert_eq!(g[0].query, "version=0");
    assert_eq!(g[0].authorization, "");
    // CommodityOptions sent a header and no rows: skipped, not an error.
    assert_eq!(e.fake.calls("/CommodityOptions").len(), 1);
}

/// Web #2198: a scrip group that fails refuses the whole download (the
/// stored master is kept) instead of being skipped.
#[tokio::test]
async fn master_contract_download_fails_on_a_failed_group() {
    let fake = Arc::new(Fake::default());
    fake.down_groups.lock().push("NSEOptions");
    let base = serve_rest(fake.clone()).await;
    let broker = TradejiniBroker::with_urls(SymbolResolver::new(), base, "ws://127.0.0.1:1");
    let auth = AuthToken::new(format!("{}:{}", KEY, ACCESS));
    let msg = broker
        .download_master_contract(&auth)
        .await
        .unwrap_err()
        .client_message();
    assert!(
        msg.contains("Could not download the Tradejini NSEOptions symbols"),
        "{}",
        msg
    );
    assert!(msg.contains("existing symbols were kept"), "{}", msg);
}

#[tokio::test]
async fn orders_forms_and_responses() {
    let e = env().await;
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 10,
        price: 0.0,
        order_type: "MARKET".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    let o = ResolvedOrder::resolve(&req, e.broker.symbols().unwrap()).unwrap();
    let r = e.broker.place_order(&e.auth, &o).await.unwrap();
    assert_eq!(r.order_id, "9001");
    let call = &e.fake.calls("place-order")[0];
    assert!(call
        .form
        .contains(&("symId".into(), "EQT_SBIN_EQ_NSE".into())));
    assert!(call.form.contains(&("mktProt".into(), "2".into())));
    assert!(call.form.contains(&("side".into(), "buy".into())));

    // An error envelope (with a 400) becomes the broker's message.
    let mut sell = req.clone();
    sell.symbol = "RELIANCE".into();
    sell.side = "SELL".into();
    sell.product = "CNC".into();
    let o = ResolvedOrder::resolve(&sell, e.broker.symbols().unwrap()).unwrap();
    let err = e.broker.place_order(&e.auth, &o).await.unwrap_err();
    assert_eq!(err.client_message(), "RMS: insufficient holdings");

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "MIS".into(),
        pricetype: "LIMIT".into(),
        quantity: 12,
        price: 801.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("26100300001", &m, e.broker.symbols().unwrap()).unwrap();
    assert_eq!(
        e.broker.modify_order(&e.auth, &rm).await.unwrap().order_id,
        "26100300001"
    );
    let mc = &e.fake.calls("modify-order")[0];
    assert_eq!(mc.method, "PUT");
    assert!(mc.form.contains(&("limitPrice".into(), "801.0".into())));
    assert!(mc.form.contains(&("qty".into(), "12".into())));

    assert_eq!(
        e.broker
            .cancel_order(&e.auth, "26100300001")
            .await
            .unwrap()
            .order_id,
        "26100300001"
    );
    let cc = &e.fake.calls("cancel-order")[0];
    assert_eq!(
        (cc.method.as_str(), cc.query.as_str()),
        ("DELETE", "orderId=26100300001")
    );
}

#[tokio::test]
async fn cancel_all_uses_web_statuses() {
    let e = env().await;
    let r = e.broker.cancel_all_orders(&e.auth).await.unwrap();
    // open + modified cancelled; the trigger-pending one is refused.
    assert_eq!(r.cancelled, ["26100300001", "26100300005"]);
    assert_eq!(r.failed, ["26100300002"]);
}

#[tokio::test]
async fn books_are_openalgo_symbols() {
    let e = env().await;
    let ob = e.broker.get_order_book(&e.auth).await.unwrap();
    assert_eq!(ob[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(ob[1].status, "trigger pending");
    let calls = e.fake.calls("/api/oms/orders");
    assert_eq!(calls[0].query, "symDetails=true");
    let tb = e.broker.get_trade_book(&e.auth).await.unwrap();
    assert_eq!(tb[0].symbol, "RELIANCE");
    let pb = e.broker.get_positions(&e.auth).await.unwrap();
    assert_eq!(pb[0].symbol, "SBIN");
    let hb = e.broker.get_holdings(&e.auth).await.unwrap();
    assert_eq!(hb.len(), 2);
    let f = e.broker.get_funds(&e.auth).await.unwrap();
    assert_eq!(f.available_cash, 105000.75);
    assert!(matches!(
        e.broker.calculate_margin(&e.auth, &[]).await,
        Err(openalgo_desktop_lib::error::AppError::Unsupported("margin"))
    ));

    let q = e
        .broker
        .get_open_position(&e.auth, "NIFTY27OCT2625000CE", Exchange::Nfo, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, -75);
    let flat = e
        .broker
        .get_open_position(&e.auth, "RELIANCE", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(flat, 0);
}

#[tokio::test]
async fn close_all_squares_off_open_rows() {
    let e = env().await;
    let r = e.broker.close_all_positions(&e.auth).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    let forms: Vec<_> = e
        .fake
        .calls("place-order")
        .into_iter()
        .map(|c| c.form)
        .collect();
    assert!(forms[0].contains(&("side".into(), "sell".into())));
    assert!(forms[0].contains(&("qty".into(), "10".into())));
    assert!(forms[1].contains(&("side".into(), "buy".into())));
    assert!(forms[1].contains(&("product".into(), "normal".into())));
    assert!(forms[1].contains(&("type".into(), "market".into())));
}

#[tokio::test]
async fn expired_session_is_reported() {
    let e = env().await;
    let stale = AuthToken::new(format!("{}:old", KEY));
    let err = e.broker.get_order_book(&stale).await.unwrap_err();
    assert!(err.client_message().contains("session has expired"));
    let err = e
        .broker
        .get_quote(&stale, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap_err();
    assert!(err.client_message().contains("session has expired"));
}

#[tokio::test]
async fn history_request_and_parse() {
    let e = env().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "SBIN"),
        interval: "5m".into(),
        start: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2026, 10, 3).unwrap(),
    };
    let c = e.broker.get_history(&e.auth, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    let q = &e.fake.calls("interval-data")[0].query;
    assert_eq!(
        q,
        "id=EQT_SBIN_EQ_NSE&interval=5&from=1790826300&to=1791052199"
    );
    let mut bad = req.clone();
    bad.interval = "D".into();
    assert!(e.broker.get_history(&e.auth, &bad).await.is_err());
}

#[tokio::test]
async fn quote_over_a_short_lived_socket() {
    let e = env().await;
    let q = e
        .broker
        .get_quote(&e.auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(q.ltp, 801.5);
    assert_eq!(q.close, 798.0);
    assert_eq!(q.bid, 801.45);
    assert_eq!(q.volume, 123_456);
    sockets_closed(&e.fake).await;
    let frames = e.fake.ws_frames.lock().clone();
    assert_eq!(
        frames[0],
        json!({"type":"L1","action":"sub","tokens":[{"t":"3045_NSE"}]})
    );

    // Index: the quote never holds bid/ask, so it is returned at the end
    // of the first step, as the web does.
    let n = e
        .broker
        .get_quote(&e.auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(n.ltp, 25012.35);
    assert_eq!(n.close, 24900.0);
    sockets_closed(&e.fake).await;
}

#[tokio::test]
async fn multiquotes_one_socket_in_request_order() {
    let e = env().await;
    let keys = vec![
        QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        QuoteKey::new("NSE", "NOPE"),
        QuoteKey::new("NSE", "SBIN"),
    ];
    let r = e.broker.get_multiquotes(&e.auth, &keys).await.unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].data.as_ref().unwrap().oi, 1500);
    assert_eq!(r[1].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(r[2].data.as_ref().unwrap().ltp, 801.5);
    sockets_closed(&e.fake).await;
    assert_eq!(e.fake.ws_total.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn depth_over_a_short_lived_socket() {
    let e = env().await;
    let d = e
        .broker
        .get_market_depth(&e.auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.bids[0].price, 801.45);
    assert_eq!(d.asks[0].quantity, 70);
    assert_eq!(d.total_buy_qty, 150);
    sockets_closed(&e.fake).await;
    assert_eq!(
        e.fake.ws_frames.lock()[0],
        json!({"type":"L5","action":"sub","tokens":[{"t":"3045_NSE"}]})
    );
}

#[tokio::test]
async fn unknown_symbol_and_silent_feed() {
    let e = env().await;
    assert!(e
        .broker
        .get_quote(&e.auth, &QuoteKey::new("NSE", "NOPE"))
        .await
        .is_err());
    // Nothing answered: an error after the bounded wait, socket closed.
    let s = e.broker.symbols().unwrap().clone();
    s.load(vec![
        openalgo_desktop_lib::brokers::common::symbols::SymToken {
            symbol: "SILENT".into(),
            brsymbol: "EQT_SILENT_EQ_NSE".into(),
            name: "SILENT".into(),
            exchange: "NSE".into(),
            brexchange: "NSE".into(),
            token: "99999".into(),
            expiry: String::new(),
            strike: 0.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        },
    ]);
    let err = e
        .broker
        .get_quote(&e.auth, &QuoteKey::new("NSE", "SILENT"))
        .await
        .unwrap_err();
    assert!(err.client_message().contains("sent no price"));
    assert!(e
        .broker
        .get_market_depth(&e.auth, &QuoteKey::new("NSE", "SILENT"))
        .await
        .is_err());
    sockets_closed(&e.fake).await;
}

#[test]
fn order_constants_used() {
    // Keeps the imports honest across refactors.
    assert_eq!(Action::Buy.as_str(), "BUY");
    assert_eq!(PriceType::SlM.as_str(), "SL-M");
}
