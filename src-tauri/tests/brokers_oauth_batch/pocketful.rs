//! Pocketful against a local fake: OAuth2 code exchange with Basic client
//! auth and the trading_info lookup, orders (MARKET sent as a protected
//! LIMIT priced from the feed), cancel-all, close-all, open position, books
//! normalised to OpenAlgo symbols, funds, quotes and depth over a fake
//! market-data socket, and the master-contract ZIP.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::pocketful::master_contract::parse_archive;
use openalgo_desktop_lib::brokers::pocketful::{zip, PocketfulBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response as WsResponse};
use tokio_tungstenite::tungstenite::Message;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../fixtures/brokers/pocketful/", $name))
    };
}

const FILES: [(&str, &str); 5] = [
    ("NSECompactScrip.csv", fixture!("NSECompactScrip.csv")),
    ("BSECompactScrip.csv", fixture!("BSECompactScrip.csv")),
    ("NFOCompactScrip.csv", fixture!("NFOCompactScrip.csv")),
    ("BFOCompactScrip.csv", fixture!("BFOCompactScrip.csv")),
    ("MCXCompactScrip.csv", fixture!("MCXCompactScrip.csv")),
];

fn master() -> SymbolResolver {
    let files: Vec<(String, Vec<u8>)> = FILES
        .iter()
        .map(|(n, t)| (n.to_string(), t.as_bytes().to_vec()))
        .collect();
    let r = SymbolResolver::new();
    r.load(parse_archive(&files).unwrap());
    r
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    authorization: String,
    body: String,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    /// Text frames the fake socket received, and its request paths.
    ws_frames: Mutex<Vec<Value>>,
    ws_uris: Mutex<Vec<String>>,
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

fn route(method: &Method, path: &str, query: &str, body: &str, auth: &str) -> Response {
    if path == "/oauth2/token" {
        if body.contains("code=bad") {
            return (
                StatusCode::BAD_REQUEST,
                json!({"error": "invalid_grant", "message": "code expired"}).to_string(),
            )
                .into_response();
        }
        return ok(
            json!({"access_token": "acc-pf-1", "token_type": "bearer", "refresh_token": "r"}),
        );
    }
    if auth == "Bearer expired" {
        return (StatusCode::UNAUTHORIZED, "{}").into_response();
    }
    match (method.as_str(), path) {
        ("GET", "/api/v1/user/trading_info") => {
            ok(json!({"status": "success", "data": {"client_id": "<USER_ID>"}}))
        }
        ("POST", "/api/v1/orders") => {
            ok(json!({"status": "success", "data": {"oms_order_id": "OMS1"}}))
        }
        ("PUT", "/api/v1/orders") => {
            ok(json!({"status": "success", "data": {"oms_order_id": "OMS2"}}))
        }
        ("DELETE", p) if p.starts_with("/api/v1/orders/") => {
            let id = p.rsplit('/').next().unwrap_or("");
            if id == "260930000000105" {
                ok(json!({"status": "error", "message": "Order already traded"}))
            } else {
                ok(json!({"status": "success", "data": {"oms_order_id": id}}))
            }
        }
        ("GET", "/api/v1/orders") if query.contains("type=completed") => {
            ok(fixture!("orders_completed.json"))
        }
        ("GET", "/api/v1/orders") => ok(fixture!("orders_pending.json")),
        ("GET", "/api/v1/trades") => ok(fixture!("trades.json")),
        ("GET", "/api/v1/positions") => ok(fixture!("positions.json")),
        ("GET", "/api/v1/holdings") => ok(fixture!("holdings.json")),
        ("GET", "/api/v2/funds/view") => ok(fixture!("funds.json")),
        ("GET", "/contract") => {
            let files: Vec<(&str, &[u8])> = FILES.iter().map(|(n, t)| (*n, t.as_bytes())).collect();
            (StatusCode::OK, zip::build(&files)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "no such endpoint").into_response(),
    }
}

async fn serve_rest(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let body = String::from_utf8_lossy(&body).to_string();
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let path = uri.path().to_string();
                let query = uri.query().unwrap_or("").to_string();
                fake.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    query: query.clone(),
                    authorization: auth.clone(),
                    body: body.clone(),
                });
                route(&method, &path, &query, &body, &auth)
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

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

/// Detailed packet (web `packet_decoder.py:160-186`).
fn detailed(code: u8, token: u32, ltp_paise: u32) -> Vec<u8> {
    let mut b = vec![0u8; 102];
    b[0] = 1;
    b[1] = code;
    put32(&mut b, 2, token);
    put32(&mut b, 6, ltp_paise);
    put32(&mut b, 18, 1000);
    put32(&mut b, 22, ltp_paise - 5);
    put32(&mut b, 30, ltp_paise + 5);
    put32(&mut b, 62, ltp_paise - 500);
    put32(&mut b, 74, ltp_paise - 1000);
    b
}

/// Snapquote packet (web `packet_decoder.py:83-137`).
fn snapquote(code: u8, token: u32) -> Vec<u8> {
    let mut b = vec![0u8; 166];
    b[0] = 4;
    b[1] = code;
    put32(&mut b, 2, token);
    for i in 0..5u32 {
        let o = 4 * i as usize;
        put32(&mut b, 6 + o, 3 + i);
        put32(&mut b, 26 + o, 81_200 - 5 * i);
        put32(&mut b, 46 + o, 100 + i);
        put32(&mut b, 66 + o, 7 + i);
        put32(&mut b, 86 + o, 81_210 + 5 * i);
        put32(&mut b, 106 + o, 200 + i);
    }
    put32(&mut b, 126, 81_150);
    put32(&mut b, 142, 80_500);
    put32(&mut b, 162, 4242);
    b
}

fn ltp_for(token: u32) -> u32 {
    match token {
        3045 => 81_225,
        35003 => 2_500_000,
        26000 => 2_510_050,
        _ => 10_000,
    }
}

/// A fake market-data socket: answers each subscribe with one packet of
/// the subscribed type (an unknown token never answers).
async fn serve_ws(fake: Arc<Fake>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let fake = fake.clone();
            tokio::spawn(async move {
                let uri_slot = fake.clone();
                let cb = move |req: &Request, resp: WsResponse| {
                    uri_slot.ws_uris.lock().push(req.uri().to_string());
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(t) = msg else { continue };
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    fake.ws_frames.lock().push(v.clone());
                    if v["a"] != "subscribe" {
                        continue;
                    }
                    let code = v["v"][0][0].as_u64().unwrap_or(0) as u8;
                    let token = v["v"][0][1].as_u64().unwrap_or(0) as u32;
                    if token == 999_999 {
                        continue;
                    }
                    let packet = match v["m"].as_str() {
                        Some("full_snapquote") => snapquote(code, token),
                        _ => detailed(code, token, ltp_for(token)),
                    };
                    // Noise first: another instrument's packet.
                    let _ = ws.send(Message::Binary(detailed(code, 1, 1000))).await;
                    let _ = ws.send(Message::Binary(packet)).await;
                }
            });
        }
    });
    format!("ws://{}", addr)
}

/// Whether the fake socket logged `frame` (the client may return before
/// the server task has read its last frames, so wait briefly).
async fn saw_frame(fake: &Fake, frame: Value) -> bool {
    for _ in 0..100 {
        if fake.ws_frames.lock().contains(&frame) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

async fn setup() -> (PocketfulBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let rest = serve_rest(fake.clone()).await;
    let ws = serve_ws(fake.clone()).await;
    let b = PocketfulBroker::with_base_url(master(), &rest, ws, format!("{}/contract", rest));
    (
        b,
        fake,
        AuthToken::new("acc-pf-1").with_user_id("<USER_ID>"),
    )
}

fn order(symbol: &str, exchange: &str, pricetype: &str, price: f64) -> ResolvedOrder {
    let req = OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &master()).unwrap()
}

#[tokio::test]
async fn pocketful_login_exchanges_the_code_with_basic_auth() {
    let (b, fake, _) = setup().await;
    let creds = BrokerCredentials {
        api_key: "cid-1".into(),
        api_secret: Some("sec-1".into()),
        auth_code: Some("good-code".into()),
        ..Default::default()
    };
    let r = b.authenticate(creds.clone()).await.unwrap();
    assert_eq!(r.auth_token, "acc-pf-1");
    assert_eq!(r.user_id, "<USER_ID>");
    assert!(r.feed_token.is_none());
    let tok = fake.calls("POST", "/oauth2/token");
    assert_eq!(tok.len(), 1);
    // base64("cid-1:sec-1")
    assert_eq!(tok[0].authorization, "Basic Y2lkLTE6c2VjLTE=");
    let form: Vec<(String, String)> = serde_urlencoded::from_str(&tok[0].body).unwrap();
    assert!(form.contains(&("grant_type".into(), "authorization_code".into())));
    assert!(form.contains(&("code".into(), "good-code".into())));
    assert!(form
        .iter()
        .any(|(k, v)| k == "redirect_uri" && v.ends_with("/pocketful/callback")));
    let info = fake.calls("GET", "/api/v1/user/trading_info");
    assert_eq!(info[0].authorization, "Bearer acc-pf-1");

    let bad = BrokerCredentials {
        auth_code: Some("bad".into()),
        ..creds.clone()
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let missing = BrokerCredentials {
        api_secret: None,
        ..creds
    };
    assert!(b.authenticate(missing).await.is_err());
}

#[tokio::test]
async fn pocketful_orders_and_market_protection() {
    let (b, fake, auth) = setup().await;
    let r = b
        .place_order(&auth, &order("SBIN", "NSE", "LIMIT", 811.5))
        .await
        .unwrap();
    assert_eq!(r.order_id, "OMS1");
    let sent: Value = serde_json::from_str(&fake.calls("POST", "/api/v1/orders")[0].body).unwrap();
    assert_eq!(sent["order_type"], "LIMIT");
    assert_eq!(sent["price"], 811.5);
    assert_eq!(sent["instrument_token"], 3045);
    assert_eq!(sent["client_id"], "<USER_ID>");
    assert_eq!(sent["order_side"], "BUY");
    assert_eq!(sent["device"], "WEB");
    assert_eq!(
        fake.calls("POST", "/api/v1/orders")[0].authorization,
        "Bearer acc-pf-1"
    );

    // MARKET: LTP 812.25 from the feed, EQ above 500 -> 0.5%, tick 0.05.
    b.place_order(&auth, &order("SBIN", "NSE", "MARKET", 0.0))
        .await
        .unwrap();
    let sent: Value = serde_json::from_str(&fake.calls("POST", "/api/v1/orders")[1].body).unwrap();
    assert_eq!(sent["order_type"], "LIMIT");
    assert_eq!(sent["price"], 816.3);
    // The price lookup subscribed detailed data and released it.
    assert!(
        saw_frame(
            &fake,
            json!({"a":"subscribe","v":[[1,3045]],"m":"marketdata"})
        )
        .await
    );
    assert!(
        saw_frame(
            &fake,
            json!({"a":"unsubscribe","v":[[1,3045]],"m":"marketdata"})
        )
        .await
    );
    let uri = fake.ws_uris.lock()[0].clone();
    assert_eq!(
        uri,
        "/ws/v1/feeds?login_id=%3CUSER_ID%3E&access_token=acc-pf-1"
    );

    let m = ModifyOrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        action: "SELL".into(),
        product: "MIS".into(),
        pricetype: "SL-M".into(),
        quantity: 10,
        price: 0.0,
        trigger_price: 800.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("260930000000104", &m, &master()).unwrap();
    assert_eq!(b.modify_order(&auth, &rm).await.unwrap().order_id, "OMS2");
    let sent: Value = serde_json::from_str(&fake.calls("PUT", "/api/v1/orders")[0].body).unwrap();
    assert_eq!(sent["oms_order_id"], "260930000000104");
    assert_eq!(sent["order_type"], "SLM");
    assert_eq!(sent["trigger_price"], 800.0);

    let c = b.cancel_order(&auth, "260930000000104").await.unwrap();
    assert_eq!(c.order_id, "260930000000104");
    let del = fake.calls("DELETE", "/api/v1/orders/260930000000104");
    assert_eq!(del[0].query, "client_id=%3CUSER_ID%3E");

    // Cancel-all reads the pending book; one cancel is refused.
    let all = b.cancel_all_orders(&auth).await.unwrap();
    assert_eq!(all.cancelled, ["260930000000104", "260930000000106"]);
    assert_eq!(all.failed, ["260930000000105"]);
    assert!(fake
        .seen
        .lock()
        .iter()
        .any(|s| s.path == "/api/v1/orders" && s.query.contains("type=pending")));
}

#[tokio::test]
async fn pocketful_books_funds_and_positions() {
    let (b, fake, auth) = setup().await;
    let orders = b.get_order_book(&auth).await.unwrap();
    assert_eq!(orders.len(), 6);
    assert_eq!(orders[0].symbol, "SBIN");
    assert_eq!(orders[3].status, "trigger pending");
    assert!(orders.iter().all(|o| o.status == o.status.to_lowercase()));
    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades[1].symbol, "NIFTY27OCT26FUT");
    let positions = b.get_positions(&auth).await.unwrap();
    assert_eq!(positions.len(), 3);
    assert_eq!(positions[1].symbol, "NIFTY27OCT26FUT");
    let pos_call = &fake.calls("GET", "/api/v1/positions")[0];
    assert_eq!(pos_call.query, "client_id=%3CUSER_ID%3E&type=live");
    let holdings = b.get_holdings(&auth).await.unwrap();
    assert_eq!(holdings[0].symbol, "SBIN");
    let funds = b.get_funds(&auth).await.unwrap();
    assert_eq!(funds.available_cash, 85234.57);
    assert_eq!(funds.collateral, 25000.0);
    assert_eq!(
        fake.calls("GET", "/api/v2/funds/view")[0].query,
        "client_id=%3CUSER_ID%3E&type=all"
    );

    // Open position matches the broker symbol, exchange and product.
    let q = b
        .get_open_position(&auth, "SBIN", Exchange::Nse, Product::Mis)
        .await
        .unwrap();
    assert_eq!(q, 10);
    let q = b
        .get_open_position(&auth, "SBIN", Exchange::Nse, Product::Cnc)
        .await
        .unwrap();
    assert_eq!(q, 0);

    // Close-all: SBIN SELL 10 MIS and NIFTY future BUY 75 NRML, both as
    // protected limits priced from the feed; the flat row is skipped.
    let before = fake.calls("POST", "/api/v1/orders").len();
    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2);
    assert!(r.failed.is_empty());
    let posts = fake.calls("POST", "/api/v1/orders");
    assert_eq!(posts.len() - before, 2);
    let a: Value = serde_json::from_str(&posts[before].body).unwrap();
    assert_eq!(
        (a["order_side"].as_str(), a["quantity"].as_i64()),
        (Some("SELL"), Some(10))
    );
    assert_eq!(a["product"], "MIS");
    assert_eq!(a["price"], 808.2);
    let n: Value = serde_json::from_str(&posts[before + 1].body).unwrap();
    assert_eq!(
        (n["order_side"].as_str(), n["quantity"].as_i64()),
        (Some("BUY"), Some(75))
    );
    assert_eq!(n["instrument_token"], 35003);
    assert_eq!(n["exchange"], "NFO");
    assert_eq!(n["price"], 25125.0);
}

#[tokio::test]
async fn pocketful_client_id_lookup_and_expiry() {
    let (b, fake, _) = setup().await;
    // No user id on the session: trading_info once, then cached.
    let bare = AuthToken::new("acc-pf-1");
    b.get_funds(&bare).await.unwrap();
    b.get_holdings(&bare).await.unwrap();
    assert_eq!(fake.calls("GET", "/api/v1/user/trading_info").len(), 1);
    // The cached id now lets the feed be built.
    assert!(b.create_feed(&bare).is_ok());

    let expired = AuthToken::new("expired").with_user_id("<USER_ID>");
    let e = b.get_order_book(&expired).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
    let e = b.get_funds(&expired).await.unwrap_err();
    assert!(e.client_message().contains("session has expired"));
    assert!(b
        .get_history(
            &expired,
            &HistoryRequest {
                key: QuoteKey::new("NSE", "SBIN"),
                interval: "D".into(),
                start: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                end: chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn pocketful_quotes_and_depth_over_the_socket() {
    let (b, fake, auth) = setup().await;
    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(
        (q.ltp, q.bid, q.ask, q.close),
        (812.25, 812.2, 812.3, 802.25)
    );
    assert_eq!(q.volume, 1000);

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "SBIN"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(
        (d.bids[0].price, d.bids[0].quantity, d.bids[0].orders),
        (812.0, 100, 3)
    );
    assert_eq!(d.asks[4].price, 812.3);
    assert_eq!(d.ltp, 811.5);
    assert_eq!(d.volume, 4242);
    assert!(
        saw_frame(
            &fake,
            json!({"a":"subscribe","v":[[1,3045]],"m":"full_snapquote"})
        )
        .await
    );
    assert!(saw_frame(&fake, json!({"a":"h"})).await);
    assert!(
        saw_frame(
            &fake,
            json!({"a":"unsubscribe","v":[[1,3045]],"m":"full_snapquote"})
        )
        .await
    );

    let res = b
        .get_multiquotes(
            &auth,
            &[
                QuoteKey::new("NSE", "SBIN"),
                QuoteKey::new("NSE", "NOPE"),
                QuoteKey::new("NSE_INDEX", "NIFTY"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(res.len(), 3);
    // web order: unresolved first, then resolved in request order.
    assert_eq!(res[0].symbol, "NOPE");
    assert_eq!(res[0].error.as_deref(), Some("Could not resolve token"));
    assert_eq!(res[1].data.as_ref().unwrap().ltp, 812.25);
    assert_eq!(res[2].symbol, "NIFTY");
    assert_eq!(res[2].data.as_ref().unwrap().ltp, 25100.5);
    assert!(
        saw_frame(
            &fake,
            json!({"a":"subscribe","v":[[1,26000]],"m":"marketdata"})
        )
        .await
    );
}

#[tokio::test]
async fn pocketful_master_contract_download() {
    let (b, _, auth) = setup().await;
    let rows = b.download_master_contract(&auth).await.unwrap();
    assert_eq!(rows.len(), 26);
    let fut = rows.iter().find(|r| r.symbol == "NIFTY27OCT26FUT").unwrap();
    assert_eq!(fut.expiry, "27-OCT-26");
    assert_eq!(fut.brsymbol, "NIFTY26OCTFUT");
    assert_eq!(fut.token, "35003");
    assert!(rows
        .iter()
        .any(|r| r.exchange == "NSE_INDEX" && r.symbol == "BANKNIFTY"));
}
