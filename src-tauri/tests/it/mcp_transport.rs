//! The two transports end to end: the `GET /mcp` event stream (bounded,
//! released when the client leaves, ended by its lifetime and by shutdown)
//! and the stdio bridge (`openalgo-desktop mcp`) speaking MCP over a pipe to
//! a real listener, with the token kept out of the logs.

use crate::mcp_support::{LOCAL, M};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use futures_util::StreamExt;
use openalgo_desktop_lib::mcp::http::{MAX_STREAMS, SSE_BUSY_MESSAGE};
use openalgo_desktop_lib::mcp::stdio::{self, Bridge};
use openalgo_desktop_lib::mcp::store::TokenScope;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;

async fn open_stream(m: &M, token: &str) -> axum::response::Response {
    let mut req = Request::builder()
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(LOCAL, 40000)));
    crate::with_host(&mut req, &m.h.ctx);
    openalgo_desktop_lib::server::app(m.h.ctx.clone())
        .oneshot(req)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_streams_are_capped_and_released() {
    let m = M::new().await;
    let tokens: Vec<String> = (0..3).map(|_| m.token(TokenScope::Read)).collect();
    // Unauthenticated: no stream.
    let resp = open_stream(&m, "oamcp_bad").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(m.h.ctx.mcp.open_streams(), 0);

    let mut open = Vec::new();
    for k in 0..MAX_STREAMS {
        let resp = open_stream(&m, &tokens[k % 3]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
        let mut body = resp.into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert_eq!(&first[..], b"retry: 30000\n: openalgo-mcp connected\n\n");
        open.push(body);
    }
    assert_eq!(m.h.ctx.mcp.open_streams(), MAX_STREAMS);
    let busy = open_stream(&m, &tokens[2]).await;
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(busy.headers()[header::RETRY_AFTER], "30");
    let text = axum::body::to_bytes(busy.into_body(), 1024).await.unwrap();
    assert_eq!(&text[..], SSE_BUSY_MESSAGE.as_bytes());

    // A client that goes away releases its slot.
    drop(open);
    assert_eq!(m.h.ctx.mcp.open_streams(), 0);

    // Per-token stream rate: 5 a minute (token 0 opened 2 so far).
    for _ in 0..3 {
        assert_eq!(open_stream(&m, &tokens[0]).await.status(), StatusCode::OK);
    }
    assert_eq!(m.h.ctx.mcp.open_streams(), 0, "responses dropped unread");
    assert_eq!(
        open_stream(&m, &tokens[0]).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_streams_end_by_lifetime_and_shutdown_with_keepalives() {
    let m = M::new().await;
    m.h.ctx
        .mcp
        .set_stream_timing(Duration::from_millis(350), Duration::from_millis(100));
    let t = m.token(TokenScope::Read);
    let mut body = open_stream(&m, &t).await.into_body().into_data_stream();
    let mut chunks = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(c) = body.next().await {
            chunks.push(c.unwrap());
        }
    })
    .await;
    assert!(ended.is_ok(), "stream outlived its lifetime");
    assert!(chunks.iter().skip(1).all(|c| &c[..] == b": keepalive\n\n"));
    assert!(chunks.len() >= 3, "{:?}", chunks);
    assert_eq!(m.h.ctx.mcp.open_streams(), 0);

    m.h.ctx
        .mcp
        .set_stream_timing(Duration::from_secs(300), Duration::from_secs(15));
    let t2 = m.token(TokenScope::Read);
    let mut body = open_stream(&m, &t2).await.into_body().into_data_stream();
    body.next().await.unwrap().unwrap();
    assert_eq!(m.h.ctx.mcp.open_streams(), 1);
    m.h.ctx.shutdown.cancel();
    let ended = tokio::time::timeout(Duration::from_secs(5), body.next()).await;
    assert!(matches!(ended, Ok(None)), "stream did not end on shutdown");
    drop(body);
    assert_eq!(m.h.ctx.mcp.open_streams(), 0);
    m.h.shutdown().await;
}

/// Captures every log line written while it is the default subscriber.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn line(r: &mut (impl AsyncBufReadExt + Unpin)) -> Value {
    let mut s = String::new();
    tokio::time::timeout(Duration::from_secs(10), r.read_line(&mut s))
        .await
        .expect("no reply")
        .unwrap();
    serde_json::from_str(&s).unwrap_or_else(|e| panic!("{}: {}", e, s))
}

async fn send(w: &mut (impl AsyncWriteExt + Unpin), v: Value) {
    w.write_all(format!("{}\n", v).as_bytes()).await.unwrap();
    w.flush().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_bridge_speaks_mcp_to_the_running_app() {
    let capture = Capture::default();
    let sink = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let m = M::new().await;
    m.h.analyze(true);
    let token = m.token(TokenScope::ReadWrite);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // The Host check (DNS rebinding) only answers on the configured port.
    m.h.ctx.config.write().http_port = addr.port();
    let app = openalgo_desktop_lib::server::app(m.h.ctx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<axum::extract::Request>::into_make_service_with_connect_info::<
                SocketAddr,
            >(app),
        )
        .await;
    });

    let (client, server_side) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_side);
    let bridge = Bridge::new(&format!("http://{}", addr), &token).unwrap();
    let session = tokio::spawn(stdio::serve(bridge, sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let mut cr = BufReader::new(cr);

    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "1"}}}),
    )
    .await;
    let init = line(&mut cr).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "openalgo", "{}", init);
    assert!(init["result"]["capabilities"]["tools"].is_object());
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )
    .await;
    let list = line(&mut cr).await;
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", list));
    assert_eq!(tools.len(), 49);
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/mcp/tools_list.json")).unwrap();
    for t in tools {
        let w = fixture["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["name"] == t["name"])
            .unwrap();
        assert_eq!(t["inputSchema"], w["inputSchema"]);
        assert_eq!(t["description"], w["description"]);
    }

    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "place_order",
        "arguments": {"symbol": "SBIN", "quantity": 1, "action": "BUY"}}}),
    )
    .await;
    let call = line(&mut cr).await;
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", call));
    let out: Value = serde_json::from_str(text).unwrap();
    assert_eq!(out["data"]["mode"], "analyze", "{}", out);
    assert!(!call["result"]["isError"].as_bool().unwrap_or(false));

    // Scope errors pass through as protocol errors.
    let ro = m.token(TokenScope::Read);
    drop(cw);
    let _ = tokio::time::timeout(Duration::from_secs(5), session).await;
    let (client, server_side) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_side);
    let session = tokio::spawn(stdio::serve(
        Bridge::new(&format!("http://{}", addr), &ro).unwrap(),
        sr,
        sw,
    ));
    let (cr, mut cw) = tokio::io::split(client);
    let mut cr = BufReader::new(cr);
    send(&mut cw, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
    line(&mut cr).await;
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "cancel_all_orders", "arguments": {}}}),
    )
    .await;
    let denied = line(&mut cr).await;
    assert_eq!(
        denied["error"]["message"], "insufficient_scope",
        "{}",
        denied
    );
    drop(cw);
    let _ = tokio::time::timeout(Duration::from_secs(5), session).await;

    server.abort();
    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(!logs.contains(&token), "token leaked into the logs");
    assert!(!logs.contains(&ro), "token leaked into the logs");
    m.h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_bridge_reports_a_closed_app_and_a_bad_token() {
    // Nothing listens on this port.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let bridge = Bridge::new(&format!("http://127.0.0.1:{}", port), "oamcp_x").unwrap();
    let (client, server_side) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_side);
    let session = tokio::spawn(stdio::serve(bridge, sr, sw));
    let (cr, mut cw) = tokio::io::split(client);
    let mut cr = BufReader::new(cr);
    send(&mut cw, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
    assert_eq!(
        line(&mut cr).await["result"]["serverInfo"]["name"],
        "openalgo"
    );
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    send(
        &mut cw,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "get_funds", "arguments": {}}}),
    )
    .await;
    let r = line(&mut cr).await;
    assert_eq!(r["result"]["isError"], true, "{}", r);
    assert_eq!(r["result"]["content"][0]["text"], stdio::NOT_RUNNING);
    drop(cw);
    let _ = tokio::time::timeout(Duration::from_secs(5), session).await;

    // A running app that refuses the token.
    let m = M::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // The Host check (DNS rebinding) only answers on the configured port.
    m.h.ctx.config.write().http_port = addr.port();
    let app = openalgo_desktop_lib::server::app(m.h.ctx.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<axum::extract::Request>::into_make_service_with_connect_info::<
                SocketAddr,
            >(app),
        )
        .await;
    });
    let bridge = Bridge::new(&format!("http://{}", addr), "oamcp_revoked").unwrap();
    assert_eq!(
        bridge
            .forward("tools/call", json!({"name": "get_funds"}))
            .await,
        Err(stdio::Failure::Message(stdio::BAD_TOKEN.into()))
    );
    server.abort();
    m.h.shutdown().await;
}

/// A server that reads one whole request, then answers with `answer` (or
/// closes the connection when it is empty). Returns its base address.
async fn after_send_server(answer: &'static str) -> String {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16384];
            let _ = s.read(&mut buf).await;
            if !answer.is_empty() {
                let _ = s.write_all(answer.as_bytes()).await;
            }
            drop(s);
        }
    });
    format!("http://{}", addr)
}

/// N-03: once the request was written to the app, a reset connection or a
/// server error means it may have run; only a connection that never opened
/// reads as "could not be sent".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_bridge_never_calls_a_sent_request_unsent() {
    for answer in [
        "",
        "HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 40\r\nconnection: close\r\n\r\n{\"jsonrpc\"",
    ] {
        let base = after_send_server(answer).await;
        let bridge = Bridge::new(&base, "oamcp_x").unwrap();
        let r = bridge
            .forward(
                "tools/call",
                json!({"name": "place_order", "arguments": {}}),
            )
            .await;
        assert_eq!(
            r,
            Err(stdio::Failure::Message(stdio::ANSWER_LOST.into())),
            "answer {:?}",
            answer
        );
    }
}
