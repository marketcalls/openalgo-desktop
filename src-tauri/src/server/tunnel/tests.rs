//! What a caller through the built-in tunnel reaches, over real HTTP/1.1
//! connections served the way the tunnel serves them.

use super::*;
use crate::brokers::BrokerRegistry;
use crate::state::testing::{build, TestCtx};
use crate::state::ServerStatus;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TUNNEL_HOST: &str = "desk.ngrok-free.app";

fn ctx() -> TestCtx {
    let t = build(BrokerRegistry::with(vec![]), chrono::Utc::now());
    let conn = t.ctx.sqlite.conn().unwrap();
    crate::config::save(
        &conn,
        &crate::config::ServerConfigUpdate {
            host_server: Some(format!("https://{}", TUNNEL_HOST)),
            ngrok_allow: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    t.ctx.reload_config().unwrap();
    t
}

struct Answer {
    status: u16,
    head: String,
    body: Value,
}

/// Send `raw` (one HTTP/1.1 request) through a tunnel connection.
async fn send(ctx: &Arc<AppState>, raw: String) -> Answer {
    let serve = connections(ctx);
    let (mut caller, ours) = tokio::io::duplex(256 * 1024);
    let conn = tokio::spawn(serve(Box::new(ours), CancellationToken::new()));
    caller.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    caller.read_to_end(&mut out).await.unwrap();
    conn.await.unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // The body may be chunked; the JSON is the part between braces.
    let json = body
        .find('{')
        .zip(body.rfind('}'))
        .and_then(|(a, b)| serde_json::from_str(&body[a..=b]).ok())
        .unwrap_or(Value::Null);
    Answer {
        status,
        head: head.to_string(),
        body: json,
    }
}

/// A request as ngrok forwards it: the tunnel's host and its forwarding
/// headers.
fn via_ngrok(method: &str, path: &str, body: &str) -> String {
    format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nX-Forwarded-For: 203.0.113.9\r\nX-Forwarded-Proto: https\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method,
        path,
        TUNNEL_HOST,
        body.len(),
        body
    )
}

#[test]
fn only_the_public_routes_are_reachable_by_their_exact_shape() {
    for p in [
        "/api/v1/placeorder",
        "/api/v1/placesmartorder/",
        "/api/v1",
        "/strategy/webhook/abc123",
        "/chartink/webhook/0b9c-uuid",
        "/mcp",
        "/mcp/healthz",
    ] {
        assert_eq!(reach(p), Reach::App, "{}", p);
    }
    for p in [
        "/zerodha/callback",
        "/fyers/callback/",
        "/compositedge/callback",
    ] {
        assert_eq!(reach(p), Reach::ThisComputer, "{}", p);
    }
    for p in [
        "/",
        "",
        "/dashboard",
        "/login",
        "/auth/login",
        "/auth/logout",
        "/setup",
        "/api/tunnel",
        "/api/broker/credentials",
        "/api/config/host",
        "/apikey",
        "/socket.io/",
        "/assets/index.js",
        "/strategy/webhook",
        "/strategy/webhook/a/b",
        "/strategy/webhook//",
        "/chartink/webhook/a/../../dashboard",
        "/api/v1/../../dashboard",
        "/api/v1/./funds",
        "/api//v1/funds",
        "/notabroker/callback",
        "/zerodha/initiate-oauth",
        "/zerodha/callback/extra",
        "/openscript/runner/host/1/spec",
        "/mcp/other",
        "/e2e/scripted-broker",
    ] {
        assert_eq!(reach(p), Reach::Refused, "{}", p);
    }
}

#[tokio::test]
async fn the_app_its_sign_in_and_session_routes_stay_on_this_computer() {
    let t = ctx();
    for (method, path) in [
        ("GET", "/"),
        ("GET", "/dashboard"),
        ("GET", "/login"),
        ("POST", "/auth/login"),
        ("GET", "/auth/csrf-token"),
        ("POST", "/setup"),
        ("GET", "/api/tunnel"),
        ("GET", "/socket.io/?EIO=4&transport=polling"),
    ] {
        let a = send(&t.ctx, via_ngrok(method, path, "")).await;
        assert_eq!(a.status, 404, "{} {}: {}", method, path, a.head);
        assert_eq!(a.body["message"], NOT_THROUGH_THE_TUNNEL, "{}", path);
        assert!(
            !a.head.to_ascii_lowercase().contains("set-cookie"),
            "{}",
            path
        );
    }
}

#[tokio::test]
async fn the_public_routes_are_served_by_the_app() {
    let t = ctx();
    // An unknown webhook address: the app's own answer.
    let a = send(
        &t.ctx,
        via_ngrok("POST", "/strategy/webhook/not-a-token", "{}"),
    )
    .await;
    assert_eq!(a.status, 404, "{}", a.head);
    assert_ne!(a.body["message"], NOT_THROUGH_THE_TUNNEL);
    // The API: the web's envelope (here nobody has signed in yet).
    let a = send(
        &t.ctx,
        via_ngrok("POST", "/api/v1/funds", r#"{"apikey":"nope"}"#),
    )
    .await;
    assert_ne!(a.status, 404, "{}", a.head);
    assert_eq!(a.body["status"], "error", "{}", a.body);
}

/// A request through the built-in tunnel is a tunnel caller even when it
/// looks like this computer (no forwarding header, `Host` naming the app
/// on loopback): `/mcp` with Remote MCP off is not there for it.
#[tokio::test]
async fn a_tunnel_caller_is_never_this_computer_whatever_its_headers_say() {
    let t = ctx();
    let port = t.ctx.server_config().http_port;
    let raw = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n\
         Content-Length: 2\r\nConnection: close\r\n\r\n{{}}",
        port
    );
    let a = send(&t.ctx, raw).await;
    assert_eq!(a.status, 404, "{}", a.head);
    assert_eq!(a.body["path"], "/mcp", "{}", a.body);
}

/// A broker's sign-in redirect goes on to the app on this computer, with
/// its query, where the browser session that started the sign-in is.
#[tokio::test]
async fn a_broker_redirect_is_sent_on_to_this_computer() {
    let t = ctx();
    let path = "/zerodha/callback?request_token=abc&action=login&status=success";
    let a = send(&t.ctx, via_ngrok("GET", path, "")).await;
    assert_eq!(a.status, 503, "not listening yet: {}", a.head);

    *t.ctx.server_status.write() = ServerStatus::Running {
        host: "127.0.0.1".into(),
        port: 5599,
    };
    let a = send(&t.ctx, via_ngrok("GET", path, "")).await;
    assert_eq!(a.status, 307, "{}", a.head);
    let head = a.head.to_ascii_lowercase();
    assert!(
        head.contains(&format!(
            "location: http://127.0.0.1:5599{}",
            path.to_ascii_lowercase()
        )),
        "{}",
        a.head
    );
    assert!(head.contains("referrer-policy: no-referrer"), "{}", a.head);
    // A form post keeps its method and body (307).
    let a = send(
        &t.ctx,
        via_ngrok("POST", "/compositedge/callback?state=s", "a=b"),
    )
    .await;
    assert_eq!(a.status, 307, "{}", a.head);
}
