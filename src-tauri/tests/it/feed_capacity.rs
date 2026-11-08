//! Audit C02 through the production source: a subscribe is acknowledged
//! only once the managers took it. The room on the broker connection is
//! counted across every consumer, a batch that crosses it starts what fits
//! and refuses the rest per item, and a deep book whose socket is down or
//! refused is refused while the main feed streams.

use crate::feed_support;

use feed_support::{brokers, eventually, Client, API_KEY, BROKER, USER_ID};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::streaming::{FeedMode, FeedSubscription};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::mock::MockFeed;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::{start, FeedConfig as ServerConfig, FeedDeps, FeedHandle};
use openalgo_desktop_lib::websocket::{FeedConfig, FeedStatus, WebSocketManager};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn row(symbol: &str, token: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: "NSE".into(),
        brexchange: "NSE".into(),
        token: token.into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: "EQ".into(),
        tick_size: 0.05,
    }
}

fn symbols() -> SymbolResolver {
    let s = SymbolResolver::new();
    s.load(vec![
        row("RELIANCE", "738561"),
        row("SBIN", "779521"),
        row("INFY", "408065"),
    ]);
    s
}

async fn serve(bridge: &Arc<BrokerBridge>) -> FeedHandle {
    bridge.start();
    start(
        ServerConfig {
            port: 0,
            ..ServerConfig::default()
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
    .expect("feed server starts on an ephemeral port")
}

async fn client(h: &FeedHandle) -> Client {
    let mut c = Client::connect(&format!("ws://{}", h.local_addr())).await;
    c.auth().await;
    c
}

fn subscribe(symbols: &[&str], mode: Value, depth: u8) -> Value {
    let items: Vec<Value> = symbols
        .iter()
        .map(|s| json!({"symbol": s, "exchange": "NSE"}))
        .collect();
    json!({"action": "subscribe", "mode": mode, "depth": depth, "symbols": items})
}

fn statuses(ack: &Value) -> Vec<String> {
    ack["subscriptions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| i["status"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The core probe's CAP_WIRE_ACK, the other way round: with room for two
/// instruments, a request for three starts two and refuses the third in
/// the same acknowledgement (it answered every item success and streamed
/// none); another client and an internal consumer use the same room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_room_is_shared_and_a_batch_past_it_starts_what_fits() {
    let manager = Arc::new(WebSocketManager::with_config(FeedConfig {
        max_instruments: 2,
        ..FeedConfig::default()
    }));
    let bridge = BrokerBridge::new(manager.clone(), symbols());
    let h = serve(&bridge).await;
    let mut a = client(&h).await;

    let ack = a
        .request(subscribe(&["RELIANCE", "SBIN", "INFY"], json!(1), 5))
        .await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(statuses(&ack), ["success", "success", "error"]);
    let refused = &ack["subscriptions"][2];
    assert_eq!(refused["symbol"], "INFY");
    assert!(
        refused["message"]
            .as_str()
            .is_some_and(|m| m.contains("at most 2 instruments")),
        "{}",
        refused
    );
    assert_eq!(manager.instrument_count(), 2);
    assert_eq!(h.stats().client_subscriptions, 2);

    // Another client: a held instrument fits, a new one does not.
    let mut b = client(&h).await;
    let ack = b.request(subscribe(&["INFY", "SBIN"], json!(2), 5)).await;
    assert_eq!(statuses(&ack), ["error", "success"], "{}", ack);

    // A strategy or the sandbox takes the room freed by a release.
    let ack = a
        .request(json!({"action": "unsubscribe", "mode": 1,
        "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    assert!(eventually(Duration::from_secs(5), || manager.instrument_count() == 1).await);
    let internal = FeedSubscription {
        symbol: "INFY".into(),
        exchange: "NSE".into(),
        token: "408065".into(),
        brsymbol: "INFY".into(),
        brexchange: "NSE".into(),
        mode: FeedMode::Ltp,
        depth: 5,
    };
    manager
        .subscribe(vec![internal])
        .await
        .expect("room for one");
    let ack = b.request(subscribe(&["RELIANCE"], json!(1), 5)).await;
    assert_eq!(statuses(&ack), ["error"], "{}", ack);
    assert_eq!(manager.instrument_count(), 2);

    h.stop().await;
    bridge.stop().await;
}

/// A broker socket that accepts the connection and then refuses the
/// session (`{"auth": "denied"}` to `MockFeed`).
async fn refusing_broker() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let url = format!("ws://{}", listener.local_addr().expect("bound"));
    let task = tokio::spawn(async move {
        let Ok((tcp, _)) = listener.accept().await else {
            return;
        };
        let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
            return;
        };
        let (mut w, mut r) = ws.split();
        let _ = w
            .send(Message::Text(json!({"auth": "denied"}).to_string()))
            .await;
        while r.next().await.is_some() {}
    });
    (url, task)
}

/// The deep book's socket is its own (Dhan 20, Fyers 50): one that did
/// not start, or that the broker refused, refuses Depth at its levels
/// with why, while 5-level depth and quotes on the main feed are taken
/// (it acknowledged them and streamed nothing).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deep_book_whose_socket_is_down_is_refused_while_the_main_feed_streams() {
    let depth = Arc::new(WebSocketManager::new());
    let bridge = BrokerBridge::with_depth_manager(
        Arc::new(WebSocketManager::new()),
        depth.clone(),
        symbols(),
    );
    bridge.set_depth_capability(Arc::new(|_| vec![5, 20]));
    bridge.set_deep_unavailable(20);
    let h = serve(&bridge).await;
    let mut c = client(&h).await;

    let ack = c.request(subscribe(&["SBIN"], json!("Depth"), 20)).await;
    assert_eq!(statuses(&ack), ["error"], "{}", ack);
    assert!(
        ack["subscriptions"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("20-level market depth feed did not start")),
        "{}",
        ack
    );
    let ack = c.request(subscribe(&["SBIN"], json!("Depth"), 5)).await;
    assert_eq!(statuses(&ack), ["success"], "{}", ack);

    // A depth socket the broker refused, the main feed untouched.
    bridge.reset();
    bridge.set_depth_capability(Arc::new(|_| vec![5, 20]));
    bridge.set_deep_levels(Some(20));
    let (url, broker) = refusing_broker().await;
    depth
        .connect(Box::new(MockFeed::new(url)))
        .await
        .expect("connects");
    assert!(
        eventually(Duration::from_secs(5), || matches!(
            depth.status(),
            FeedStatus::AuthFailed { .. }
        ))
        .await,
        "{:?}",
        depth.status()
    );
    let ack = c
        .request(subscribe(&["RELIANCE"], json!("Depth"), 20))
        .await;
    assert_eq!(statuses(&ack), ["error"], "{}", ack);
    assert_eq!(ack["subscriptions"][0]["message"], "denied");
    let ack = c.request(subscribe(&["RELIANCE"], json!("Quote"), 5)).await;
    assert_eq!(statuses(&ack), ["success"], "{}", ack);
    assert!(bridge.applied_deep().is_empty());

    h.stop().await;
    bridge.stop().await;
    depth.disconnect().await.expect("stops");
    broker.abort();
}
