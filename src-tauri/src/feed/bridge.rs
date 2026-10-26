//! The production [`MarketDataSource`]: the feed server on top of the
//! broker-agnostic outbound manager (`crate::websocket::WebSocketManager`).
//!
//! * Symbols resolve against the shared symbol master (`SymbolResolver`), so
//!   an unknown symbol gets the web's "Token not found" refusal and a known
//!   one becomes a `FeedSubscription` with the broker token and exchange.
//! * `subscribe` / `unsubscribe` only edit the desired set and wake the
//!   reconcile task, which diffs desired against applied and calls
//!   `WebSocketManager::subscribe` / `unsubscribe`. The feed server already
//!   reference counts across its clients, so the bridge holds exactly one
//!   manager reference per `(instrument, mode, depth)`; the manager merges
//!   modes per instrument for the broker.
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
    ClientLosses, DepthBook, DepthLevel, InstrumentKey, MarketDataSource, MarketUpdate, Mode,
    QuoteFields, DEFAULT_DEPTH,
};
use crate::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, MarketEvent, NormalizedDepth, NormalizedTick,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::websocket::{FeedStatus, Reader, WebSocketManager};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;

/// Capacity of the update channel between the bridge and feed servers.
pub const BRIDGE_UPDATE_CAP: usize = 8192;

/// A subscribe item's error when the session's broker has no live feed in
/// the desktop (WS-01).
pub const NO_LIVE_FEED: &str =
    "This broker has no live market data feed in OpenAlgo Desktop, so nothing can be streamed.";
/// A subscribe item's error when the session's live feed did not start.
pub const FEED_NOT_STARTED: &str = "The live market data feed did not start for this broker \
session. Log in to your broker again to resume streaming.";

/// One desired manager reference.
pub type DesiredKey = (InstrumentKey, Mode, u8);

/// Depth levels per exchange for the connected broker.
pub type DepthCapability = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

/// What one desired key holds on the managers.
#[derive(Debug, Clone)]
enum Applied {
    Main(FeedSubscription),
    /// Depth socket book plus the main feed's quote for the same instrument.
    Deep {
        depth: FeedSubscription,
        quote: FeedSubscription,
    },
}

impl Applied {
    fn main_subs(&self) -> Vec<FeedSubscription> {
        match self {
            Applied::Main(s) => vec![s.clone()],
            Applied::Deep { quote, .. } => vec![quote.clone()],
        }
    }
}

/// The references feed clients want, and how many of them name each
/// instrument: every tick asks whether its instrument is held, under the
/// state lock, and a scan of every reference made that cost grow with the
/// number held (3,000 instruments in several modes at a busy open).
#[derive(Default)]
struct Desired {
    keys: HashSet<DesiredKey>,
    per_instrument: HashMap<InstrumentKey, usize>,
}

impl Desired {
    fn insert(&mut self, key: DesiredKey) -> bool {
        let instrument = key.0.clone();
        let added = self.keys.insert(key);
        if added {
            *self.per_instrument.entry(instrument).or_insert(0) += 1;
        }
        added
    }

    fn remove(&mut self, key: &DesiredKey) -> bool {
        let removed = self.keys.remove(key);
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
        self.keys.contains(key)
    }

    fn iter(&self) -> impl Iterator<Item = &DesiredKey> {
        self.keys.iter()
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
    /// Last tick per held instrument, for the quote fields of a separate
    /// depth snapshot.
    last_tick: HashMap<InstrumentKey, NormalizedTick>,
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
    symbols: SymbolResolver,
    tx: broadcast::Sender<Arc<MarketUpdate>>,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    depths: Mutex<DepthCapability>,
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
            symbols,
            tx,
            state: Arc::new(Mutex::new(State::default())),
            changed: Arc::new(Notify::new()),
            depths: Mutex::new(Arc::new(|_| vec![DEFAULT_DEPTH])),
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

    /// References the bridge currently holds on the main manager.
    pub fn applied(&self) -> Vec<FeedSubscription> {
        self.state
            .lock()
            .applied
            .values()
            .flat_map(Applied::main_subs)
            .collect()
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
        }
        *self.deep.lock() = None;
        *self.unavailable.lock() = None;
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
                me.changed.notified().await;
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
            st.applied.drain().map(|(_, s)| s).collect()
        };
        self.release(held).await;
    }

    /// Drop manager references held for `applied` entries.
    async fn release(&self, applied: Vec<Applied>) {
        let mut main = Vec::new();
        let mut deep = Vec::new();
        for a in applied {
            match a {
                Applied::Main(s) => main.push(s),
                Applied::Deep { depth, quote } => {
                    deep.push(depth);
                    main.push(quote);
                }
            }
        }
        if !main.is_empty() {
            if let Err(e) = self.manager.unsubscribe(main).await {
                tracing::warn!("Could not release market data subscriptions: {}", e);
            }
        }
        if !deep.is_empty() {
            if let Err(e) = self.depth_manager.unsubscribe(deep).await {
                tracing::warn!("Could not release market depth subscriptions: {}", e);
            }
        }
    }

    /// How a desired key is applied (deep route when its levels match).
    fn applied_for(&self, key: &DesiredKey) -> Option<Applied> {
        let sub = self.subscription(key)?;
        let deep = *self.deep.lock();
        Some(match deep {
            Some(levels) if key.1 == Mode::Depth && key.2 == levels => Applied::Deep {
                quote: FeedSubscription {
                    mode: FeedMode::Quote,
                    depth: DEFAULT_DEPTH,
                    ..sub.clone()
                },
                depth: sub,
            },
            _ => Applied::Main(sub),
        })
    }

    fn subscription(&self, key: &DesiredKey) -> Option<FeedSubscription> {
        let (k, mode, depth) = key;
        let row = self.symbols.by_symbol(&k.exchange, &k.symbol)?;
        Some(FeedSubscription {
            symbol: k.symbol.clone(),
            exchange: k.exchange.clone(),
            token: row.token.clone(),
            brsymbol: row.br_symbol().to_string(),
            brexchange: row.br_exchange().to_string(),
            mode: mode_of(*mode),
            depth: *depth,
        })
    }

    /// Bring the manager in line with the desired set.
    async fn reconcile(&self) {
        let (add, remove) = {
            let mut st = self.state.lock();
            let add: Vec<DesiredKey> = st
                .desired
                .iter()
                .filter(|k| !st.applied.contains_key(*k))
                .cloned()
                .collect();
            let gone: Vec<DesiredKey> = st
                .applied
                .keys()
                .filter(|k| !st.desired.contains(k))
                .cloned()
                .collect();
            let remove: Vec<Applied> = gone.iter().filter_map(|k| st.applied.remove(k)).collect();
            let State {
                desired, last_tick, ..
            } = &mut *st;
            last_tick.retain(|k, _| desired.holds(k));
            (add, remove)
        };
        self.release(remove).await;
        let mut subs = Vec::with_capacity(add.len());
        let mut keys = Vec::with_capacity(add.len());
        for k in add {
            match self.applied_for(&k) {
                Some(s) => {
                    subs.push(s);
                    keys.push(k);
                }
                None => tracing::warn!(
                    "{} on {} is not in the symbol master; not streaming it",
                    k.0.symbol,
                    k.0.exchange
                ),
            }
        }
        if subs.is_empty() {
            return;
        }
        let mut main = Vec::new();
        let mut deep = Vec::new();
        for a in &subs {
            match a {
                Applied::Main(s) => main.push(s.clone()),
                Applied::Deep { depth, quote } => {
                    deep.push(depth.clone());
                    main.push(quote.clone());
                }
            }
        }
        let mut result = self.manager.subscribe(main.clone()).await;
        if result.is_ok() && !deep.is_empty() {
            result = self.depth_manager.subscribe(deep).await;
            if result.is_err() {
                // Do not keep the quote references of a refused deep book.
                let _ = self.manager.unsubscribe(main).await;
            }
        }
        match result {
            Ok(()) => {
                let mut st = self.state.lock();
                for (k, s) in keys.into_iter().zip(subs) {
                    st.applied.insert(k, s);
                }
                // A key dropped meanwhile is released on the next pass.
                drop(st);
                self.changed.notify_one();
            }
            Err(e) => tracing::warn!("Market data subscribe failed: {}", e),
        }
    }

    async fn relay(&self, mut rx: broadcast::Receiver<MarketEvent>, from: &WebSocketManager) {
        let mut skipped: u64 = 0;
        loop {
            match rx.recv().await {
                Ok(ev) => {
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

    fn convert(&self, ev: &FeedEvent) -> Option<MarketUpdate> {
        match ev {
            FeedEvent::Tick(t) => {
                let key = InstrumentKey::new(t.symbol.clone(), t.exchange.clone());
                {
                    let mut st = self.state.lock();
                    if !st.holds(&key) {
                        return None;
                    }
                    st.last_tick.insert(key.clone(), t.clone());
                }
                Some(tick_update(key, t))
            }
            FeedEvent::Depth(d) => {
                let key = InstrumentKey::new(d.symbol.clone(), d.exchange.clone());
                let last = {
                    let st = self.state.lock();
                    if !st.holds(&key) {
                        return None;
                    }
                    st.last_tick.get(&key).cloned()
                };
                Some(depth_update(key, d, last.as_ref()))
            }
            _ => None,
        }
    }
}

fn quote_fields(key: &InstrumentKey, t: &NormalizedTick, with_oi: bool) -> QuoteFields {
    let index = key.is_index();
    let opt = |v: f64| if index && v == 0.0 { None } else { Some(v) };
    QuoteFields {
        volume: t.volume,
        last_quantity: t.last_quantity,
        average_price: t.average_price,
        total_buy_quantity: t.total_buy_quantity,
        total_sell_quantity: t.total_sell_quantity,
        open: opt(t.open),
        high: opt(t.high),
        low: opt(t.low),
        close: opt(t.close),
        oi: with_oi.then_some(t.oi),
        price_change: index.then_some(t.change),
        price_change_percent: index.then_some(t.change_percent),
    }
}

fn ltt(t: &NormalizedTick) -> Option<i64> {
    Some(if t.last_trade_time_ms > 0 {
        t.last_trade_time_ms
    } else {
        t.timestamp_ms
    })
}

/// A tick. A non-index full tick is served at Quote: its book follows as a
/// separate `Depth` event.
fn tick_update(key: InstrumentKey, t: &NormalizedTick) -> MarketUpdate {
    let raw = Mode::from_u8(t.mode).unwrap_or(Mode::Ltp);
    let mode = if raw == Mode::Depth && !key.is_index() {
        Mode::Quote
    } else {
        raw
    };
    MarketUpdate {
        quote: (mode >= Mode::Quote).then(|| quote_fields(&key, t, raw == Mode::Depth)),
        key,
        mode,
        ltp: t.ltp,
        ltt: ltt(t),
        timestamp: t.timestamp_ms,
        depth: None,
        exact_mode: false,
    }
}

/// A depth snapshot for Depth holders, with the quote fields of the latest
/// tick for the instrument.
fn depth_update(
    key: InstrumentKey,
    d: &NormalizedDepth,
    last: Option<&NormalizedTick>,
) -> MarketUpdate {
    let base = last.cloned().unwrap_or_default();
    let mut q = quote_fields(&key, &base, true);
    q.total_buy_quantity = d.total_buy_quantity;
    q.total_sell_quantity = d.total_sell_quantity;
    let side = |v: &[crate::brokers::types::DepthLevel]| -> Vec<DepthLevel> {
        v.iter()
            .map(|l| DepthLevel {
                price: l.price,
                quantity: l.quantity,
                orders: l.orders,
            })
            .collect()
    };
    MarketUpdate {
        key,
        mode: Mode::Depth,
        ltp: if d.ltp > 0.0 { d.ltp } else { base.ltp },
        ltt: last.and_then(ltt).or(Some(d.timestamp_ms)),
        timestamp: d.timestamp_ms,
        quote: Some(q),
        depth: Some(DepthBook {
            buy: side(&d.buy),
            sell: side(&d.sell),
        }),
        exact_mode: true,
    }
}

impl MarketDataSource for BrokerBridge {
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        if self.state.lock().desired.insert((key.clone(), mode, depth)) {
            self.changed.notify_one();
        }
    }

    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        if self
            .state
            .lock()
            .desired
            .remove(&(key.clone(), mode, depth))
        {
            self.changed.notify_one();
        }
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

    /// The broker refused the feed (it stays down until the next sign-in,
    /// which closes every authenticated feed client anyway), or this
    /// session's feed has none or did not start.
    fn unavailable(&self) -> Option<String> {
        if let FeedStatus::AuthFailed { message, .. } = self.manager.status() {
            return Some(message);
        }
        self.unavailable.lock().map(String::from)
    }

    fn client_losses(&self) -> Option<Arc<ClientLosses>> {
        Some(self.client_losses.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_tick(symbol: &str, exchange: &str) -> NormalizedTick {
        NormalizedTick {
            symbol: symbol.into(),
            exchange: exchange.into(),
            mode: 3,
            ltp: 1167.7,
            open: 1180.1,
            close: 1187.0,
            volume: 10,
            oi: 0,
            last_trade_time_ms: 1_791_000_000_000,
            timestamp_ms: 1_791_000_000_100,
            ..Default::default()
        }
    }

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
        bridge.subscribe(&sbin, Mode::Ltp, 5);
        bridge.subscribe(&sbin, Mode::Depth, 20);
        bridge.subscribe(&tcs, Mode::Quote, 5);
        bridge.subscribe(&tcs, Mode::Quote, 5);
        assert!(relayed(&bridge, "SBIN") && relayed(&bridge, "TCS"));
        bridge.unsubscribe(&sbin, Mode::Depth, 20);
        assert!(relayed(&bridge, "SBIN"), "the LTP reference still holds it");
        bridge.unsubscribe(&sbin, Mode::Ltp, 5);
        assert!(!relayed(&bridge, "SBIN"));
        // A reference added twice is one reference.
        bridge.unsubscribe(&tcs, Mode::Quote, 5);
        assert!(!relayed(&bridge, "TCS"));
        bridge.subscribe(&tcs, Mode::Ltp, 5);
        bridge.stop().await;
        assert!(!relayed(&bridge, "TCS"));
    }

    #[test]
    fn full_equity_tick_serves_quote_and_depth_serves_depth_holders() {
        let k = InstrumentKey::new("RELIANCE", "NSE");
        let t = full_tick("RELIANCE", "NSE");
        let u = tick_update(k.clone(), &t);
        assert_eq!(u.mode, Mode::Quote);
        assert!(!u.exact_mode);
        assert_eq!(u.quote.as_ref().and_then(|q| q.oi), Some(0));
        let d = NormalizedDepth {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            ltp: 1167.7,
            buy: vec![Default::default(); 5],
            sell: vec![Default::default(); 5],
            total_buy_quantity: 7,
            total_sell_quantity: 8,
            timestamp_ms: 1_791_000_000_200,
        };
        let u = depth_update(k, &d, Some(&t));
        assert_eq!(u.mode, Mode::Depth);
        assert!(u.exact_mode);
        let q = u.quote.unwrap();
        assert_eq!((q.total_buy_quantity, q.volume), (7, 10));
        assert_eq!(u.ltt, Some(1_791_000_000_000));
        assert_eq!(u.depth.unwrap().buy.len(), 5);
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

    #[test]
    fn index_full_tick_reaches_every_mode_with_change_fields() {
        let k = InstrumentKey::new("NIFTY", "NSE_INDEX");
        let mut t = full_tick("NIFTY", "NSE_INDEX");
        t.open = 0.0;
        t.change = 1.5;
        let u = tick_update(k, &t);
        assert_eq!(u.mode, Mode::Depth);
        let q = u.quote.unwrap();
        assert_eq!(q.open, None);
        assert_eq!(q.price_change, Some(1.5));
    }
}
