//! Nubra adapter suite against a local fake broker: phone-OTP sign-in (the
//! SMS OTP sent when the login page opens, redeemed once with its temp
//! token, then the MPIN), the same through the `/nubra/callback` routes,
//! the unrouted TOTP path (with the leading-zero retry), V3 intent-order
//! bodies in paise, bucketed books
//! normalised to OpenAlgo symbols, funds, margin, quotes, depth, index
//! snapshot over a fake market socket, history chunking, the master
//! contract, and the order-update stream through the loopback relay.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use chrono::NaiveDate;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::mapping::{Exchange, Product};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, Message};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::nubra::NubraBroker;
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../fixtures/brokers/nubra/", $name))
    };
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    query: String,
    authorization: String,
    device: String,
    temp: String,
    body: Value,
}

#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<Seen>>,
    holdings_429: AtomicUsize,
    order_ws: Mutex<String>,
    /// The BSE instrument list answers 500.
    bse_down: std::sync::atomic::AtomicBool,
}

impl Fake {
    fn calls(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }
}

fn reply(status: StatusCode, v: impl ToString) -> Response {
    (
        status,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

fn ok(v: impl ToString) -> Response {
    reply(StatusCode::OK, v)
}

fn route(fake: &Fake, s: &Seen) -> Response {
    let authed = s.authorization == "Bearer SESS1";
    if s.authorization == "Bearer EXPIRED" {
        return reply(StatusCode::from_u16(440).unwrap(), "");
    }
    match (s.method.as_str(), s.path.as_str()) {
        ("POST", "/sendphoneotp") => {
            // The official SDK (and the web) send no device id here.
            if !s.device.is_empty() {
                return reply(StatusCode::BAD_REQUEST, json!({"error": "device"}));
            }
            match (s.body["phone"].as_str(), s.temp.as_str()) {
                (Some("8888888888"), _) => ok(json!({"temp_token": "T", "next": "VERIFY_EMAIL"})),
                (Some("9999999999"), "")
                    if s.body == json!({"phone": "9999999999", "flow": "", "skip_totp": false}) =>
                {
                    ok(fixture!("sendphoneotp_verify_totp.json"))
                }
                (Some("9999999999"), "TEMP1") if s.body["skip_totp"] == true => {
                    ok(fixture!("sendphoneotp_verify_mobile.json"))
                }
                _ => reply(
                    StatusCode::BAD_REQUEST,
                    json!({"message": "Invalid phone number"}),
                ),
            }
        }
        ("POST", "/verifyphoneotp") => {
            if s.temp == "TEMP2"
                && s.device == "OPENALGO"
                && s.body == json!({"phone": "9999999999", "otp": "123456"})
            {
                ok(fixture!("verifyphoneotp.json"))
            } else {
                reply(
                    StatusCode::UNAUTHORIZED,
                    fixture!("verifyphoneotp_invalid.json"),
                )
            }
        }
        ("POST", "/totp/login") => {
            if s.device != "OPENALGO" {
                return reply(StatusCode::BAD_REQUEST, json!({"error": "device"}));
            }
            match &s.body["totp"] {
                // The leading-zero code only matches as a string.
                Value::String(t) if t == "012345" => {
                    ok(json!({"auth_token": "AUTH1", "next": "VERIFY_PIN"}))
                }
                Value::Number(n) if n.as_u64() == Some(123456) => {
                    ok(json!({"auth_token": "AUTH1", "next": "VERIFY_PIN"}))
                }
                _ => reply(
                    StatusCode::UNAUTHORIZED,
                    json!({"error": "Invalid TOTP", "nubra_error_code": ""}),
                ),
            }
        }
        ("POST", "/verifypin") => {
            if s.authorization == "Bearer AUTH1" && s.body["pin"] == "4321" {
                ok(json!({"session_token": "SESS1"}))
            } else {
                reply(StatusCode::UNAUTHORIZED, json!({"error": "Invalid MPIN"}))
            }
        }
        ("GET", "/public/indexes") => ok(fixture!("indexes.csv")),
        _ if !authed => reply(StatusCode::UNAUTHORIZED, json!({"error": "unauthorized"})),
        ("POST", "/sentinel/orders/create") => {
            if s.body["orders"][0]["refId"] == 61001 {
                reply(
                    StatusCode::BAD_REQUEST,
                    json!({"error": "Insufficient funds", "nubra_error_code": ""}),
                )
            } else {
                reply(
                    StatusCode::CREATED,
                    json!({"orders": [{"intentOrderId": 5001}]}),
                )
            }
        }
        ("POST", "/sentinel/orders/modify") => {
            ok(json!({"orders": [{"intentOrderId": s.body["orders"][0]["orderId"]}]}))
        }
        ("POST", "/sentinel/orders/cancel") => {
            if s.body["orders"][0]["orderId"] == 1002 {
                reply(
                    StatusCode::BAD_REQUEST,
                    json!({"error": "Order already cancelled"}),
                )
            } else {
                (StatusCode::NO_CONTENT, "").into_response()
            }
        }
        ("POST", "/sentinel/orders/funds_required") => ok(fixture!("margin.json")),
        ("GET", "/sentinel/orders") => ok(fixture!("orders.json")),
        ("GET", "/sentinel/portfolio/positions") => ok(fixture!("positions.json")),
        ("GET", "/sentinel/portfolio/holdings") => {
            if fake.holdings_429.fetch_add(1, Ordering::SeqCst) == 0 {
                return reply(StatusCode::TOO_MANY_REQUESTS, "{}");
            }
            ok(fixture!("holdings.json"))
        }
        ("GET", "/sentinel/portfolio/user_funds_and_margin") => ok(fixture!("funds.json")),
        ("GET", "/orderbooks/72329") => {
            if s.query.contains("levels=5") {
                ok(fixture!("orderbook_l5.json"))
            } else {
                ok(fixture!("orderbook_l1.json"))
            }
        }
        ("POST", "/charts/timeseries") => ok(fixture!("timeseries.json")),
        ("GET", "/userinfo") => ok(json!({"env_info": {
            "user_ws_url": fake.order_ws.lock().clone(),
            "market_ws_url": "wss://unused"
        }})),
        ("GET", p) if p.starts_with("/refdata/refdata/") => {
            if s.query.contains("exchange=NSE") {
                ok(fixture!("refdata_nse.json"))
            } else if s.query.contains("exchange=MCX") {
                ok(fixture!("refdata_mcx.json"))
            } else if fake.bse_down.load(Ordering::SeqCst) {
                reply(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": "down"}))
            } else {
                ok(fixture!("refdata_bse.json"))
            }
        }
        _ => reply(StatusCode::NOT_FOUND, json!({"error": "no such endpoint"})),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let fake = fake.clone();
            async move {
                let h = |k: &str| {
                    headers
                        .get(k)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string()
                };
                let seen = Seen {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    query: uri.query().unwrap_or("").to_string(),
                    authorization: h("authorization"),
                    device: h("x-device-id"),
                    temp: h("x-temp-token"),
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                };
                fake.seen.lock().push(seen.clone());
                route(&fake, &seen)
            }
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn frame(name: &str) -> Vec<u8> {
    let v: Value = serde_json::from_str(fixture!("feed_frames.json")).unwrap();
    hex::decode(v[name].as_str().unwrap()).unwrap()
}

/// A WebSocket server that checks the session headers, records the first
/// text frame and answers it with `reply`.
async fn ws_server(reply: Vec<u8>, first_text: Arc<Mutex<Vec<String>>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            let reply = reply.clone();
            let first_text = first_text.clone();
            tokio::spawn(async move {
                use tokio_tungstenite::tungstenite::handshake::server::{
                    ErrorResponse, Request, Response as WsResponse,
                };
                let check =
                    |req: &Request, resp: WsResponse| -> Result<WsResponse, ErrorResponse> {
                        let auth = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok());
                        let dev = req
                            .headers()
                            .get("x-device-id")
                            .and_then(|v| v.to_str().ok());
                        if auth == Some("Bearer SESS1") && dev == Some("OPENALGO") {
                            Ok(resp)
                        } else {
                            let mut r = ErrorResponse::new(None);
                            *r.status_mut() =
                                tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                            Err(r)
                        }
                    };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, check).await else {
                    return;
                };
                while let Some(Ok(m)) = ws.next().await {
                    if let Message::Text(t) = m {
                        let first = first_text.lock().is_empty();
                        first_text.lock().push(t);
                        if first {
                            let _ = ws.send(Message::Binary(reply.clone())).await;
                        }
                    }
                }
            });
        }
    });
    url
}

fn master() -> SymbolResolver {
    let mut rows = Vec::new();
    for f in [fixture!("refdata_nse.json"), fixture!("refdata_mcx.json")] {
        let v: Value = serde_json::from_str(f).unwrap();
        rows.extend(
            openalgo_desktop_lib::brokers::nubra::master_contract::parse_refdata(
                v["refdata"].as_array().unwrap(),
            ),
        );
    }
    rows.extend(
        openalgo_desktop_lib::brokers::nubra::master_contract::parse_indexes(fixture!(
            "indexes.csv"
        )),
    );
    let r = SymbolResolver::new();
    r.load(rows);
    r
}

async fn setup() -> (NubraBroker, Arc<Fake>, AuthToken) {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = NubraBroker::with_urls(master(), host, "ws://127.0.0.1:1").with_fast_timings();
    (b, fake, AuthToken::new("SESS1"))
}

fn creds(totp: &str) -> BrokerCredentials {
    BrokerCredentials {
        api_key: "9999999999".into(),
        api_secret: Some("4321".into()),
        totp: Some(totp.into()),
        ..Default::default()
    }
}

fn order_req(
    symbol: &str,
    exchange: &str,
    pricetype: &str,
    price: f64,
    trigger: f64,
) -> OrderRequest {
    OrderRequest {
        symbol: symbol.into(),
        exchange: exchange.into(),
        side: "BUY".into(),
        quantity: 10,
        price,
        order_type: pricetype.into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: Some(trigger),
        disclosed_quantity: None,
        amo: false,
    }
}

#[tokio::test]
async fn phone_otp_is_sent_then_redeemed_once_with_the_mpin() {
    let (b, fake, _) = setup().await;
    assert!(!b.otp_pending());

    // Opening the login page: a TOTP-enrolled account answers VERIFY_TOTP,
    // so the SMS is forced with the first temp token and skip_totp.
    let msg = b.send_login_otp(&creds("")).await.unwrap();
    assert_eq!(
        msg,
        "An OTP has been sent to your registered mobile number 99999***99."
    );
    let sent = fake.calls("/sendphoneotp");
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0].body,
        json!({"phone": "9999999999", "flow": "", "skip_totp": false})
    );
    assert_eq!((sent[0].temp.as_str(), sent[0].device.as_str()), ("", ""));
    assert_eq!(
        sent[1].body,
        json!({"phone": "9999999999", "flow": "", "skip_totp": true})
    );
    assert_eq!(sent[1].temp, "TEMP1");
    assert!(b.otp_pending());

    // The form posts the OTP: verified with the second temp token, then
    // the MPIN for the session.
    let r = b.authenticate(creds(" 123456 ")).await.unwrap();
    assert_eq!(r.auth_token, "SESS1");
    assert_eq!(r.user_id, "9999999999");
    assert!(r.feed_token.is_none());
    let verify = fake.calls("/verifyphoneotp");
    assert_eq!(verify.len(), 1);
    assert_eq!(verify[0].temp, "TEMP2");
    assert_eq!(verify[0].device, "OPENALGO");
    assert_eq!(
        verify[0].body,
        json!({"phone": "9999999999", "otp": "123456"})
    );
    let pin = fake.calls("/verifypin");
    assert_eq!(pin.len(), 1);
    assert_eq!(pin[0].authorization, "Bearer AUTH1");
    assert_eq!(pin[0].device, "OPENALGO");
    assert_eq!(pin[0].temp, "");
    assert_eq!(pin[0].body, json!({"pin": "4321"}));

    // Single use: the same OTP again needs a new login.
    assert!(!b.otp_pending());
    let e = b.authenticate(creds("123456")).await.unwrap_err();
    assert!(
        e.client_message()
            .contains("Start the Nubra login again from the broker page"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.calls("/verifyphoneotp").len(), 1);
}

#[tokio::test]
async fn a_refused_or_malformed_otp_still_uses_up_the_login() {
    let (b, fake, _) = setup().await;

    // Nubra refuses the code: its reason reaches the trader, and the temp
    // token is gone (the web pops it before the attempt).
    b.send_login_otp(&creds("")).await.unwrap();
    let e = b.authenticate(creds("111111")).await.unwrap_err();
    assert!(
        e.client_message().contains("Invalid OTP"),
        "{}",
        e.client_message()
    );
    assert!(!b.otp_pending());
    let e = b.authenticate(creds("123456")).await.unwrap_err();
    assert!(
        e.client_message().contains("expired"),
        "{}",
        e.client_message()
    );

    // Not digits: refused before any call to Nubra.
    b.send_login_otp(&creds("")).await.unwrap();
    let before = fake.calls("/verifyphoneotp").len();
    let e = b.authenticate(creds("12a456")).await.unwrap_err();
    assert!(
        e.client_message().contains("digits only"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.calls("/verifyphoneotp").len(), before);
    assert!(!b.otp_pending());

    // A wrong MPIN after a good OTP.
    b.send_login_otp(&creds("")).await.unwrap();
    let mut bad_pin = creds("123456");
    bad_pin.api_secret = Some("0000".into());
    let e = b.authenticate(bad_pin).await.unwrap_err();
    assert!(
        e.client_message().contains("MPIN"),
        "{}",
        e.client_message()
    );

    // The OTP was sent to another number than the one signing in.
    b.send_login_otp(&creds("")).await.unwrap();
    let before = fake.calls("/verifyphoneotp").len();
    let mut other_phone = creds("123456");
    other_phone.api_key = "9999999998".into();
    let e = b.authenticate(other_phone).await.unwrap_err();
    assert!(
        e.client_message().contains("number was changed"),
        "{}",
        e.client_message()
    );
    assert_eq!(fake.calls("/verifyphoneotp").len(), before);
    assert!(!b.otp_pending());

    // Phone or MPIN missing: nothing is sent.
    let calls = fake.seen.lock().len();
    let mut no_phone = creds("");
    no_phone.api_key = " ".into();
    assert!(b.send_login_otp(&no_phone).await.is_err());
    let mut no_mpin = creds("123456");
    no_mpin.api_secret = None;
    assert!(b.authenticate(no_mpin).await.is_err());
    assert_eq!(fake.seen.lock().len(), calls);

    // An unexpected next step is refused and nothing is kept.
    let mut other = creds("");
    other.api_key = "8888888888".into();
    let e = b.send_login_otp(&other).await.unwrap_err();
    assert!(e.client_message().contains("unexpected login step"));
    assert!(!b.otp_pending());

    // Logout drops a pending OTP.
    b.send_login_otp(&creds("")).await.unwrap();
    assert!(b.otp_pending());
    b.on_logout().await;
    assert!(!b.otp_pending());
}

#[tokio::test]
async fn a_pending_otp_expires() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = NubraBroker::with_urls(master(), host, "ws://127.0.0.1:1")
        .with_fast_timings()
        .with_otp_ttl(std::time::Duration::from_millis(30));
    b.send_login_otp(&creds("")).await.unwrap();
    assert!(b.otp_pending());
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    assert!(!b.otp_pending());
    let e = b.authenticate(creds("123456")).await.unwrap_err();
    assert!(e.client_message().contains("expired"));
    assert!(fake.calls("/verifyphoneotp").is_empty());
}

/// The web's unrouted TOTP path, kept in `auth` for reference.
#[tokio::test]
async fn totp_login_then_mpin() {
    use openalgo_desktop_lib::brokers::nubra::auth::authenticate_with_totp;
    let (b, fake, _) = setup().await;
    let r = authenticate_with_totp(&b, creds("123456")).await.unwrap();
    assert_eq!(r.auth_token, "SESS1");
    assert_eq!(r.user_id, "9999999999");
    let login = fake.calls("/totp/login");
    assert_eq!(login.len(), 1);
    assert_eq!(
        login[0].body,
        json!({"phone": "9999999999", "totp": 123456, "otp": ""})
    );

    // Leading zero: integer first, then the zero-padded string.
    let r = authenticate_with_totp(&b, creds("12345")).await.unwrap();
    assert_eq!(r.auth_token, "SESS1");
    let login = fake.calls("/totp/login");
    assert_eq!(login[1].body["totp"], json!(12345));
    assert_eq!(login[2].body["totp"], json!("012345"));

    let e = authenticate_with_totp(&b, creds("111111"))
        .await
        .unwrap_err();
    assert!(
        e.client_message().contains("Invalid TOTP"),
        "{}",
        e.client_message()
    );
    let mut no_totp = creds("x");
    no_totp.totp = None;
    assert!(authenticate_with_totp(&b, no_totp).await.is_err());
}

/// The in-app flow through the web's routes: GET `/nubra/callback` sends
/// the OTP and opens the OTP page, but only when OpenAlgo itself opened it
/// (a cross-site open sends nothing, keeps a pending OTP and does not count
/// against the login limit; the page then offers Send OTP, a CSRF-checked,
/// same-origin POST); POST `/nubra/callback` (form `otp` or `totp`, CSRF
/// token, signed-in session) signs in with the web's JSON.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callback_routes_send_and_redeem_the_otp() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use openalgo_desktop_lib::brokers::BrokerRegistry;
    use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
    use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
    use openalgo_desktop_lib::security::Secret;
    use openalgo_desktop_lib::services::auth_service::AuthService;
    use openalgo_desktop_lib::state::{AppState, OpenOptions};
    use tower::ServiceExt;

    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let symbols = SymbolResolver::new();
    let nubra = Arc::new(
        NubraBroker::with_urls(symbols.clone(), host, "ws://127.0.0.1:1")
            .with_fast_timings()
            .with_order_ws_fallback("ws://127.0.0.1:1"),
    );
    let dir = tempfile::tempdir().unwrap();
    let ctx = AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: openalgo_desktop_lib::clock::ManualClock::new(
                chrono::TimeZone::with_ymd_and_hms(
                    &chrono_tz::Asia::Kolkata,
                    2026,
                    10,
                    5,
                    10,
                    0,
                    0,
                )
                .unwrap()
                .with_timezone(&chrono::Utc),
            ),
            brokers: Arc::new(BrokerRegistry::with_symbols(
                symbols,
                vec![nubra.clone() as Arc<dyn Broker>],
            )),
        },
    )
    .unwrap();
    AuthService::setup(&ctx, "trader", "trader@example.com", "Secret@123").unwrap();
    {
        let conn = ctx.sqlite.conn().unwrap();
        credentials::save(
            &conn,
            &ctx.security,
            "nubra",
            CredentialUpdate {
                api_key: Some(Secret::new("9999999999")),
                api_secret: Some(Secret::new("4321")),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let web = ctx.sessions.create(ctx.now());
    ctx.sessions
        .update(&web.id, |s| s.user = Some("trader".into()));
    let cookie = format!("session={}", web.id);

    let send = |req: Request<Body>, ip: u8| {
        let ctx = ctx.clone();
        async move {
            let mut req = req;
            req.extensions_mut()
                .insert(ConnectInfo(std::net::SocketAddr::from((
                    [10, 0, 0, ip],
                    40000,
                ))));
            crate::with_host(&mut req, &ctx);
            let resp = openalgo_desktop_lib::server::app(ctx)
                .oneshot(req)
                .await
                .unwrap();
            let status = resp.status().as_u16();
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            (status, location, v)
        }
    };
    let get_with = |cookie: Option<&str>, extra: &[(&str, &str)]| {
        let mut b = Request::builder().uri("/nubra/callback");
        if let Some(c) = cookie {
            b = b.header(header::COOKIE, c);
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    };
    // The app's own navigation from the broker page.
    let get = |cookie: Option<&str>| get_with(cookie, &[("sec-fetch-site", "same-origin")]);
    let app_origin = format!("http://127.0.0.1:{}", ctx.server_config().http_port);
    let post_with = |form: &str, extra: Option<(&str, &str)>| {
        let mut b = Request::builder()
            .method("POST")
            .uri("/nubra/callback")
            .header(header::COOKIE, cookie.as_str())
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some((k, v)) = extra {
            b = b.header(k, v);
        }
        b.body(Body::from(form.to_string())).unwrap()
    };
    let post = |form: &str| post_with(form, None);

    // Not signed in to OpenAlgo: the page opens, nothing is sent.
    let (status, location, _) = send(get(None), 1).await;
    assert!((300..400).contains(&status), "{}", status);
    assert_eq!(location, "/broker/nubra/totp");
    assert!(fake.calls("/sendphoneotp").is_empty());

    // Signed in, but the page was opened by another site (the Lax session
    // cookie rides along) or by nothing that shows it came from OpenAlgo:
    // the page opens with its Send OTP action, nothing is sent, and none
    // of it counts against the login limit (5 a minute).
    let foreign: [&[(&str, &str)]; 6] = [
        &[("sec-fetch-site", "cross-site")],
        &[("sec-fetch-site", "same-site")],
        &[("origin", "http://evil.example")],
        &[("origin", "null")],
        &[("referer", "http://evil.example/page")],
        &[],
    ];
    for _ in 0..2 {
        for extra in foreign {
            let (status, location, _) = send(get_with(Some(&cookie), extra), 2).await;
            assert!((300..400).contains(&status), "{:?}: {}", extra, status);
            assert_eq!(location, "/broker/nubra/totp?otp=send", "{:?}", extra);
        }
    }
    assert!(fake.calls("/sendphoneotp").is_empty());
    assert!(!nubra.otp_pending());

    // Signed in and opened from OpenAlgo: the OTP is sent, then the OTP
    // page opens (same address as above, so not rate-limited).
    let (status, location, _) = send(get(Some(&cookie)), 2).await;
    assert!((300..400).contains(&status), "{}", status);
    assert_eq!(location, "/broker/nubra/totp");
    assert_eq!(fake.calls("/sendphoneotp").len(), 2);
    assert!(nubra.otp_pending());

    // A cross-site open now leaves the pending OTP as it is.
    let (_, location, _) = send(
        get_with(Some(&cookie), &[("sec-fetch-site", "cross-site")]),
        2,
    )
    .await;
    assert_eq!(location, "/broker/nubra/totp?otp=send");
    assert_eq!(fake.calls("/sendphoneotp").len(), 2);
    assert!(nubra.otp_pending());

    // A typed address, and webviews without Sec-Fetch-Site whose Origin
    // or Referer names the app: sent.
    let referer = format!("{}/broker", app_origin);
    let own: [&[(&str, &str)]; 3] = [
        &[("sec-fetch-site", "none")],
        &[("origin", app_origin.as_str())],
        &[("referer", referer.as_str())],
    ];
    for (i, extra) in own.into_iter().enumerate() {
        let before = fake.calls("/sendphoneotp").len();
        let (_, location, _) = send(get_with(Some(&cookie), extra), 30 + i as u8).await;
        assert_eq!(location, "/broker/nubra/totp", "{:?}", extra);
        assert_eq!(fake.calls("/sendphoneotp").len(), before + 2, "{:?}", extra);
    }
    assert!(nubra.otp_pending());

    // The page's Send OTP action is a POST that needs the CSRF token and a
    // same-origin request: without the token, or with it from another
    // site, nothing is sent and the pending OTP stays as it is.
    let csrf = urlencoding::encode(&web.csrf_token).into_owned();
    let resend = format!("action=resend&csrf_token={}", csrf);
    let sent = fake.calls("/sendphoneotp").len();
    let (status, _, _) = send(post("action=resend"), 10).await;
    assert!(status == 400 || status == 403, "{}", status);
    for foreign in [
        ("origin", "http://evil.example"),
        ("origin", "null"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
    ] {
        let (status, _, _) = send(post_with(&resend, Some(foreign)), 10).await;
        assert_eq!(status, 403, "{:?}", foreign);
    }
    assert_eq!(fake.calls("/sendphoneotp").len(), sent);
    assert!(nubra.otp_pending());

    // No CSRF token: refused, and the pending OTP is untouched.
    let (status, _, _) = send(post("otp=123456"), 3).await;
    assert!(status == 400 || status == 403, "{}", status);
    assert!(nubra.otp_pending());

    // The right CSRF token from another site: refused as well, untouched.
    let good = format!("otp=123456&csrf_token={}", csrf);
    for foreign in [
        ("origin", "http://evil.example"),
        ("origin", "null"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let (status, _, _) = send(post_with(&good, Some(foreign)), 3).await;
        assert_eq!(status, 403, "{:?}", foreign);
    }
    assert!(nubra.otp_pending());
    assert!(fake.calls("/verifyphoneotp").is_empty());

    // No OTP: the field error, still pending.
    let (status, _, v) = send(post(&format!("csrf_token={}", csrf)), 4).await;
    assert_eq!(status, 400, "{}", v);
    assert_eq!(v["status"], "error");
    assert_eq!(
        v["message"],
        "Enter the OTP sent to your registered mobile number to sign in."
    );
    assert!(nubra.otp_pending());

    // The OTP under the web's fallback name `totp`: signed in.
    let (status, _, v) = send(post(&format!("totp=123456&csrf_token={}", csrf)), 5).await;
    assert_eq!(status, 200, "{}", v);
    assert_eq!(
        v,
        json!({"status": "success", "message": "Authentication successful", "redirect": "/dashboard"})
    );
    assert_eq!(
        ctx.get_broker_session().map(|s| s.broker_id),
        Some("nubra".to_string())
    );
    assert_eq!(fake.calls("/verifyphoneotp")[0].temp, "TEMP2");

    // The same OTP again: the login has to start over.
    let (status, _, v) = send(post(&format!("otp=123456&csrf_token={}", csrf)), 6).await;
    assert_eq!(status, 401, "{}", v);
    assert_eq!(v["status"], "error");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("Start the Nubra login again"),
        "{}",
        v
    );
    assert_eq!(fake.calls("/verifyphoneotp").len(), 1);

    // The page's Send OTP action from OpenAlgo, with its CSRF token: sent.
    let sent = fake.calls("/sendphoneotp").len();
    let (status, _, v) = send(
        post_with(&resend, Some(("sec-fetch-site", "same-origin"))),
        8,
    )
    .await;
    assert_eq!(status, 200, "{}", v);
    assert_eq!(v["status"], "success", "{}", v);
    assert_eq!(fake.calls("/sendphoneotp").len(), sent + 2);
    assert!(nubra.otp_pending());

    // A wrong OTP: Nubra's reason reaches the trader, and the login is
    // used up.
    let (status, _, v) = send(post(&format!("otp=111111&csrf_token={}", csrf)), 8).await;
    assert_eq!(status, 401, "{}", v);
    assert!(
        v["message"].as_str().unwrap().contains("Invalid OTP"),
        "{}",
        v
    );
    assert!(!nubra.otp_pending());

    // An OTP sent to the saved number is not redeemed for a number saved
    // over it since: refused before Nubra is asked.
    send(get(Some(&cookie)), 9).await;
    assert!(nubra.otp_pending());
    {
        let conn = ctx.sqlite.conn().unwrap();
        credentials::save(
            &conn,
            &ctx.security,
            "nubra",
            CredentialUpdate {
                api_key: Some(Secret::new("9999999998")),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let verified = fake.calls("/verifyphoneotp").len();
    let (status, _, v) = send(post(&good), 9).await;
    assert_eq!(status, 401, "{}", v);
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("number was changed"),
        "{}",
        v
    );
    assert_eq!(fake.calls("/verifyphoneotp").len(), verified);
    assert!(!nubra.otp_pending());

    // Opening the page is login-limited like the form (5 a minute).
    for _ in 0..5 {
        send(get(Some(&cookie)), 7).await;
    }
    let (status, _, _) = send(get(Some(&cookie)), 7).await;
    assert_eq!(status, 429);

    ctx.shutdown().await;
}

#[tokio::test]
async fn orders_go_out_as_intent_items_in_paise() {
    let (b, fake, auth) = setup().await;
    let s = b.symbols().unwrap().clone();
    let o =
        ResolvedOrder::resolve(&order_req("RELIANCE", "NSE", "LIMIT", 1425.5, 0.0), &s).unwrap();
    let r = b.place_order(&auth, &o).await.unwrap();
    assert_eq!(r.order_id, "5001");
    let sent = &fake.calls("/sentinel/orders/create")[0];
    assert_eq!(sent.device, "OPENALGO");
    assert_eq!(
        sent.body,
        json!({"orders": [{"refId": 72329, "qty": 10, "side": "BUY", "deliveryType": "IDAY",
            "priceType": "LIMIT", "validityType": "DAY", "isMultiLeg": false,
            "executionMode": "ENTRY", "entryPrice": 142550, "stratTags": ["openalgo"]}]})
    );

    let slm = ResolvedOrder::resolve(
        &order_req("NIFTY27OCT26FUT", "NFO", "SL-M", 0.0, 25000.0),
        &s,
    )
    .unwrap();
    b.place_order(&auth, &slm).await.unwrap();
    let sent = &fake.calls("/sentinel/orders/create")[1];
    assert_eq!(sent.body["orders"][0]["refId"], 88888);
    assert_eq!(sent.body["orders"][0]["validityType"], "IOC");
    assert_eq!(
        sent.body["orders"][0]["entryConfig"],
        json!({"triggers": {"ltp": {"atOrAbove": {"value": 2500000}}}})
    );

    // A stop without a trigger never reaches the broker.
    let bad = ResolvedOrder::resolve(&order_req("RELIANCE", "NSE", "SL", 10.0, 0.0), &s).unwrap();
    assert!(b.place_order(&auth, &bad).await.is_err());
    assert_eq!(fake.calls("/sentinel/orders/create").len(), 2);

    // Index rows have no numeric ref id.
    let idx =
        ResolvedOrder::resolve(&order_req("NIFTY", "NSE_INDEX", "MARKET", 0.0, 0.0), &s).unwrap();
    assert!(b.place_order(&auth, &idx).await.is_err());

    let m = ModifyOrderRequest {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        action: "BUY".into(),
        product: "CNC".into(),
        pricetype: "LIMIT".into(),
        quantity: 2,
        price: 1400.0,
        trigger_price: 0.0,
        disclosed_quantity: 0,
    };
    let rm = ResolvedModify::resolve("1001", &m, &s).unwrap();
    assert_eq!(b.modify_order(&auth, &rm).await.unwrap().order_id, "1001");
    assert_eq!(
        fake.calls("/sentinel/orders/modify")[0].body,
        json!({"orders": [{"orderId": 1001, "qty": 2, "deliveryType": "CNC", "priceType": "LIMIT",
            "validityType": "DAY", "executionMode": "ENTRY", "entryPrice": 140000}]})
    );

    assert_eq!(
        b.cancel_order(&auth, "1001").await.unwrap().order_id,
        "1001"
    );
    assert_eq!(
        fake.calls("/sentinel/orders/cancel")[0].body,
        json!({"orders": [{"orderId": 1001}]})
    );
    let e = b.cancel_order(&auth, "1002").await.unwrap_err();
    assert_eq!(e.client_message(), "Order already cancelled");
    assert!(b.cancel_order(&auth, "abc").await.is_err());
}

#[tokio::test]
async fn refusals_carry_the_broker_reason() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let mut rows = master().snapshot().rows().to_vec();
    rows.push(SymbolData {
        symbol: "SBIN".into(),
        brsymbol: "SBIN".into(),
        name: "SBIN".into(),
        exchange: "BSE".into(),
        brexchange: "BSE".into(),
        token: "61001".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    });
    let s = SymbolResolver::new();
    s.load(rows);
    let b = NubraBroker::with_urls(s.clone(), host, "ws://127.0.0.1:1").with_fast_timings();
    let o = ResolvedOrder::resolve(&order_req("SBIN", "BSE", "MARKET", 0.0, 0.0), &s).unwrap();
    let e = b
        .place_order(&AuthToken::new("SESS1"), &o)
        .await
        .unwrap_err();
    assert_eq!(e.client_message(), "Insufficient funds");

    // HTTP 440: log in again, never an empty book.
    let e = b
        .get_order_book(&AuthToken::new("EXPIRED"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("Log in to Nubra again"));
    let e = b.get_funds(&AuthToken::new("EXPIRED")).await.unwrap_err();
    assert!(e.client_message().contains("expired"));
}

#[tokio::test]
async fn books_funds_and_margin() {
    let (b, fake, auth) = setup().await;
    let book = b.get_order_book(&auth).await.unwrap();
    assert_eq!(book.len(), 6);
    let opt = book.iter().find(|o| o.order_id == "1002").unwrap();
    assert_eq!(
        (opt.symbol.as_str(), opt.exchange.as_str()),
        ("NIFTY27OCT2625000CE", "NFO")
    );
    assert_eq!(opt.status, "trigger pending");
    assert_eq!(
        book.iter().find(|o| o.order_id == "1003").unwrap().status,
        "complete"
    );

    let trades = b.get_trade_book(&auth).await.unwrap();
    assert_eq!(trades.len(), 2);

    let pos = b.get_positions(&auth).await.unwrap();
    assert_eq!(pos[1].symbol, "NIFTY27OCT2625000CE");
    assert_eq!(
        b.get_open_position(&auth, "NIFTY27OCT2625000CE", Exchange::Nfo, Product::Cnc)
            .await
            .unwrap(),
        -75
    );
    assert_eq!(
        b.get_open_position(&auth, "RELIANCE", Exchange::Nse, Product::Cnc)
            .await
            .unwrap(),
        0
    );

    // 429 once, then the holdings.
    let h = b.get_holdings(&auth).await.unwrap();
    assert_eq!(h[0].symbol, "RELIANCE");
    assert_eq!(fake.calls("/sentinel/portfolio/holdings").len(), 2);

    let f = b.get_funds(&auth).await.unwrap();
    assert_eq!(f.available_cash, 100000.5);
    assert_eq!(f.utilised_debits, 3500.0);

    let legs = vec![MarginLeg {
        key: QuoteKey::new("NFO", "NIFTY27OCT2625000CE"),
        action: openalgo_desktop_lib::brokers::common::mapping::Action::Sell,
        quantity: 75,
        product: Product::Nrml,
        pricetype: openalgo_desktop_lib::brokers::common::mapping::PriceType::Limit,
        price: 120.5,
        trigger_price: 0.0,
    }];
    let m = b.calculate_margin(&auth, &legs).await.unwrap();
    assert_eq!(m.total_margin_required, 121350.0);
    assert_eq!(
        fake.calls("/sentinel/orders/funds_required")[0].body,
        json!({"requestType": "NEW", "orders": [{"refId": 99999, "qty": 75, "side": "SELL",
            "deliveryType": "CNC", "priceType": "LIMIT", "validityType": "DAY",
            "isMultiLeg": false, "executionMode": "ENTRY", "entryPrice": 12050,
            "stratTags": ["openalgo-margin"]}]})
    );
    let none = vec![MarginLeg {
        key: QuoteKey::new("NSE", "NOPE"),
        ..legs[0].clone()
    }];
    assert!(b.calculate_margin(&auth, &none).await.is_err());
}

#[tokio::test]
async fn cancel_all_and_close_all() {
    let (b, fake, auth) = setup().await;
    let r = b.cancel_all_orders(&auth).await.unwrap();
    // open + gtt buckets: 1001, 1002 (refused), 1006.
    let mut c = r.cancelled.clone();
    c.sort();
    assert_eq!(c, ["1001", "1006"]);
    assert_eq!(r.failed, ["1002"]);
    assert_eq!(fake.calls("/sentinel/orders/cancel").len(), 3);

    let r = b.close_all_positions(&auth).await.unwrap();
    assert_eq!(r.placed.len(), 2, "{:?}", r.failed);
    let sent = fake.calls("/sentinel/orders/create");
    let first = &sent[0].body["orders"][0];
    assert_eq!(
        (first["refId"].clone(), first["side"].clone()),
        (json!(72329), json!("SELL"))
    );
    assert_eq!(first["deliveryType"], "IDAY");
    assert_eq!(first["priceType"], "MARKET");
    let second = &sent[1].body["orders"][0];
    assert_eq!(
        (
            second["refId"].clone(),
            second["side"].clone(),
            second["qty"].clone()
        ),
        (json!(99999), json!("BUY"), json!(75))
    );
}

#[tokio::test]
async fn quotes_depth_and_index_snapshot() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let subs = Arc::new(Mutex::new(Vec::new()));
    let market = ws_server(frame("bucket"), subs.clone()).await;
    let b = NubraBroker::with_urls(master(), host, market).with_fast_timings();
    let auth = AuthToken::new("SESS1");

    let q = b
        .get_quote(&auth, &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!((q.ltp, q.bid, q.ask), (1426.5, 1426.0, 1427.0));
    assert!(fake.calls("/orderbooks/72329")[0]
        .query
        .contains("levels=1"));

    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE", "RELIANCE"))
        .await
        .unwrap();
    assert_eq!(d.bids.len(), 5);
    assert_eq!(d.asks[0].price, 1427.0);

    let mq = b
        .get_multiquotes(
            &auth,
            &[
                QuoteKey::new("NSE", "RELIANCE"),
                QuoteKey::new("NSE", "NOPE"),
            ],
        )
        .await
        .unwrap();
    assert!(mq[0].data.is_some());
    assert!(mq[1].error.is_some());

    // Index: one-shot index_bucket subscription on the market socket.
    let n = b
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!((n.ltp, n.open, n.high), (25012.35, 24900.0, 25100.0));
    let sent = subs.lock().clone();
    assert_eq!(
        sent[0],
        r#"batch_subscribe SESS1 index_bucket {"instruments":[],"indexes":["NIFTY"]} 1m NSE"#
    );
    // Index depth is empty, as on the web.
    let d = b
        .get_market_depth(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(d.ltp, 0.0);

    // Unreachable socket: zeros, not an error.
    let b2 = NubraBroker::with_urls(master(), "http://127.0.0.1:1", "ws://127.0.0.1:1")
        .with_fast_timings();
    let z = b2
        .get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
        .await
        .unwrap();
    assert_eq!(z.ltp, 0.0);
}

#[tokio::test]
async fn history_is_chunked_and_paced() {
    let (b, fake, auth) = setup().await;
    let req = HistoryRequest {
        key: QuoteKey::new("NSE", "RELIANCE"),
        interval: "1m".into(),
        start: NaiveDate::from_ymd_opt(2025, 8, 1).unwrap(),
        end: NaiveDate::from_ymd_opt(2025, 9, 15).unwrap(),
    };
    let c = b.get_history(&auth, &req).await.unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].timestamp, 1759463100);
    let calls = fake.calls("/charts/timeseries");
    assert_eq!(calls.len(), 2);
    let q0 = &calls[0].body["query"][0];
    assert_eq!(q0["startDate"], "2025-08-01T00:00:00.000Z");
    assert_eq!(q0["endDate"], "2025-08-30T23:59:59.000Z");
    assert_eq!(q0["interval"], "1m");
    assert_eq!(q0["type"], "STOCK");
    assert_eq!(
        calls[1].body["query"][0]["startDate"],
        "2025-08-31T00:00:00.000Z"
    );

    let mut bad = req.clone();
    bad.interval = "4h".into();
    assert!(b.get_history(&auth, &bad).await.is_err());
}

#[tokio::test]
async fn master_contract_download() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let b = NubraBroker::with_urls(SymbolResolver::new(), host, "ws://127.0.0.1:1");
    let rows = b
        .download_master_contract(&AuthToken::new("SESS1"))
        .await
        .unwrap();
    // NSE 4 + BSE 2 + MCX 1 + 7 indices.
    assert_eq!(rows.len(), 14);
    assert!(rows.iter().any(|r| r.exchange == "BFO"), "{:?}", rows);
    assert!(rows
        .iter()
        .any(|r| r.symbol == "NIFTY27OCT2625000CE" && r.exchange == "NFO"));
    assert!(rows
        .iter()
        .any(|r| r.symbol == "INDIAVIX" && r.exchange == "NSE_INDEX"));
    let calls = fake.calls("/public/indexes");
    assert_eq!(calls[0].authorization, "");
    let refdata: Vec<Seen> = fake
        .seen
        .lock()
        .iter()
        .filter(|s| s.path.starts_with("/refdata/refdata/"))
        .cloned()
        .collect();
    assert_eq!(refdata.len(), 3);
    assert_eq!(refdata[0].device, "OPENALGO");

    let e = b
        .download_master_contract(&AuthToken::new("EXPIRED"))
        .await
        .unwrap_err();
    assert!(e.client_message().contains("expired"));

    // MC-02 (a hardening over the web, which skips the exchange): one
    // exchange refused fails the download, naming it, so the stored master
    // is kept.
    fake.bse_down.store(true, Ordering::SeqCst);
    let msg = b
        .download_master_contract(&AuthToken::new("SESS1"))
        .await
        .unwrap_err()
        .client_message();
    assert!(msg.contains("did not send its BSE instruments"), "{}", msg);
    assert!(msg.contains("existing symbols were kept"), "{}", msg);
}

#[tokio::test]
async fn order_updates_flow_through_the_relay() {
    let fake = Arc::new(Fake::default());
    let host = serve(fake.clone()).await;
    let texts = Arc::new(Mutex::new(Vec::new()));
    let order_ws = ws_server(frame("order"), texts.clone()).await;
    *fake.order_ws.lock() = order_ws;
    let b = NubraBroker::with_urls(master(), host, "ws://127.0.0.1:1");
    let mut feed = b.order_socket(&AuthToken::new("SESS1")).unwrap();
    assert!(feed.awaits_auth_ack());
    let req = feed.ws_request().unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut events = Vec::new();
    while events.len() < 2 {
        let m = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        events.extend(feed.parse(&m));
    }
    assert_eq!(events[0], FeedEvent::AuthOk);
    let FeedEvent::OrderUpdate(u) = &events[1] else {
        panic!("{:?}", events[1])
    };
    assert_eq!(u.orderid, "1234567890");
    assert_eq!(
        (u.symbol.as_str(), u.order_status.as_str()),
        ("RELIANCE", "complete")
    );
    assert_eq!(
        texts.lock()[0],
        "subscribe SESS1 notifications notification"
    );
    assert_eq!(fake.calls("/userinfo")[0].authorization, "Bearer SESS1");

    // A refused session stops the stream with a trader-facing reason.
    let b = NubraBroker::with_urls(
        master(),
        serve(Arc::new(Fake::default())).await,
        "ws://127.0.0.1:1",
    );
    let mut feed = b.order_socket(&AuthToken::new("EXPIRED")).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    let m = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(feed.parse(&m)[..], [FeedEvent::AuthFailed(_)]));
}

#[tokio::test]
async fn market_feed_handshake_against_a_fake_socket() {
    let subs = Arc::new(Mutex::new(Vec::new()));
    let market = ws_server(frame("orderbook"), subs.clone()).await;
    let b = NubraBroker::with_urls(master(), "http://127.0.0.1:1", market);
    let mut feed = b.create_feed(&AuthToken::new("SESS1")).unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(feed.ws_request().unwrap())
        .await
        .unwrap();
    let sub = openalgo_desktop_lib::brokers::common::streaming::FeedSubscription {
        symbol: "RELIANCE".into(),
        exchange: "NSE".into(),
        token: "72329".into(),
        brsymbol: "RELIANCE".into(),
        brexchange: "NSE".into(),
        mode: openalgo_desktop_lib::brokers::common::streaming::FeedMode::Depth,
        depth: 5,
    };
    for f in feed.subscribe_frames(&[sub]) {
        ws.send(f).await.unwrap();
    }
    let m = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ev = feed.parse(&m);
    assert!(matches!(ev[..], [FeedEvent::Tick(_), FeedEvent::Depth(_)]));
    assert!(subs.lock()[0].starts_with("batch_subscribe SESS1 index "));
}
