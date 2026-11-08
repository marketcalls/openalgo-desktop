//! The production pieces a case runs through, wired as the app wires them
//! for a broker session (`services::broker_runtime`): the broker's own
//! `BrokerFeed`s on real `WebSocketManager`s connected to fake broker
//! sockets, `BrokerBridge` over the symbol master, the 8765 feed server
//! (`feed::start`) on an ephemeral port, and order updates from the
//! managers through the runtime's `order_event` onto an `EventBus` that the
//! feed's `OrderRelay` listens on. No real broker, no 8765, no data folder.

use super::case::{Case, Socket};
use super::upstream::{Upstream, DEADLINE};
use crate::feed_support::{brokers, eventually, API_KEY, USER_ID};
use openalgo_desktop_lib::brokers::common::streaming::{BrokerFeed, FeedEvent};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::events::EventBus;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::orders::OrderRelay;
use openalgo_desktop_lib::feed::{start, FeedConfig, FeedDeps, FeedHandle};
use openalgo_desktop_lib::services::broker_runtime::order_event;
use openalgo_desktop_lib::websocket::{FeedConfig as ManagerConfig, WebSocketManager};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;

/// What a broker's feed constructor gets: the fake sockets' addresses and
/// the case's symbol master.
pub(crate) struct Env {
    urls: BTreeMap<Socket, String>,
    symbols: SymbolResolver,
}

impl Env {
    /// The `ws://` address of the fake socket `name` ("main", "depth" or
    /// "orders"), for the feed's `with_url` constructor.
    pub(crate) fn url(&self, name: &str) -> String {
        Socket::named(name)
            .and_then(|s| self.urls.get(&s).cloned())
            .unwrap_or_else(|| panic!("no fake socket \"{}\"", name))
    }

    /// The case's symbol master, shared with the bridge as the app shares
    /// it.
    pub(crate) fn symbols(&self) -> SymbolResolver {
        self.symbols.clone()
    }
}

/// The feeds a broker session runs: market data, and the deep-book and
/// order-update sockets where the broker has them.
pub(crate) struct Feeds {
    market: Box<dyn BrokerFeed>,
    depth: Option<(u8, Box<dyn BrokerFeed>)>,
    orders: Option<Box<dyn BrokerFeed>>,
}

impl Feeds {
    pub(crate) fn market(feed: impl BrokerFeed + 'static) -> Feeds {
        Feeds {
            market: Box::new(feed),
            depth: None,
            orders: None,
        }
    }

    /// The broker's separate socket for books of `levels` levels (Dhan 20,
    /// Fyers 50), as `Broker::create_depth_feed` gives it: a Depth
    /// subscription at those levels goes there, with a Quote subscription
    /// on the market feed for its price (`BrokerBridge::set_deep_levels`).
    pub(crate) fn with_depth(mut self, levels: u8, feed: impl BrokerFeed + 'static) -> Feeds {
        self.depth = Some((levels, Box::new(feed)));
        self
    }

    pub(crate) fn with_orders(mut self, feed: impl BrokerFeed + 'static) -> Feeds {
        self.orders = Some(Box::new(feed));
        self
    }
}

/// The managers' timings for a test: reconnect at once, never call a
/// quiet socket stalled within a case.
fn manager_config() -> ManagerConfig {
    ManagerConfig {
        backoff_base: Duration::from_millis(5),
        backoff_max: Duration::from_millis(20),
        stall_timeout: Duration::from_secs(60),
        connect_timeout: Duration::from_secs(2),
        stable_after: Duration::from_secs(60),
        ..ManagerConfig::default()
    }
}

pub(crate) struct World {
    upstreams: BTreeMap<Socket, Upstream>,
    /// The feed server's `ws://` address.
    pub(crate) feed_url: String,
    market: Arc<WebSocketManager>,
    deep: Arc<WebSocketManager>,
    orders: Arc<WebSocketManager>,
    bridge: Arc<BrokerBridge>,
    bus: Arc<EventBus>,
    relays: JoinSet<()>,
    server: FeedHandle,
}

impl World {
    pub(crate) async fn start(case: &Case, feeds: fn(&Env) -> Feeds) -> World {
        let mut upstreams = BTreeMap::new();
        for s in Socket::ALL {
            upstreams.insert(s, Upstream::bind(case.upstream_ignore.clone()).await);
        }
        let symbols = SymbolResolver::new();
        symbols.load(case.instruments.clone());
        let env = Env {
            urls: upstreams
                .iter()
                .map(|(s, u)| (*s, u.url().to_string()))
                .collect(),
            symbols: symbols.clone(),
        };
        let feeds = feeds(&env);

        let market = Arc::new(WebSocketManager::with_config(manager_config()));
        let deep = Arc::new(WebSocketManager::with_config(manager_config()));
        let orders = Arc::new(WebSocketManager::with_config(manager_config()));
        let bridge = BrokerBridge::with_depth_manager(market.clone(), deep.clone(), symbols);
        let levels = case.depth_levels.clone();
        bridge.set_depth_capability(Arc::new(move |exchange: &str| {
            levels.get(exchange).cloned().unwrap_or_else(|| vec![5])
        }));

        // Order updates: listened for before any socket connects, as the
        // runtime does, so none is missed.
        let bus = Arc::new(EventBus::new());
        let relay = OrderRelay::register(&bus);
        let generation = bus.begin_session();
        let mut relays = JoinSet::new();
        for m in [&market, &orders] {
            relay_orders(&mut relays, m, bus.clone(), case.broker.clone(), generation);
        }

        bridge.start();
        let mut connected = vec![Socket::Main];
        market
            .connect(feeds.market)
            .await
            .expect("the market data feed starts");
        // A case that never names the deep-book or order socket leaves it
        // closed, so its frames need not be expected and its books do not
        // move off the market feed.
        if let Some((levels, feed)) = feeds
            .depth
            .filter(|_| case.sockets.contains(&Socket::Depth))
        {
            bridge.set_deep_levels(Some(levels));
            deep.connect(feed).await.expect("the depth feed starts");
            connected.push(Socket::Depth);
        }
        if let Some(feed) = feeds
            .orders
            .filter(|_| case.sockets.contains(&Socket::Orders))
        {
            orders.connect(feed).await.expect("the order feed starts");
            connected.push(Socket::Orders);
        }
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
                    broker: Some(case.broker.clone()),
                }),
                orders: Some(relay.receiver()),
                supported_brokers: brokers(),
            },
        )
        .await
        .expect("the feed server starts on an ephemeral port");
        for s in connected {
            if upstreams[&s].connection(1).await.is_none() {
                panic!(
                    "feed corpus {}: the {} feed never connected to its fake broker",
                    case.id,
                    s.name()
                );
            }
        }
        World {
            feed_url: format!("ws://{}", server.local_addr()),
            upstreams,
            market,
            deep,
            orders,
            bridge,
            bus,
            relays,
            server,
        }
    }

    pub(crate) fn upstream(&self, s: Socket) -> &Upstream {
        &self.upstreams[&s]
    }

    /// Once every client has gone: check the feed server and the broker
    /// hold nothing for them any more, then stop everything.
    pub(crate) async fn stop(mut self, id: &str) {
        let gone = eventually(DEADLINE, || self.server.stats().clients == 0).await;
        let released = eventually(DEADLINE, || {
            self.bridge.applied().is_empty()
                && self.bridge.applied_deep().is_empty()
                && self.market.instrument_count() == 0
                && self.deep.instrument_count() == 0
        })
        .await;
        self.server.stop().await;
        self.bridge.stop().await;
        for m in [&self.market, &self.deep, &self.orders] {
            let _ = m.disconnect().await;
        }
        self.relays.abort_all();
        while self.relays.join_next().await.is_some() {}
        self.bus.shutdown(Duration::from_millis(200)).await;
        assert!(
            gone,
            "feed corpus {}: a client entry outlived its socket",
            id
        );
        assert!(
            released,
            "feed corpus {}: subscriptions still held at the broker after every client left",
            id
        );
    }
}

/// `services::broker_runtime`'s order relay: every order update a feed
/// decodes goes on the bus as the runtime's `order_event`.
fn relay_orders(
    set: &mut JoinSet<()>,
    feed: &WebSocketManager,
    bus: Arc<EventBus>,
    broker: String,
    generation: u64,
) {
    let mut rx = feed.subscribe_ticks();
    set.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let FeedEvent::OrderUpdate(u) = &*ev {
                        bus.publish(order_event(u, &broker, generation));
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    panic!("the order relay fell behind by {} events", n)
                }
                Err(RecvError::Closed) => return,
            }
        }
    });
}
