//! The Dhan market and order-update feeds driven by the real feed manager
//! against a local fake socket: the subscribe frame the manager sends, the
//! little-endian packets it decodes, and the subscription replayed after a
//! reconnect.

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::dhan::streaming::{DhanFeed, DhanOrderFeed};
use openalgo_desktop_lib::websocket::{FeedConfig, WebSocketManager};
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

/// A code 4 quote packet for NSE_EQ 3045 (offsets per
/// `dhan_websocket._parse_quote_packet`).
fn quote_packet(ltp: f32) -> Vec<u8> {
    let mut p = vec![4u8];
    p.extend(50u16.to_le_bytes());
    p.push(1);
    p.extend(3045u32.to_le_bytes());
    p.extend(ltp.to_le_bytes()); // @0 ltp
    p.extend(5u16.to_le_bytes()); // @4 ltq
    p.extend(1_790_999_702u32.to_le_bytes()); // @6 ltt
    p.extend(813.12f32.to_le_bytes()); // @10 atp
    p.extend(4_823_170u32.to_le_bytes()); // @14 volume
    p.extend(689_415u32.to_le_bytes()); // @18 total sell
    p.extend(512_633u32.to_le_bytes()); // @22 total buy
    for v in [810.0f32, 812.35, 816.0, 808.5] {
        p.extend(v.to_le_bytes()); // @26 open, @30 close, @34 high, @38 low
    }
    assert_eq!(p.len(), 50);
    p
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
async fn market_feed_subscribes_decodes_and_resubscribes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
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
                let cb = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    path = req.uri().to_string();
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                if let Some(Ok(Message::Text(t))) = ws.next().await {
                    rec.lock()
                        .await
                        .push((n, serde_json::from_str(&t).unwrap(), path.clone()));
                }
                let _ = ws
                    .send(Message::Binary(quote_packet(if n == 1 {
                        814.1
                    } else {
                        815.0
                    })))
                    .await;
                // First connection drops after one packet; the second stays.
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
        .connect(Box::new(DhanFeed::with_url(&url, "tok", "1100012345")))
        .await
        .unwrap();

    let mut ltps = Vec::new();
    while ltps.len() < 2 {
        let ev = match tokio::time::timeout(Duration::from_secs(5), ticks.recv()).await {
            Ok(e) => e.unwrap(),
            Err(_) => panic!(
                "no tick: {:?} status {:?} stats {:?}",
                received.lock().await,
                manager.status(),
                manager.stats()
            ),
        };
        if let FeedEvent::Tick(t) = ev.as_ref() {
            assert_eq!(
                (t.symbol.as_str(), t.exchange.as_str(), t.mode),
                ("SBIN", "NSE", 2)
            );
            assert_eq!((t.close, t.total_buy_quantity), (812.35, 512_633));
            ltps.push(t.ltp);
        }
    }
    assert_eq!(ltps, vec![814.1, 815.0]);
    let rec = received.lock().await.clone();
    assert_eq!(rec.len(), 2);
    for (_, frame, path) in &rec {
        assert_eq!(frame["RequestCode"], 17);
        assert_eq!(frame["InstrumentList"][0]["SecurityId"], "3045");
        assert_eq!(frame["InstrumentList"][0]["ExchangeSegment"], "NSE_EQ");
        assert!(path.contains("version=2&token=tok&clientId=1100012345&authType=2"));
    }
    manager.disconnect().await.unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_feed_logs_in_and_publishes_updates() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let login: Arc<Mutex<Option<Value>>> = Arc::default();
    let lg = login.clone();
    let server = tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await {
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                *lg.lock().await = serde_json::from_str(&t).ok();
            }
            let update = serde_json::json!({"Type": "order_alert", "Data": {
                "orderNo": "77", "status": "Traded", "quantity": 10, "tradedQty": 10,
                "exchange": "NSE", "segment": "E", "securityId": "3045", "symbol": "SBIN",
                "txnType": "B", "orderType": "MKT", "product": "I", "avgTradedPrice": 814.1}});
            let _ = ws.send(Message::Text(update.to_string())).await;
            while ws.next().await.is_some() {}
        }
    });
    let manager = WebSocketManager::with_config(config());
    let mut events = manager.subscribe_ticks();
    manager
        .connect(Box::new(DhanOrderFeed::with_url(
            &url,
            "tok",
            "1100012345",
            SymbolResolver::new(),
        )))
        .await
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("update within 5 s")
        .unwrap();
    match ev.as_ref() {
        FeedEvent::OrderUpdate(u) => {
            assert_eq!(
                (u.orderid.as_str(), u.order_status.as_str()),
                ("77", "complete")
            );
            assert_eq!(
                (u.product.as_str(), u.pricetype.as_str()),
                ("MIS", "MARKET")
            );
            // Without a master the broker symbol is kept.
            assert_eq!(u.symbol, "SBIN");
        }
        other => panic!("unexpected {:?}", other),
    }
    let l = login.lock().await.clone().unwrap();
    assert_eq!(l["LoginReq"]["MsgCode"], 42);
    assert_eq!(l["LoginReq"]["ClientId"], "1100012345");
    assert_eq!(l["UserType"], "SELF");
    manager.disconnect().await.unwrap();
    server.abort();
}
