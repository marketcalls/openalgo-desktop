//! Resource hygiene of the broker feed manager (CLAUDE.md "Measure, do not
//! just read"): drive 150 reconnects against a local fake WebSocket server
//! that drops every connection, and 100 connect/disconnect cycles, then
//! check that descriptors and RSS stay flat. The test runs in its own
//! process (`isolated!`) so parallel tests do not disturb the count.

use futures_util::StreamExt;
use openalgo_desktop_lib::brokers::common::streaming::{FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::mock::MockFeed;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

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
async fn reconnect_loop_does_not_leak_descriptors_or_memory() {
    crate::isolated!(reconnect_loop_does_not_leak_descriptors_or_memory);
    // Fake broker feed: accept the handshake, read one frame, drop.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicU64::new(0));
    let acc = accepted.clone();
    let server = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            acc.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                    let _ = tokio::time::timeout(Duration::from_millis(50), ws.next()).await;
                    let _ = ws.close(None).await;
                }
            });
        }
    });

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
            token: "1".into(),
            brsymbol: "SBIN-EQ".into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Quote,
            depth: 5,
        }])
        .await
        .unwrap();

    // Warm up (first connections allocate runtime and TLS-free socket state).
    manager
        .connect(Box::new(MockFeed::new(url.clone())))
        .await
        .unwrap();
    while accepted.load(Ordering::Relaxed) < 10 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let fds_before = open_fds();
    let rss_before = rss_kb();
    let connects_before = manager.stats().connects.load(Ordering::Relaxed);

    let mut peak_fds = fds_before;
    while accepted.load(Ordering::Relaxed) < 160 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        peak_fds = peak_fds.max(open_fds());
    }
    let reconnects = manager.stats().connects.load(Ordering::Relaxed) - connects_before;
    // Let the last dropped sockets close.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let fds_after = open_fds();
    let rss_after = rss_kb();

    // 100 full connect/disconnect cycles: every supervisor task and socket
    // is released on disconnect.
    for _ in 0..100 {
        manager
            .connect(Box::new(MockFeed::new(url.clone())))
            .await
            .unwrap();
        manager.disconnect().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let fds_cycles = open_fds();
    assert!(!manager.is_running());
    server.abort();

    eprintln!(
        "feed hygiene: {} reconnects; fds before={} peak={} after={} after-100-cycles={}; rss before={}KB after={}KB",
        reconnects, fds_before, peak_fds, fds_after, fds_cycles, rss_before, rss_after
    );
    assert!(reconnects >= 140, "only {} reconnects", reconnects);
    // Flat: at most a handful of descriptors of jitter (sockets mid-close).
    assert!(
        fds_after <= fds_before + 4,
        "descriptor leak: {} -> {}",
        fds_before,
        fds_after
    );
    assert!(
        fds_cycles <= fds_before + 4,
        "descriptor leak over cycles: {} -> {}",
        fds_before,
        fds_cycles
    );
    assert!(
        peak_fds <= fds_before + 8,
        "descriptor growth while reconnecting: {}",
        peak_fds
    );
    // A leak per reconnect would show as tens of MB; allow allocator noise.
    if rss_before > 0 {
        assert!(
            rss_after < rss_before + 16 * 1024,
            "RSS grew {} -> {} KB",
            rss_before,
            rss_after
        );
    }
}
