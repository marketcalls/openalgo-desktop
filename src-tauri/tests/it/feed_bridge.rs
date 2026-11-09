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

/// A broker whose feed does not stream an exchange (Groww and MCX, web
/// openalgo#2341) answers each subscribe item on it with why, before the
/// symbol is looked up as Groww's adapter does (groww_adapter.py:388), and
/// nothing is held for it; the other items subscribe. A broker session's
/// end (`reset`) drops the refusal, and the unknown symbol is then answered
/// in Groww's words.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exchange_the_feed_does_not_stream_is_refused_per_item() {
    let manager = Arc::new(WebSocketManager::new());
    let symbols = SymbolResolver::new();
    symbols.load(vec![row("SBIN", "NSE", "779521")]);
    let bridge = BrokerBridge::new(manager.clone(), symbols);
    bridge.set_exchange_refusal(Arc::new(|exchange: &str| {
        (exchange == "MCX").then(|| format!("No live prices for {}", exchange))
    }));
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
                broker: Some("groww".into()),
            }),
            orders: None,
            supported_brokers: brokers(),
        },
    )
    .await
    .unwrap();
    let mut c = Client::connect(&format!("ws://{}", h.local_addr())).await;
    c.auth().await;
    let subscribe = json!({"action": "subscribe", "mode": 2, "symbols": [
        {"symbol": "SBIN", "exchange": "NSE"},
        {"symbol": "CRUDEOIL19NOV26FUT", "exchange": "MCX"}]});

    let ack = c.request(subscribe.clone()).await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(ack["subscriptions"][0]["status"], "success", "{}", ack);
    let mcx = &ack["subscriptions"][1];
    assert_eq!(mcx["status"], "error", "{}", ack);
    assert_eq!(mcx["message"], "No live prices for MCX");
    assert_eq!(h.stats().client_subscriptions, 1);

    bridge.reset();
    let ack = c.request(subscribe).await;
    assert_eq!(
        ack["subscriptions"][1]["message"],
        "Symbol CRUDEOIL19NOV26FUT not found for exchange MCX"
    );

    h.stop().await;
    bridge.stop().await;
}

/// WS-01: once the broker refuses the live feed (the manager stops until
/// the trader signs in again), a subscribe is not acknowledged as success:
/// every item is answered with the reason, the answer carries the request
/// id, and nothing is held for the client. The web answers the same way
/// when its adapter is not running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_broker_feed_answers_each_item_with_the_reason() {
    use openalgo_desktop_lib::websocket::FeedStatus;
    let (broker_url, _seen, push, broker_task) = fake_broker().await;
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
    let mut c = Client::connect(&format!("ws://{}", h.local_addr())).await;
    c.auth().await;

    assert!(eventually(Duration::from_secs(5), || manager.is_connected()).await);
    push.send(json!({"auth": "denied"}).to_string())
        .await
        .unwrap();
    assert!(
        eventually(Duration::from_secs(5), || matches!(
            manager.status(),
            FeedStatus::AuthFailed { .. }
        ))
        .await,
        "{:?}",
        manager.status()
    );

    let ack = c
        .request(
            json!({"action": "subscribe", "request_id": "r-down", "mode": 1,
            "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"},
                        {"symbol": "SBIN", "exchange": "NSE"}]}),
        )
        .await;
    assert_eq!(ack["type"], "subscribe");
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(ack["request_id"], "r-down");
    let items = ack["subscriptions"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    for item in items {
        assert_eq!(item["status"], "error", "{}", item);
        assert_eq!(item["message"], "denied", "{}", item);
        assert_eq!(item["broker"], BROKER);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(bridge.applied().is_empty());
    assert_eq!(manager.instrument_count(), 0);
    assert_eq!(h.stats().client_subscriptions, 0);

    h.stop().await;
    bridge.stop().await;
    manager.disconnect().await.unwrap();
    broker_task.abort();
}

/// A broker that answers subscribes apart from the socket, as XTS does
/// over REST: SBIN refused, the rest taken.
struct RefusingFeed(MockFeed);

impl openalgo_desktop_lib::websocket::BrokerFeed for RefusingFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(
        &self,
    ) -> openalgo_desktop_lib::error::Result<
        openalgo_desktop_lib::brokers::common::streaming::WsRequest,
    > {
        self.0.ws_request()
    }
    fn subscribe_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> Vec<Message> {
        self.0.subscribe_frames(subs)
    }
    fn unsubscribe_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> Vec<Message> {
        self.0.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<openalgo_desktop_lib::websocket::FeedEvent> {
        self.0.parse(msg)
    }
    fn admission_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> (
        Vec<Message>,
        Option<openalgo_desktop_lib::brokers::common::streaming::ReleaseAnswer>,
    ) {
        let (to, answer) = tokio::sync::oneshot::channel();
        let refused = subs
            .iter()
            .filter(|s| s.symbol == "SBIN")
            .map(|s| {
                (
                    s.clone(),
                    "The broker refused to stream this instrument (instrument not allowed)."
                        .to_string(),
                )
            })
            .collect();
        let _ = to.send(refused);
        (self.0.subscribe_frames(subs), Some(answer))
    }
}

/// Review R107-03 at the feed client: an instrument the broker refuses
/// after the subscribe was sent (XTS's REST answer) is that item's error
/// in the reply, and nothing holds it; the other item is taken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscribe_the_broker_refuses_after_sending_is_the_items_error() {
    let (broker_url, _seen, _push, broker_task) = fake_broker().await;
    let manager = Arc::new(WebSocketManager::new());
    manager
        .connect(Box::new(RefusingFeed(MockFeed::new(broker_url))))
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
    assert!(eventually(Duration::from_secs(5), || manager.is_connected()).await);
    let mut c = Client::connect(&format!("ws://{}", h.local_addr())).await;
    c.auth().await;
    let ack = c
        .request(json!({"action": "subscribe", "mode": 2, "symbols": [
            {"symbol": "RELIANCE", "exchange": "NSE"},
            {"symbol": "SBIN", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "partial", "{}", ack);
    assert_eq!(ack["subscriptions"][0]["status"], "success", "{}", ack);
    let refused = &ack["subscriptions"][1];
    assert_eq!(refused["status"], "error", "{}", ack);
    assert_eq!(
        refused["message"],
        "The broker refused to stream this instrument (instrument not allowed)."
    );
    let held: Vec<String> = manager
        .subscriptions()
        .into_iter()
        .map(|s| s.symbol)
        .collect();
    assert_eq!(held, ["RELIANCE"]);
    assert_eq!(h.stats().client_subscriptions, 1);
    h.stop().await;
    bridge.stop().await;
    manager.disconnect().await.unwrap();
    broker_task.abort();
}

/// A broker that keeps the answer to a RELIANCE subscribe until the test
/// lets it go, as XTS does while it works through a refused batch; every
/// other subscribe is answered at once.
struct HoldingFeed {
    feed: MockFeed,
    held: Arc<
        Mutex<
            Option<
                tokio::sync::oneshot::Sender<
                    openalgo_desktop_lib::brokers::common::streaming::ReleaseRefusals,
                >,
            >,
        >,
    >,
}

impl openalgo_desktop_lib::websocket::BrokerFeed for HoldingFeed {
    fn broker(&self) -> &'static str {
        "mock"
    }
    fn ws_request(
        &self,
    ) -> openalgo_desktop_lib::error::Result<
        openalgo_desktop_lib::brokers::common::streaming::WsRequest,
    > {
        self.feed.ws_request()
    }
    fn subscribe_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> Vec<Message> {
        self.feed.subscribe_frames(subs)
    }
    fn unsubscribe_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> Vec<Message> {
        self.feed.unsubscribe_frames(subs)
    }
    fn parse(&mut self, msg: &Message) -> Vec<openalgo_desktop_lib::websocket::FeedEvent> {
        self.feed.parse(msg)
    }
    fn admission_frames(
        &mut self,
        subs: &[openalgo_desktop_lib::websocket::FeedSubscription],
    ) -> (
        Vec<Message>,
        Option<openalgo_desktop_lib::brokers::common::streaming::ReleaseAnswer>,
    ) {
        let (to, answer) = tokio::sync::oneshot::channel();
        if subs.iter().any(|s| s.symbol == "RELIANCE") {
            *self.held.lock() = Some(to);
        } else {
            let _ = to.send(Vec::new());
        }
        (self.feed.subscribe_frames(subs), Some(answer))
    }
}

/// A running feed server over a bridge whose broker is `feed`.
async fn holding_server(
    feed: HoldingFeed,
    config: FeedConfig,
) -> (openalgo_desktop_lib::feed::FeedHandle, Arc<BrokerBridge>) {
    let manager = Arc::new(WebSocketManager::new());
    manager.connect(Box::new(feed)).await.unwrap();
    let symbols = SymbolResolver::new();
    symbols.load(vec![
        row("RELIANCE", "NSE", "738561"),
        row("SBIN", "NSE", "779521"),
    ]);
    let bridge = BrokerBridge::new(manager.clone(), symbols);
    bridge.start();
    let h = start(
        config,
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
    assert!(eventually(Duration::from_secs(5), || manager.is_connected()).await);
    (h, bridge)
}

/// Review X4: while one instrument's admission waits for the broker, other
/// clients' subscribes and unsubscribes go on: they queued behind it, one
/// slow answer holding up every client's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_admission_holds_up_nobody_else() {
    let (broker_url, _seen, _push, broker_task) = fake_broker().await;
    let held = Arc::new(Mutex::new(None));
    let feed = HoldingFeed {
        feed: MockFeed::new(broker_url),
        held: held.clone(),
    };
    let config = FeedConfig {
        port: 0,
        ..FeedConfig::default()
    };
    let (h, bridge) = holding_server(feed, config).await;
    let url = format!("ws://{}", h.local_addr());
    let mut a = Client::connect(&url).await;
    a.auth().await;
    let mut b = Client::connect(&url).await;
    b.auth().await;

    a.send(&json!({"action": "subscribe", "mode": 2,
        "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]}))
        .await;
    assert!(
        eventually(Duration::from_secs(5), || held.lock().is_some()).await,
        "the broker has the slow admission"
    );

    let sbin = json!({"action": "subscribe", "mode": 2,
        "symbols": [{"symbol": "SBIN", "exchange": "NSE"}]});
    let ack = tokio::time::timeout(Duration::from_secs(5), b.request(sbin))
        .await
        .expect("another client's subscribe goes on");
    assert_eq!(ack["status"], "success", "{}", ack);
    let gone = json!({"action": "unsubscribe", "mode": 2,
        "symbols": [{"symbol": "SBIN", "exchange": "NSE"}]});
    let ack = tokio::time::timeout(Duration::from_secs(5), b.request(gone))
        .await
        .expect("and its unsubscribe");
    assert_eq!(ack["status"], "success", "{}", ack);
    assert!(
        a.recv_within(Duration::from_millis(100)).await.is_none(),
        "the slow one is still waiting"
    );

    let answer = held.lock().take().expect("the answer it holds");
    let _ = answer.send(Vec::new());
    let ack = a.recv().await;
    assert_eq!(ack["status"], "success", "{}", ack);
    h.stop().await;
    bridge.stop().await;
    broker_task.abort();
}

/// Review X4: while a client's own subscribe waits for the broker its pongs
/// are not read; the wait is not the client's silence, and the ping check
/// does not close it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_is_not_closed_for_a_subscribe_it_is_waiting_on() {
    let (broker_url, _seen, _push, broker_task) = fake_broker().await;
    let held = Arc::new(Mutex::new(None));
    let feed = HoldingFeed {
        feed: MockFeed::new(broker_url),
        held: held.clone(),
    };
    let config = FeedConfig {
        port: 0,
        ping_interval: Duration::from_millis(200),
        ping_timeout: Duration::from_millis(100),
        ..FeedConfig::default()
    };
    let (h, bridge) = holding_server(feed, config).await;
    let mut c = Client::connect(&format!("ws://{}", h.local_addr())).await;
    c.auth().await;
    let releasing = {
        let held = held.clone();
        tokio::spawn(async move {
            while held.lock().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
            if let Some(answer) = held.lock().take() {
                let _ = answer.send(Vec::new());
            }
        })
    };
    let ack = c
        .request(json!({"action": "subscribe", "mode": 2,
            "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "{}", ack);
    releasing.await.unwrap();
    let ack = c
        .request(json!({"action": "subscribe", "mode": 2,
            "symbols": [{"symbol": "SBIN", "exchange": "NSE"}]}))
        .await;
    assert_eq!(ack["status"], "success", "still connected: {}", ack);
    h.stop().await;
    bridge.stop().await;
    broker_task.abort();
}
