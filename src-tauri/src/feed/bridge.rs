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
    DepthBook, DepthLevel, InstrumentKey, MarketDataSource, MarketUpdate, Mode, QuoteFields,
    DEFAULT_DEPTH,
};
use crate::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, MarketEvent, NormalizedDepth, NormalizedTick,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::websocket::WebSocketManager;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;

/// Capacity of the update channel between the bridge and feed servers.
pub const BRIDGE_UPDATE_CAP: usize = 8192;

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

#[derive(Default)]
struct State {
    desired: HashSet<DesiredKey>,
    applied: HashMap<DesiredKey, Applied>,
    /// Last tick per held instrument, for the quote fields of a separate
    /// depth snapshot.
    last_tick: HashMap<InstrumentKey, NormalizedTick>,
}

impl State {
    fn holds(&self, key: &InstrumentKey) -> bool {
        self.desired.iter().any(|(k, _, _)| k == key)
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
    tasks: Mutex<Option<JoinSet<()>>>,
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
            tasks: Mutex::new(None),
        })
    }

    /// Install the connected broker's depth capability (called on broker
    /// login; the default is 5 levels everywhere, as on the web).
    pub fn set_depth_capability(&self, f: DepthCapability) {
        *self.depths.lock() = f;
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
        let me = self.clone();
        let rx = self.manager.subscribe_ticks();
        set.spawn(async move { me.relay(rx).await });
        let me = self.clone();
        let rx = self.depth_manager.subscribe_ticks();
        set.spawn(async move { me.relay(rx).await });
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
                .filter(|k| !st.desired.contains(*k))
                .cloned()
                .collect();
            let remove: Vec<Applied> = gone.iter().filter_map(|k| st.applied.remove(k)).collect();
            let held: HashSet<InstrumentKey> =
                st.desired.iter().map(|(k, _, _)| k.clone()).collect();
            st.last_tick.retain(|k, _| held.contains(k));
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

    async fn relay(&self, mut rx: broadcast::Receiver<MarketEvent>) {
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
