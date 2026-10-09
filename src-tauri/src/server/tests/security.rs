//! Regression tests for the 2026-10-09 security review
//! (`docs/audit/2026-10-09/01-security.md`). Each test names its finding.

use super::*;

fn count(h: &H, sql: &str) -> i64 {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

// ------------------------------------------------- S-01 account reset

/// S-01: the account cannot be reset over HTTP any more, whatever the
/// caller sends (a session, its CSRF token and the confirmation).
#[tokio::test]
async fn s01_account_reset_is_not_reachable_over_http() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let (cookie, csrf) = h.session(false);
    let (s, _) = h
        .json(with_session(
            post_json("/auth/reset-account", json!({"confirm": "RESET"})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!AuthService::needs_setup(h.ctx()).unwrap(), "account kept");
    assert!(h.ctx().is_broker_connected(), "broker session kept");
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "API key kept");
}

/// S-07: a reset leaves nothing usable behind: MCP tokens, strategy and
/// Chartink webhook secrets, Telegram and WhatsApp links.
#[tokio::test]
async fn s07_account_reset_revokes_every_outside_credential() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let now = h.ctx().now();
    let (mcp_token, strategy_hash) = {
        let c = h.ctx().sqlite.conn().unwrap();
        let (_, mcp_token) = crate::mcp::store::create_token(
            &c,
            "claude",
            crate::mcp::store::TokenScope::ReadWrite,
            now,
        )
        .unwrap();
        let token = crate::strategy::store::generate_webhook_token();
        let strategy_hash = crate::strategy::store::hash_webhook_token(&token);
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [&strategy_hash],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id)
             VALUES ('scan', '11111111-1111-4111-8111-111111111111')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO telegram_users (telegram_id, openalgo_username) VALUES (42, 'trader')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username)
             VALUES ('919@s.whatsapp.net', '919', 'trader')",
            [],
        )
        .unwrap();
        (mcp_token, strategy_hash)
    };

    AuthService::reset_account_everywhere(h.ctx())
        .await
        .unwrap();

    let c = h.ctx().sqlite.conn().unwrap();
    assert!(crate::mcp::store::find_token(&c, &mcp_token)
        .unwrap()
        .is_none());
    let hash: String = c
        .query_row("SELECT webhook_token_hash FROM sm_strategy", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(hash, strategy_hash, "strategy webhook rotated");
    let id: String = c
        .query_row("SELECT webhook_id FROM chartink_strategies", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(id, "11111111-1111-4111-8111-111111111111");
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    drop(c);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM telegram_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM whatsapp_users"), 0);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM users"), 0);
    assert!(!h.ctx().is_broker_connected());
    assert!(AuthService::needs_setup(h.ctx()).unwrap());
    // Strategy and Chartink definitions are kept.
    assert_eq!(count(&h, "SELECT COUNT(*) FROM sm_strategy"), 1);
    assert_eq!(count(&h, "SELECT COUNT(*) FROM chartink_strategies"), 1);
}

// --------------------------------------------------------- shared helpers

const LAN: &str = "192.168.1.50";

/// The port the test server's configuration names (development ports in
/// debug builds).
fn app_port() -> u16 {
    if crate::config::dev_ports_enabled() {
        crate::config::DEV_HTTP_PORT
    } else {
        crate::config::DEFAULT_HTTP_PORT
    }
}

/// The request as if it came over a socket from `ip`, naming the app.
fn from_peer(mut r: Request<Body>, ip: &str) -> Request<Body> {
    r.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
            ip.parse().unwrap(),
            50000,
        )));
    r.headers_mut().insert(
        header::HOST,
        format!("127.0.0.1:{}", app_port()).parse().unwrap(),
    );
    r
}

// ------------------------------------------- S-06 secrets in the traffic log

/// S-06: a strategy webhook and a Chartink alert sent through the app leave
/// no secret in the traffic log or the unknown-page tracker.
#[tokio::test]
async fn s06_webhook_secrets_never_reach_the_traffic_log() {
    let h = H::new();
    h.setup();
    let token = crate::strategy::store::generate_webhook_token();
    let chartink = "33333333-3333-4333-8333-333333333333";
    let _ = h
        .send(from_peer(
            post_json(&format!("/strategy/webhook/{}", token), json!({})),
            LAN,
        ))
        .await;
    let _ = h
        .send(from_peer(
            post_json(&format!("/chartink/webhook/{}", chartink), json!({})),
            LAN,
        ))
        .await;
    h.ctx().monitor.drain_now(h.ctx());
    let c = h.ctx().logs.conn().unwrap();
    let paths: Vec<String> = c
        .prepare("SELECT path FROM traffic_logs")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        paths.iter().any(|p| p == "/strategy/webhook/<redacted>"),
        "the call was logged: {:?}",
        paths
    );
    let tried: Vec<String> = c
        .prepare("SELECT paths_attempted FROM error_404_tracker")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for text in paths.iter().chain(tried.iter()) {
        assert!(!text.contains(&token), "{}", text);
        assert!(!text.contains(chartink), "{}", text);
    }
}

// ------------------------------------------- S-09 live updates after logout

/// S-09: a signed-in page's Socket.IO connection is closed by the server
/// when the session ends, not left receiving order and position pushes.
#[tokio::test]
async fn s09_live_update_connection_is_closed_on_sign_out() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    *h.ctx().server_status.write() = crate::state::ServerStatus::Running {
        host: "127.0.0.1".into(),
        port: addr.port(),
    };
    let svc = crate::server::app(h.ctx().clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            axum::ServiceExt::<Request<Body>>::into_make_service_with_connect_info::<
                std::net::SocketAddr,
            >(svc),
        )
        .await;
    });

    let mut req = format!(
        "ws://127.0.0.1:{}/socket.io/?EIO=4&transport=websocket",
        addr.port()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    // Engine.IO open, then join the default namespace.
    let open = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(open.to_text().unwrap().starts_with('0'), "{:?}", open);
    ws.send(Message::Text("40".into())).await.unwrap();
    // Wait for the namespace join, answering Engine.IO pings ("2") that
    // may arrive first on a slow runner.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("no namespace join before the deadline")
            .unwrap()
            .unwrap();
        let text = m.to_text().unwrap_or_default().to_string();
        if text == "2" {
            ws.send(Message::Text("3".into())).await.unwrap();
            continue;
        }
        assert!(text.starts_with("40"), "{:?}", m);
        break;
    }

    // Sign out everywhere, as /auth/logout does.
    h.ctx().sessions.clear();
    h.ctx().bus.publish(crate::events::Event::ForceLogout {
        message: "Signed out".into(),
    });

    let mut closed = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) if t.as_str() == "41" => {
                closed = true;
                break;
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => break,
        }
    }
    assert!(closed, "the server kept the signed-out connection open");
    server.abort();
}

// ------------------------------------------------------------ S-13, S-14

/// S-13: a request over a connection without a Host header is refused.
#[tokio::test]
async fn s13_a_connection_without_a_host_header_is_refused() {
    let h = H::new();
    let mut r = from_peer(get("/auth/check-setup"), "127.0.0.1");
    r.headers_mut().remove(header::HOST);
    let (s, _, _) = h.send(r).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, _) = h
        .send(from_peer(get("/auth/check-setup"), "127.0.0.1"))
        .await;
    assert_eq!(s, StatusCode::OK);
    // HTTP/2 names the host in `:authority` (the URI), not in `Host`: it is
    // checked the same way.
    for (authority, expected) in [
        (format!("127.0.0.1:{}", app_port()), StatusCode::OK),
        ("evil.example".to_string(), StatusCode::BAD_REQUEST),
    ] {
        let mut r = from_peer(
            get(&format!("http://{}/auth/check-setup", authority)),
            "127.0.0.1",
        );
        r.headers_mut().remove(header::HOST);
        let (s, _, _) = h.send(r).await;
        assert_eq!(s, expected, "{}", authority);
    }
}

/// S-14: no path under `/webhook/` skips the CSRF check (no route lives
/// there; a future one must not be exempt by accident).
#[tokio::test]
async fn s14_webhook_prefix_is_not_exempt_from_csrf() {
    let h = H::new();
    h.setup();
    let (s, v) = h.json(post_json("/webhook/anything", json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert_eq!(
        v["message"],
        "Your session has expired. Refresh the page and try again."
    );
}

// --------------------------------------- S-08 the live port, fail closed

fn set_bind_host(h: &H, host: &str) {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute("UPDATE settings SET bind_host = ?1 WHERE id = 1", [host])
        .unwrap();
    h.ctx().reload_config().unwrap();
}

/// S-08 follow-up: the port the app window trusts is the one the listener
/// is bound to, read from the bound socket; before the first bind, once
/// the listener stops and after a failed bind it is 0, and the window then
/// trusts no page on this computer. The listener only ever binds an
/// ephemeral port here (pinned to 0), never the app's own ports.
#[tokio::test]
async fn s08_the_live_port_is_the_bound_listener_and_fails_closed() {
    use crate::commands::main_window_may_load;
    let h = H::new();
    h.ctx().pin_listener_ports(0, 0);
    h.ctx().reload_config().unwrap();
    let page = |p: u16| url::Url::parse(&format!("http://127.0.0.1:{}/", p)).unwrap();
    let trusted = |p: u16| main_window_may_load(&page(p), h.ctx().live_port(), false);

    // Before the first bind.
    assert_eq!(h.ctx().live_port(), 0);

    // A successful bind: the port actually bound.
    let server = crate::server::start(h.ctx().clone()).await.unwrap();
    let bound = server.addr.port();
    assert_ne!(bound, 0);
    assert_eq!(h.ctx().live_port(), bound);
    assert!(trusted(bound));

    // The listener stops.
    server.stop().await;
    assert_eq!(h.ctx().live_port(), 0);
    assert!(!trusted(bound), "a stopped listener is still trusted");

    // A restart whose bind fails: an address this computer does not have
    // (TEST-NET), refused the same way on every system.
    set_bind_host(&h, "192.0.2.1");
    assert!(crate::server::start(h.ctx().clone()).await.is_err());
    assert_eq!(h.ctx().live_port(), 0);
    assert!(!trusted(bound), "a failed bind trusts the old port");
}
