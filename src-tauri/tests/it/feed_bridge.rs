//! End to end through the production source: a feed client subscribes, the
//! bridge drives the real `WebSocketManager` with `MockFeed` against a fake
//! broker socket, broker ticks come back as `market_data`, and leaving
//! (including an abrupt disconnect) unsubscribes at the broker.

use crate::feed_support;

use feed_support::{brokers, eventually, Client, API_KEY, BROKER, USER_ID};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::MockFeed;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::{start, FeedConfig, FeedDeps};
use openalgo_desktop_lib::websocket::WebSocketManager;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

fn row(symbol: &str, exchange: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

/// Fake broker: records every frame the manager sends; frames pushed on the
/// returned channel are sent to the connected manager.
async fn fake_broker() -> (
    String,
    Arc<Mutex<Vec<String>>>,
    mpsc::Sender<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let (tx, mut rx) = mpsc::channel::<String>(64);
    let seen2 = seen.clone();
    let task = tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
            return;
        };
        let (mut w, mut r) = ws.split();
        loop {
            tokio::select! {
                m = r.next() => match m {
                    Some(Ok(Message::Text(t))) => seen2.lock().push(t),
                    Some(Ok(_)) => {}
                    _ => return,
                },
                out = rx.recv() => match out {
                    Some(t) => { let _ = w.send(Message::Text(t)).await; }
                    None => return,
                },
            }
        }
    });
    (url, seen, tx, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_client_to_broker_and_back_through_the_manager() {
    let (broker_url, seen, push, broker_task) = fake_broker().await;
    let manager = Arc::new(WebSocketManager::new());
    manager
        .connect(Box::new(MockFeed::new(broker_url)))
        .await
        .unwrap();
    let symbols = SymbolResolver::new();
    symbols.load(vec![
        row("RELIANCE", "NSE", "738561"),
        row("SBIN", "NSE", "779521"),
    ]);
    let bridge = BrokerBridge::new(manager.clone(), symbols);
    bridge.start();
    let h = start(
        FeedConfig {
            port: 0,
            ..FeedConfig::default()
        },
        FeedDeps {
            source: bridge.clone(),
            auth: Arc::new(StaticAuth {
                api_key: API_KEY.into(),
                user_id: USER_ID.into(),
                broker: Some(BROKER.into()),
            }),
            orders: None,
            supported_brokers: brokers(),
        },
    )
    .await
    .unwrap();
    let url = format!("ws://{}", h.local_addr());

    let mut a = Client::connect(&url).await;
    a.auth().await;
    let ack = a
        .request(json!({"action": "subscribe", "symbols": [
            {"symbol": "RELIANCE", "exchange": "NSE"},
            {"symbol": "FOOBARBAZ", "exchange": "NSE"}], "mode": 1}))
        .await;
    assert_eq!(ack["status"], "partial");
    assert_eq!(
        ack["subscriptions"][1]["message"],
        "Token not found for FOOBARBAZ on NSE"
    );

    // The broker is asked for RELIANCE once, at LTP.
    assert!(
        eventually(Duration::from_secs(5), || seen
            .lock()
            .iter()
            .any(|f| f.contains("NSE:RELIANCE:1")))
        .await,
        "broker frames: {:?}",
        seen.lock()
    );
    assert_eq!(manager.instrument_count(), 1);

    // A second client on the same instrument adds no broker subscription.
    let mut b = Client::connect(&url).await;
    b.auth().await;
    b.request(
        json!({"action": "subscribe", "symbol": "RELIANCE", "exchange": "NSE", "mode": "LTP"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(bridge.applied().len(), 1);

    // A broker tick reaches both clients as a web market_data frame.
    push.send(json!({"t": "RELIANCE", "x": "NSE", "p": 1167.7}).to_string())
        .await
        .unwrap();
    for c in [&mut a, &mut b] {
        let v = c.recv().await;
        assert_eq!(v["type"], "market_data");
        assert_eq!(v["mode"], 1);
        assert_eq!(v["broker"], BROKER);
        assert_eq!(v["data"]["ltp"], 1167.7);
        assert_eq!(v["data"]["mode"], "ltp");
        assert!(v["data"]["timestamp"].is_i64());
    }

    // One client leaves cleanly, the other drops its socket: the broker is
    // told to unsubscribe once the last holder is gone.
    let v = a.request(json!({"action": "unsubscribe_all"})).await;
    assert_eq!(v["successful"].as_array().map(Vec::len), Some(1));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!seen.lock().iter().any(|f| f.contains("unsub")));
    drop(b);
    assert!(
        eventually(Duration::from_secs(5), || seen
            .lock()
            .iter()
            .any(|f| f.contains("unsub") && f.contains("NSE:RELIANCE:1")))
        .await,
        "broker frames: {:?}",
        seen.lock()
    );
    assert!(eventually(Duration::from_secs(5), || manager.instrument_count() == 0).await);
    assert!(bridge.applied().is_empty());

    h.stop().await;
    bridge.stop().await;
    manager.disconnect().await.unwrap();
    broker_task.abort();
}
