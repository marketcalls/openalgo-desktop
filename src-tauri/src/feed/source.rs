//! The boundary the feed server consumes: a market-data source that can be
//! told to subscribe and unsubscribe, and that broadcasts normalized updates.
//!
//! The server owns per-client bookkeeping and reference counting. A source
//! sees exactly one `subscribe` per `(instrument, mode, depth)` while at least
//! one client holds it, and exactly one matching `unsubscribe` when the last
//! client lets go (explicitly or by disconnecting).
//!
//! Production wires the broker streaming manager through `feed::bridge`;
//! tests use [`FakeSource`].

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Subscription mode, numbered as on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Mode {
    Ltp = 1,
    Quote = 2,
    Depth = 3,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Ltp, Mode::Quote, Mode::Depth];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(n: u8) -> Option<Mode> {
        match n {
            1 => Some(Mode::Ltp),
            2 => Some(Mode::Quote),
            3 => Some(Mode::Depth),
            _ => None,
        }
    }

    /// Canonical label used in acks: `LTP`, `Quote`, `Depth`.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Ltp => "LTP",
            Mode::Quote => "Quote",
            Mode::Depth => "Depth",
        }
    }

    /// The label carried inside `market_data.data.mode`, as the recorded
    /// web transcripts carry it (the Zerodha adapter's Kite mode names):
    /// Depth frames say `full` (docs-7).
    pub fn data_label(self) -> &'static str {
        match self {
            Mode::Ltp => "ltp",
            Mode::Quote => "quote",
            Mode::Depth => "full",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize - 1
    }
}

/// Order-book depth levels a client may request.
pub const DEPTH_LEVELS: [u8; 4] = [5, 20, 30, 50];
/// Depth used when the client does not ask for one.
pub const DEFAULT_DEPTH: u8 = 5;

/// An instrument in OpenAlgo symbology (`RELIANCE` on `NSE`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrumentKey {
    pub symbol: String,
    pub exchange: String,
}

impl InstrumentKey {
    pub fn new(symbol: impl Into<String>, exchange: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            exchange: exchange.into(),
        }
    }

    /// Index instruments (`NSE_INDEX`, `BSE_INDEX`, ...) carry
    /// `price_change` / `price_change_percent` instead of the order-flow
    /// fields in quote and depth frames.
    pub fn is_index(&self) -> bool {
        self.exchange.ends_with("_INDEX")
    }
}

/// One price level of the order book.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i64,
    pub orders: i64,
}

/// Bid and ask ladders, best price first. The server truncates each side to
/// the depth the client asked for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DepthBook {
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
}

/// Quote-mode fields. OHLC is optional because an index outside market
/// hours has none; for other instruments a missing value is sent as `0.0`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuoteFields {
    pub volume: i64,
    pub last_quantity: i64,
    pub average_price: f64,
    pub total_buy_quantity: i64,
    pub total_sell_quantity: i64,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    pub close: Option<f64>,
    /// Open interest; sent as both `oi` and `open_interest` when present.
    pub oi: Option<i64>,
    /// Change since previous close (index instruments).
    pub price_change: Option<f64>,
    pub price_change_percent: Option<f64>,
}

/// A normalized update from the broker feed.
///
/// `mode` is the richest mode this update fills: an `Ltp` update reaches
/// only LTP subscribers, a `Quote` update reaches Quote and LTP subscribers,
/// a `Depth` update reaches all three, each frame shaped for its mode.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketUpdate {
    pub key: InstrumentKey,
    pub mode: Mode,
    pub ltp: f64,
    /// Last trade time, epoch milliseconds.
    pub ltt: Option<i64>,
    /// Exchange or receive time, epoch milliseconds.
    pub timestamp: i64,
    pub quote: Option<QuoteFields>,
    pub depth: Option<DepthBook>,
    /// Deliver only to holders of exactly `mode` (a broker that sends the
    /// depth snapshot apart from its tick has already served the lower
    /// modes with the tick).
    pub exact_mode: bool,
}

impl MarketUpdate {
    /// An LTP-only update.
    pub fn ltp(key: InstrumentKey, ltp: f64, ts_ms: i64) -> Self {
        Self {
            key,
            mode: Mode::Ltp,
            ltp,
            ltt: Some(ts_ms),
            timestamp: ts_ms,
            quote: None,
            depth: None,
            exact_mode: false,
        }
    }
}

/// What the feed server needs from a market-data source.
///
/// `subscribe` / `unsubscribe` must not block: they are called while the
/// server holds its subscription lock, so an implementation hands the
/// request to its own task. There is no reply: a subscribe acknowledged
/// with success means "accepted into the desired set", applied to the
/// broker now or as soon as its feed (re)connects. The one failure known
/// up front is [`unavailable`](MarketDataSource::unavailable).
pub trait MarketDataSource: Send + Sync + 'static {
    /// Start streaming `key` at `mode`. `depth` is the order-book depth for
    /// `Mode::Depth` and [`DEFAULT_DEPTH`] otherwise.
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8);

    /// Stop streaming a key previously passed to `subscribe` with the same
    /// arguments.
    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8);

    /// A receiver of every normalized update (ticks and depth snapshots).
    fn updates(&self) -> broadcast::Receiver<Arc<MarketUpdate>>;

    /// Whether the symbol exists in the loaded master contract. `false`
    /// becomes the per-symbol "Token not found for X on Y" refusal.
    fn resolve(&self, key: &InstrumentKey) -> bool;

    /// Depth levels the connected broker streams for `exchange`.
    fn supported_depths(&self, _exchange: &str) -> Vec<u8> {
        vec![DEFAULT_DEPTH]
    }

    /// Why nothing subscribed now would stream, when that is known (WS-01):
    /// the broker refused the live feed, or it has none or it did not
    /// start for this session. Each subscribe item is then answered with
    /// this as its error, as the web answers when its adapter is not
    /// running. `None` while the feed streams or may yet (connecting,
    /// reconnecting).
    fn unavailable(&self) -> Option<String> {
        None
    }

    /// Where the feed server counts what it could not deliver to its
    /// clients (NEW-REL-05); `None` when nobody reads the counts.
    fn client_losses(&self) -> Option<Arc<ClientLosses>> {
        None
    }
}

/// What the feed server did not deliver to its clients since it started
/// (NEW-REL-05): market frames a slow client's queue folded into a newer
/// one for the same instrument and mode (that client gets the latest price,
/// by design), frames its queue dropped past its cap, and updates the
/// server's own loop skipped when it fell behind the source.
#[derive(Debug, Default)]
pub struct ClientLosses {
    pub coalesced: AtomicU64,
    pub dropped: AtomicU64,
    pub lagged: AtomicU64,
}

/// A call the server made on a source (recorded by [`FakeSource`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceCall {
    Subscribe(InstrumentKey, Mode, u8),
    Unsubscribe(InstrumentKey, Mode, u8),
}

type ActiveKey = (InstrumentKey, Mode, u8);

/// In-memory source for tests and the soak harness. Counts active
/// subscriptions and flags a double subscribe or an unsubscribe of
/// something not subscribed.
pub struct FakeSource {
    tx: broadcast::Sender<Arc<MarketUpdate>>,
    known: Option<HashSet<InstrumentKey>>,
    depths: Mutex<Vec<u8>>,
    active: Mutex<HashMap<ActiveKey, usize>>,
    calls: Mutex<Vec<SourceCall>>,
    violations: Mutex<Vec<String>>,
    record_calls: bool,
    client_losses: Arc<ClientLosses>,
}

impl FakeSource {
    /// A source that knows only `known` instruments and streams depth 5, 20,
    /// 30 and 50.
    pub fn new(known: impl IntoIterator<Item = InstrumentKey>) -> Arc<Self> {
        Self::build(Some(known.into_iter().collect()), true)
    }

    /// A source that resolves every symbol (load and soak tests). Calls are
    /// counted but not recorded, so its own memory stays flat.
    pub fn permissive() -> Arc<Self> {
        Self::build(None, false)
    }

    fn build(known: Option<HashSet<InstrumentKey>>, record_calls: bool) -> Arc<Self> {
        let (tx, _) = broadcast::channel(4096);
        Arc::new(Self {
            tx,
            known,
            depths: Mutex::new(DEPTH_LEVELS.to_vec()),
            active: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            violations: Mutex::new(Vec::new()),
            record_calls,
            client_losses: Arc::new(ClientLosses::default()),
        })
    }

    /// Broadcast an update; returns the number of receivers.
    pub fn publish(&self, update: MarketUpdate) -> usize {
        self.tx.send(Arc::new(update)).unwrap_or(0)
    }

    /// Keys currently subscribed at the source, sorted.
    pub fn active(&self) -> Vec<ActiveKey> {
        let mut v: Vec<_> = self.active.lock().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn active_count(&self) -> usize {
        self.active.lock().len()
    }

    pub fn calls(&self) -> Vec<SourceCall> {
        self.calls.lock().clone()
    }

    /// Double subscribes and unknown unsubscribes seen so far.
    pub fn violations(&self) -> Vec<String> {
        self.violations.lock().clone()
    }
}

impl MarketDataSource for FakeSource {
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        let mut active = self.active.lock();
        let n = active.entry((key.clone(), mode, depth)).or_insert(0);
        *n += 1;
        if *n > 1 {
            self.violations.lock().push(format!(
                "double subscribe {}:{} {:?} {}",
                key.exchange, key.symbol, mode, depth
            ));
        }
        if self.record_calls {
            self.calls
                .lock()
                .push(SourceCall::Subscribe(key.clone(), mode, depth));
        }
    }

    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        let mut active = self.active.lock();
        let k = (key.clone(), mode, depth);
        match active.get_mut(&k) {
            Some(n) if *n > 1 => *n -= 1,
            Some(_) => {
                active.remove(&k);
            }
            None => self.violations.lock().push(format!(
                "unsubscribe without subscribe {}:{} {:?} {}",
                key.exchange, key.symbol, mode, depth
            )),
        }
        if self.record_calls {
            self.calls
                .lock()
                .push(SourceCall::Unsubscribe(key.clone(), mode, depth));
        }
    }

    fn updates(&self) -> broadcast::Receiver<Arc<MarketUpdate>> {
        self.tx.subscribe()
    }

    fn resolve(&self, key: &InstrumentKey) -> bool {
        match &self.known {
            Some(set) => set.contains(key),
            None => true,
        }
    }

    fn supported_depths(&self, _exchange: &str) -> Vec<u8> {
        self.depths.lock().clone()
    }

    fn client_losses(&self) -> Option<Arc<ClientLosses>> {
        Some(self.client_losses.clone())
    }
}
