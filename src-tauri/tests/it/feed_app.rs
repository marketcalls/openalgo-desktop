//! The feed as the app runs it: authentication through the real
//! `ApiKeyService` against a real `AppState`, and `FeedService` start,
//! restart on a port change, port-in-use reporting and stop. Ports are
//! ephemeral; 5000/8765 are never used.

use crate::feed_support;

use feed_support::Client;
use openalgo_desktop_lib::brokers::BrokerRegistry;
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::db::sqlite::user;
use openalgo_desktop_lib::feed::FeedService;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::services::apikey_service::ApiKeyService;
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions, ServerStatus};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn open_ctx(dir: &tempfile::TempDir) -> Arc<AppState> {
    AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(BrokerRegistry::new()),
        },
    )
    .expect("open app state")
}

async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    l.local_addr().unwrap().port()
}

fn set_ws_port(ctx: &AppState, port: u16) {
    let mut c = ctx.config.write();
    c.bind_host = "127.0.0.1".into();
    c.ws_port = port;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_auth_uses_the_stored_api_key_and_broker_session() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_ctx(&dir);
    {
        let conn = ctx.sqlite.conn().unwrap();
        user::insert(
            &conn,
            &ctx.security,
            "alice",
            "alice@example.com",
            "not-a-real-hash",
            "JBSWY3DPEHPK3PXP",
        )
        .unwrap();
    }
    let key = ApiKeyService::regenerate(&ctx, "alice").unwrap();
    set_ws_port(&ctx, free_port().await);

    let feed = FeedService::new(ctx.clone());
    assert!(matches!(feed.start().await, ServerStatus::Running { .. }));
    let url = format!("ws://{}", feed.local_addr().await.unwrap());
    let mut c = Client::connect(&url).await;

    let v = c
        .request(json!({"action": "authenticate", "api_key": "0".repeat(64)}))
        .await;
    assert_eq!(
        v,
        json!({"status": "error", "code": "AUTHENTICATION_ERROR", "message": "Invalid API key"})
    );

    // Valid key, no broker connected: web answers BROKER_ERROR and treats
    // the socket as authenticated.
    let v = c
        .request(json!({"action": "authenticate", "api_key": key.expose()}))
        .await;
    assert_eq!(
        v,
        json!({"status": "error", "code": "BROKER_ERROR", "message": "No broker configuration found for user"})
    );
    let v = c
        .request(
            json!({"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "request_id": 7}),
        )
        .await;
    assert_eq!(
        v,
        json!({"status": "error", "code": "BROKER_ERROR", "message": "Broker adapter not found", "request_id": 7})
    );

    ctx.set_broker_session(Some(BrokerSession {
        broker_id: "zerodha".into(),
        auth_token: Secret::new("t"),
        feed_token: None,
        user_id: "AB1234".into(),
        user_name: None,
        authenticated_at: chrono::Utc::now(),
    }));
    let v = c
        .request(json!({"action": "auth", "apikey": key.expose()}))
        .await;
    assert_eq!(
        v,
        json!({"type": "auth", "status": "success", "message": "Authentication successful",
               "broker": "zerodha", "user_id": "alice",
               "supported_features": {"ltp": true, "quote": true, "depth": true}})
    );
    // No master contract loaded: the bridge refuses the symbol like the web.
    let v = c
        .request(json!({"action": "subscribe", "symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "mode": 1}))
        .await;
    assert_eq!(v["status"], "partial");
    assert_eq!(
        v["subscriptions"][0]["message"],
        "Token not found for SBIN on NSE"
    );
    let v = c.request(json!({"action": "get_broker_info"})).await;
    assert_eq!(
        v,
        json!({"type": "broker_info", "status": "success", "broker": "zerodha",
               "adapter_status": "connected", "user_id": "alice"})
    );
    drop(c);
    feed.stop().await;
    ctx.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_moves_on_port_change_and_reports_a_taken_port() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_ctx(&dir);
    let p1 = free_port().await;
    set_ws_port(&ctx, p1);
    let feed = FeedService::new(ctx.clone());
    assert_eq!(
        feed.start().await,
        ServerStatus::Running {
            host: "127.0.0.1".into(),
            port: p1
        }
    );
    let mut c = Client::connect(&format!("ws://127.0.0.1:{}", p1)).await;
    assert_eq!(c.request(json!({"action": "ping"})).await["type"], "pong");

    // Port change: the old listener closes its clients and frees the port.
    let p2 = free_port().await;
    set_ws_port(&ctx, p2);
    assert_eq!(
        feed.apply_config().await,
        ServerStatus::Running {
            host: "127.0.0.1".into(),
            port: p2
        }
    );
    assert_eq!(
        c.expect_close(Duration::from_secs(5)).await.map(|g| g.0),
        Some(1001)
    );
    assert!(tokio::net::TcpListener::bind(("127.0.0.1", p1))
        .await
        .is_ok());
    let mut c2 = Client::connect(&format!("ws://127.0.0.1:{}", p2)).await;
    assert_eq!(c2.request(json!({"action": "ping"})).await["type"], "pong");
    drop(c2);

    // A taken port is reported with a message for the trader.
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p3 = blocker.local_addr().unwrap().port();
    set_ws_port(&ctx, p3);
    match feed.apply_config().await {
        ServerStatus::PortInUse { port, message } => {
            assert_eq!(port, p3);
            assert!(message.contains(&p3.to_string()), "{}", message);
            assert!(message.contains("already used by another program"));
        }
        other => panic!("expected PortInUse, got {:?}", other),
    }
    assert!(matches!(feed.status(), ServerStatus::PortInUse { .. }));

    // Once the other program lets go, the watcher brings the feed up.
    drop(blocker);
    let mut up = false;
    for _ in 0..50 {
        if matches!(feed.status(), ServerStatus::Running { port, .. } if port == p3) {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "feed did not recover: {:?}", feed.status());

    feed.stop().await;
    assert!(tokio::net::TcpListener::bind(("127.0.0.1", p3))
        .await
        .is_ok());
    ctx.shutdown().await;
}
