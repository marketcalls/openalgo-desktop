//! The production [`MarketDataSource`]: the feed server on top of the
//! broker-agnostic outbound manager (`crate::websocket::WebSocketManager`).
//!
//! * Symbols resolve against the shared symbol master (`SymbolResolver`), so
//!   an unknown symbol gets the web's "Token not found" refusal and a known
//!   one becomes a `FeedSubscription` with the broker token and exchange.
//! * `subscribe` / `unsubscribe` edit the desired set and wake the
//!   reconcile task (`apply`), which diffs desired against applied, asks
//!   the managers, and answers the feed client waiting on the key: taken or
//!   refused, released or still streamed (audit C01, C02). The feed server
//!   already reference counts across its clients, so the bridge holds
//!   exactly one manager reference per `(instrument, mode, depth)`; the
//!   manager merges modes per instrument for the broker.
//! * The relay task turns the manager's `MarketEvent`s into
//!   [`MarketUpdate`]s. A broker that sends a full tick as `Tick` plus a
//!   separate `Depth` (Kite) is served as: the tick to Quote and LTP holders,
//!   the depth snapshot (with the tick's quote fields) to Depth holders.
//!   Index ticks carry no book and reach every mode directly.
//!
//! * Deep books: when the broker runs a separate depth socket (Fyers 50
//!   levels, Dhan 20; `set_deep_levels`), a Depth subscription asking for
//!   exactly those levels goes to the depth manager, with a Quote
//!   subscription on the main manager for its price and quote fields.
//!
//! Broker `OrderUpdate` events are not relayed here: order updates reach
//! feed clients through the `order.update` bus topic (`feed::orders`).
//!
//! Both tasks are owned (`JoinSet`), started by `start` and aborted by
//! `stop`, which also releases every manager reference the bridge holds.
//! Every map is bounded by what feed clients currently hold.

use super::source::{
    answered, ClientLosses, InstrumentKey, MarketDataSource, MarketUpdate, Mode, SourceOp,
    DEFAULT_DEPTH,
};
use crate::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, MarketEvent, QuoteField,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::websocket::{Reader, WebSocketManager};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;

mod apply;
mod deep;
mod render;
pub(super) mod snapshot;
use apply::{Applied, Ops};
pub use apply::{Operations, Refused};
use deep::DeepBooks;
pub(crate) use render::{depth_update, tick_update};
use snapshot::Snapshot;

/// Capacity of the update channel between the bridge and feed servers.
pub const BRIDGE_UPDATE_CAP: usize = 8192;

/// A subscribe item's error when the session's broker has no live feed in
/// the desktop (WS-01).
pub const NO_LIVE_FEED: &str =
    "This broker has no live market data feed in OpenAlgo Desktop, so nothing can be streamed.";
/// A subscribe item's error when the session's live feed did not start.
pub const FEED_NOT_STARTED: &str = "The live market data feed did not start for this broker \
session. Log in to your broker again to resume streaming.";
/// A subscribe item's error when the feed stopped before taking it.
pub const FEED_STOPPED: &str = "The live market data feed stopped before it could take this \
subscription. Subscribe again once it is running.";

/// One desired manager reference.
pub type DesiredKey = (InstrumentKey, Mode, u8);

/// Depth levels per exchange for the connected broker.
pub type DepthCapability = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

/// Why the connected broker's feed does not stream an exchange, if it
/// does not (`Broker::feed_refusal`).
pub type ExchangeRefusal = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The references feed clients want, in the order they were first wanted,
/// and how many of them name each instrument: every tick asks whether its
/// instrument is held, under the state lock, and a scan of every reference
/// made that cost grow with the number held (3,000 instruments in several
/// modes at a busy open).
#[derive(Default)]
struct Desired {
    /// Each key with when it was wanted: a request past the feed's room is
    /// taken in its own order, first come first served, as the web's
    /// proxy subscribes its items one by one (audit C02).
    keys: HashMap<DesiredKey, u64>,
    next: u64,
    per_instrument: HashMap<InstrumentKey, usize>,
}

impl Desired {
    fn insert(&mut self, key: DesiredKey) -> bool {
        if self.keys.contains_key(&key) {
            return false;
        }
        *self.per_instrument.entry(key.0.clone()).or_insert(0) += 1;
        self.keys.insert(key, self.next);
        self.next += 1;
        true
    }

    /// `keys` in the order they were wanted.
    fn in_order(&self, keys: &mut [DesiredKey]) {
        keys.sort_by_key(|k| self.keys.get(k).copied().unwrap_or(u64::MAX));
    }

    fn remove(&mut self, key: &DesiredKey) -> bool {
        let removed = self.keys.remove(key).is_some();
        if removed {
            if let Some(n) = self.per_instrument.get_mut(&key.0) {
                *n -= 1;
                if *n == 0 {
                    self.per_instrument.remove(&key.0);
                }
            }
        }
        removed
    }

    fn clear(&mut self) {
        self.keys.clear();
        self.per_instrument.clear();
    }

    fn contains(&self, key: &DesiredKey) -> bool {
        self.keys.contains_key(key)
    }

    fn iter(&self) -> impl Iterator<Item = &DesiredKey> {
        self.keys.keys()
    }

    /// Whether any reference names this instrument.
    fn holds(&self, instrument: &InstrumentKey) -> bool {
        self.per_instrument.contains_key(instrument)
    }
}

#[derive(Default)]
struct State {
    desired: Desired,
    applied: HashMap<DesiredKey, Applied>,
    /// The merged quote per held instrument (`snapshot`): what a tick's
    /// frame and a separate depth snapshot's quote fields are built on.
    last_tick: HashMap<InstrumentKey, Snapshot>,
    /// Who waits on what the managers do, and what is retried.
    ops: Ops,
    /// Moves on `reset`: what a manager answers after it forgot every
    /// reference (broker logout) changes nothing.
    generation: u64,
    /// Deep books watched for silence, and those that fell back (`deep`).
    deep_books: DeepBooks,
}

impl State {
    fn holds(&self, key: &InstrumentKey) -> bool {
        self.desired.holds(key)
    }
}

pub struct BrokerBridge {
    manager: Arc<WebSocketManager>,
    /// The broker's separate depth socket (idle unless `deep` is set).
    depth_manager: Arc<WebSocketManager>,
    /// Depth levels served by `depth_manager`, when it runs.
    deep: Mutex<Option<u8>>,
    /// Depth levels whose socket did not start for this session: a Depth
    /// subscribe at them is served the main feed's levels (`deep`).
    deep_down: Mutex<Option<u8>>,
    symbols: SymbolResolver,
    tx: broadcast::Sender<Arc<MarketUpdate>>,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    depths: Mutex<DepthCapability>,
    /// The exchanges the connected broker's feed refuses (none by default).
    refusal: Mutex<Option<ExchangeRefusal>>,
    /// Why this session's feed cannot stream (it has none, or it did not
    /// start); cleared by `reset`.
    unavailable: Mutex<Option<&'static str>>,
    tasks: Mutex<Option<JoinSet<()>>>,
    /// What the feed server did not deliver to apps (NEW-REL-05).
    client_losses: Arc<ClientLosses>,
}

fn mode_of(m: Mode) -> FeedMode {
    match m {
        Mode::Ltp => FeedMode::Ltp,
        Mode::Quote => FeedMode::Quote,
        Mode::Depth => FeedMode::Depth,
    }
}

impl BrokerBridge {
    pub fn new(manager: Arc<WebSocketManager>, symbols: SymbolResolver) -> Arc<Self> {
        Self::with_depth_manager(manager, Arc::new(WebSocketManager::new()), symbols)
    }

    /// A bridge that can route deep books to a second manager.
    pub fn with_depth_manager(
        manager: Arc<WebSocketManager>,
        depth_manager: Arc<WebSocketManager>,
        symbols: SymbolResolver,
    ) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BRIDGE_UPDATE_CAP);
        Arc::new(Self {
            manager,
            depth_manager,
            deep: Mutex::new(None),
            deep_down: Mutex::new(None),
            symbols,
            tx,
            state: Arc::new(Mutex::new(State::default())),
            changed: Arc::new(Notify::new()),
            depths: Mutex::new(Arc::new(|_| vec![DEFAULT_DEPTH])),
            refusal: Mutex::new(None),
            unavailable: Mutex::new(None),
            tasks: Mutex::new(None),
            client_losses: Arc::new(ClientLosses::default()),
        })
    }

    /// Install the connected broker's depth capability (called on broker
    /// login; the default is 5 levels everywhere, as on the web).
    pub fn set_depth_capability(&self, f: DepthCapability) {
        *self.depths.lock() = f;
    }

    /// Install the exchanges the connected broker's feed refuses (called on
    /// broker login; cleared by `reset`).
    pub fn set_exchange_refusal(&self, f: ExchangeRefusal) {
        *self.refusal.lock() = Some(f);
    }

    /// Record that this session's feed cannot stream ([`NO_LIVE_FEED`],
    /// [`FEED_NOT_STARTED`]): subscribes are refused with it until `reset`.
    pub fn set_unavailable(&self, reason: &'static str) {
        *self.unavailable.lock() = Some(reason);
    }

    /// Route Depth subscriptions for `levels` to the depth manager (`None`:
    /// everything on the main manager). Set before `resync`.
    pub fn set_deep_levels(&self, levels: Option<u8>) {
        *self.deep.lock() = levels;
    }

    /// How long a deep book may stay silent before its instrument falls
    /// back to the main feed's levels (`None`: never; Dhan 30 s, audit
    /// P-10). Set with `set_deep_levels`.
    pub fn set_deep_book_timeout(&self, timeout: Option<std::time::Duration>) {
        self.state.lock().deep_books.reset(timeout);
    }

    /// The broker's socket for `levels`-level books did not start this
    /// session: Depth subscribes at those levels are served the main feed's
    /// levels (until `reset`), as the web's adapters fall back.
    pub fn set_deep_unavailable(&self, levels: u8) {
        *self.deep_down.lock() = Some(levels);
    }

    /// References the bridge currently holds on the main manager.
    pub fn applied(&self) -> Vec<FeedSubscription> {
        self.state
            .lock()
            .applied
            .values()
            .flat_map(Applied::main_subs)
            .collect()
    }

    /// What the bridge's subscriptions stand at now, for health.
    pub fn operations(&self) -> Operations {
        self.state.lock().operations()
    }

    /// References the bridge currently holds on the depth manager.
    pub fn applied_deep(&self) -> Vec<FeedSubscription> {
        self.state
            .lock()
            .applied
            .values()
            .filter_map(|a| match a {
                Applied::Deep { depth, .. } => Some(depth.clone()),
                Applied::Main(_) => None,
            })
            .collect()
    }

    /// Forget what was applied after the managers dropped their
    /// subscriptions (broker logout): feed clients keep their desired set,
    /// which `resync` re-applies on the next broker session. Depth routing
    /// and the depth capability go back to the defaults.
    pub fn reset(&self) {
        {
            let mut st = self.state.lock();
            st.applied.clear();
            st.last_tick.clear();
            st.generation += 1;
            st.ops.forget_retries();
            st.deep_books.reset(None);
        }
        *self.deep.lock() = None;
        *self.deep_down.lock() = None;
        *self.unavailable.lock() = None;
        *self.refusal.lock() = None;
        self.set_depth_capability(Arc::new(|_| vec![DEFAULT_DEPTH]));
    }

    /// Re-apply the desired set (a broker session started, or the symbol
    /// master was loaded).
    pub fn resync(&self) {
        self.changed.notify_one();
    }

    /// Start the reconcile and relay tasks (no-op when running).
    pub fn start(self: &Arc<Self>) {
        let mut tasks = self.tasks.lock();
        if tasks.is_some() {
            return;
        }
        let mut set = JoinSet::new();
        let me = self.clone();
        set.spawn(async move {
            loop {
                me.reconcile().await;
                let retry = {
                    let st = me.state.lock();
                    [st.ops.next_retry(), st.deep_books.next_deadline()]
                        .into_iter()
                        .flatten()
                        .min()
                };
                match retry {
                    Some(at) => tokio::select! {
                        _ = me.changed.notified() => {}
                        _ = tokio::time::sleep_until(at) => {}
                    },
                    None => me.changed.notified().await,
                }
            }
        });
        for feed in [self.manager.clone(), self.depth_manager.clone()] {
            let me = self.clone();
            let rx = feed.subscribe_ticks();
            set.spawn(async move { me.relay(rx, &feed).await });
        }
        *tasks = Some(set);
    }

    /// Stop both tasks and release every manager reference.
    pub async fn stop(&self) {
        let tasks = self.tasks.lock().take();
        if let Some(mut set) = tasks {
            set.abort_all();
            while set.join_next().await.is_some() {}
        }
        let held: Vec<Applied> = {
            let mut st = self.state.lock();
            st.desired.clear();
            st.last_tick.clear();
            st.ops.clear();
            st.deep_books.reset(None);
            st.applied.drain().map(|(_, s)| s).collect()
        };
        self.release(held).await;
    }

    async fn relay(&self, mut rx: broadcast::Receiver<MarketEvent>, from: &WebSocketManager) {
        let mut skipped: u64 = 0;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if std::ptr::eq(from, self.depth_manager.as_ref()) {
                        self.note_deep_book(&ev);
                    }
                    if let Some(u) = self.convert(&ev) {
                        let _ = self.tx.send(Arc::new(u));
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    skipped += n;
                    from.stats().note_skipped(Reader::FeedClients, n);
                    tracing::warn!(
                        "Market data relay fell behind the broker feed; skipped {} events ({} total)",
                        n,
                        skipped
                    );
                }
                Err(RecvError::Closed) => return,
            }
        }
    }

    /// The instrument's broker token from the symbol master.
    fn token(&self, key: &InstrumentKey) -> Option<Arc<str>> {
        self.symbols
            .by_symbol(&key.exchange, &key.symbol)
            .map(|row| Arc::from(row.token.as_str()))
    }

    /// The main feed's connection count: a snapshot belongs to the
    /// connection that made it (`snapshot`).
    fn connection(&self) -> u64 {
        self.manager
            .stats()
            .connects
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn convert(&self, ev: &FeedEvent) -> Option<MarketUpdate> {
        match ev {
            FeedEvent::Tick(t) => {
                let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
                let epoch = self.connection();
                let mut st = self.state.lock();
                if !st.holds(&key) {
                    return None;
                }
                let mut merged = snapshot::merge(st.last_tick.get(&key), t, epoch);
                if merged.token().is_none() {
                    merged.set_token(self.token(&key));
                }
                // A packet without a trade (OI, circuits) only updates the
                // snapshot; the next priced packet carries what it brought.
                let update = t
                    .knows(QuoteField::Ltp)
                    .then(|| tick_update(key.clone(), merged.tick()))
                    .flatten()
                    .map(|u| MarketUpdate {
                        token: merged.token(),
                        ..u
                    });
                st.last_tick.insert(key, merged);
                update
            }
            FeedEvent::Depth(d) => {
                let key = InstrumentKey::new(d.symbol.clone(), d.exchange.clone());
                let epoch = self.connection();
                let st = self.state.lock();
                if !st.holds(&key) {
                    return None;
                }
                let last = st.last_tick.get(&key).filter(|s| s.of_epoch(epoch));
                let lacks = last.map(Snapshot::book_lacks).unwrap_or_default();
                let token = last.and_then(Snapshot::token).or_else(|| self.token(&key));
                let update = depth_update(key, d, last.map(Snapshot::tick), lacks);
                Some(MarketUpdate { token, ..update })
            }
            _ => None,
        }
    }
}

impl MarketDataSource for BrokerBridge {
    /// Taken at once when the key is held already; else when the managers
    /// answer the next pass (`apply`).
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp {
        let k = (key.clone(), mode, depth);
        let taken = {
            let mut st = self.state.lock();
            st.desired.insert(k.clone());
            st.ops.wanted_again(&k);
            if st.applied.contains_key(&k) && !st.ops.releasing(&k) {
                None
            } else {
                Some(st.ops.wait_admission(k))
            }
        };
        let Some(taken) = taken else {
            return answered(Ok(()));
        };
        self.changed.notify_one();
        Box::pin(async move { taken.await.unwrap_or_else(|_| Err(FEED_STOPPED.into())) })
    }

    /// Released at once when the managers hold nothing for the key; else
    /// when the broker answers the next pass. An answer that never comes
    /// because the bridge stopped is a release: it let go of everything.
    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp {
        let k = (key.clone(), mode, depth);
        let released = {
            let mut st = self.state.lock();
            st.desired.remove(&k);
            // Once nothing names the instrument, its quote goes at once, so
            // a book subscribed again merges only a quote of its own (web
            // openalgo#2376, `forget_quote`).
            if !st.desired.holds(key) {
                st.last_tick.remove(key);
            }
            st.ops.unwanted(&k, FEED_STOPPED);
            if !st.applied.contains_key(&k) && !st.ops.releasing(&k) {
                None
            } else {
                Some(st.ops.wait_release(k))
            }
        };
        let Some(released) = released else {
            return answered(Ok(()));
        };
        self.changed.notify_one();
        Box::pin(async move { released.await.unwrap_or(Ok(())) })
    }

    fn updates(&self) -> broadcast::Receiver<Arc<MarketUpdate>> {
        self.tx.subscribe()
    }

    fn resolve(&self, key: &InstrumentKey) -> bool {
        self.symbols.by_symbol(&key.exchange, &key.symbol).is_some()
    }

    fn supported_depths(&self, exchange: &str) -> Vec<u8> {
        let f = self.depths.lock().clone();
        f(exchange)
    }

    fn refusal(&self, exchange: &str) -> Option<String> {
        let f = self.refusal.lock().clone();
        f.and_then(|f| f(exchange))
    }

    /// The broker refused the feed (it stays down until the next sign-in,
    /// which closes every authenticated feed client anyway), or this
    /// session's feed has none or did not start.
    fn unavailable(&self) -> Option<String> {
        if let Some(message) = self.manager.status().refusal() {
            return Some(message.to_string());
        }
        self.unavailable.lock().map(String::from)
    }

    fn client_losses(&self) -> Option<Arc<ClientLosses>> {
        Some(self.client_losses.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::render::tests::full_tick;
    use super::*;
    use crate::brokers::common::streaming::{NormalizedDepth, NormalizedTick};

    /// NEW-REL-05: what the relay to the apps' feed skipped because it fell
    /// behind is counted against the broker feed it reads (it was a log
    /// line only).
    #[tokio::test]
    async fn events_the_relay_skips_are_counted_against_its_feed() {
        let manager = Arc::new(WebSocketManager::new());
        let bridge = BrokerBridge::new(manager.clone(), SymbolResolver::new());
        let (tx, rx) = broadcast::channel::<MarketEvent>(2);
        for _ in 0..5 {
            let _ = tx.send(Arc::new(FeedEvent::Tick(full_tick("SBIN", "NSE"))));
        }
        drop(tx);
        bridge.relay(rx, &manager).await;
        let s = manager.stats().since_connect();
        assert!(s.skipped.contains(&(Reader::FeedClients, 3)), "{:?}", s);
    }

    /// A tick is relayed while any reference names its instrument, in any
    /// mode or depth, and not once the last one goes or the bridge stops.
    #[tokio::test]
    async fn ticks_are_relayed_while_any_reference_holds_the_instrument() {
        let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
        let sbin = InstrumentKey::new("SBIN", "NSE");
        let tcs = InstrumentKey::new("TCS", "NSE");
        let relayed = |b: &BrokerBridge, symbol: &str| {
            b.convert(&FeedEvent::Tick(full_tick(symbol, "NSE")))
                .is_some()
        };
        assert!(!relayed(&bridge, "SBIN"));
        drop(bridge.subscribe(&sbin, Mode::Ltp, 5));
        drop(bridge.subscribe(&sbin, Mode::Depth, 20));
        drop(bridge.subscribe(&tcs, Mode::Quote, 5));
        drop(bridge.subscribe(&tcs, Mode::Quote, 5));
        assert!(relayed(&bridge, "SBIN") && relayed(&bridge, "TCS"));
        drop(bridge.unsubscribe(&sbin, Mode::Depth, 20));
        assert!(relayed(&bridge, "SBIN"), "the LTP reference still holds it");
        drop(bridge.unsubscribe(&sbin, Mode::Ltp, 5));
        assert!(!relayed(&bridge, "SBIN"));
        // A reference added twice is one reference.
        drop(bridge.unsubscribe(&tcs, Mode::Quote, 5));
        assert!(!relayed(&bridge, "TCS"));
        drop(bridge.subscribe(&tcs, Mode::Ltp, 5));
        bridge.stop().await;
        assert!(!relayed(&bridge, "TCS"));
    }

    /// WS-01: a session's "cannot stream" lasts until the session ends
    /// (`reset`), and a refused broker feed reads from the manager.
    #[test]
    fn unavailable_until_the_session_is_reset() {
        let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
        assert_eq!(bridge.unavailable(), None);
        bridge.set_unavailable(NO_LIVE_FEED);
        assert_eq!(bridge.unavailable().as_deref(), Some(NO_LIVE_FEED));
        bridge.reset();
        assert_eq!(bridge.unavailable(), None);
    }

    /// A deep book from its own socket (Dhan's 20 levels here; Fyers' TBT
    /// alike, web test/test_fyers_quote_feed.py::test_tbt_depth_before_any_quote_has_no_invented_fields)
    /// that comes before the instrument's first quote tick carries its book,
    /// its totals, `ltp` and `timestamp`, and leaves every quote field out
    /// (web docs/api/websocket-streaming/depth.md at a1b9e3250); it sent
    /// them as 0. Once the tick has come, the next book carries its quote.
    #[test]
    fn a_depth_before_the_first_tick_carries_no_quote_fields() {
        let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
        let key = InstrumentKey::new("RELIANCE", "NSE");
        drop(bridge.subscribe(&key, Mode::Depth, 20));
        let level = crate::brokers::types::DepthLevel {
            price: 1167.5,
            quantity: 10,
            orders: 2,
        };
        let book = FeedEvent::Depth(NormalizedDepth {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            ltp: 0.0,
            buy: vec![level; 20],
            sell: vec![level; 20],
            total_buy_quantity: 7,
            total_sell_quantity: 8,
            timestamp_ms: 1_791_000_000_200,
            ..Default::default()
        });
        let data = |b: &BrokerBridge| {
            let u = b.convert(&book).expect("held");
            let frame = crate::feed::protocol::market_data(&u, Mode::Depth, 20, "dhan");
            serde_json::from_str::<serde_json::Value>(&frame).unwrap()["data"].clone()
        };
        let early = data(&bridge);
        let quote_keys = [
            "open",
            "high",
            "low",
            "close",
            "volume",
            "average_price",
            "last_quantity",
            "oi",
            "ltt",
        ];
        for k in quote_keys {
            assert!(early.get(k).is_none(), "{}: {}", k, early);
        }
        assert_eq!(
            (early["ltp"].as_f64(), early["timestamp"].as_i64()),
            (Some(0.0), Some(1_791_000_000_200))
        );
        assert_eq!(
            (
                early["total_buy_quantity"].as_i64(),
                early["total_sell_quantity"].as_i64()
            ),
            (Some(7), Some(8))
        );
        assert_eq!(early["depth"]["buy"].as_array().map(Vec::len), Some(20));

        bridge.convert(&FeedEvent::Tick(full_tick("RELIANCE", "NSE")));
        let later = data(&bridge);
        for k in quote_keys {
            assert!(later.get(k).is_some(), "{}: {}", k, later);
        }
        assert_eq!(
            (later["ltp"].as_f64(), later["open"].as_f64()),
            (Some(1167.7), Some(1180.1))
        );
    }

    /// Web test/test_fyers_quote_feed.py::test_tbt_resubscription_does_not_merge_the_old_quote
    /// and test_tbt_release_keeps_the_quote_a_5_level_depth_still_reads:
    /// the instrument's quote goes as soon as nothing names it, so a book
    /// subscribed again merges no old quote; while another reference holds
    /// the instrument, its quote stays.
    #[test]
    fn a_book_subscribed_again_merges_no_old_quote() {
        let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
        let key = InstrumentKey::new("SBIN", "NSE");
        let book = FeedEvent::Depth(NormalizedDepth {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            buy: vec![Default::default(); 50],
            sell: vec![Default::default(); 50],
            ..Default::default()
        });
        let quoted = |b: &BrokerBridge| {
            b.convert(&book)
                .and_then(|u| u.quote)
                .is_some_and(|q| q.open.is_some())
        };

        drop(bridge.subscribe(&key, Mode::Depth, 50));
        bridge.convert(&FeedEvent::Tick(full_tick("SBIN", "NSE")));
        assert!(quoted(&bridge));
        drop(bridge.unsubscribe(&key, Mode::Depth, 50));
        drop(bridge.subscribe(&key, Mode::Depth, 50));
        assert!(!quoted(&bridge), "the old quote is not merged");

        drop(bridge.subscribe(&key, Mode::Depth, 5));
        bridge.convert(&FeedEvent::Tick(full_tick("SBIN", "NSE")));
        drop(bridge.unsubscribe(&key, Mode::Depth, 50));
        assert!(quoted(&bridge), "the 5-level book still reads the quote");
    }

    /// Audit regression matrix, "Reconnect with first packet partial": once
    /// the broker feed has connected again, a book is framed with no quote
    /// until the new connection's first tick, and a partial first tick does
    /// not bring the old connection's quote back.
    #[test]
    fn a_new_broker_connection_starts_every_quote_afresh() {
        let manager = Arc::new(WebSocketManager::new());
        let bridge = BrokerBridge::new(manager.clone(), SymbolResolver::new());
        let key = InstrumentKey::new("SBIN", "NSE");
        drop(bridge.subscribe(&key, Mode::Depth, 5));
        bridge.convert(&FeedEvent::Tick(full_tick("SBIN", "NSE")));
        let book = FeedEvent::Depth(NormalizedDepth {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            buy: vec![Default::default(); 5],
            sell: vec![Default::default(); 5],
            ..Default::default()
        });
        let open = |b: &BrokerBridge| b.convert(&book).and_then(|u| u.quote).and_then(|q| q.open);
        assert_eq!(open(&bridge), Some(1180.1));
        manager
            .stats()
            .connects
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(open(&bridge), None, "the old connection's quote");
        let mut partial = full_tick("SBIN", "NSE");
        partial.mark_absent(QuoteField::Open);
        let u = bridge.convert(&FeedEvent::Tick(partial)).expect("a price");
        assert_eq!(u.quote.and_then(|q| q.open), None);
    }

    /// C03: a packet without a trade is not published (no LTP of 0 reaches a
    /// client) and what it carried reaches the instrument's next frame.
    #[test]
    fn a_packet_without_a_trade_waits_for_the_price() {
        let bridge = BrokerBridge::new(Arc::new(WebSocketManager::new()), SymbolResolver::new());
        let key = InstrumentKey::new("NIFTY28NOV26FUT", "NFO");
        drop(bridge.subscribe(&key, Mode::Quote, 5));
        let mut oi = NormalizedTick {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            mode: 1,
            oi: 777,
            ..Default::default()
        };
        oi.mark_absent(QuoteField::Ltp);
        oi.mark_carried(QuoteField::OpenInterest);
        assert!(bridge.convert(&FeedEvent::Tick(oi.clone())).is_none());
        let mut quote = full_tick("NIFTY28NOV26FUT", "NFO");
        quote.mode = 2;
        let u = bridge
            .convert(&FeedEvent::Tick(quote.clone()))
            .expect("a price");
        assert_eq!(u.quote.and_then(|q| q.oi), Some(777));
        oi.oi = 778;
        assert!(
            bridge.convert(&FeedEvent::Tick(oi)).is_none(),
            "an OI packet after a price is not published either"
        );
        let u = bridge.convert(&FeedEvent::Tick(quote)).expect("a price");
        assert_eq!(u.quote.and_then(|q| q.oi), Some(778));
    }
}
