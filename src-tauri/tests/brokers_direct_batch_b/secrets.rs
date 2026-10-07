//! Credentials never leak: every auth, REST, socket and MQTT error path of
//! the five adapters is driven with sentinel credentials, and the sentinel
//! must not appear in the captured tracing output (all levels), in the
//! error's `Display` or `Debug`, or in the trader-facing message.
//!
//! Each path runs twice: against a closed port (transport errors, whose
//! text carries the request URL) and against a fake broker that refuses
//! every request (the adapters' own refusal logging).

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Router;
use base64::Engine;
use futures_util::StreamExt;
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::fivepaisa::{FivepaisaBroker, Session as FivepaisaSession};
use openalgo_desktop_lib::brokers::iiflcapital::mqtt_relay::MqttEndpoint;
use openalgo_desktop_lib::brokers::iiflcapital::IiflCapitalBroker;
use openalgo_desktop_lib::brokers::indmoney::{Endpoints, IndmoneyBroker};
use openalgo_desktop_lib::brokers::nubra::NubraBroker;
use openalgo_desktop_lib::brokers::tradejini::{TradejiniBroker, WsTimings};
use openalgo_desktop_lib::brokers::types::*;
use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
use openalgo_desktop_lib::error::AppError;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Text sentinel for keys, secrets, tokens and passwords.
const S: &str = "SENTINELq7Zx";
/// Digit sentinel for TOTP / PIN fields that must be numeric.
const D: &str = "918273";

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Every event at every level, on this thread (tests run on the
/// current-thread runtime, so spawned relay tasks log here too).
fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let c = Captured::default();
    let w = c.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || w.clone())
        .finish();
    (c, tracing::subscriber::set_default(sub))
}

fn clean(what: &str, text: &str) {
    assert!(!text.contains(S), "{} leaked the sentinel: {}", what, text);
    assert!(
        !text.contains(D),
        "{} leaked the digit sentinel: {}",
        what,
        text
    );
}

/// The capture saw the adapter's logging (so the check is not vacuous)
/// and none of it carries a credential.
fn logs_clean(what: &str, log: &Captured) {
    let text = log.text();
    assert!(!text.is_empty(), "{}: nothing was captured", what);
    clean(what, &text);
}

fn check_err(what: &str, e: &AppError) {
    clean(what, &format!("{} | {:?} | {}", e, e, e.client_message()));
}

fn check_result<T>(what: &str, r: openalgo_desktop_lib::error::Result<T>) {
    if let Err(e) = r {
        check_err(what, &e);
    }
}

// ---------------------------------------------------------------------------
// Upstreams
// ---------------------------------------------------------------------------

/// A loopback port with nothing listening.
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn closed_http() -> String {
    format!("http://127.0.0.1:{}", closed_port())
}

fn closed_ws() -> String {
    format!("ws://127.0.0.1:{}", closed_port())
}

/// A fake broker that refuses every request with HTTP 401 and a generic
/// body (no credential echoed back).
async fn refusing_http() -> String {
    let app = Router::new().fallback(|| async {
        (
            StatusCode::UNAUTHORIZED,
            [("content-type", "application/json")],
            r#"{"status":"error","message":"unauthorized","error":"unauthorized"}"#,
        )
            .into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{}", addr)
}

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.trim_end_matches("_INDEX").into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn symbols() -> SymbolResolver {
    let r = SymbolResolver::new();
    r.load(vec![
        row("SBIN", "NSE", "3045"),
        row("NIFTY", "NSE_INDEX", "26000"),
    ]);
    r
}

fn order() -> ResolvedOrder {
    let req = OrderRequest {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        side: "BUY".into(),
        quantity: 1,
        price: 800.0,
        order_type: "LIMIT".into(),
        product: "MIS".into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };
    ResolvedOrder::resolve(&req, &symbols()).unwrap()
}

/// Every REST-backed trait method with this session.
async fn drive_rest(name: &str, b: &dyn Broker, auth: &AuthToken) {
    let q = QuoteKey::new("NSE", "SBIN");
    check_result(name, b.place_order(auth, &order()).await);
    check_result(name, b.cancel_order(auth, "1").await);
    check_result(name, b.cancel_all_orders(auth).await);
    check_result(name, b.close_all_positions(auth).await);
    check_result(name, b.get_order_book(auth).await);
    check_result(name, b.get_trade_book(auth).await);
    check_result(name, b.get_positions(auth).await);
    check_result(name, b.get_holdings(auth).await);
    check_result(name, b.get_funds(auth).await);
    check_result(name, b.get_quote(auth, &q).await);
    if let Ok(rows) = b.get_multiquotes(auth, std::slice::from_ref(&q)).await {
        for r in rows {
            clean(name, r.error.as_deref().unwrap_or(""));
        }
    }
    check_result(name, b.get_market_depth(auth, &q).await);
    check_result(name, b.download_master_contract(auth).await);
}

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: format!("{S}:::{S}:::{S}"),
        api_secret: Some(S.into()),
        client_id: Some(S.into()),
        password: Some(D.into()),
        totp: Some(D.into()),
        request_token: Some(format!("{S}:::{S}")),
        auth_code: Some(format!("{S}:::{S}")),
        ..Default::default()
    }
}

/// A JWT whose claims name a user and whose signature is the sentinel, so
/// the MQTT password (`OPENID~~<jwt>~`) carries it.
fn sentinel_jwt() -> String {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.{}",
        b64.encode(r#"{"alg":"none"}"#),
        b64.encode(r#"{"preferred_username":"CL1"}"#),
        S
    )
}

// ---------------------------------------------------------------------------
// Per broker
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tradejini_never_leaks_credentials() {
    let (log, _g) = capture();
    let timings = WsTimings {
        connect: Duration::from_millis(500),
        ..WsTimings::default()
    };
    for base in [closed_http(), refusing_http().await] {
        let b = TradejiniBroker::with_urls(symbols(), base, closed_ws()).with_timings(timings);
        check_result("tradejini auth", b.authenticate(creds()).await);
        drive_rest("tradejini", &b, &AuthToken::new(format!("{S}:{S}"))).await;
        // The live feed's request carries the token (it must), but nothing
        // that prints the feed does.
        let _ = b.create_feed(&AuthToken::new(format!("{S}:{S}")));
    }
    logs_clean("tradejini logs", &log);
}

#[tokio::test]
async fn fivepaisa_never_leaks_credentials() {
    let (log, _g) = capture();
    let auth = AuthToken::new(
        FivepaisaSession {
            api_key: S.into(),
            client_code: "50001234".into(),
            access_token: S.into(),
        }
        .encode(),
    );
    for base in [closed_http(), refusing_http().await] {
        let b = FivepaisaBroker::with_urls(symbols(), base.clone(), format!("{}/master", base))
            .with_feed_url(closed_ws());
        check_result("5paisa auth", b.authenticate(creds()).await);
        drive_rest("5paisa", &b, &auth).await;
        let mut rx = b
            .start_order_updates(&auth, Duration::from_millis(1))
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(1200), rx.recv()).await;
        b.stop_order_updates();
    }
    logs_clean("5paisa logs", &log);
}

#[tokio::test]
async fn nubra_never_leaks_credentials() {
    let (log, _g) = capture();
    let auth = AuthToken::new(S);
    for base in [closed_http(), refusing_http().await] {
        let b = NubraBroker::with_urls(symbols(), base, closed_ws())
            .with_fast_timings()
            .with_order_ws_fallback(closed_ws());
        check_result("nubra auth", b.authenticate(creds()).await);
        drive_rest("nubra", &b, &auth).await;
        // Index quote: a one-shot market socket.
        check_result(
            "nubra index quote",
            b.get_quote(&auth, &QuoteKey::new("NSE_INDEX", "NIFTY"))
                .await,
        );
        check_result("nubra margin", b.calculate_margin(&auth, &[]).await);
        // Order-update socket through the relay: /userinfo fails or is
        // refused, the fallback socket is closed.
        let mut feed = b.create_order_feed(&auth).unwrap();
        if let Ok((mut ws, _)) = tokio_tungstenite::connect_async(feed.ws_request().unwrap()).await
        {
            while let Ok(Some(Ok(m))) =
                tokio::time::timeout(Duration::from_secs(5), ws.next()).await
            {
                for ev in feed.parse(&m) {
                    clean("nubra order feed", &format!("{:?}", ev));
                }
            }
        }
    }
    logs_clean("nubra logs", &log);
}

#[tokio::test]
async fn indmoney_never_leaks_credentials() {
    let (log, _g) = capture();
    for base in [closed_http(), refusing_http().await] {
        let b = IndmoneyBroker::with_endpoints(
            symbols(),
            Endpoints {
                api: base,
                prices_ws: closed_ws(),
                orders_ws: closed_ws(),
            },
        );
        // Pasted token path, then the MPIN + TOTP path.
        check_result("indmoney auth", b.authenticate(creds()).await);
        let mut c = creds();
        c.api_secret = None;
        check_result("indmoney auth (mpin)", b.authenticate(c).await);
        drive_rest("indmoney", &b, &AuthToken::new(S)).await;
        check_result(
            "indmoney margin",
            b.calculate_margin(&AuthToken::new(S), &[]).await,
        );
    }
    logs_clean("indmoney logs", &log);
}

#[tokio::test]
async fn iiflcapital_never_leaks_credentials() {
    let (log, _g) = capture();
    let jwt = sentinel_jwt();
    let auth = AuthToken::new(jwt.clone());
    for base in [closed_http(), refusing_http().await] {
        let b = IiflCapitalBroker::with_base_url(symbols(), base)
            .with_backoff(Duration::from_millis(5), Duration::from_millis(5))
            .with_mqtt(MqttEndpoint::plain("127.0.0.1", closed_port()));
        check_result("iifl auth", b.authenticate(creds()).await);
        drive_rest("iifl", &b, &auth).await;
        check_result("iifl margin", b.calculate_margin(&auth, &[]).await);
    }
    logs_clean("iifl logs", &log);
}

/// Reads one MQTT CONNECT and answers CONNACK with the given return code
/// (5 = not authorized), or closes at once with `None`.
async fn fake_bridge(code: Option<u8>) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            if let Some(c) = code {
                let _ = s.write_all(&[0x20, 0x02, 0x00, c]).await;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    });
    port
}

#[tokio::test]
async fn iiflcapital_mqtt_errors_never_leak_the_session() {
    let (log, _g) = capture();
    let auth = AuthToken::new(sentinel_jwt()).with_user_id("778");
    let refused = fake_bridge(Some(5)).await;
    let dropped = fake_bridge(None).await;
    for port in [closed_port(), refused, dropped] {
        let b = IiflCapitalBroker::with_base_url(symbols(), closed_http())
            .with_mqtt(MqttEndpoint::plain("127.0.0.1", port));
        for mut feed in [
            b.create_feed(&auth).unwrap(),
            b.create_order_feed(&auth).unwrap(),
        ] {
            let req = feed.ws_request().unwrap();
            clean("iifl relay address", &format!("{:?}", req.uri()));
            let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
            while let Ok(Some(Ok(m))) =
                tokio::time::timeout(Duration::from_secs(10), ws.next()).await
            {
                for ev in feed.parse(&m) {
                    clean("iifl feed event", &format!("{:?}", ev));
                }
            }
        }
    }
    logs_clean("iifl mqtt logs", &log);
}
