//! Resource hygiene of the Groww feed (CLAUDE.md "Measure, do not just
//! read"): a fake Groww accepts the NATS handshake and then drops every
//! connection, so the manager reconnects through the loopback relay 150
//! times (each with a fresh socket token and key pair). Descriptors and RSS
//! must stay flat, and disconnecting must release the relay listener. The
//! test runs in its own process (`isolated!`) so the count is not disturbed.

use axum::routing::post;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::groww::GrowwBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

#[cfg(not(unix))]
fn open_fds() -> usize {
    0
}

fn rss_kb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_reconnects_do_not_leak() {
    crate::isolated!(relay_reconnects_do_not_leak);
    let app = Router::new().route(
        "/token",
        post(|| async { Json(json!({"token": "jwt", "subscriptionId": "s"})) }),
    );
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_url = format!("http://{}/token", l.local_addr().unwrap());
    let http = tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });

    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!("ws://{}", l.local_addr().unwrap());
    let accepted = Arc::new(AtomicU64::new(0));
    let acc = accepted.clone();
    let server = tokio::spawn(async move {
        while let Ok((tcp, _)) = l.accept().await {
            acc.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    let _ = ws
                        .send(Message::Text("INFO {\"nonce\":\"n\"}\r\n".into()))
                        .await;
                    // Read the CONNECT, then drop the connection.
                    let _ = tokio::time::timeout(Duration::from_millis(200), ws.next()).await;
                    let _ = ws.close(None).await;
                }
            });
        }
    });

    let broker = GrowwBroker::with_base_url(SymbolResolver::new(), "http://127.0.0.1:9")
        .with_feed_endpoints(&token_url, &ws_url);
    let auth = AuthToken::new("session");
    let manager = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(4),
        stall_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        stable_after: Duration::from_secs(60),
        ..FeedConfig::default()
    });
    manager
        .subscribe(vec![FeedSubscription {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            token: "3045".into(),
            brsymbol: "SBIN".into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Quote,
            depth: 5,
        }])
        .await
        .unwrap();
    let fds_idle = open_fds();
    manager
        .connect(broker.create_feed(&auth).unwrap())
        .await
        .unwrap();
    while accepted.load(Ordering::Relaxed) < 10 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let fds_before = open_fds();
    let rss_before = rss_kb();
    let mut peak = fds_before;
    while accepted.load(Ordering::Relaxed) < 160 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        peak = peak.max(open_fds());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let fds_after = open_fds();
    let rss_after = rss_kb();
    manager.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let fds_stopped = open_fds();
    server.abort();
    http.abort();

    eprintln!(
        "groww feed hygiene: fds idle={} before={} peak={} after={} stopped={}; rss {}KB -> {}KB",
        fds_idle, fds_before, peak, fds_after, fds_stopped, rss_before, rss_after
    );
    assert!(
        fds_after <= fds_before + 6,
        "descriptor leak: {} -> {}",
        fds_before,
        fds_after
    );
    assert!(
        peak <= fds_before + 12,
        "descriptor growth while reconnecting: {}",
        peak
    );
    // Disconnect drops the feed, which stops the relay listener.
    assert!(
        fds_stopped <= fds_idle + 6,
        "relay not released: idle {} -> stopped {}",
        fds_idle,
        fds_stopped
    );
    if rss_before > 0 {
        assert!(
            rss_after < rss_before + 16 * 1024,
            "RSS grew {} -> {} KB",
            rss_before,
            rss_after
        );
    }
}
