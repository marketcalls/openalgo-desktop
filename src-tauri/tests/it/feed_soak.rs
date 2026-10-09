//! Soak: 1000 clients connect, authenticate, subscribe, receive ticks and
//! leave (half with a close frame, half by dropping the socket), in waves.
//! Descriptor count and RSS are sampled after a warm-up wave and at the end
//! and must be flat; every registry entry and source subscription must be
//! gone.
//!
//! Ignored by default (it takes a while):
//! `cargo test --test it feed_soak -- --ignored --nocapture`

use crate::feed_support;

use feed_support::*;
use openalgo_desktop_lib::feed::{FakeSource, InstrumentKey, MarketUpdate, Mode};
use serde_json::json;
use std::time::Duration;

const TOTAL: usize = 1000;
const WAVE: usize = 100;

async fn wave(url: &str, h: &Harness, wave_no: usize) {
    let mut clients = Vec::with_capacity(WAVE);
    for i in 0..WAVE {
        let mut c = Client::connect(url).await;
        c.auth().await;
        let n = (wave_no * WAVE + i) % 40;
        let ack = c
            .request(json!({"action": "subscribe", "symbols": [
                {"symbol": format!("S{}", n), "exchange": "NSE"},
                {"symbol": format!("S{}", n + 1), "exchange": "NSE"},
                {"symbol": "NIFTY", "exchange": "NSE_INDEX"}], "mode": i % 3 + 1}))
            .await;
        assert_eq!(ack["status"], "success");
        clients.push(c);
    }
    // A depth-level update reaches LTP, Quote and Depth holders alike.
    let mut u = MarketUpdate::ltp(
        InstrumentKey::new("NIFTY", "NSE_INDEX"),
        22421.95,
        1_791_000_000_000,
    );
    u.mode = Mode::Depth;
    u.quote = Some(Default::default());
    h.source.publish(u);
    for c in clients.iter_mut() {
        let v = c.recv().await;
        assert_eq!(v["type"], "market_data");
    }
    for (i, mut c) in clients.into_iter().enumerate() {
        if i % 2 == 0 {
            let _ = c.ws.close(None).await;
            let _ = c.expect_close(Duration::from_secs(2)).await;
        }
        drop(c);
    }
    assert!(
        eventually(Duration::from_secs(10), || h.handle.connections() == 0).await,
        "connections still open after wave {}",
        wave_no
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn soak_1000_clients_fd_and_rss_flat() {
    crate::isolated!(soak_1000_clients_fd_and_rss_flat);
    // Every client is a program on this computer: its own pool.
    let h = start_with(FakeSource::permissive(), |c| {
        c.max_connections = WAVE + 8;
        c.local_connections = WAVE + 8;
    })
    .await;
    let url = h.url.clone();

    // Warm-up wave: lets the allocator, the runtime and the registry maps
    // reach their working size.
    wave(&url, &h, 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let fd_warm = fd_count();
    let rss_warm = rss_kib();

    for w in 1..(TOTAL / WAVE) {
        wave(&url, &h, w).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let fd_end = fd_count();
    let rss_end = rss_kib();
    eprintln!(
        "soak: {} clients; fds {} -> {}; RSS {} KiB -> {} KiB ({:+} KiB)",
        TOTAL,
        fd_warm,
        fd_end,
        rss_warm,
        rss_end,
        rss_end as i64 - rss_warm as i64
    );

    let stats = h.handle.stats();
    assert_eq!(stats.clients, 0, "{:?}", stats);
    assert_eq!(stats.instruments, 0);
    assert_eq!(stats.source_keys, 0);
    assert_eq!(h.source.active_count(), 0);
    assert!(h.source.violations().is_empty());
    assert!(
        fd_end <= fd_warm + 4,
        "descriptors grew: {} -> {}",
        fd_warm,
        fd_end
    );
    assert!(
        rss_end <= rss_warm + 16 * 1024,
        "RSS grew: {} KiB -> {} KiB",
        rss_warm,
        rss_end
    );
    h.handle.stop().await;
}
