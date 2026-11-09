//! Motilal's wire key follows who holds an instrument. A packet names only
//! its exchange character and scrip, so NSE cash RELIANCE and the NFO
//! contract with the same code share ('N', 2885), and the desktop refuses
//! the second while the first is held (`motilal/wire_keys.rs`). A release
//! that reaches no socket has to free the key as well: nothing else tells
//! the feed. The real manager, bridge, feed server and Motilal adapter; only
//! Motilal's socket is faked. Review round 3, M2 (Codex B107-04, whose probe
//! asserted the defect; these assert the right behaviour).

use crate::feed_support::{eventually, Client, API_KEY, USER_ID};
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::brokers::common::symbols::{SymToken, SymbolResolver};
use openalgo_desktop_lib::brokers::motilal::MotilalBroker;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::{start, FeedConfig, FeedDeps, FeedHandle};
use openalgo_desktop_lib::services::broker_runtime::install_market_hooks;
use openalgo_desktop_lib::websocket::{FeedConfig as ManagerConfig, WebSocketManager};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const LIMIT: Duration = Duration::from_secs(5);
const CASH: &str = "RELIANCE";
const FO: &str = "NIFTY28OCT26FUT";

enum Control {
    Push(Vec<u8>),
    DropSocket,
}

/// Motilal's market socket. After the first connection, an upgrade waits
/// for `reopen`, so the feed stays down for as long as a test needs.
struct Fake {
    url: String,
    control: mpsc::Sender<Control>,
    seen: mpsc::Receiver<Vec<u8>>,
    reopen: Arc<Notify>,
    task: JoinHandle<()>,
}

impl Fake {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (control, mut commands) = mpsc::channel::<Control>(16);
        let (record, seen) = mpsc::channel::<Vec<u8>>(64);
        let reopen = Arc::new(Notify::new());
        let gate = reopen.clone();
        let task = tokio::spawn(async move {
            let mut connection = 0;
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                connection += 1;
                if connection > 1 {
                    gate.notified().await;
                }
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    continue;
                };
                let login = tokio::time::timeout(LIMIT, ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert!(matches!(login, Message::Binary(ref b) if b.first() == Some(&b'Q')));
                // A 30-byte heartbeat: the first binary frame after the
                // login authenticates the session.
                let mut hello = vec![0; 30];
                hello[0] = b'N';
                hello[9] = b'1';
                ws.send(Message::Binary(hello)).await.unwrap();
                loop {
                    tokio::select! {
                        command = commands.recv() => match command {
                            Some(Control::Push(bytes)) => {
                                if ws.send(Message::Binary(bytes)).await.is_err() {
                                    break;
                                }
                            }
                            Some(Control::DropSocket) => {
                                let _ = ws.close(None).await;
                                break;
                            }
                            None => return,
                        },
                        wire = ws.next() => match wire {
                            Some(Ok(Message::Binary(bytes))) => {
                                if record.send(bytes).await.is_err() {
                                    return;
                                }
                            }
                            Some(Ok(Message::Ping(bytes))) => {
                                let _ = ws.send(Message::Pong(bytes)).await;
                            }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            _ => {}
                        }
                    }
                }
            }
        });
        Self {
            url,
            control,
            seen,
            reopen,
            task,
        }
    }

    /// The next registration packet is for scrip 2885, on `segment`
    /// (`C` cash, `D` derivatives), adding or removing it.
    async fn registration(&mut self, segment: u8, add: u8) {
        let next = tokio::time::timeout(LIMIT, async {
            loop {
                let bytes = self.seen.recv().await.expect("the fake socket runs");
                if bytes.first() == Some(&b'D') {
                    return bytes;
                }
            }
        })
        .await
        .expect("the broker receives a registration");
        assert_eq!(next.len(), 10);
        assert_eq!(next[3], b'N');
        assert_eq!(next[4], segment);
        assert_eq!(i32::from_le_bytes(next[5..9].try_into().unwrap()), 2885);
        assert_eq!(next[9], add);
    }

    /// Motilal's last-trade packet on the shared ('N', 2885).
    async fn price(&self, price: f32) {
        let mut raw = vec![b'N'];
        raw.extend(2885_i32.to_le_bytes());
        raw.extend(1_790_000_000_i32.to_le_bytes());
        raw.push(b'A');
        raw.extend(price.to_le_bytes());
        raw.extend(25_i32.to_le_bytes());
        raw.extend(40000_i32.to_le_bytes());
        raw.extend((price - 1.0).to_le_bytes());
        raw.extend(1500_i32.to_le_bytes());
        assert_eq!(raw.len(), 30);
        self.control.send(Control::Push(raw)).await.unwrap();
    }
}

fn row(symbol: &str, exchange: &str, broker_exchange: &str) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: symbol.into(),
        exchange: exchange.into(),
        brexchange: broker_exchange.into(),
        token: "2885".into(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: 1,
        instrument_type: if exchange == "NFO" { "FUT" } else { "EQ" }.into(),
        tick_size: 0.05,
    }
}

/// The pipeline a session runs, with the session's hooks installed.
struct Rig {
    fake: Fake,
    manager: Arc<WebSocketManager>,
    bridge: Arc<BrokerBridge>,
    server: FeedHandle,
    client: Client,
    _broker: Arc<dyn Broker>,
}

impl Rig {
    async fn start() -> Self {
        let fake = Fake::start().await;
        let symbols = SymbolResolver::new();
        symbols.load(vec![row(CASH, "NSE", "NSE"), row(FO, "NFO", "NSEFO")]);
        let broker: Arc<dyn Broker> = Arc::new(MotilalBroker::with_urls(
            symbols.clone(),
            fake.url.replace("ws://", "http://"),
            fake.url.clone(),
            fake.url.clone(),
        ));
        let auth = AuthToken::new("at:::acc:::C1:::k");
        let manager = Arc::new(WebSocketManager::with_config(ManagerConfig {
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(20),
            connect_timeout: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(60),
            ..ManagerConfig::default()
        }));
        install_market_hooks(&manager, &broker, &auth);
        let bridge = BrokerBridge::new(manager.clone(), symbols);
        bridge.start();
        manager
            .connect(broker.create_feed(&auth).unwrap())
            .await
            .unwrap();
        assert!(
            eventually(LIMIT, || manager.is_connected()).await,
            "the Motilal handshake completes"
        );
        let server = start(
            FeedConfig {
                port: 0,
                ..FeedConfig::default()
            },
            FeedDeps {
                source: bridge.clone(),
                auth: Arc::new(StaticAuth {
                    api_key: API_KEY.into(),
                    user_id: USER_ID.into(),
                    broker: Some("motilal".into()),
                }),
                orders: None,
                supported_brokers: vec!["motilal".into()],
            },
        )
        .await
        .unwrap();
        let mut client = Client::connect(&format!("ws://{}", server.local_addr())).await;
        client.auth().await;
        Self {
            fake,
            manager,
            bridge,
            server,
            client,
            _broker: broker,
        }
    }

    async fn request(&mut self, action: &str, symbol: &str, exchange: &str) -> Value {
        self.client
            .request(json!({
                "action": action,
                "mode": 2,
                "symbols": [{"symbol": symbol, "exchange": exchange}],
            }))
            .await
    }

    /// The cash instrument, registered and streaming.
    async fn hold_cash(&mut self) {
        let reply = self.request("subscribe", CASH, "NSE").await;
        assert_eq!(reply["subscriptions"][0]["status"], "success", "{reply}");
        self.fake.registration(b'C', 1).await;
        self.fake.price(1300.5).await;
        let frame = self.client.recv().await;
        assert_eq!(frame["symbol"], CASH, "{frame}");
        assert_eq!(frame["data"]["ltp"], 1300.5);
    }

    /// The socket drops and stays down, and the manager has seen it.
    async fn go_down(&mut self) {
        self.fake.control.send(Control::DropSocket).await.unwrap();
        assert!(
            eventually(LIMIT, || !self.manager.is_connected()).await,
            "the manager sees the socket drop"
        );
    }

    /// The feed comes back and its session is accepted.
    async fn come_back(&mut self) {
        self.fake.reopen.notify_one();
        assert!(
            eventually(LIMIT, || self.manager.is_connected()).await,
            "the feed reconnects"
        );
    }

    async fn release_cash(&mut self) {
        let released = self.request("unsubscribe", CASH, "NSE").await;
        assert_eq!(released["status"], "success", "{released}");
        assert_eq!(self.manager.instrument_count(), 0);
    }

    async fn subscribe_fo(&mut self) -> Value {
        self.request("subscribe", FO, "NFO").await
    }

    /// A real last-trade packet on the shared key reaches the client as
    /// the NFO contract.
    async fn nfo_packet_arrives_as_nfo(&mut self) {
        self.fake.price(25010.5).await;
        let frame = self.client.recv().await;
        assert_eq!(frame["symbol"], FO, "{frame}");
        assert_eq!(frame["exchange"], "NFO");
        assert_eq!(frame["data"]["ltp"], 25010.5);
        assert_eq!(frame["data"]["oi"], 1500);
    }

    async fn stop(mut self) {
        let _ = self.client.ws.close(None).await;
        self.server.stop().await;
        self.bridge.stop().await;
        self.manager.disconnect().await.unwrap();
        self.fake.task.abort();
        let _ = self.fake.task.await;
    }
}

fn accepted(reply: &Value) -> bool {
    reply["subscriptions"][0]["status"] == "success"
}

/// The control: a release a live socket carries frees the code, and the
/// NFO contract streams as NFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_release_frees_the_code_for_the_nfo_contract() {
    let mut rig = Rig::start().await;
    rig.hold_cash().await;
    rig.release_cash().await;
    rig.fake.registration(b'C', 0).await;
    let fo = rig.subscribe_fo().await;
    assert!(accepted(&fo), "{fo}");
    rig.fake.registration(b'D', 1).await;
    rig.nfo_packet_arrives_as_nfo().await;
    rig.stop().await;
}

/// The same collision with the socket down: nothing carries the release,
/// the code is free at once, and the next session registers only the NFO
/// contract, whose packet is delivered as NFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_during_an_outage_frees_the_code_at_once() {
    let mut rig = Rig::start().await;
    rig.hold_cash().await;
    rig.go_down().await;
    rig.release_cash().await;
    let fo = rig.subscribe_fo().await;
    assert!(accepted(&fo), "during the outage: {fo}");
    rig.come_back().await;
    rig.fake.registration(b'D', 1).await;
    rig.nfo_packet_arrives_as_nfo().await;
    rig.stop().await;
}

/// The release during the outage and the NFO subscribe after the reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_release_during_an_outage_leaves_the_code_free_after_the_reconnect() {
    let mut rig = Rig::start().await;
    rig.hold_cash().await;
    rig.go_down().await;
    rig.release_cash().await;
    rig.come_back().await;
    let fo = rig.subscribe_fo().await;
    assert!(accepted(&fo), "after the reconnect: {fo}");
    rig.fake.registration(b'D', 1).await;
    rig.nfo_packet_arrives_as_nfo().await;
    rig.stop().await;
}

/// Protection stays: an instrument still held keeps its code through the
/// outage and the reconnect, and its packets stay its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instrument_still_held_keeps_its_code_through_an_outage() {
    let mut rig = Rig::start().await;
    rig.hold_cash().await;
    rig.go_down().await;
    let during = rig.subscribe_fo().await;
    assert_eq!(during["subscriptions"][0]["status"], "error", "{during}");
    assert!(
        during["subscriptions"][0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("RELIANCE on NSE")),
        "{during}"
    );
    rig.come_back().await;
    rig.fake.registration(b'C', 1).await;
    let after = rig.subscribe_fo().await;
    assert_eq!(after["subscriptions"][0]["status"], "error", "{after}");
    rig.fake.price(1301.5).await;
    let frame = rig.client.recv().await;
    assert_eq!(frame["symbol"], CASH, "{frame}");
    assert_eq!(frame["exchange"], "NSE");
    rig.stop().await;
}
