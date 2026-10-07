//! Shared harness for the batch A broker suites: a local fake HTTP broker
//! (axum on an ephemeral loopback port) that records every request, and a
//! one-connection-at-a-time fake WebSocket server.

#![allow(dead_code, unused_imports)]

pub use axum::http::{HeaderMap, Method, StatusCode};
pub use axum::response::{IntoResponse, Response};
pub use chrono::NaiveDate;
pub use openalgo_desktop_lib::brokers::common::mapping::{
    Action, Exchange, PriceType, Product, Validity,
};
pub use openalgo_desktop_lib::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
};
pub use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
pub use openalgo_desktop_lib::brokers::types::*;
pub use openalgo_desktop_lib::brokers::{Broker, BrokerCredentials};
pub use parking_lot::Mutex;
pub use serde_json::{json, Value};
pub use std::sync::Arc;

use axum::body::Bytes;
use axum::http::Uri;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use std::future::Future;
use std::pin::Pin;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;

/// `include_str!` of `tests/fixtures/brokers/<broker>/<name>`.
#[macro_export]
macro_rules! fixture {
    ($b:literal, $name:literal) => {
        include_str!(concat!("../fixtures/brokers/", $b, "/", $name))
    };
}

/// One request the fake broker received.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: Method,
    pub path: String,
    pub query: String,
    pub headers: HeaderMap,
    pub body: String,
}

impl Req {
    /// Body parsed as JSON (`Null` when it is not JSON).
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// A header value, empty when absent.
    pub fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    /// A query parameter, percent-decoded.
    pub fn param(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.query.as_bytes())
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }
}

type Handler = dyn Fn(&Req) -> Response + Send + Sync;

/// A fake broker on `127.0.0.1:<ephemeral>`. Dropping it stops the server.
pub struct Fake {
    pub base: String,
    seen: Arc<Mutex<Vec<Req>>>,
    task: JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fake {
    /// Serve every request with `handler`, recording it first.
    pub async fn start(handler: impl Fn(&Req) -> Response + Send + Sync + 'static) -> Self {
        let seen: Arc<Mutex<Vec<Req>>> = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let rec = seen.clone();
        let app = Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let rec = rec.clone();
                let handler = handler.clone();
                async move {
                    let req = Req {
                        method,
                        path: uri.path().to_string(),
                        query: uri.query().unwrap_or_default().to_string(),
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    };
                    rec.lock().push(req.clone());
                    handler(&req)
                }
            },
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base: format!("http://{}", addr),
            seen,
            task,
        }
    }

    /// Every recorded request whose path ends with `suffix`.
    pub fn calls(&self, suffix: &str) -> Vec<Req> {
        self.seen
            .lock()
            .iter()
            .filter(|r| r.path.ends_with(suffix))
            .cloned()
            .collect()
    }

    /// Every recorded request.
    pub fn all(&self) -> Vec<Req> {
        self.seen.lock().clone()
    }
}

/// A JSON 200 response.
pub fn ok(v: impl ToString) -> Response {
    with_status(StatusCode::OK, v)
}

/// A JSON response with this status.
pub fn with_status(status: StatusCode, v: impl ToString) -> Response {
    (
        status,
        [("content-type", "application/json")],
        v.to_string(),
    )
        .into_response()
}

/// A plain-text (CSV) 200 response.
pub fn text(v: impl Into<String>) -> Response {
    (StatusCode::OK, [("content-type", "text/plain")], v.into()).into_response()
}

/// A binary 200 response.
pub fn bytes(v: Vec<u8>) -> Response {
    (StatusCode::OK, v).into_response()
}

pub type ServerWs = WebSocketStream<tokio::net::TcpStream>;
type WsHandler = dyn Fn(ServerWs) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

/// A fake WebSocket server; each accepted connection is handed to the
/// handler. Records the request path+query of every handshake.
pub struct FakeWs {
    pub url: String,
    pub handshakes: Arc<Mutex<Vec<(String, HeaderMap)>>>,
    task: JoinHandle<()>,
}

impl Drop for FakeWs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeWs {
    pub async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn(ServerWs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let handler: Arc<WsHandler> = Arc::new(move |ws| Box::pin(handler(ws)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handshakes: Arc<Mutex<Vec<(String, HeaderMap)>>> = Arc::new(Mutex::new(Vec::new()));
        let hs = handshakes.clone();
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                let hs = hs.clone();
                let cb = move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                               resp| {
                    let pq = req
                        .uri()
                        .path_and_query()
                        .map(|p| p.to_string())
                        .unwrap_or_default();
                    hs.lock().push((pq, req.headers().clone()));
                    Ok(resp)
                };
                if let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await {
                    let h = handler.clone();
                    conns.spawn(async move { h(ws).await });
                }
            }
        });
        Self {
            url: format!("ws://{}", addr),
            handshakes,
            task,
        }
    }
}

/// Next text frame from a server-side socket (skipping pings), or `None`
/// when the client closed.
pub async fn next_text(ws: &mut ServerWs) -> Option<String> {
    while let Some(Ok(m)) = ws.next().await {
        match m {
            Message::Text(t) => return Some(t.to_string()),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
    None
}

/// Next binary frame from a server-side socket.
pub async fn next_binary(ws: &mut ServerWs) -> Option<Vec<u8>> {
    while let Some(Ok(m)) = ws.next().await {
        match m {
            Message::Binary(b) => return Some(b.to_vec()),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
    None
}

/// Send a text frame.
pub async fn send_text(ws: &mut ServerWs, s: impl Into<String>) {
    let _ = ws.send(Message::Text(s.into())).await;
}

/// Send a binary frame.
pub async fn send_binary(ws: &mut ServerWs, b: Vec<u8>) {
    let _ = ws.send(Message::Binary(b)).await;
}

/// A master-contract row.
pub fn row(
    symbol: &str,
    brsymbol: &str,
    exchange: &str,
    brexchange: &str,
    token: &str,
    lot: i32,
    tick: f64,
) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: brsymbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: brexchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: lot,
        instrument_type: "EQ".into(),
        tick_size: tick,
    }
}

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

// ---------------------------------------------------------------------------
// Secret hygiene
// ---------------------------------------------------------------------------

/// A credential value that must never show up in a log line, an error's
/// `Display`/`Debug`, or a trader-facing message.
pub const SENTINEL: &str = "SENTINEL9q7z";

/// Every log line emitted on this thread (all levels) while the capture is
/// alive. `#[tokio::test]` runs on one thread, so spawned tasks are caught.
pub struct LogCapture {
    buf: Arc<Mutex<Vec<u8>>>,
    _guard: tracing::subscriber::DefaultGuard,
}

struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CaptureWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn capture_logs() -> LogCapture {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || CaptureWriter(w.clone()))
        .finish();
    LogCapture {
        buf,
        _guard: tracing::subscriber::set_default(sub),
    }
}

impl LogCapture {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock()).into_owned()
    }

    /// Fails when the sentinel reached any captured line.
    pub fn assert_clean(&self) {
        let t = self.text();
        assert!(!t.contains(SENTINEL), "secret in logs:\n{}", t);
    }
}

/// Fails when the sentinel shows in the error's `Display`, `Debug` or the
/// trader-facing message.
pub fn assert_error_clean(e: &openalgo_desktop_lib::error::AppError) {
    let shown = format!("{} | {:?} | {}", e, e, e.client_message());
    assert!(!shown.contains(SENTINEL), "secret in error: {}", shown);
}

/// The error of `r`, checked for the sentinel.
pub fn clean_err<T>(r: openalgo_desktop_lib::error::Result<T>) {
    if let Err(e) = r {
        assert_error_clean(&e);
    }
}

/// A loopback base URL where nothing listens (bound, then released).
pub fn closed_base() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://127.0.0.1:{}", l.local_addr().unwrap().port())
}

/// `closed_base` as a socket URL.
pub fn closed_ws() -> String {
    closed_base().replacen("http://", "ws://", 1)
}

/// Credentials whose every field is the sentinel.
pub fn sentinel_creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: SENTINEL.into(),
        api_secret: Some(SENTINEL.into()),
        client_id: Some(SENTINEL.into()),
        password: Some(SENTINEL.into()),
        totp: Some(SENTINEL.into()),
        request_token: Some(format!("{}:{}", SENTINEL, SENTINEL)),
        auth_code: Some(format!("{}:{}", SENTINEL, SENTINEL)),
        ..Default::default()
    }
}

/// A fake broker that refuses everything with 401 and an error body, and
/// answers sockets with nothing.
pub async fn refusing_fake() -> Fake {
    Fake::start(|_req: &Req| {
        with_status(
            StatusCode::UNAUTHORIZED,
            json!({"status": "error", "stat": "Not_Ok", "message": "refused"}),
        )
    })
    .await
}
