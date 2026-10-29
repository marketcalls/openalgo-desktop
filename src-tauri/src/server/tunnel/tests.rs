//! What a caller through the built-in tunnel reaches, over real HTTP/1.1
//! connections served the way the tunnel serves them.

use super::*;
use crate::brokers::BrokerRegistry;
use crate::state::testing::{build, TestCtx};
use crate::state::ServerStatus;
use serde_json::Value;
use std::time::Duration;
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
        // Flow's webhooks (T-106, D-12): the token, with or without a symbol.
        "/flow/webhook/abc",
        "/flow/webhook/abc/SBIN",
        "/flow/webhook/abc/RELIANCE/",
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
        "/flow/webhook",
        "/flow/webhook//",
        "/flow/webhook/a/b/c",
        "/flow/webhook/a/../../dashboard",
        "/flow/api/workflows",
        "/flow/api/monitor/status",
        "/flow",
        "/flow/editor/1",
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

// ------------------------------------------------- limits (security review)

/// Short limits, so the tests see each one act.
const QUICK: Limits = Limits {
    header: Duration::from_millis(300),
    body: Duration::from_millis(300),
    answer: Duration::from_millis(300),
    lifetime: Duration::from_secs(5),
};

/// A connection served with `app` and `limits`; the caller's end, and the
/// connection's task (its place among the tunnel's connections).
fn open(
    ctx: &Arc<AppState>,
    app: App,
    limits: Limits,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    open_with(ctx, app, limits, ctx.tunnel.work())
}

fn open_with(
    ctx: &Arc<AppState>,
    app: App,
    limits: Limits,
    work: Work,
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
    let serve = serving(ctx, app, limits, work);
    let (caller, ours) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(serve(Box::new(ours), CancellationToken::new()));
    (caller, task)
}

/// The app itself.
fn router(ctx: &Arc<AppState>) -> App {
    App::new(NormalizePathLayer::trim_trailing_slash().layer(crate::server::router(ctx.clone()).0))
}

/// Everything the caller is sent until the connection closes, and how long
/// that took.
async fn until_closed(caller: &mut tokio::io::DuplexStream) -> (String, Duration) {
    let started = std::time::Instant::now();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), caller.read_to_end(&mut out))
        .await
        .expect("the connection was closed")
        .unwrap();
    (
        String::from_utf8_lossy(&out).into_owned(),
        started.elapsed(),
    )
}

#[tokio::test]
async fn a_slow_body_is_cut_off_at_its_limit_and_frees_its_place() {
    let t = ctx();
    let (mut caller, place) = open(&t.ctx, router(&t.ctx), QUICK);
    caller
        .write_all(
            b"POST /api/v1/placeorder HTTP/1.1\r\nHost: desk.ngrok-free.app\r\n\
              Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"apikey\"",
        )
        .await
        .unwrap();
    let (answer, took) = until_closed(&mut caller).await;
    assert!(answer.starts_with("HTTP/1.1 408"), "{}", answer);
    assert!(answer.contains("took too long to arrive"), "{}", answer);
    assert!(took < Duration::from_secs(3), "{:?}", took);
    tokio::time::timeout(Duration::from_secs(2), place)
        .await
        .expect("the place is free")
        .unwrap();
}

#[tokio::test]
async fn an_idle_connection_is_closed_and_none_is_kept_alive() {
    let t = ctx();
    // Says nothing at all: closed at the header limit.
    let (mut idle, place) = open(&t.ctx, router(&t.ctx), QUICK);
    let (_, took) = until_closed(&mut idle).await;
    assert!(took < Duration::from_secs(3), "{:?}", took);
    place.await.unwrap();
    // A request that asks to keep the connection: answered, then closed.
    let (mut caller, place) = open(&t.ctx, router(&t.ctx), QUICK);
    caller
        .write_all(b"GET /dashboard HTTP/1.1\r\nHost: desk.ngrok-free.app\r\nConnection: keep-alive\r\n\r\n")
        .await
        .unwrap();
    let (answer, _) = until_closed(&mut caller).await;
    assert!(answer.starts_with("HTTP/1.1 404"), "{}", answer);
    assert!(
        answer.to_ascii_lowercase().contains("connection: close"),
        "{}",
        answer
    );
    place.await.unwrap();
}

#[tokio::test]
async fn a_connection_is_closed_at_its_lifetime_whatever_it_is_doing() {
    let t = ctx();
    let limits = Limits {
        header: Duration::from_secs(30),
        lifetime: Duration::from_millis(300),
        ..Limits::STANDARD
    };
    let (mut caller, place) = open(&t.ctx, router(&t.ctx), limits);
    caller
        .write_all(b"POST /api/v1/placeorder HTTP/1.1\r\nHost: desk.ngrok-free.app\r\nX-Slo")
        .await
        .unwrap();
    let (_, took) = until_closed(&mut caller).await;
    assert!(took < Duration::from_secs(3), "{:?}", took);
    place.await.unwrap();
}

/// An answer slower than its limit: the caller is told and its place
/// freed, and the work is not cut short (an order is never dropped
/// half-way).
#[tokio::test]
async fn a_slow_answer_is_told_to_the_caller_and_the_work_runs_to_its_end() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let t = ctx();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let slow = App::new(tower::service_fn(move |_req: Request<Body>| {
        let finished = finished.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            finished.store(true, Ordering::SeqCst);
            Ok::<_, Infallible>(StatusCode::OK.into_response())
        }
    }));
    let (mut caller, place) = open(&t.ctx, slow, QUICK);
    caller
        .write_all(b"GET /api/v1/ping HTTP/1.1\r\nHost: desk.ngrok-free.app\r\n\r\n")
        .await
        .unwrap();
    let (answer, took) = until_closed(&mut caller).await;
    assert!(answer.starts_with("HTTP/1.1 504"), "{}", answer);
    assert!(
        answer.contains("still working on this request"),
        "{}",
        answer
    );
    assert!(took < Duration::from_secs(2), "{:?}", took);
    place.await.unwrap();
    assert!(
        !done.load(Ordering::SeqCst),
        "answered before the work ended"
    );
    for _ in 0..800 {
        if done.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the work was cut short");
}

/// One request to `/api/v1/ping` through a connection served with `app`;
/// the answer and how long it took.
async fn ask(ctx: &Arc<AppState>, app: &App, work: &Work) -> (String, Duration) {
    let (mut caller, place) = open_with(ctx, app.clone(), QUICK, work.clone());
    caller
        .write_all(b"GET /api/v1/ping HTTP/1.1\r\nHost: desk.ngrok-free.app\r\n\r\n")
        .await
        .unwrap();
    let answer = until_closed(&mut caller).await;
    place.await.unwrap();
    answer
}

/// Detached work stays bounded (second security review of the tunnel):
/// with every permit held by work that outlives its wait, the next request
/// is answered 503 at once and nothing of it runs; a permit returns when
/// its work ends.
#[tokio::test]
async fn work_that_outlives_its_wait_holds_a_place_until_it_ends() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let t = ctx();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let (g, s) = (gate.clone(), started.clone());
    let app = App::new(tower::service_fn(move |_req: Request<Body>| {
        let (gate, started) = (g.clone(), s.clone());
        async move {
            started.fetch_add(1, Ordering::SeqCst);
            let _pass = gate.acquire().await;
            Ok::<_, Infallible>(StatusCode::OK.into_response())
        }
    }));
    let work: Work = Arc::new(tokio::sync::Semaphore::new(2));
    for _ in 0..2 {
        let (answer, _) = ask(&t.ctx, &app, &work).await;
        assert!(answer.starts_with("HTTP/1.1 504"), "{}", answer);
    }
    assert_eq!(work.available_permits(), 0, "the work still runs");

    let (answer, took) = ask(&t.ctx, &app, &work).await;
    assert!(answer.starts_with("HTTP/1.1 503"), "{}", answer);
    assert!(
        answer.contains("still working on earlier requests"),
        "{}",
        answer
    );
    assert!(took < Duration::from_secs(1), "refused at once: {:?}", took);
    assert_eq!(started.load(Ordering::SeqCst), 2, "nothing of it ran");

    gate.add_permits(10);
    for _ in 0..500 {
        if work.available_permits() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(work.available_permits(), 2, "the permits came back");
    let (answer, _) = ask(&t.ctx, &app, &work).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{}", answer);
}
