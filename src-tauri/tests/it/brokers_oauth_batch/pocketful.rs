//! Pocketful against a local fake: OAuth2 code exchange with Basic client
//! auth and the trading_info lookup, an expired or account-less session,
//! quotes and depth over a fake market-data socket, and the master-contract
//! ZIP. Orders, books and close-all are ported from the web's tests in
//! `brokers/pocketful/rest_tests/orders.rs`.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::pocketful::master_contract::parse_archive;
use openalgo_desktop_lib::brokers::pocketful::{auth, zip, PocketfulBroker};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::websocket::handshake;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response as WsResponse};
use tokio_tungstenite::tungstenite::Message;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/pocketful/", $name))
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
    authorization: String,
    access_header: String,
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

fn route(method: &Method, path: &str, body: &str, auth: &str) -> Response {
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
            ok(json!({"status": "success", "data": {"client_id": "DEMO1"}}))
        }
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
                let header = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let auth = header("authorization");
                let access_header = header("x-authorization-token");
                let path = uri.path().to_string();
                fake.seen.lock().push(Seen {
                    method: method.to_string(),
                    path: path.clone(),
                    authorization: auth.clone(),
                    access_header,
                    body: body.clone(),
                });
                route(&method, &path, &body, &auth)
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
                let cb = handshake::check(move |req: &Request, resp: WsResponse| {
                    uri_slot.ws_uris.lock().push(req.uri().to_string());
                    Ok(resp)
                });
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
    assert_eq!(r.user_id, "DEMO1");
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
    assert_eq!(info[0].access_header, "acc-pf-1");

    let bad = BrokerCredentials {
        auth_code: Some("bad".into()),
        ..creds.clone()
    };
    let e = b.authenticate(bad).await.unwrap_err();
    assert_eq!(e.client_message(), auth::CODE_REFUSED);
    let missing = BrokerCredentials {
        api_secret: None,
        ..creds
    };
    assert!(b.authenticate(missing).await.is_err());
}

/// A session without its trading account (an older build's row, a
/// damaged one) is expired: nothing is sent, the trader logs in again.
#[tokio::test]
async fn pocketful_a_session_without_its_account_is_expired() {
    let (b, fake, _) = setup().await;
    let bare = AuthToken::new("acc-pf-1");
    let e = b.get_funds(&bare).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b.get_holdings(&bare).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    assert!(fake.seen.lock().is_empty(), "nothing was sent");
    assert!(b.create_feed(&bare).is_err());

    let expired = AuthToken::new("expired").with_user_id("<USER_ID>");
    let e = b.get_order_book(&expired).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
    let e = b.get_funds(&expired).await.unwrap_err();
    assert!(e.client_message().contains("Log in to Pocketful again"));
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
        .unwrap()
        .is_empty());
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
    assert_eq!(
        fake.ws_uris.lock()[0],
        "/ws/v1/feeds?login_id=%3CUSER_ID%3E&access_token=acc-pf-1"
    );

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
    // The snapquote's 811.5 is its average trade price, not a trade: the
    // depth's price is the detailed packet's, streamed beside the book
    // (lead decision 2026-11-09, review round; the web answers 811.5).
    assert_eq!(d.ltp, 812.25);
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
