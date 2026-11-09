//! The boundary the feed server consumes: a market-data source that can be
//! told to subscribe and unsubscribe, and that broadcasts normalized updates.
//!
//! The server owns per-client bookkeeping and reference counting. A source
//! sees one `subscribe` per `(instrument, mode, depth)` while at least one
//! client holds it, and one matching `unsubscribe` when the last client lets
//! go (explicitly or by disconnecting). Both answer ([`SourceOp`]): the
//! server acknowledges a subscribe only once the source took the key, and
//! an unsubscribe only once the broker let go of it (audit C01, C02).
//!
//! Production wires the broker streaming manager through `feed::bridge`;
//! tests use [`FakeSource`].

use crate::brokers::common::streaming::{BrokerFields, NumberForms, QuoteExtras};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// A source's answer to a subscription change: `Ok` once it took the key
/// (subscribe) or the broker let go of it (unsubscribe), else why not, for
/// the trader.
pub type SourceOutcome = Result<(), String>;

/// The answer to come, awaited after the server's lock is released.
pub type SourceOp = Pin<Box<dyn Future<Output = SourceOutcome> + Send>>;

/// An answer known at once.
pub fn answered(outcome: SourceOutcome) -> SourceOp {
    Box::pin(std::future::ready(outcome))
}

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

/// One price level of the order book. `orders` is `None` where the
/// broker's book has no order count (Upstox), and the frame then leaves it
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i64,
    pub orders: Option<i64>,
}

/// Bid and ask ladders, best price first. The server truncates each side to
/// the depth the client asked for.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DepthBook {
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
}

/// The quote fields an update knows, each `None` when the broker's stream
/// has not carried it: a frame leaves out what is `None` (web openalgo#2376)
/// and sends a known zero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QuoteFields {
    pub volume: Option<i64>,
    pub last_quantity: Option<i64>,
    pub average_price: Option<f64>,
    pub total_buy_quantity: Option<i64>,
    pub total_sell_quantity: Option<i64>,
    pub open: Option<f64>,
    pub high: Option<f64>,
    pub low: Option<f64>,
    /// Previous close.
    pub close: Option<f64>,
    /// Open interest; sent as both `oi` and `open_interest`.
    pub oi: Option<i64>,
    /// Change since the previous close.
    pub price_change: Option<f64>,
    pub price_change_percent: Option<f64>,
    /// The exchange's trade time the broker sent, epoch ms (`ltt` falls back
    /// to the receive time; this does not).
    pub trade_time: Option<i64>,
    /// The optional shared fields (circuits, 52-week range, best bid and
    /// ask, ...), sent under the broker's web names only.
    pub extras: QuoteExtras,
    /// The broker-only fields of the packet.
    pub broker_fields: BrokerFields,
    /// The number forms the packet marked (`NormalizedTick::numbers`).
    pub numbers: NumberForms,
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
    /// The broker's instrument token, from the symbol master (the web's
    /// Arrow, HDFC, Rupeezy, Upstox and Fyers adapters send it as `token`).
    pub token: Option<Arc<str>>,
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
            token: None,
        }
    }
}

/// What the feed server needs from a market-data source.
///
/// `subscribe` / `unsubscribe` must not block: they are called while the
/// server holds its subscription lock, so an implementation hands the
/// request to its own task and returns its answer to come, which the
/// server awaits after letting go of the lock.
pub trait MarketDataSource: Send + Sync + 'static {
    /// Start streaming `key` at `mode`. `depth` is the order-book depth for
    /// `Mode::Depth` and [`DEFAULT_DEPTH`] otherwise. The answer says
    /// whether the source took it (room on the broker connection, its
    /// depth socket up), not that a tick has come: applied to the broker
    /// now, or as soon as its feed (re)connects.
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp;

    /// Stop streaming a key previously passed to `subscribe` with the same
    /// arguments. The answer says whether the broker let go; until it has,
    /// the server keeps its last owner (web `_unsubscribe_owned_subscription`).
    /// A disconnect drops the answer, and the source keeps letting go on
    /// its own.
    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp;

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

    /// Why the connected broker's feed does not stream `exchange`, when it
    /// does not (`Broker::feed_refusal`): each subscribe item on it is
    /// answered with this as its error, before the symbol is looked up, as
    /// the web's adapter refuses it.
    fn refusal(&self, _exchange: &str) -> Option<String> {
        None
    }

    /// Where the feed server counts what it could not deliver to its
    /// clients (NEW-REL-05); `None` when nobody reads the counts.
    fn client_losses(&self) -> Option<Arc<ClientLosses>> {
        None
    }

    /// A feed client signed in: a broker feed ended for a reason the
    /// trader can clear (too many connections on one login) connects
    /// again (audit P-12; `WebSocketManager::resume`).
    fn resume(&self) {}
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

/// What a [`FakeSource`] answers instead of success (tests script it).
#[derive(Default)]
struct Script {
    /// Subscribes of these instruments are refused with this reason.
    refused: HashMap<InstrumentKey, String>,
    /// The next unsubscribes of a key: `Some(why)` refuses it, `None`
    /// never answers. Either way the key stays active.
    releases: HashMap<(InstrumentKey, Mode), VecDeque<Option<String>>>,
    /// Most instruments active at once (the broker connection's room).
    capacity: Option<usize>,
    /// Keys whose last unsubscribe was not done (an owner joining again
    /// re-subscribes them).
    unreleased: HashSet<ActiveKey>,
    /// Instruments whose subscribes are answered only when the test says
    /// (`hold_subscribe`, `answer_subscribe`), with the answers waiting.
    slow: HashSet<InstrumentKey>,
    waiting: Vec<(InstrumentKey, tokio::sync::oneshot::Sender<SourceOutcome>)>,
}

/// In-memory source for tests and the soak harness. Counts active
/// subscriptions and flags a double subscribe or an unsubscribe of
/// something not subscribed. Answers success at once unless a test
/// scripted a refusal (`refuse`, `fail_release`, `hold_release`,
/// `set_capacity`).
pub struct FakeSource {
    tx: broadcast::Sender<Arc<MarketUpdate>>,
    known: Option<HashSet<InstrumentKey>>,
    depths: Mutex<Vec<u8>>,
    active: Mutex<HashMap<ActiveKey, usize>>,
    calls: Mutex<Vec<SourceCall>>,
    violations: Mutex<Vec<String>>,
    record_calls: bool,
    client_losses: Arc<ClientLosses>,
    script: Mutex<Script>,
    resumes: AtomicU64,
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
            script: Mutex::new(Script::default()),
            resumes: AtomicU64::new(0),
        })
    }

    /// How often a feed client's sign-in asked the source to resume.
    pub fn resumes(&self) -> u64 {
        self.resumes.load(Ordering::Relaxed)
    }

    /// Refuse every subscribe of `key` with `why` (a broker that cannot
    /// take it).
    pub fn refuse(&self, key: InstrumentKey, why: &str) {
        self.script.lock().refused.insert(key, why.to_string());
    }

    /// Refuse the next unsubscribe of `key` at `mode` with `why`: the
    /// broker still streams it.
    pub fn fail_release(&self, key: InstrumentKey, mode: Mode, why: &str) {
        self.script
            .lock()
            .releases
            .entry((key, mode))
            .or_default()
            .push_back(Some(why.to_string()));
    }

    /// Never answer the next unsubscribe of `key` at `mode` (a broker that
    /// does not reply); the key stays active.
    pub fn hold_release(&self, key: InstrumentKey, mode: Mode) {
        self.script
            .lock()
            .releases
            .entry((key, mode))
            .or_default()
            .push_back(None);
    }

    /// Take at most `instruments` distinct instruments at once.
    pub fn set_capacity(&self, instruments: usize) {
        self.script.lock().capacity = Some(instruments);
    }

    /// Answer the subscribes of `key` only once `answer_subscribe` is
    /// called (a broker that takes its time); prices may flow meanwhile.
    pub fn hold_subscribe(&self, key: InstrumentKey) {
        self.script.lock().slow.insert(key);
    }

    /// Answer the subscribes of `key` waiting on `hold_subscribe`: taken.
    pub fn answer_subscribe(&self, key: &InstrumentKey) {
        let mut script = self.script.lock();
        script.slow.remove(key);
        let (now, rest): (Vec<_>, Vec<_>) = script.waiting.drain(..).partition(|(k, _)| k == key);
        script.waiting = rest;
        for (_, tx) in now {
            let _ = tx.send(Ok(()));
        }
    }

    /// Why a subscribe of `key` is refused now, if it is.
    fn refusal(&self, active: &HashMap<ActiveKey, usize>, key: &InstrumentKey) -> Option<String> {
        let script = self.script.lock();
        if let Some(why) = script.refused.get(key) {
            return Some(why.clone());
        }
        let cap = script.capacity?;
        let held: HashSet<&InstrumentKey> = active.keys().map(|(k, _, _)| k).collect();
        (!held.contains(key) && held.len() >= cap)
            .then(|| format!("You can stream at most {} instruments at once.", cap))
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
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp {
        if self.record_calls {
            self.calls
                .lock()
                .push(SourceCall::Subscribe(key.clone(), mode, depth));
        }
        let mut active = self.active.lock();
        let k = (key.clone(), mode, depth);
        if self.script.lock().unreleased.remove(&k) {
            // Held still: an owner joined while its release was refused.
            return answered(Ok(()));
        }
        if let Some(why) = self.refusal(&active, key) {
            return answered(Err(why));
        }
        let n = active.entry(k).or_insert(0);
        *n += 1;
        if *n > 1 {
            self.violations.lock().push(format!(
                "double subscribe {}:{} {:?} {}",
                key.exchange, key.symbol, mode, depth
            ));
        }
        let mut script = self.script.lock();
        if script.slow.contains(key) {
            let (tx, rx) = tokio::sync::oneshot::channel();
            script.waiting.push((key.clone(), tx));
            return Box::pin(async move { rx.await.unwrap_or(Ok(())) });
        }
        answered(Ok(()))
    }

    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) -> SourceOp {
        if self.record_calls {
            self.calls
                .lock()
                .push(SourceCall::Unsubscribe(key.clone(), mode, depth));
        }
        let mut active = self.active.lock();
        let k = (key.clone(), mode, depth);
        let scripted = {
            let mut script = self.script.lock();
            let next = script
                .releases
                .get_mut(&(key.clone(), mode))
                .and_then(VecDeque::pop_front);
            if next.is_some() {
                script.unreleased.insert(k.clone());
            }
            next
        };
        match scripted {
            Some(Some(why)) => return answered(Err(why)),
            Some(None) => return Box::pin(std::future::pending()),
            None => {}
        }
        self.script.lock().unreleased.remove(&k);
        match active.get_mut(&k) {
            Some(n) if *n > 1 => *n -= 1,
            Some(_) => {
                active.remove(&k);
            }
            None if self.script.lock().refused.contains_key(key) => {}
            None => self.violations.lock().push(format!(
                "unsubscribe without subscribe {}:{} {:?} {}",
                key.exchange, key.symbol, mode, depth
            )),
        }
        answered(Ok(()))
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

    fn resume(&self) {
        self.resumes.fetch_add(1, Ordering::Relaxed);
    }
}
