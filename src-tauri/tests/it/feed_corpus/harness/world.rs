//! The production pieces a case runs through, wired as the app wires them
//! for a broker session (`services::broker_runtime`): the broker's own
//! `BrokerFeed`s on real `WebSocketManager`s connected to fake broker
//! sockets, a fake broker REST API for what feeds and order pollers read
//! over HTTP, `BrokerBridge` over the symbol master, the 8765 feed server
//! (`feed::start`) on an ephemeral port, and order updates (from a socket,
//! or from an order-book poller's stream) through the runtime's
//! `order_event` onto an `EventBus` that the feed's `OrderRelay` listens
//! on. No real broker, no 8765, no data folder.

use super::case::{Case, HttpAnswer, Socket};
use super::http::{HttpFake, Route};
use super::upstream::{Upstream, DEADLINE};
use crate::feed_support::{brokers, eventually, API_KEY, USER_ID};
use openalgo_desktop_lib::brokers::common::streaming::{
    BrokerFeed, FeedEvent, OrderFeed, OrderUpdate,
};
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::types::AuthToken;
use openalgo_desktop_lib::brokers::Broker;
use openalgo_desktop_lib::error::AppError;
use openalgo_desktop_lib::events::EventBus;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::bridge::BrokerBridge;
use openalgo_desktop_lib::feed::orders::OrderRelay;
use openalgo_desktop_lib::feed::{start, FeedConfig, FeedDeps, FeedHandle};
use openalgo_desktop_lib::services::broker_runtime::{self, order_event};
use openalgo_desktop_lib::websocket::{
    FeedConfig as ManagerConfig, FeedFactory, PoolLimits, WebSocketManager,
};
use parking_lot::Mutex;
use serde_json::Value;
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinSet;

/// What a broker's feed constructor gets: the fake sockets' and API's
/// addresses, the case's symbol master, and whether the case asks for
/// order updates.
pub(crate) struct Env {
    urls: BTreeMap<Socket, String>,
    http: Arc<HttpFake>,
    symbols: SymbolResolver,
    orders: bool,
}

impl Env {
    /// The `ws://` address of the fake socket `name` ("main", "depth" or
    /// "orders"), for the feed's `with_url` constructor.
    pub(crate) fn url(&self, name: &str) -> String {
        Socket::named(name)
            .and_then(|s| self.urls.get(&s).cloned())
            .unwrap_or_else(|| panic!("no fake socket \"{}\"", name))
    }

    /// The fake broker REST API's base address (`http://127.0.0.1:<port>`),
    /// for the broker's `with_urls` constructor.
    pub(crate) fn http_url(&self) -> String {
        self.http.url().to_string()
    }

    /// Answer every request to `path` (any method) with `body` and 200,
    /// as many times as it is read: what a feed reads before it connects
    /// (Upstox's authorize answer naming `env.url("main")`). A case's
    /// `http_serve` steps can replace it.
    pub(crate) fn serve_json(&self, path: &str, body: Value) {
        self.http
            .serve(Route::new(None, path, 200, body.to_string()));
    }

    /// The case's symbol master, shared with the bridge as the app shares
    /// it.
    pub(crate) fn symbols(&self) -> SymbolResolver {
        self.symbols.clone()
    }

    /// Whether the case asks for order updates (`subscribe_orders`, or the
    /// `orders` socket): start an order-book poller only then, or its
    /// reads would be requests no step expects.
    pub(crate) fn orders(&self) -> bool {
        self.orders
    }
}

/// The feeds a broker session runs: market data, and the deep-book socket
/// and the order updates where the broker has them.
pub(crate) struct Feeds {
    market: Box<dyn BrokerFeed>,
    depth: Option<(u8, Box<dyn BrokerFeed>)>,
    orders: Option<OrderFeed>,
    /// What must live as long as the case (a broker whose poller stops
    /// when it is dropped).
    keep: Vec<Box<dyn Any + Send + Sync>>,
    /// The session's broker and auth (`with_broker`).
    broker: Session,
    /// The market feed for each further pooled connection (`with_pool`).
    pool: Option<PoolFeed>,
}

/// Makes the market feed for a further pooled connection, given its fake
/// socket's address.
type PoolFeed = Arc<dyn Fn(&str) -> Box<dyn BrokerFeed> + Send + Sync>;

/// The session's broker and its auth, as `broker_runtime` holds them for
/// the feeds' hooks.
type Session = Option<(Arc<dyn Broker>, AuthToken)>;

impl Feeds {
    pub(crate) fn market(feed: impl BrokerFeed + 'static) -> Feeds {
        Feeds::boxed(Box::new(feed))
    }

    /// The market feed as `Broker::create_feed` returns it.
    pub(crate) fn boxed(feed: Box<dyn BrokerFeed>) -> Feeds {
        Feeds {
            market: feed,
            depth: None,
            orders: None,
            keep: Vec::new(),
            broker: None,
            pool: None,
        }
    }

    /// The market feed of each further connection of the market data pool
    /// (`WebSocketManager::connect_pool`, as a session opens one when the
    /// first is full: `Broker::create_feed` again), made for the fake
    /// socket of that connection (`main#2`, `main#3`, ...) at the address
    /// given. Without it the pool keeps to one connection.
    pub(crate) fn with_pool(
        mut self,
        feed: impl Fn(&str) -> Box<dyn BrokerFeed> + Send + Sync + 'static,
    ) -> Feeds {
        self.pool = Some(Arc::new(feed));
        self
    }

    /// The session's broker and its `auth`, as `broker_runtime` installs
    /// them: the bridge takes its depth levels (`Broker::feed_depth_levels`,
    /// in place of `case.json`'s `depth_levels`) and its exchange refusals
    /// (`Broker::feed_refusal`), the market feed its step before a new
    /// instrument (`Broker::feed_presubscribe`), and the order relay
    /// completes each socket update with it
    /// (`Broker::complete_order_update`); it lives until the case ends.
    pub(crate) fn with_broker(mut self, broker: Arc<dyn Broker>, auth: &AuthToken) -> Feeds {
        self.broker = Some((broker.clone(), auth.clone()));
        self.keeping(broker)
    }

    /// The broker's separate socket for books of `levels` levels (Dhan 20,
    /// Fyers 50), as `Broker::create_depth_feed` gives it: a Depth
    /// subscription at those levels goes there, with a Quote subscription
    /// on the market feed for its price (`BrokerBridge::set_deep_levels`).
    /// With `with_broker`, the socket starts as a session starts it
    /// (`broker_runtime::connect_deep_feed`: the pool and room refusal, the
    /// broker's silence timeout, Dhan's 30 s), or, for a case whose
    /// `case.json` says `"deep_feed_not_made": true`, as a session does when
    /// `Broker::create_depth_feed` failed.
    pub(crate) fn with_depth(mut self, levels: u8, feed: impl BrokerFeed + 'static) -> Feeds {
        self.depth = Some((levels, Box::new(feed)));
        self
    }

    /// A dedicated order-update socket.
    pub(crate) fn with_orders(self, feed: impl BrokerFeed + 'static) -> Feeds {
        self.with_order_feed(OrderFeed::Socket(Box::new(feed)))
    }

    /// Order updates as `Broker::create_order_feed` returns them: a socket,
    /// or the stream of an order-book poller (5paisa, Samco, Groww; start
    /// it with a short interval, `start_order_updates(&auth, 1 s)`).
    pub(crate) fn with_order_feed(mut self, feed: OrderFeed) -> Feeds {
        self.orders = Some(feed);
        self
    }

    /// Keep `owner` until the case ends: the broker whose poller feeds
    /// `with_order_feed` stops polling when it is dropped.
    pub(crate) fn keeping(mut self, owner: impl Any + Send + Sync) -> Feeds {
        self.keep.push(Box::new(owner));
        self
    }
}

/// The further pooled connections a case's script names (`main#2` up to
/// the highest named), each its own fake socket.
fn pool_sockets(case: &Case) -> Vec<Socket> {
    let highest = case
        .sockets
        .iter()
        .filter_map(|s| s.pool_slot())
        .max()
        .unwrap_or(1);
    (2..=highest).map(Socket::Pool).collect()
}

/// The market data pool a session opens (`broker_runtime::start_session`):
/// the broker's pool limits (`broker_runtime::pool_limits`), with the case's
/// own instruments per connection where it sets one, and a further
/// connection's feed made by the module (`Feeds::with_pool`) for the next
/// fake pool socket, while the case has one.
fn market_pool(
    case: &Case,
    feeds: &Feeds,
    upstreams: &BTreeMap<Socket, Upstream>,
) -> (FeedFactory, PoolLimits) {
    let mut limits = broker_runtime::pool_limits(&case.broker);
    if let Some(n) = case.pool_per_connection {
        if limits.connections < 2 {
            panic!(
                "feed corpus {}: {}'s pool has one connection; pool_per_connection does not apply",
                case.id, case.broker
            );
        }
        limits.per_connection = n;
    }
    let urls: Vec<String> = pool_sockets(case)
        .into_iter()
        .map(|s| upstreams[&s].url().to_string())
        .collect();
    let opened = Arc::new(AtomicUsize::new(0));
    let make = feeds.pool.clone();
    let more: FeedFactory = Arc::new(move || {
        let make = make.as_ref()?;
        let url = urls.get(opened.fetch_add(1, Ordering::SeqCst))?;
        Some(make(url))
    });
    (more, limits)
}

/// Every broker module builds its session's broker (`Feeds::with_broker`),
/// as a session does, except those still on the ratchet list
/// (`feed_corpus::WITHOUT_BROKER`), which may not pass one.
fn session_required(case: &Case, feeds: &Feeds) {
    let listed = crate::feed_corpus::WITHOUT_BROKER.contains(&case.broker.as_str());
    match (feeds.broker.is_some(), listed) {
        (false, false) => panic!(
            "feed corpus {}: build the session's broker and pass it with \
             Feeds::with_broker(broker, &auth), as broker_runtime does",
            case.id
        ),
        (true, true) => panic!(
            "feed corpus {}: {} passes its broker now: take it off WITHOUT_BROKER",
            case.id, case.broker
        ),
        _ => {}
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
    http: Arc<HttpFake>,
    /// The feed server's `ws://` address.
    pub(crate) feed_url: String,
    market: Arc<WebSocketManager>,
    deep: Arc<WebSocketManager>,
    orders: Arc<WebSocketManager>,
    bridge: Arc<BrokerBridge>,
    bus: Arc<EventBus>,
    relays: JoinSet<()>,
    server: FeedHandle,
    keep: Vec<Box<dyn Any + Send + Sync>>,
    /// The sockets a `disconnect` step closed.
    disconnected: Mutex<BTreeSet<Socket>>,
}

impl World {
    pub(crate) async fn start(case: &Case, feeds: fn(&Env) -> Feeds) -> World {
        let mut upstreams = BTreeMap::new();
        for s in Socket::ALL.into_iter().chain(pool_sockets(case)) {
            let up = Upstream::bind(case.upstream_ignore.clone()).await;
            if case.close_sockets.contains(&s) {
                up.expect_close();
            }
            if let Some((status, times)) = case.refused_at_start.get(&s) {
                up.refuse(*status, *times);
            }
            upstreams.insert(s, up);
        }
        let urls: BTreeMap<Socket, String> = upstreams
            .iter()
            .map(|(s, u)| (*s, u.url().to_string()))
            .collect();
        let http = Arc::new(HttpFake::bind().await);
        for a in &case.http {
            http.serve(route(a, &urls, http.url()));
        }
        let symbols = SymbolResolver::new();
        symbols.load(case.instruments.clone());
        let env = Env {
            urls,
            http: http.clone(),
            symbols: symbols.clone(),
            orders: case.orders,
        };
        let feeds = feeds(&env);
        session_required(case, &feeds);

        let market = Arc::new(WebSocketManager::with_config(manager_config()));
        let deep = Arc::new(WebSocketManager::with_config(manager_config()));
        let orders = Arc::new(WebSocketManager::with_config(manager_config()));
        let bridge = BrokerBridge::with_depth_manager(market.clone(), deep.clone(), symbols);
        match &feeds.broker {
            // `broker_runtime::start_session`'s wiring.
            Some((b, auth)) => {
                if !case.depth_levels.is_empty() {
                    panic!(
                        "feed corpus {}: depth levels come from the broker; drop case.json's",
                        case.id
                    );
                }
                let caps = b.clone();
                bridge.set_depth_capability(Arc::new(move |exchange: &str| {
                    caps.feed_depth_levels(exchange)
                }));
                let refusal = b.clone();
                bridge.set_exchange_refusal(Arc::new(move |exchange: &str| {
                    refusal.feed_refusal(exchange)
                }));
                broker_runtime::install_market_hooks(&market, b, auth);
            }
            None => {
                let levels = case.depth_levels.clone();
                bridge.set_depth_capability(Arc::new(move |exchange: &str| {
                    levels.get(exchange).cloned().unwrap_or_else(|| vec![5])
                }));
            }
        }

        // Order updates: listened for before any socket connects, as the
        // runtime does, so none is missed.
        let bus = Arc::new(EventBus::new());
        let relay = OrderRelay::register(&bus);
        let generation = bus.begin_session();
        let mut relays = JoinSet::new();
        for m in [&market, &orders] {
            let session = (case.broker.clone(), feeds.broker.clone(), generation);
            relay_orders(&mut relays, m, bus.clone(), session);
        }

        bridge.start();
        let mut connected = vec![Socket::Main];
        let (more, limits) = market_pool(case, &feeds, &upstreams);
        market
            .connect_pool(feeds.market, more, limits)
            .await
            .expect("the market data feed starts");
        // A case that never names the deep-book or order socket leaves it
        // closed, so its frames need not be expected and its books do not
        // move off the market feed.
        let depth_used = case.sockets.contains(&Socket::Depth);
        if case.deep_feed_not_made && (feeds.depth.is_none() || feeds.broker.is_none()) {
            panic!(
                "feed corpus {}: deep_feed_not_made needs a broker's with_depth and with_broker",
                case.id
            );
        }
        match (feeds.depth, &feeds.broker) {
            // `broker_runtime::start_deep_feed`'s wiring, from the feed on.
            (Some((levels, _)), Some((b, _))) if case.deep_feed_not_made => {
                let feed = Err(AppError::Unsupported("depth_feed"));
                broker_runtime::connect_deep_feed(&deep, &bridge, b.id(), levels, feed).await;
            }
            (Some((levels, feed)), Some((b, _))) if depth_used => {
                broker_runtime::connect_deep_feed(&deep, &bridge, b.id(), levels, Ok(feed)).await;
                connected.push(Socket::Depth);
            }
            (Some((levels, feed)), None) if depth_used => {
                bridge.set_deep_levels(Some(levels));
                deep.connect(feed).await.expect("the depth feed starts");
                connected.push(Socket::Depth);
            }
            _ => {}
        }
        if let Some(t) = case.deep_book_timeout {
            let prod = feeds
                .broker
                .as_ref()
                .and_then(|(b, _)| broker_runtime::deep_book_timeout(b.id()));
            if prod.is_none() {
                panic!(
                    "feed corpus {}: deep_book_timeout_ms shortens a broker's deep-book timeout, \
                     and this session's broker has none (or the case has no with_broker)",
                    case.id
                );
            }
            bridge.set_deep_book_timeout(Some(t));
        }
        match feeds.orders {
            Some(OrderFeed::Socket(feed)) if case.sockets.contains(&Socket::Orders) => {
                orders.connect(feed).await.expect("the order feed starts");
                connected.push(Socket::Orders);
            }
            Some(OrderFeed::Stream(rx)) => relay_order_stream(
                &mut relays,
                rx,
                bus.clone(),
                case.broker.clone(),
                generation,
            ),
            _ => {}
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
        // A socket refused from the start never connects; the case says
        // what that makes of it (`upstream_connections`).
        for s in connected
            .into_iter()
            .filter(|s| !case.refused_at_start.contains_key(s))
        {
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
            http,
            market,
            deep,
            orders,
            bridge,
            bus,
            relays,
            server,
            keep: feeds.keep,
            disconnected: Mutex::new(BTreeSet::new()),
        }
    }

    pub(crate) fn upstream(&self, s: Socket) -> &Upstream {
        &self.upstreams[&s]
    }

    /// The manager whose feed connects to socket `s`.
    fn manager(&self, s: Socket) -> &WebSocketManager {
        match s {
            Socket::Main | Socket::Pool(_) => &self.market,
            Socket::Depth => &self.deep,
            Socket::Orders => &self.orders,
        }
    }

    /// Every fake broker socket of the case: main, its further pooled
    /// connections, depth and orders.
    pub(crate) fn sockets(&self) -> impl Iterator<Item = Socket> + '_ {
        self.upstreams.keys().copied()
    }

    /// Close socket `s` deliberately, as a logout or a broker switch does
    /// (`WebSocketManager::disconnect`), with clients still subscribed: the
    /// fake broker records the feed's farewell frames and its close. A
    /// pooled connection's manager is the market feed's: it closes them
    /// all.
    pub(crate) async fn disconnect(&self, s: Socket) {
        self.upstream(s).expect_close();
        let manager = if s.pool_slot().is_some() {
            Socket::Main
        } else {
            s
        };
        self.disconnected.lock().insert(manager);
        if let Err(e) = self.manager(s).disconnect().await {
            panic!("the {} feed did not disconnect: {}", s.name(), e);
        }
    }

    pub(crate) fn http(&self) -> &HttpFake {
        &self.http
    }

    /// Answer as `a` says from now on (an `http_serve` step).
    pub(crate) fn serve(&self, a: &HttpAnswer) {
        let urls = self
            .upstreams
            .iter()
            .map(|(s, u)| (*s, u.url().to_string()))
            .collect();
        self.http.serve(route(a, &urls, self.http.url()));
    }

    /// Once every client has gone: check the feed server and the broker
    /// hold nothing for them any more, then stop everything.
    pub(crate) async fn stop(mut self, id: &str) {
        let gone = eventually(DEADLINE, || self.server.stats().clients == 0).await;
        // A manager a `disconnect` step closed forgot its subscriptions
        // while clients held them; the bridge's release of those cannot
        // reach the broker, so only the others are checked.
        let closed = self.disconnected.lock().clone();
        let main_open = !closed.contains(&Socket::Main);
        let deep_open = !closed.contains(&Socket::Depth);
        let released = eventually(DEADLINE, || {
            let main = self.bridge.applied().is_empty() && self.market.instrument_count() == 0;
            let deep = self.bridge.applied_deep().is_empty() && self.deep.instrument_count() == 0;
            (main || !main_open) && (deep || !deep_open)
        })
        .await;
        self.server.stop().await;
        self.bridge.stop().await;
        // By field: `self.server` has gone, so `self.manager` cannot be used.
        let managers = [
            (Socket::Main, &self.market),
            (Socket::Depth, &self.deep),
            (Socket::Orders, &self.orders),
        ];
        for (_, m) in managers.into_iter().filter(|(s, _)| !closed.contains(s)) {
            let _ = m.disconnect().await;
        }
        // Dropping an owner stops its poller, which ends its stream.
        self.keep.clear();
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

/// The fake API's route for an answer, its body's `{ws:<socket>}` and
/// `{http}` filled in with the fakes' addresses.
fn route(a: &HttpAnswer, urls: &BTreeMap<Socket, String>, http: &str) -> Route {
    let mut body = a.body.text().replace("{http}", http);
    for (s, url) in urls {
        body = body.replace(&format!("{{ws:{}}}", s.name()), url);
    }
    let mut r = Route::new(a.method.as_deref(), &a.path, a.status, body);
    r.record = a.record;
    r.when_body = a.when_body.clone();
    r
}

/// How long `broker_runtime`'s socket relay waits for the broker to
/// complete an order update (its `ORDER_COMPLETION_DEADLINE`) before it
/// publishes the update as it came.
const ORDER_COMPLETION_DEADLINE: Duration = Duration::from_secs(5);

/// `u` as `broker_runtime::completed` makes it: completed by the session's
/// broker with its auth (IndMoney looks an order it did not see placed up
/// in the order book), or as it came when there is none or it is late.
async fn completed(session: &Session, u: &OrderUpdate) -> OrderUpdate {
    let Some((broker, auth)) = session else {
        return u.clone();
    };
    let asked = broker.complete_order_update(auth, u.clone());
    tokio::time::timeout(ORDER_COMPLETION_DEADLINE, asked)
        .await
        .unwrap_or_else(|_| u.clone())
}

/// `services::broker_runtime`'s socket order relay: every order update a
/// feed decodes is completed by the broker and goes on the bus as the
/// runtime's `order_event`.
fn relay_orders(
    set: &mut JoinSet<()>,
    feed: &WebSocketManager,
    bus: Arc<EventBus>,
    (broker, session, generation): (String, Session, u64),
) {
    let mut rx = feed.subscribe_ticks();
    set.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let FeedEvent::OrderUpdate(u) = &*ev {
                        let u = completed(&session, u).await;
                        bus.publish(order_event(&u, &broker, generation));
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

/// `services::broker_runtime`'s relay of an order-book poller's stream.
fn relay_order_stream(
    set: &mut JoinSet<()>,
    mut rx: tokio::sync::mpsc::Receiver<OrderUpdate>,
    bus: Arc<EventBus>,
    broker: String,
    generation: u64,
) {
    set.spawn(async move {
        while let Some(u) = rx.recv().await {
            bus.publish(order_event(&u, &broker, generation));
        }
    });
}
