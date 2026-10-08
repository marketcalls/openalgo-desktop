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
use crate::strategy::dispatch::{Book, OrderGateway, OrderPayload, RunMode};
use crate::strategy::tick_feed::{Key, PriceSource};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Rate limit on trailing-stop writes (bounds restart staleness).
pub const PERSIST_THROTTLE_MS: i64 = 1500;
/// Debounce on browser pushes of a trailing move.
pub const EMIT_THROTTLE_MS: i64 = 1000;
/// Retry spacing for a leg whose exit keeps being refused.
pub const EXIT_RETRY_COOLDOWN_MS: i64 = 3000;
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

/// The net quantity of one leg in a position book body.
pub fn net_quantity(book: &Value, symbol: &str, exchange: &str, product: &str) -> i64 {
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
            return match p.get("quantity") {
                Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) as i64,
                Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0) as i64,
                _ => 0,
            };
        }
    }
    0
}

/// What one exit attempt came to.
#[derive(Debug, Clone, PartialEq)]
pub enum ExitOutcome {
    /// The leg was already flat; nothing was sent.
    Flat,
    /// Every exit order was accepted.
    Closed { orders: usize },
    /// Refused: the leg stays managed. Trader-facing reason.
    Refused(String),
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

struct Leg {
    /// Changes whenever a sync installs the leg, so a decision made on an
    /// older copy is recognised as stale.
    gen: u64,
    sl: SlState,
}

#[derive(Default)]
struct Inner {
    states: HashMap<String, Leg>,
    next_gen: u64,
    subscribed: HashSet<Key>,
    inflight: HashSet<String>,
    last_exit: HashMap<String, DateTime<Utc>>,
    last_persist: HashMap<String, DateTime<Utc>>,
    last_emit: HashMap<String, DateTime<Utc>>,
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
        self.last_exit.retain(|k, _| keys.contains(k));
        self.last_persist.retain(|k, _| keys.contains(k));
        self.last_emit.retain(|k, _| keys.contains(k));
    }
}

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
}

fn elapsed_ms(now: DateTime<Utc>, then: Option<&DateTime<Utc>>) -> i64 {
    then.map(|t| (now - *t).num_milliseconds())
        .unwrap_or(i64::MAX)
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
        })
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
    /// the in-memory legs (the stored rows stay; the next start reloads
    /// them). Idempotent.
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

    /// Entries across every per-leg map (bounded by the managed legs).
    pub fn tracked_entries(&self) -> usize {
        let i = self.inner.lock();
        i.states.len()
            + i.inflight.len()
            + i.last_exit.len()
            + i.last_persist.len()
            + i.last_emit.len()
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
        let (to_add, to_remove) = {
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
            for sl in rows {
                inner.next_gen += 1;
                let gen = inner.next_gen;
                let key = slkey(&sl.mode, &sl.symbol, &sl.exchange, &sl.product);
                states.insert(key, Leg { gen, sl });
            }
            inner.states = states;
            let keep: HashSet<String> = inner.states.keys().cloned().collect();
            inner.inflight.retain(|k| keep.contains(k));
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
            (add, remove)
        };
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
        let mut exits: Vec<(String, u64, SlState, Option<BreachReason>)> = Vec::new();
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
        gen: u64,
        sl: SlState,
        reason: Option<BreachReason>,
        ltp: f64,
    ) {
        let outcome = self.execute_exit(&sl).await;
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
            ExitOutcome::Closed { orders } => {
                tracing::info!(
                    "Scalping {} exit for {} sent in {} order(s) near {:.2}",
                    why,
                    sl.symbol,
                    orders,
                    ltp
                );
                self.clear(&key, gen, &sl).await;
            }
            ExitOutcome::Refused(msg) => {
                tracing::error!(
                    "Scalping {} exit for {} was refused and the position is still open; it stays protected and will be retried: {}",
                    why,
                    sl.symbol,
                    msg
                );
            }
        }
        // Release the claim only after the clear, so no tick in between can
        // send a second exit for a leg that was just closed.
        self.inner.lock().inflight.remove(&key);
    }

    /// Exit one leg in the mode it was opened in: read its net position,
    /// then send whole-lot, freeze-safe MARKET orders that flatten it.
    pub async fn execute_exit(&self, sl: &SlState) -> ExitOutcome {
        let mode = if sl.mode == MODE_LIVE {
            RunMode::Live
        } else {
            RunMode::Sandbox
        };
        if let Err(e) = self.gateway.authorised(mode) {
            return ExitOutcome::Refused(e);
        }
        let book = match self.gateway.book(mode, Book::Positions).await {
            Ok(b) => b,
            Err(body) => {
                let msg = body
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("The position book could not be read")
                    .to_string();
                // Never treated as flat: an unreadable book is not a closed
                // position.
                return ExitOutcome::Refused(msg);
            }
        };
        let net = net_quantity(&book, &sl.symbol, &sl.exchange, &sl.product);
        if net == 0 {
            return ExitOutcome::Flat;
        }
        let action = if net > 0 { "SELL" } else { "BUY" };
        let qty = net.abs();
        let chunk = match exit_chunk(&self.symbols, &sl.symbol, &sl.exchange, qty) {
            Ok(c) => c,
            Err(e) => return ExitOutcome::Refused(e),
        };
        let parts = chunks(qty, chunk);
        let n = parts.len();
        for q in parts {
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
            if !r.ok {
                return ExitOutcome::Refused(
                    r.error
                        .unwrap_or_else(|| "The exit order was rejected".into()),
                );
            }
        }
        ExitOutcome::Closed { orders: n }
    }

    /// Forget a closed leg: its row, its state, its subscription when no
    /// other leg needs the instrument, then tell the terminal.
    async fn clear(&self, key: &str, _gen: u64, sl: &SlState) {
        if let Err(e) = self
            .store
            .delete_sl(&sl.symbol, &sl.exchange, &sl.product, Some(&sl.mode))
        {
            tracing::error!(
                "Scalping stop for {} could not be removed: {}",
                sl.symbol,
                e
            );
        }
        let symkey: Key = (sl.symbol.clone(), sl.exchange.clone());
        let unsub = {
            let mut inner = self.inner.lock();
            inner.states.remove(key);
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
