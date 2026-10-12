//! The scalping risk monitor (web `services/scalping_risk_monitor_service.py`):
//! server-side, event-driven stop loss, target and trailing stop for the legs
//! the terminal manages.
//!
//! * Prices arrive as live ticks; each tick drives one evaluation. The watched
//!   set changes only when a stop is saved or deleted (`request_sync`), never
//!   on a timer.
//! * The rules are not here: every tick is judged by `crate::risk`
//!   (`evaluate_position`). This module translates a leg to the core's input
//!   and its decision back; it does not decide.
//! * **Claim under the same lock that checks.** A breach is claimed (the
//!   in-flight marker and the retry clock) in the same hold that found it, so
//!   concurrent ticks produce exactly one exit. A refused exit releases the
//!   claim and leaves the leg managed; it is retried after a short cooldown.
//! * **Protection stays until the position is flat** (LOG-05, a deliberate
//!   departure: the web clears on acceptance). An exit is an attempt with
//!   child orders, recorded in memory and on the stop's row. The stop, its
//!   row and its price subscription stay while any child works; the order
//!   reconciler follows the children in the broker's order book, and the leg
//!   is cleared only once every child has ended and the position book shows
//!   it flat. A child that was rejected or cancelled leaves the leg managed,
//!   and a retry sends only what was neither filled nor is still working, so
//!   a partly refused or lagging exit never doubles up.
//! * **A decision is only acted on for the stop it was made on** (LOG-06).
//!   Each installed stop carries its row identity and version; an exit worker
//!   re-checks it before placing and before clearing, and never deletes a
//!   replacement. A running worker keeps its claim until it ends.
//! * **A leg's destination is the mode it was opened in.** A live leg exits
//!   to the broker with `force_live`; a sandbox leg exits to the sandbox. The
//!   analyzer toggle is never re-read for an exit, so switching analyzer mode
//!   on while a live leg is open cannot divert its exit into the sandbox.
//!   (The web skips such an exit and re-reads the toggle; see the module
//!   notes in `mod.rs`.)
//! * Trailing moves and clears are pushed as `scalping_sl_update` with the
//!   web payload.
//!
//! Hygiene: one owned consumer task (ticks and sync requests) plus one owned
//! task per exit in flight (bounded by the managed legs), all in one
//! `JoinSet`, aborted by `stop`. `stop` also releases every price
//! subscription. Every map is keyed by a managed leg and pruned with it.

use super::store::{SlState, Store, MODE_LIVE};
use crate::brokers::common::streaming::FeedEvent;
use crate::brokers::common::symbols::SymbolResolver;
use crate::clock::Clock;
use crate::events::subscribers::socketio::UiEmitter;
use crate::risk::{evaluate_position, BreachReason, PositionRisk, Side, TrailMode};
use crate::services::order_reconciler::{
    find_placed, held_message, Absence, AbsenceTracker, BookSnapshot, Found, HeldOrder, OrderOwner,
    Unconfirmed,
};
use crate::strategy::dispatch::{Book, BookOrder, OrderGateway, OrderPayload, RunMode};
use crate::strategy::tick_feed::{Key, PriceSource};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Rate limit on trailing-stop writes (bounds restart staleness).
pub const PERSIST_THROTTLE_MS: i64 = 1500;
/// Debounce on browser pushes of a trailing move.
pub const EMIT_THROTTLE_MS: i64 = 1000;
/// Retry spacing for a leg whose exit keeps being refused.
pub const EXIT_RETRY_COOLDOWN_MS: i64 = 3000;
/// How long a leg whose exit orders have all filled waits for the position
/// book to show it before the book is taken as it stands.
pub const BOOK_LAG_GRACE_MS: i64 = 30_000;
/// Strategy tag on every scalping order.
pub const SCALPING_STRATEGY: &str = "Scalping";
/// Most lots one manual order may carry (and the exit chunk when the
/// exchange freeze is unknown).
pub const MAX_LOTS: i64 = 20;

/// The Socket.IO event the terminal listens to.
pub const SL_UPDATE_EVENT: &str = "scalping_sl_update";

/// Derivative venues: lot rules and freeze-safe chunking apply.
pub const LEG_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS"];

pub fn slkey(mode: &str, symbol: &str, exchange: &str, product: &str) -> String {
    format!("{}:{}:{}:{}", mode, exchange, symbol, product)
}

fn run_mode(mode: &str) -> RunMode {
    if mode == MODE_LIVE {
        RunMode::Live
    } else {
        RunMode::Sandbox
    }
}

/// The risk core's input for one leg.
pub fn leg_risk(key: &str, s: &SlState) -> PositionRisk {
    PositionRisk {
        identifier: key.to_string(),
        side: if s.side.eq_ignore_ascii_case("SELL") {
            Side::Sell
        } else {
            Side::Buy
        },
        entry_price: s.entry_price,
        quantity: s.quantity.unsigned_abs() as f64,
        stop_price: s.current_sl,
        initial_stop_price: s.initial_sl,
        target_price: s.target,
        trailing_enabled: s.trailing_enabled,
        trail_step: s.trailing_step.unwrap_or(0.0),
        // The web's MIN_TRAIL_PROFIT: no trail until one point in profit.
        trail_trigger: crate::risk::DEFAULT_TRAIL_TRIGGER,
        trail_mode: TrailMode::Continuous,
        highest_price: s.highest_price,
        lowest_price: s.lowest_price,
    }
}

/// The size of each exit order for `quantity`: whole lots, never above the
/// exchange freeze (`MAX_LOTS` lots when it is unknown). `Ok(None)` means no
/// chunking (cash venues). `Err` is a trader-facing refusal.
pub fn exit_chunk(
    symbols: &SymbolResolver,
    symbol: &str,
    exchange: &str,
    quantity: i64,
) -> Result<Option<i64>, String> {
    if !LEG_EXCHANGES.contains(&exchange) {
        return Ok(None);
    }
    let lot = symbols
        .by_symbol(exchange, symbol)
        .map(|r| i64::from(r.lot_size))
        .filter(|l| *l > 0)
        .ok_or_else(|| format!("Unknown symbol: {}", symbol))?;
    if quantity % lot != 0 {
        return Err(format!("quantity must be a whole number of lots ({})", lot));
    }
    let raw = crate::services::symbol_service::freeze_qty_for_option(symbol, exchange);
    let freeze = if raw > 0 { (raw / lot) * lot } else { 0 };
    Ok(Some(if freeze > 0 { freeze } else { MAX_LOTS * lot }))
}

/// Split `quantity` into orders of at most `chunk` (the last one smaller).
pub fn chunks(quantity: i64, chunk: Option<i64>) -> Vec<i64> {
    match chunk {
        Some(c) if c > 0 && quantity > c => {
            let mut out = vec![c; (quantity / c) as usize];
            if quantity % c != 0 {
                out.push(quantity % c);
            }
            out
        }
        _ => vec![quantity],
    }
}

/// What a position book says about one leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegPosition {
    /// A row for the leg with this net quantity (0: positively flat).
    Net(i64),
    /// No row for the leg: an empty or lagging book (right after an entry),
    /// or a broker that drops closed rows. Never read as flat on its own.
    Missing,
    /// A row whose quantity cannot be read.
    Unreadable,
}

/// One leg in a position book body.
pub fn leg_position(book: &Value, symbol: &str, exchange: &str, product: &str) -> LegPosition {
    let rows = book
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for p in rows {
        let field = |k: &str| {
            p.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_uppercase()
        };
        if p.get("symbol").and_then(Value::as_str) == Some(symbol)
            && field("exchange") == exchange.to_ascii_uppercase()
            && field("product") == product.to_ascii_uppercase()
        {
            let q = match p.get("quantity") {
                Some(Value::Number(n)) => n.as_f64(),
                Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
                _ => None,
            };
            return match q.filter(|q| q.is_finite()) {
                Some(q) => LegPosition::Net(q as i64),
                None => LegPosition::Unreadable,
            };
        }
    }
    LegPosition::Missing
}

/// The net quantity of one leg in a position book body (0 when the book has
/// no readable row; never use it to decide a leg is flat).
pub fn net_quantity(book: &Value, symbol: &str, exchange: &str, product: &str) -> i64 {
    match leg_position(book, symbol, exchange, product) {
        LegPosition::Net(n) => n,
        _ => 0,
    }
}

/// What the position book and a leg's exit orders together prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flatness {
    /// The leg is flat: it may be cleared.
    Flat,
    /// Open with this signed net quantity.
    Open(i64),
    /// Nothing proves it either way: it stays protected.
    Unknown,
}

/// Whether a leg is flat (the monitor's fail-closed rule):
///
/// * a row at quantity 0 is flat, unless every exit order of the leg ended
///   with nothing filled (then the zero is not the exit's doing: a lagging
///   or stale row is never taken for it);
/// * a missing row is flat only when the leg's own exit orders filled the
///   whole position they were sent for;
/// * an unreadable row is never flat.
pub fn flatness(pos: LegPosition, attempt: Option<&ExitAttempt>) -> Flatness {
    let none_filled =
        attempt.is_some_and(|a| !a.children.is_empty() && !a.working() && a.filled() == 0);
    match pos {
        LegPosition::Net(0) if none_filled => Flatness::Unknown,
        LegPosition::Net(0) => Flatness::Flat,
        LegPosition::Net(n) => Flatness::Open(n),
        LegPosition::Missing => match attempt {
            Some(a) if a.net0 != 0 && !a.working() && a.filled() >= a.net0.abs() => Flatness::Flat,
            _ => Flatness::Unknown,
        },
        LegPosition::Unreadable => Flatness::Unknown,
    }
}

/// Extra position-book reads before a leg the book does not show is left
/// protected and the trader alerted.
pub const POSITION_REREADS: u32 = 2;
/// Spacing of those reads.
pub const POSITION_REREAD_MS: u64 = 300;

// ------------------------------------------------------------ exit attempts

/// Where one exit order stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildState {
    /// Acknowledged by the broker, not yet ended.
    Working,
    /// Sent with no definite answer: it may be at the broker (LOG-08).
    Unconfirmed,
    /// Completely filled.
    Filled,
    /// Rejected or cancelled (possibly after a partial fill), or confirmed
    /// never placed.
    Dead,
}

/// One exit order of an attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitChild {
    pub order_id: Option<String>,
    #[serde(default)]
    pub client_tag: Option<String>,
    /// `BUY` / `SELL`.
    pub action: String,
    pub qty: i64,
    #[serde(default)]
    pub filled: i64,
    pub state: ChildState,
    pub placed_at: DateTime<Utc>,
}

impl ExitChild {
    pub fn terminal(&self) -> bool {
        matches!(self.state, ChildState::Filled | ChildState::Dead)
    }

    /// Quantity that may still fill.
    pub fn open_qty(&self) -> i64 {
        if self.terminal() {
            0
        } else {
            (self.qty - self.filled).max(0)
        }
    }
}

/// The exit of one leg: every order sent to flatten it, since the first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitAttempt {
    pub id: String,
    /// The signed net position when the first exit went out.
    pub net0: i64,
    pub children: Vec<ExitChild>,
    /// When every child had ended.
    #[serde(default)]
    pub settled_at: Option<DateTime<Utc>>,
}

impl ExitAttempt {
    /// Some child may still fill.
    pub fn working(&self) -> bool {
        self.children.iter().any(|c| !c.terminal())
    }

    pub fn filled(&self) -> i64 {
        self.children.iter().map(|c| c.filled.max(0)).sum()
    }

    pub fn open(&self) -> i64 {
        self.children.iter().map(ExitChild::open_qty).sum()
    }

    /// What may still be sent: the position the attempt started from, less
    /// what filled and what is still working.
    pub fn unexited(&self) -> i64 {
        (self.net0.abs() - self.filled() - self.open()).max(0)
    }

    fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

/// What one exit attempt came to.
#[derive(Debug, Clone, PartialEq)]
pub enum ExitOutcome {
    /// The leg is flat and nothing of it is working: cleared.
    Flat,
    /// Exit orders went out; the leg stays protected until they end and the
    /// position is flat.
    Placed { orders: usize, unconfirmed: usize },
    /// Earlier exit orders still cover the position (or the position book
    /// has not caught up with their fills): nothing more is sent now.
    Waiting,
    /// Refused: the leg stays managed. Trader-facing reason.
    Refused(String),
    /// The stop was edited or replaced while this exit was being decided:
    /// nothing was sent, the next tick decides on the new stop.
    Stale,
}

/// Everything the monitor needs from the app, injectable for tests.
pub struct MonitorDeps {
    pub store: Store,
    pub gateway: Arc<dyn OrderGateway>,
    pub prices: Option<Arc<dyn PriceSource>>,
    pub ui: Arc<dyn UiEmitter>,
    pub clock: Arc<dyn Clock>,
    pub symbols: SymbolResolver,
}

/// A stop's identity: its row and the version of its settings. A decision
/// made on one is never acted on for another (LOG-06).
type Gen = (i64, i64);

struct Leg {
    gen: Gen,
    sl: SlState,
}

#[derive(Default)]
struct Inner {
    states: HashMap<String, Leg>,
    subscribed: HashSet<Key>,
    /// Exit workers running. Only a worker removes its own marker: a sync
    /// that drops or replaces the stop never frees it.
    inflight: HashSet<String>,
    /// The exit attempt each leg has in flight.
    attempts: HashMap<String, ExitAttempt>,
    last_exit: HashMap<String, DateTime<Utc>>,
    last_persist: HashMap<String, DateTime<Utc>>,
    last_emit: HashMap<String, DateTime<Utc>>,
    /// Legs whose stored exit orders could not be read: no exit is sent
    /// until the trader reconciles them (fail closed).
    broken: HashSet<String>,
    /// Alerts already raised, per leg and reason (once each).
    alerted: HashSet<String>,
}

impl Inner {
    fn wanted(&self) -> HashSet<Key> {
        self.states
            .values()
            .map(|l| (l.sl.symbol.clone(), l.sl.exchange.clone()))
            .collect()
    }

    fn prune(&mut self) {
        let keys: HashSet<String> = self.states.keys().cloned().collect();
        self.attempts.retain(|k, _| keys.contains(k));
        self.last_exit.retain(|k, _| keys.contains(k));
        self.last_persist.retain(|k, _| keys.contains(k));
        self.last_emit.retain(|k, _| keys.contains(k));
        self.broken.retain(|k| keys.contains(k));
        self.alerted
            .retain(|k| keys.contains(k.split('|').next().unwrap_or_default()));
    }
}

/// Where the monitor's trader alerts go (the Health page in the app).
pub type MonitorAlert = Arc<dyn Fn(String) + Send + Sync>;

pub struct RiskMonitor {
    store: Store,
    gateway: Arc<dyn OrderGateway>,
    prices: Option<Arc<dyn PriceSource>>,
    ui: Arc<dyn UiEmitter>,
    clock: Arc<dyn Clock>,
    symbols: SymbolResolver,
    inner: Mutex<Inner>,
    sync_wanted: Notify,
    tasks: Mutex<JoinSet<()>>,
    running: Mutex<Option<CancellationToken>>,
    /// Serialises syncs so subscription diffs never interleave.
    sync_lock: tokio::sync::Mutex<()>,
    /// Reads that missed an unconfirmed exit order (pruned every pass).
    absence: AbsenceTracker,
    alert: std::sync::OnceLock<MonitorAlert>,
}

fn elapsed_ms(now: DateTime<Utc>, then: Option<&DateTime<Utc>>) -> i64 {
    then.map(|t| (now - *t).num_milliseconds())
        .unwrap_or(i64::MAX)
}

/// Fold one book row into a child order.
fn fold_child(child: &mut ExitChild, b: &BookOrder) {
    let status = b.status.trim().to_ascii_lowercase().replace('_', " ");
    let filled = b.filled_quantity.max(0).min(child.qty);
    child.filled = child.filled.max(filled);
    match status.as_str() {
        "complete" | "completed" | "filled" | "executed" | "traded" => {
            child.state = ChildState::Filled;
            if child.filled == 0 {
                child.filled = child.qty;
            }
        }
        "rejected" | "cancelled" | "canceled" => child.state = ChildState::Dead,
        _ => child.state = ChildState::Working,
    }
}

impl RiskMonitor {
    pub fn new(deps: MonitorDeps) -> Arc<Self> {
        Arc::new(Self {
            store: deps.store,
            gateway: deps.gateway,
            prices: deps.prices,
            ui: deps.ui,
            clock: deps.clock,
            symbols: deps.symbols,
            inner: Mutex::new(Inner::default()),
            sync_wanted: Notify::new(),
            tasks: Mutex::new(JoinSet::new()),
            running: Mutex::new(None),
            sync_lock: tokio::sync::Mutex::new(()),
            absence: AbsenceTracker::new(),
            alert: std::sync::OnceLock::new(),
        })
    }

    /// Send trader alerts here (set once, by the app).
    pub fn set_alert_sink(&self, sink: MonitorAlert) {
        let _ = self.alert.set(sink);
    }

    /// Raise a trader alert once per leg and reason.
    fn alert_once(&self, key: &str, reason: &str, message: String) {
        if !self
            .inner
            .lock()
            .alerted
            .insert(format!("{}|{}", key, reason))
        {
            return;
        }
        tracing::error!("{}", message);
        if let Some(a) = self.alert.get() {
            a(message);
        }
    }

    /// The position book for `mode`, read again (bounded) while it does
    /// not prove the leg either open or flat.
    async fn read_leg(
        &self,
        mode: RunMode,
        sl: &SlState,
        attempt: Option<&ExitAttempt>,
    ) -> Result<(LegPosition, Flatness), String> {
        let mut reads = 0;
        loop {
            let book = match self.gateway.book(mode, Book::Positions).await {
                Ok(b) => b,
                Err(body) => {
                    // Never treated as flat: an unreadable book is not a
                    // closed position.
                    return Err(body
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("The position book could not be read")
                        .to_string());
                }
            };
            let pos = leg_position(&book, &sl.symbol, &sl.exchange, &sl.product);
            let f = flatness(pos, attempt);
            if f != Flatness::Unknown || reads >= POSITION_REREADS {
                return Ok((pos, f));
            }
            reads += 1;
            tokio::time::sleep(std::time::Duration::from_millis(POSITION_REREAD_MS)).await;
        }
    }

    /// Write a leg's attempt to its row; false when it could not be saved
    /// (then no further exit order is sent for it).
    fn persist_attempt(&self, row_id: i64, a: &ExitAttempt) -> bool {
        let Some(text) = a.to_json() else {
            tracing::error!("Scalping exit orders for a leg could not be encoded");
            return false;
        };
        match self.store.set_exit_attempt(row_id, Some(&text)) {
            Ok(true) => true,
            Ok(false) => {
                tracing::warn!(
                    "Scalping stop row {} is gone; its exit orders were not saved",
                    row_id
                );
                false
            }
            Err(e) => {
                tracing::error!("Scalping exit orders for a leg could not be saved: {}", e);
                false
            }
        }
    }

    // ------------------------------------------------------------ lifecycle

    /// Start the owned consumer (idempotent) and reconcile with the stored
    /// stops.
    pub fn start(self: &Arc<Self>) {
        {
            let mut running = self.running.lock();
            if running.is_some() {
                drop(running);
                self.sync_wanted.notify_one();
                return;
            }
            let token = CancellationToken::new();
            *running = Some(token.clone());
            let weak = Arc::downgrade(self);
            let mut rx = self.prices.as_ref().map(|p| p.ticks());
            self.spawn(async move {
                loop {
                    let wanted = {
                        let Some(m) = weak.upgrade() else { break };
                        // A notified future made before awaiting, so a
                        // request between iterations is never lost.
                        let fut = async move { m.sync_wanted.notified().await };
                        fut
                    };
                    tokio::select! {
                        _ = token.cancelled() => break,
                        _ = wanted => {
                            let Some(m) = weak.upgrade() else { break };
                            m.sync().await;
                        }
                        ev = recv(&mut rx) => match ev {
                            Ok(ev) => {
                                let FeedEvent::Tick(t) = ev.as_ref() else { continue };
                                let Some(m) = weak.upgrade() else { break };
                                m.process_tick(&t.symbol, &t.exchange, t.ltp).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::debug!("Scalping tick consumer lagged by {}", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                rx = None;
                            }
                        },
                    }
                }
            });
        }
        self.sync_wanted.notify_one();
    }

    /// Stop every owned task, release every price subscription and forget
    /// the in-memory legs (the stored rows, exit attempts included, stay;
    /// the next start reloads them). Idempotent.
    pub async fn stop(&self) {
        if let Some(t) = self.running.lock().take() {
            t.cancel();
        }
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let held: Vec<Key> = {
            let mut inner = self.inner.lock();
            inner.states.clear();
            inner.inflight.clear();
            inner.prune();
            inner.subscribed.drain().collect()
        };
        self.absence.retain(&HashSet::new());
        if let (Some(src), false) = (&self.prices, held.is_empty()) {
            src.unsubscribe(&held).await;
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().is_some()
    }

    /// Ask the consumer to reconcile (an SL was saved or deleted). Never
    /// blocks; a no-op while stopped (the next start reconciles).
    pub fn request_sync(&self) {
        if self.is_running() {
            self.sync_wanted.notify_one();
        }
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    // ------------------------------------------------------------ hygiene views

    /// Owned tasks alive (consumer plus exits in flight).
    pub fn task_count(&self) -> usize {
        let mut t = self.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    /// Instruments this monitor holds a price subscription for.
    pub fn subscribed(&self) -> Vec<Key> {
        let mut v: Vec<Key> = self.inner.lock().subscribed.iter().cloned().collect();
        v.sort();
        v
    }

    /// Legs under management.
    pub fn leg_count(&self) -> usize {
        self.inner.lock().states.len()
    }

    /// One managed leg, as held in memory.
    pub fn leg(&self, mode: &str, symbol: &str, exchange: &str, product: &str) -> Option<SlState> {
        self.inner
            .lock()
            .states
            .get(&slkey(mode, symbol, exchange, product))
            .map(|l| l.sl.clone())
    }

    /// One leg's exit attempt in flight.
    pub fn exit_attempt(
        &self,
        mode: &str,
        symbol: &str,
        exchange: &str,
        product: &str,
    ) -> Option<ExitAttempt> {
        self.inner
            .lock()
            .attempts
            .get(&slkey(mode, symbol, exchange, product))
            .cloned()
    }

    /// Entries across every per-leg map (bounded by the managed legs).
    pub fn tracked_entries(&self) -> usize {
        let i = self.inner.lock();
        i.states.len()
            + i.inflight.len()
            + i.attempts.len()
            + i.last_exit.len()
            + i.last_persist.len()
            + i.last_emit.len()
            + self.absence.len()
    }

    /// Wait until no exit is in flight (tests).
    pub async fn wait_idle(&self) {
        for _ in 0..2000 {
            if self.inner.lock().inflight.is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    // ------------------------------------------------------------ sync

    /// Reconcile the managed legs and the price subscriptions with the
    /// stored stops (every mode: a live leg stays protected while analyzer
    /// mode is on, and the other way round).
    pub async fn sync(&self) {
        let _serial = self.sync_lock.lock().await;
        let (to_add, to_remove, unreadable) = {
            let mut inner = self.inner.lock();
            // The read happens under the lock: an exit that clears a leg
            // deletes its row first and then takes this lock, so a stale
            // row can never be reinstalled after its exit.
            let rows = match self.store.active_sl(None) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Scalping stops could not be loaded: {}", e);
                    return;
                }
            };
            let mut states = HashMap::with_capacity(rows.len());
            let mut unreadable = Vec::new();
            for sl in rows {
                let key = slkey(&sl.mode, &sl.symbol, &sl.exchange, &sl.product);
                // An attempt survives a restart on its row; the one in memory
                // is newer while the monitor runs.
                if !inner.attempts.contains_key(&key) {
                    match sl.exit_attempt.as_deref().filter(|t| !t.trim().is_empty()) {
                        None => {
                            inner.broken.remove(&key);
                        }
                        Some(t) => match serde_json::from_str::<ExitAttempt>(t) {
                            Ok(a) => {
                                inner.broken.remove(&key);
                                inner.attempts.insert(key.clone(), a);
                            }
                            Err(e) => {
                                // Exit orders were sent and cannot be read
                                // back: never assume none (fail closed).
                                tracing::error!(
                                    "Stored scalping exit orders for {} could not be read: {}",
                                    sl.symbol,
                                    e
                                );
                                if inner.broken.insert(key.clone()) {
                                    unreadable.push((key.clone(), sl.symbol.clone()));
                                }
                            }
                        },
                    }
                }
                states.insert(
                    key,
                    Leg {
                        gen: (sl.id, sl.version),
                        sl,
                    },
                );
            }
            inner.states = states;
            // `inflight` is the workers' own: never pruned here (LOG-06).
            inner.prune();
            let wanted = inner.wanted();
            let add: Vec<Key> = wanted.difference(&inner.subscribed).cloned().collect();
            let remove: Vec<Key> = inner.subscribed.difference(&wanted).cloned().collect();
            for k in &add {
                inner.subscribed.insert(k.clone());
            }
            for k in &remove {
                inner.subscribed.remove(k);
            }
            (add, remove, unreadable)
        };
        for (key, symbol) in unreadable {
            self.alert_once(
                &key,
                "unreadable",
                format!(
                    "Scalping stop for {}: the exit orders sent for it earlier could not be read back. No further exit is sent for it automatically. Check the position and the order book at the broker, then remove and set the stop again.",
                    symbol
                ),
            );
        }
        if let Some(src) = &self.prices {
            if !to_add.is_empty() {
                src.subscribe(&to_add).await;
            }
            if !to_remove.is_empty() {
                src.unsubscribe(&to_remove).await;
            }
        }
    }

    // ------------------------------------------------------------ ticks

    /// Judge every managed leg on this instrument against one price.
    pub async fn process_tick(self: &Arc<Self>, symbol: &str, exchange: &str, ltp: f64) {
        if !(ltp.is_finite() && ltp > 0.0) {
            return;
        }
        let now = self.clock.now();
        let mut exits: Vec<(String, Gen, SlState, Option<BreachReason>)> = Vec::new();
        let mut persists: Vec<(String, SlState, bool)> = Vec::new();
        {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            for (key, leg) in inner.states.iter_mut() {
                if leg.sl.symbol != symbol || leg.sl.exchange != exchange {
                    continue;
                }
                let has_sl = leg.sl.current_sl.is_some() || leg.sl.initial_sl.is_some();
                let has_target = leg.sl.target.is_some_and(|t| t > 0.0);
                if !has_sl && !has_target {
                    continue;
                }
                let decision = evaluate_position(&leg_risk(key, &leg.sl), Some(ltp));
                if !decision.evaluated {
                    continue;
                }
                if decision.breached {
                    // The claim: in-flight marker and retry clock, written in
                    // the same hold as the check.
                    if inner.inflight.contains(key) {
                        continue;
                    }
                    // Its earlier exit orders cannot be read: never sent
                    // again blindly (the trader was alerted).
                    if inner.broken.contains(key) {
                        continue;
                    }
                    // Exit orders already cover the whole position: nothing
                    // to decide until they end.
                    if inner
                        .attempts
                        .get(key)
                        .is_some_and(|a| a.working() && a.unexited() == 0)
                    {
                        continue;
                    }
                    if elapsed_ms(now, inner.last_exit.get(key)) < EXIT_RETRY_COOLDOWN_MS {
                        continue;
                    }
                    inner.inflight.insert(key.clone());
                    inner.last_exit.insert(key.clone(), now);
                    exits.push((key.clone(), leg.gen, leg.sl.clone(), decision.reason));
                    continue;
                }
                let moved = decision.stop_price != leg.sl.current_sl
                    || decision.highest_price != leg.sl.highest_price
                    || decision.lowest_price != leg.sl.lowest_price;
                if moved {
                    leg.sl.current_sl = decision.stop_price;
                    leg.sl.highest_price = decision.highest_price;
                    leg.sl.lowest_price = decision.lowest_price;
                    let persist =
                        elapsed_ms(now, inner.last_persist.get(key)) >= PERSIST_THROTTLE_MS;
                    if persist {
                        inner.last_persist.insert(key.clone(), now);
                        let emit = elapsed_ms(now, inner.last_emit.get(key)) >= EMIT_THROTTLE_MS;
                        if emit {
                            inner.last_emit.insert(key.clone(), now);
                        }
                        persists.push((key.clone(), leg.sl.clone(), emit));
                    }
                }
            }
        }
        // Decided under the lock; the work it calls for happens after.
        for (key, gen, sl, reason) in exits {
            let me = self.clone();
            self.spawn(async move {
                me.run_exit(key, gen, sl, reason, ltp).await;
            });
        }
        for (_key, sl, emit) in persists {
            if let Err(e) = self.store.update_trail(&sl) {
                tracing::debug!("Trailing stop for {} not saved: {}", sl.symbol, e);
            }
            if emit {
                self.ui.emit(SL_UPDATE_EVENT, update_payload(&sl)).await;
            }
        }
    }

    // ------------------------------------------------------------ exits

    async fn run_exit(
        self: Arc<Self>,
        key: String,
        gen: Gen,
        sl: SlState,
        reason: Option<BreachReason>,
        ltp: f64,
    ) {
        let outcome = self.execute_exit(&key, gen, &sl).await;
        let why = match reason {
            Some(BreachReason::Target) => "target",
            _ => "stop loss",
        };
        match &outcome {
            ExitOutcome::Flat => {
                tracing::info!(
                    "Scalping {} for {} found the position already flat; cleared",
                    why,
                    sl.symbol
                );
                self.clear(&key, gen, &sl).await;
            }
            ExitOutcome::Placed {
                orders,
                unconfirmed,
            } => {
                tracing::info!(
                    "Scalping {} exit for {} sent in {} order(s) near {:.2} ({} unconfirmed); the stop stays until the position is flat",
                    why,
                    sl.symbol,
                    orders + unconfirmed,
                    ltp,
                    unconfirmed
                );
            }
            ExitOutcome::Waiting => {
                tracing::debug!(
                    "Scalping {} for {}: exit orders already cover the position",
                    why,
                    sl.symbol
                );
            }
            ExitOutcome::Refused(msg) => {
                tracing::error!(
                    "Scalping {} exit for {} was refused and the position is still open; it stays protected and will be retried: {}",
                    why,
                    sl.symbol,
                    msg
                );
            }
            ExitOutcome::Stale => {
                tracing::info!(
                    "Scalping {} for {} was changed while its exit was being decided; nothing was sent",
                    why,
                    sl.symbol
                );
                // Nothing went out: the new stop decides without a cooldown.
                self.inner.lock().last_exit.remove(&key);
            }
        }
        // Release the claim only after the clear, so no tick in between can
        // send a second exit for a leg that was just closed.
        self.inner.lock().inflight.remove(&key);
    }

    /// Exit one leg in the mode it was opened in: read its net position,
    /// then send whole-lot, freeze-safe MARKET orders for what is neither
    /// filled nor still working from earlier exits of this leg.
    pub async fn execute_exit(&self, key: &str, gen: Gen, sl: &SlState) -> ExitOutcome {
        let mode = run_mode(&sl.mode);
        if let Err(e) = self.gateway.authorised(mode) {
            return ExitOutcome::Refused(e);
        }
        if self.inner.lock().broken.contains(key) {
            return ExitOutcome::Refused(
                "The exit orders sent earlier for this stop could not be read back".into(),
            );
        }
        let known = self.inner.lock().attempts.get(key).cloned();
        let (pos, flat) = match self.read_leg(mode, sl, known.as_ref()).await {
            Ok(r) => r,
            Err(msg) => return ExitOutcome::Refused(msg),
        };
        let net = match flat {
            Flatness::Open(n) => n,
            Flatness::Flat => 0,
            Flatness::Unknown => {
                if known.as_ref().is_some_and(ExitAttempt::working) {
                    // Exit orders still working: the reconciler follows them.
                    return ExitOutcome::Waiting;
                }
                // Not flat on the book's word alone (an empty or lagging
                // book right after an entry, a stale zero row, an
                // unreadable row): the leg stays protected.
                self.alert_once(
                    key,
                    "unknown_position",
                    format!(
                        "Scalping stop for {} {}: the broker's position book does not show this position{}, so OpenAlgo cannot tell whether it is still open. The stop stays and no exit is sent until the book shows it. Check the position at the broker.",
                        sl.symbol,
                        sl.exchange,
                        if pos == LegPosition::Unreadable { " with a readable quantity" } else { "" }
                    ),
                );
                return ExitOutcome::Refused(
                    "The position book does not show this position yet".into(),
                );
            }
        };
        let now = self.clock.now();
        // Decided on the stop this exit was claimed for, and only that one
        // (LOG-06): the trader may have edited or replaced it meanwhile.
        let attempt = {
            let inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) != Some(gen) {
                return ExitOutcome::Stale;
            }
            inner.attempts.get(key).cloned()
        };
        let (qty, attempt) = match attempt {
            Some(a) => {
                let same_side = a.net0.signum() == net.signum();
                if net == 0 {
                    if a.working() {
                        // The book is ahead of the order book: let the
                        // reconciler see the children end.
                        return ExitOutcome::Waiting;
                    }
                    // `flatness` already refused a zero that the exit
                    // orders did not make.
                    return ExitOutcome::Flat;
                }
                let left = a.unexited();
                if left > 0 && same_side {
                    (left.min(net.abs()), Some(a))
                } else if a.working() {
                    return ExitOutcome::Waiting;
                } else if same_side
                    && a.settled_at
                        .is_some_and(|t| (now - t).num_milliseconds() < BOOK_LAG_GRACE_MS)
                {
                    // Every exit filled; the position book has not caught up.
                    return ExitOutcome::Waiting;
                } else {
                    // The book has had time (or the position changed side):
                    // it is the position to protect now.
                    (net.abs(), None)
                }
            }
            None => {
                if net == 0 {
                    return ExitOutcome::Flat;
                }
                (net.abs(), None)
            }
        };
        let action = if net > 0 { "SELL" } else { "BUY" };
        let chunk = match exit_chunk(&self.symbols, &sl.symbol, &sl.exchange, qty) {
            Ok(c) => c,
            Err(e) => return ExitOutcome::Refused(e),
        };
        let mut a = attempt.unwrap_or_else(|| ExitAttempt {
            id: crate::strategy::state::new_position_ref(),
            net0: net,
            children: Vec::new(),
            settled_at: None,
        });
        // Saved before anything is sent: an exit whose orders could not be
        // remembered across a restart is never started (fail closed).
        if !self.persist_attempt(sl.id, &a) {
            self.alert_once(
                key,
                "persist",
                format!(
                    "Scalping stop for {}: its exit could not be saved, so no exit order was sent. The stop stays; check the disk and the app's data folder, then try again.",
                    sl.symbol
                ),
            );
            return ExitOutcome::Refused("The exit could not be saved before sending".into());
        }
        let mut refused: Option<String> = None;
        let (mut accepted, mut unconfirmed) = (0usize, 0usize);
        let parts = chunks(qty, chunk);
        let last = parts.len().saturating_sub(1);
        for (i, q) in parts.into_iter().enumerate() {
            let order = OrderPayload {
                symbol: sl.symbol.clone(),
                exchange: sl.exchange.clone(),
                action: action.to_string(),
                quantity: q,
                product: sl.product.clone(),
                pricetype: "MARKET".into(),
                strategy: SCALPING_STRATEGY.into(),
            };
            let r = self.gateway.place(mode, &order).await;
            let state = if r.ok {
                accepted += 1;
                ChildState::Working
            } else if r.uncertain {
                // It may be at the broker: recorded, never sent again until
                // the order reconciler settles it (LOG-08).
                unconfirmed += 1;
                ChildState::Unconfirmed
            } else {
                refused = Some(
                    r.error
                        .unwrap_or_else(|| "The exit order was rejected".into()),
                );
                break;
            };
            a.children.push(ExitChild {
                order_id: r.broker_order_id.filter(|i| !i.is_empty()),
                client_tag: r.client_tag,
                action: action.to_string(),
                qty: q,
                filled: 0,
                state,
                placed_at: self.clock.now(),
            });
            a.settled_at = None;
            // Every order sent is remembered before the next goes out.
            if !self.record_attempt(key, sl.id, &a) {
                // The orders already sent are followed in memory; no more
                // are sent while they cannot be saved.
                self.alert_once(
                    key,
                    "persist",
                    format!(
                        "Scalping stop for {}: exit orders were sent but could not be saved{}. The stop stays; check the order book at the broker.",
                        sl.symbol,
                        if i < last { ", so the rest of the exit was not sent" } else { "" }
                    ),
                );
                if i < last {
                    refused = Some("The exit orders could not be saved".into());
                }
                break;
            }
        }
        match refused {
            Some(msg) => ExitOutcome::Refused(msg),
            None => ExitOutcome::Placed {
                orders: accepted,
                unconfirmed,
            },
        }
    }

    /// Keep a leg's attempt in memory and on its row. False when the row
    /// could not be written (it is still followed in memory).
    fn record_attempt(&self, key: &str, row_id: i64, a: &ExitAttempt) -> bool {
        self.inner
            .lock()
            .attempts
            .insert(key.to_string(), a.clone());
        self.persist_attempt(row_id, a)
    }

    /// Forget a closed leg: its row, its state, its subscription when no
    /// other leg needs the instrument, then tell the terminal. Only for the
    /// stop the exit was decided on: a replacement is never touched.
    async fn clear(&self, key: &str, gen: Gen, sl: &SlState) -> bool {
        {
            let inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) != Some(gen) {
                return false;
            }
        }
        match self.store.delete_sl_exact(gen.0, gen.1) {
            Ok(true) => {}
            Ok(false) => {
                // Edited since: the new stop stays, and decides next tick.
                return false;
            }
            Err(e) => {
                tracing::error!(
                    "Scalping stop for {} could not be removed: {}",
                    sl.symbol,
                    e
                );
            }
        }
        let symkey: Key = (sl.symbol.clone(), sl.exchange.clone());
        let unsub = {
            let mut inner = self.inner.lock();
            if inner.states.get(key).map(|l| l.gen) == Some(gen) {
                inner.states.remove(key);
            }
            inner.prune();
            let needed = inner.wanted().contains(&symkey);
            if !needed && inner.subscribed.remove(&symkey) {
                Some(symkey)
            } else {
                None
            }
        };
        if let (Some(src), Some(k)) = (&self.prices, unsub) {
            src.unsubscribe(std::slice::from_ref(&k)).await;
        }
        self.ui
            .emit(
                SL_UPDATE_EVENT,
                json!({
                    "symbol": sl.symbol,
                    "exchange": sl.exchange,
                    "product": sl.product,
                    "cleared": true,
                }),
            )
            .await;
        true
    }

    // ------------------------------------------------------------ reconcile

    /// An attempt the reconciler still follows: an order of it may still
    /// fill, or every order ended a moment ago and the position book may
    /// not show it yet.
    fn followed(&self, a: &ExitAttempt, now: DateTime<Utc>) -> bool {
        a.working()
            || a.settled_at
                .is_some_and(|t| (now - t).num_milliseconds() < BOOK_LAG_GRACE_MS)
    }

    /// Whether a leg of this mode has an exit the reconciler follows.
    pub fn has_working_exits(&self, mode: RunMode) -> bool {
        let now = self.clock.now();
        let inner = self.inner.lock();
        inner.attempts.iter().any(|(k, a)| {
            self.followed(a, now)
                && inner
                    .states
                    .get(k)
                    .is_some_and(|l| run_mode(&l.sl.mode) == mode)
        })
    }

    /// Whether a broker order id is one of this monitor's exit orders.
    pub fn owns_order(&self, order_id: &str) -> bool {
        self.inner.lock().attempts.values().any(|a| {
            a.children
                .iter()
                .any(|c| c.order_id.as_deref() == Some(order_id))
        })
    }

    /// Follow every working exit order of `mode` in one order-book read
    /// (the order reconciler's pass). A leg whose exit orders have all
    /// ended is cleared once the position book shows it flat. Returns how
    /// many unconfirmed orders are still unresolved.
    pub async fn reconcile_book(&self, mode: RunMode, book: &BookSnapshot) -> usize {
        let now = self.clock.now();
        let pending: Vec<(String, Gen, SlState, ExitAttempt)> = {
            let inner = self.inner.lock();
            inner
                .attempts
                .iter()
                .filter(|(_, a)| self.followed(a, now))
                .filter_map(|(k, a)| {
                    let l = inner.states.get(k)?;
                    (run_mode(&l.sl.mode) == mode)
                        .then(|| (k.clone(), l.gen, l.sl.clone(), a.clone()))
                })
                .collect()
        };
        let mut unresolved = 0;
        let mut live = HashSet::new();
        let mut claimed: HashSet<String> = HashSet::new();
        for (key, gen, sl, mut a) in pending {
            if !a.working() {
                // Every order ended: the position book may only now show
                // the leg flat.
                self.settle(&key, gen, &sl, mode).await;
                continue;
            }
            let mut changed = false;
            for (i, child) in a.children.iter_mut().enumerate() {
                if child.terminal() {
                    continue;
                }
                match (child.state, child.order_id.clone()) {
                    (ChildState::Working, Some(id)) => {
                        if let Some(b) = book.by_id(&id) {
                            let before = child.clone();
                            fold_child(child, b);
                            changed |= *child != before;
                        }
                    }
                    _ => {
                        let akey = format!("{}#{}#{}", key, a.id, i);
                        live.insert(akey.clone());
                        let u = Unconfirmed {
                            symbol: &sl.symbol,
                            exchange: &sl.exchange,
                            action: &child.action,
                            quantity: child.qty,
                            product: Some(&sl.product),
                            client_tag: child.client_tag.as_deref(),
                            placed_at: Some(child.placed_at),
                        };
                        let bound = |id: &str| claimed.contains(id) || book.owned(id);
                        match find_placed(&book.rows, &u, &bound) {
                            Found::Order(b) => {
                                self.absence.forget(&akey);
                                claimed.insert(b.order_id.clone());
                                child.order_id = Some(b.order_id.clone());
                                fold_child(child, b);
                                changed = true;
                            }
                            Found::Absent => {
                                match self.absence.judge(&akey, &akey, mode, book, &u).await {
                                    Absence::NotPlaced => {
                                        self.absence.forget(&akey);
                                        tracing::warn!(
                                            "Unconfirmed scalping exit for {} is not in the broker's books after repeated checks: it was not placed",
                                            sl.symbol
                                        );
                                        child.state = ChildState::Dead;
                                        changed = true;
                                    }
                                    Absence::Wait => unresolved += 1,
                                    Absence::Hold { alert } => {
                                        // Live: never on absence alone
                                        // (P0-3). The leg stays protected and
                                        // nothing more is sent for it.
                                        unresolved += 1;
                                        if alert {
                                            let msg = held_message(
                                                &format!("Scalping exit for {}", sl.symbol),
                                                &u,
                                            );
                                            self.absence.describe(&akey, &msg);
                                            book.alert_held(msg);
                                        }
                                    }
                                }
                            }
                            Found::Ambiguous(n) => {
                                unresolved += 1;
                                tracing::error!(
                                    "{} orders in the broker's order book match an unconfirmed scalping exit for {}; it is not sent again. Check the order book.",
                                    n,
                                    sl.symbol
                                );
                            }
                        }
                    }
                }
            }
            if !changed {
                continue;
            }
            if !a.working() {
                a.settled_at = Some(book.read_at);
            }
            // Only for the stop these orders belong to.
            if self.inner.lock().states.get(&key).map(|l| l.gen.0) != Some(gen.0) {
                continue;
            }
            self.record_attempt(&key, sl.id, &a);
            if !a.working() {
                self.settle(&key, gen, &sl, mode).await;
            }
        }
        // This destination's keys only: the other's pass keeps its own.
        let scope = format!(
            "{}:",
            if mode == RunMode::Live {
                MODE_LIVE
            } else {
                super::store::MODE_ANALYZE
            }
        );
        self.absence.retain_scope(&scope, &live);
        unresolved
    }

    /// Live exit orders held for the trader (P0-3).
    pub fn held_orders(&self) -> Vec<HeldOrder> {
        self.absence.held()
    }

    /// The trader checked the broker's books: the held exit order `id` was
    /// not placed. It ends as never placed; the leg stays protected and its
    /// next breach sends what is not covered.
    pub async fn confirm_not_placed(&self, id: &str) -> Result<bool, String> {
        if self.absence.held_key(id).is_none() {
            return Ok(false);
        }
        let mut parts = id.rsplitn(3, '#');
        let (Some(idx), Some(attempt_id), Some(key)) = (
            parts.next().and_then(|i| i.parse::<usize>().ok()),
            parts.next(),
            parts.next(),
        ) else {
            return Ok(false);
        };
        let now = self.clock.now();
        let found = {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            match (inner.states.get(key), inner.attempts.get_mut(key)) {
                (Some(leg), Some(a)) if a.id == attempt_id => {
                    match a.children.get_mut(idx).filter(|c| !c.terminal()) {
                        Some(c) => {
                            c.state = ChildState::Dead;
                            if !a.working() {
                                a.settled_at = Some(now);
                            }
                            Some((leg.gen, leg.sl.clone(), a.clone()))
                        }
                        None => None,
                    }
                }
                _ => None,
            }
        };
        self.absence.forget(id);
        let Some((gen, sl, a)) = found else {
            return Ok(false);
        };
        tracing::warn!(
            "Trader marked an unconfirmed scalping exit for {} as not placed",
            sl.symbol
        );
        self.record_attempt(key, sl.id, &a);
        if !a.working() {
            self.settle(key, gen, &sl, run_mode(&sl.mode)).await;
        }
        Ok(true)
    }

    /// Every exit order of a leg has ended: clear it if the position book
    /// shows it flat; otherwise it stays protected, and its next breach
    /// sends only what was not filled.
    async fn settle(&self, key: &str, gen: Gen, sl: &SlState, mode: RunMode) {
        let Ok(book) = self.gateway.book(mode, Book::Positions).await else {
            return;
        };
        let attempt = self.inner.lock().attempts.get(key).cloned();
        // Flat only on proof (a zero row the exits made, or exits that
        // filled the whole position): a missing row is not flat.
        if flatness(
            leg_position(&book, &sl.symbol, &sl.exchange, &sl.product),
            attempt.as_ref(),
        ) != Flatness::Flat
        {
            return;
        }
        // The installed stop may have been edited meanwhile (same row, new
        // version): clear that one, the position it protects is flat.
        let current = self.inner.lock().states.get(key).map(|l| l.gen);
        if let Some(g) = current.filter(|g| g.0 == gen.0) {
            if self.clear(key, g, sl).await {
                tracing::info!(
                    "Scalping exit for {} filled and the position is flat; cleared",
                    sl.symbol
                );
            }
        }
    }
}

/// The scalping monitor as an owner of orders (its exit orders).
pub struct ScalpingOrders(pub Weak<RiskMonitor>);

#[async_trait::async_trait]
impl OrderOwner for ScalpingOrders {
    fn name(&self) -> &'static str {
        "scalping"
    }

    fn has_open_orders(&self, mode: RunMode) -> bool {
        self.0.upgrade().is_some_and(|m| m.has_working_exits(mode))
    }

    fn owns_order(&self, order_id: &str) -> bool {
        self.0.upgrade().is_some_and(|m| m.owns_order(order_id))
    }

    async fn reconcile(&self, mode: RunMode, book: &BookSnapshot) -> usize {
        match self.0.upgrade() {
            Some(m) => m.reconcile_book(mode, book).await,
            None => 0,
        }
    }

    fn held_orders(&self) -> Vec<HeldOrder> {
        self.0
            .upgrade()
            .map(|m| m.held_orders())
            .unwrap_or_default()
    }

    async fn confirm_not_placed(&self, id: &str) -> Result<bool, String> {
        match self.0.upgrade() {
            Some(m) => m.confirm_not_placed(id).await,
            None => Ok(false),
        }
    }
}

/// The trailing-move payload (web `_emit_update` with a state).
pub fn update_payload(sl: &SlState) -> Value {
    json!({
        "symbol": sl.symbol,
        "exchange": sl.exchange,
        "product": sl.product,
        "cleared": false,
        "current_sl": sl.current_sl,
        "target": sl.target,
    })
}

async fn recv(
    rx: &mut Option<broadcast::Receiver<crate::brokers::common::streaming::MarketEvent>>,
) -> Result<crate::brokers::common::streaming::MarketEvent, broadcast::error::RecvError> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}
