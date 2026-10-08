//! The Kotak SFeed client driven by the real feed manager against a local
//! fake SFeed: the `native_batch` auth frame, subscriptions held until the
//! auth answer, the binary market-picture packets, and the handshake
//! replayed on reconnect.

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::kotak::streaming::KotakFeed;
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
use serde_json::{json, Value};
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
        brsymbol: "SBIN-EQ".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Quote,
        depth: 5,
    }
}

/// A touch-line market picture for nse_cm 3045 (layout per
/// `sfeed_protocol._MP_BODY`), prices in paise with divider 100.
fn touch_line(ltp: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(3045u32.to_le_bytes()); // token @0
    b.extend(634_710i64.to_le_bytes()); // total buy @4
    b.extend(689_415i64.to_le_bytes()); // total sell @12
    b.extend(4_823_170i64.to_le_bytes()); // volume @20
    b.extend(1_790_999_702i64.to_le_bytes()); // ltt @28
    b.extend(0i64.to_le_bytes()); // last update @36
    for v in [81_000u32, 81_235, 81_600, 80_850, ltp] {
        b.extend(v.to_le_bytes()); // open @44, close @48, high @52, low @56, ltp @60
    }
    b.extend(5i64.to_le_bytes()); // ltq @64
    b.extend(81_312u32.to_le_bytes()); // atp @72
    b.extend(0u32.to_le_bytes()); // indicative close @76
    b.extend(1u32.to_le_bytes()); // buy rows @80
    b.extend(1u32.to_le_bytes()); // sell rows @84
    b.extend(0i16.to_le_bytes()); // status @88
    b.extend(0i32.to_le_bytes()); // change % @90
    b.extend(0u32.to_le_bytes()); // oi @94
    b.extend(0f64.to_le_bytes()); // turnover @98
    b.extend(0i32.to_le_bytes()); // change @106
    b.extend([0u8; 20]); // circuits, yearly, lot @110..@130
    b.push(2); // precision @130
    b.extend(1u32.to_le_bytes()); // multiplier @131
    for (q, p, o) in [(100i64, 81_405i32, 1i32), (75, 81_410, 2)] {
        b.extend(q.to_le_bytes());
        b.extend(p.to_le_bytes());
        b.extend(o.to_le_bytes());
    }
    // Header: u16 length, u16 code, i8 exchange (1 nse_cm), u8 level 4, ...
    let mut pkt = ((9 + b.len()) as u16).to_le_bytes().to_vec();
    pkt.extend(0u16.to_le_bytes());
    pkt.extend([1u8, 4, 0, 0, 0]);
    pkt.extend(b);
    pkt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sfeed_authenticates_before_subscribing_and_replays_on_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let frames: Arc<Mutex<Vec<(usize, Value)>>> = Arc::default();
    let rec = frames.clone();
    let server = tokio::spawn(async move {
        let mut conn = 0usize;
        while let Ok((tcp, _)) = listener.accept().await {
            conn += 1;
            let rec = rec.clone();
            let n = conn;
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                // 1. auth frame, 2. auth answer, 3. subscribe, 4. ticks.
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    rec.lock()
                        .await
                        .push((n, serde_json::from_str(&t).unwrap()));
                }
                let ack = json!({"message_code": 1119, "status": "success",
                    "exchanges": {"nse_cm": {"value": 1, "divider": 100}}});
                let _ = ws.send(Message::Text(ack.to_string())).await;
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    rec.lock()
                        .await
                        .push((n, serde_json::from_str(&t).unwrap()));
                }
                let _ = ws
                    .send(Message::Text(
                        json!({"message_code": 1109, "trading_symbols": {"nse_cm|3045": "SBIN-EQ"}})
                            .to_string(),
                    ))
                    .await;
                let mut batch = touch_line(if n == 1 { 81_410 } else { 81_500 });
                batch.extend(touch_line(if n == 1 { 81_420 } else { 81_510 }));
                let _ = ws.send(Message::Binary(batch)).await;
                if n == 1 {
                    let _ = ws.close(None).await;
                } else {
                    while ws.next().await.is_some() {}
                }
            });
        }
    });

    let manager = WebSocketManager::with_config(FeedConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stall_timeout: Duration::from_secs(5),
        connect_timeout: Duration::from_secs(2),
        stable_after: Duration::from_secs(60),
        ..FeedConfig::default()
    });
    let mut ticks = manager.subscribe_ticks();
    manager.subscribe(vec![sub()]).await.unwrap();
    manager
        .connect(Box::new(KotakFeed::new(
            &url,
            "trade-sid",
            "AB1234".into(),
            SymbolResolver::new(),
        )))
        .await
        .unwrap();
    let mut ltps = Vec::new();
    while ltps.len() < 4 {
        let ev = tokio::time::timeout(Duration::from_secs(5), ticks.recv())
            .await
            .expect("tick within 5 s")
            .unwrap();
        if let FeedEvent::Tick(t) = ev.as_ref() {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.mode),
                ("SBIN", "NSE", 2)
            );
            assert_eq!((t.open, t.close, t.volume), (810.0, 812.35, 4_823_170));
            ltps.push(t.ltp);
        }
    }
    assert_eq!(ltps, vec![814.1, 814.2, 815.0, 815.1]);
    let rec = frames.lock().await.clone();
    // Each connection: the auth frame first, then the subscription.
    assert_eq!(rec.len(), 4);
    for pair in rec.chunks(2) {
        assert_eq!(pair[0].1["user"], "AB1234");
        assert_eq!(pair[0].1["auth"], "trade-sid");
        assert_eq!(pair[0].1["format"], "native_batch");
        assert_eq!(
            pair[1].1,
            json!({"event": "subscribeScrips", "inputtoken": "nse_cm|3045", "ack_symbol": true})
        );
    }
    manager.disconnect().await.unwrap();
    server.abort();
}
