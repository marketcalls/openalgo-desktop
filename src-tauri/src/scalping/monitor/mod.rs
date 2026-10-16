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
//!
//! Here: the monitor, its lifecycle, sync and ticks. `attempt`: what proves
//! a leg flat and the exit attempt; `exits`: placing and clearing;
//! `reconcile`: following exit orders in the broker's book.

use super::store::{SlState, Store, MODE_LIVE};
use crate::brokers::common::streaming::FeedEvent;
use crate::brokers::common::symbols::SymbolResolver;
use crate::clock::Clock;
use crate::events::subscribers::socketio::UiEmitter;
use crate::risk::{evaluate_position, BreachReason, PositionRisk, Side, TrailMode};
use crate::services::freeze_qty_service::FreezeLimits;
use crate::services::order_reconciler::AbsenceTracker;
use crate::strategy::dispatch::{Book, BookOrder, OrderGateway, RunMode};
use crate::strategy::tick_feed::{Key, PriceSource};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

mod attempt;
mod exits;
mod reconcile;

pub use attempt::*;
pub use reconcile::ScalpingOrders;

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
    freeze: &FreezeLimits,
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
    // A known limit is never exceeded by more than the exchange forces:
    // one lot when the limit is below a lot (a trader's entry can be).
    // `MAX_LOTS` lots only when no limit is known (web `split_chunk`).
    let raw = freeze.for_option(symbol, exchange);
    Ok(Some(if raw > 0 {
        ((raw / lot) * lot).max(lot)
    } else {
        MAX_LOTS * lot
    }))
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

/// When the current trading session began: the 03:00 IST boundary at or
/// before `now`.
pub fn session_start(now: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::TimeZone;
    let ist = now.with_timezone(&chrono_tz::Asia::Kolkata);
    let boundary = |d: chrono::NaiveDate| {
        chrono_tz::Asia::Kolkata
            .from_local_datetime(&d.and_hms_opt(3, 0, 0).unwrap_or_default())
            .single()
            .map(|t| t.with_timezone(&Utc))
    };
    let today = ist.date_naive();
    match boundary(today) {
        Some(b) if now >= b => b,
        _ => today
            .pred_opt()
            .and_then(boundary)
            .unwrap_or(now - chrono::Duration::days(1)),
    }
}

/// Everything the monitor needs from the app, injectable for tests.
pub struct MonitorDeps {
    pub store: Store,
    pub gateway: Arc<dyn OrderGateway>,
    pub prices: Option<Arc<dyn PriceSource>>,
    pub ui: Arc<dyn UiEmitter>,
    pub clock: Arc<dyn Clock>,
    pub symbols: SymbolResolver,
    /// The freeze limits exits are split by.
    pub freeze: FreezeLimits,
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
    /// When a leg with no exit orders first read as a zero row: a fresh
    /// entry's row can lag the book, so zero is flat only after
    /// BOOK_LAG_GRACE_MS of zero reads (review P1-4).
    zero_since: HashMap<String, DateTime<Utc>>,
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
        self.zero_since.retain(|k, _| keys.contains(k));
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
    freeze: FreezeLimits,
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
            freeze: deps.freeze,
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

    /// A leg's exit attempt restored from its row. A day order from an
    /// earlier session is ended (review P1-5); an attempt that cannot be
    /// read back marks the leg broken (fail closed). True when the leg has
    /// newly become unreadable.
    fn restore_attempt(&self, inner: &mut Inner, key: &str, sl: &SlState) -> bool {
        match sl.exit_attempt.as_deref().filter(|t| !t.trim().is_empty()) {
            None => {
                inner.broken.remove(key);
                false
            }
            Some(t) => match serde_json::from_str::<ExitAttempt>(t) {
                Ok(mut a) => {
                    inner.broken.remove(key);
                    // A day order from an earlier session cannot still be
                    // working (review P1-5).
                    let since = session_start(self.clock.now());
                    let mut ended = false;
                    for c in a.children.iter_mut().filter(|c| !c.terminal()) {
                        if c.placed_at < since {
                            c.state = ChildState::Dead;
                            ended = true;
                        }
                    }
                    if ended {
                        tracing::warn!(
                            "Scalping exit orders for {} from an earlier session were ended",
                            sl.symbol
                        );
                        if !a.working() && a.settled_at.is_none() {
                            a.settled_at = Some(self.clock.now());
                        }
                        self.persist_attempt(sl.id, &a);
                    }
                    inner.attempts.insert(key.to_string(), a);
                    false
                }
                Err(e) => {
                    // Exit orders were sent and cannot be read back: never
                    // assume none (fail closed).
                    tracing::error!(
                        "Stored scalping exit orders for {} could not be read: {}",
                        sl.symbol,
                        e
                    );
                    inner.broken.insert(key.to_string())
                }
            },
        }
    }

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
                if !inner.attempts.contains_key(&key) && self.restore_attempt(&mut inner, &key, &sl)
                {
                    unreadable.push((key.clone(), sl.symbol.clone()));
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
        // Held exits of a stop that is gone are no longer the trader's to
        // decide here.
        {
            let legs: HashSet<String> = self.inner.lock().states.keys().cloned().collect();
            self.absence
                .retain_by(|k| legs.contains(k.split('#').next().unwrap_or_default()));
        }
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
