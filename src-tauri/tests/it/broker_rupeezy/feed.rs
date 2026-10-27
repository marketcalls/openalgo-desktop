//! The Vortex price feed driven by the shared feed manager against a local
//! fake socket: the token on the handshake, the subscribe frame, the
//! little-endian packets decoded under the OpenAlgo symbol, and the
//! subscription replayed after a reconnect (web `RupeezyWebSocket`
//! resubscribe-on-reconnect).

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::rupeezy::streaming::RupeezyFeed;
use openalgo_desktop_lib::websocket::{handshake, FeedConfig, WebSocketManager};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

fn sub() -> FeedSubscription {
    FeedSubscription {
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        token: "3045".into(),
        brsymbol: "SBIN".into(),
        brexchange: "NSE_EQ".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

/// One frame carrying a 62-byte `ohlcv` packet for NSE_EQ 3045.
fn ohlcv_frame(ltp: f64) -> Vec<u8> {
    let mut p = b"NSE_EQ".to_vec();
    p.resize(10, 0);
    p.extend(3045i32.to_le_bytes());
    p.extend(ltp.to_le_bytes());
    p.extend(1_791_178_200i32.to_le_bytes());
    for v in [950.0f64, 957.35, 948.1, 958.75] {
        p.extend(v.to_le_bytes());
    }
    p.extend(4_823_170i32.to_le_bytes());
    assert_eq!(p.len(), 62);
    let mut f = 1u16.to_le_bytes().to_vec();
    f.extend(62u16.to_le_bytes());
    f.extend(p);
    f
}

fn config() -> FeedConfig {
    FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stall_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        stable_after: Duration::from_secs(60),
        ..FeedConfig::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_feed_subscribes_decodes_and_resubscribes_after_a_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let received: Arc<Mutex<Vec<(usize, Value, String)>>> = Arc::default();
    let rec = received.clone();
    let server = tokio::spawn(async move {
        let mut conn = 0usize;
        while let Ok((tcp, _)) = listener.accept().await {
            conn += 1;
            let rec = rec.clone();
            let n = conn;
            tokio::spawn(async move {
                let mut path = String::new();
                let cb = handshake::check(|req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    path = req.uri().to_string();
                    Ok(resp)
                });
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                // Answer the subscribe frame with one packet.
                while let Some(Ok(m)) = ws.next().await {
                    if let Message::Text(t) = m {
                        rec.lock()
                            .await
                            .push((n, serde_json::from_str(&t).unwrap(), path.clone()));
                        break;
                    }
                }
                let ltp = if n == 1 { 954.0 } else { 955.5 };
                let _ = ws.send(Message::Binary(ohlcv_frame(ltp))).await;
                // The first connection drops after one packet.
                if n == 1 {
                    let _ = ws.close(None).await;
                } else {
                    while ws.next().await.is_some() {}
                }
            });
        }
    });

    let manager = WebSocketManager::with_config(config());
    let mut ticks = manager.subscribe_ticks();
    manager.subscribe(vec![sub()]).await.unwrap();
    manager
        .connect(Box::new(RupeezyFeed::new(&url, "tok/en")))
        .await
        .unwrap();

    let mut ltps = Vec::new();
    while ltps.len() < 2 {
        let ev = tokio::time::timeout(Duration::from_secs(5), ticks.recv())
            .await
            .unwrap_or_else(|_| panic!("no tick; status {:?}", manager.status()))
            .unwrap();
        if let FeedEvent::Tick(t) = ev.as_ref() {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.mode),
                ("SBIN", "NSE", 2)
            );
            assert_eq!((t.open, t.close, t.volume), (950.0, 958.75, 4_823_170));
            assert_eq!(t.last_trade_time_ms, 1_791_178_200_000);
            ltps.push(t.ltp);
        }
    }
    assert_eq!(ltps, vec![954.0, 955.5]);
    let rec = received.lock().await.clone();
    assert_eq!(rec.len(), 2, "{:?}", rec);
    for (_, frame, path) in &rec {
        assert_eq!(
            frame,
            &serde_json::json!({"ticker": "NSE:SBIN", "mode": "ohlcv", "message_type": "subscribe"})
        );
        assert_eq!(path, "/ws?auth_token=tok%2Fen");
    }
    manager.disconnect().await.unwrap();
    server.abort();
}
