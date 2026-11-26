//! Prices for live runs (web `services/strategy_module/tick_feed.py`).
//!
//! Subscriptions are refcounted per run, so two strategies holding one
//! contract share one and it is released when the last lets go. Both the
//! streaming feed and the REST fallback decide through `decide_tick`: a leg
//! that has fallen back to polling is risk-evaluated on polled prices too.
//!
//! Two owned tasks, ended on module shutdown:
//!
//! * the **reader** takes every tick of the market-data broadcast and
//!   decides each price of a held symbol at once, in the run's lock
//!   (`StrategyModule::decide_tick`), so every price is decided, in the
//!   order it came. It never waits on an action (an exit, a stop, an event
//!   write) or on the database, so neither can make the channel overrun it
//!   and skip a deciding price. What a run's ticks need is read when its
//!   prices are subscribed (`follow_prices`); what the reader still finds
//!   missing is read in an owned task (`strategy::tick_context`). A tick it
//!   does miss is counted for health;
//! * the **evaluator** acts on what was decided (`pending`), run by run in
//!   the order they decided, and polls stale symbols every 2 s (stale after
//!   10 s without a streamed tick);
//! * the **renewer** takes again, every 2 s, what the price source holds
//!   from a broker session that ended (`keep_renewed`; the scalping monitor
//!   runs one over its own source).

pub(crate) mod pending;

use super::StrategyModule;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription, MarketEvent};
use crate::brokers::types::QuoteKey;
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub const STALE_AFTER: Duration = Duration::from_secs(10);
pub const POLL_EVERY: Duration = Duration::from_secs(2);
pub const POLL_BATCH_MAX: usize = 50;

pub type Key = (String, String);

/// Where prices come from. Production is the market-data feed plus broker
/// multiquotes; tests inject ticks directly.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Ask for these instruments' prices; which of them were taken. What
    /// was refused holds nothing, and must not be released later: another
    /// consumer's reference would go instead (review X6).
    async fn subscribe(&self, keys: &[Key]) -> Vec<Key>;
    async fn unsubscribe(&self, keys: &[Key]);
    fn ticks(&self) -> broadcast::Receiver<MarketEvent>;
    /// Last prices over REST, for symbols the stream has gone quiet on.
    async fn poll(&self, keys: &[Key]) -> Vec<(Key, f64)>;
    /// The reader fell behind the ticks and skipped `n` (health counts it
    /// against the feed, NEW-REL-05).
    fn note_skipped(&self, _n: u64) {}
    /// Take again, in the broker session running now, what this source
    /// holds from one that ended: the feed forgot those references when it
    /// ended. Web 9512aa5fe (marketcalls/openalgo#2277): the internal
    /// client the web's strategies and scalping stops read restores its
    /// subscriptions once it has authenticated again.
    async fn renew(&self) {}
}

#[derive(Default)]
struct Inner {
    refs: HashMap<Key, u32>,
    runs: HashMap<i64, Vec<Key>>,
    /// The instruments the price source took, the only ones released.
    held: HashSet<Key>,
    last_tick: HashMap<Key, Instant>,
}

/// Refcounted run subscriptions over one price source, and the decisions
/// waiting for the evaluator.
pub struct TickFeed {
    source: Option<Arc<dyn PriceSource>>,
    inner: Mutex<Inner>,
    pub(crate) pending: pending::Pending,
}

impl TickFeed {
    pub fn new(source: Option<Arc<dyn PriceSource>>) -> Self {
        Self {
            source,
            inner: Mutex::new(Inner::default()),
            pending: pending::Pending::default(),
        }
    }

    /// What the strategy tick path read, combined and missed, and the
    /// exits waiting to be acted on at `now` (health).
    pub fn health(&self, now: chrono::DateTime<chrono::Utc>) -> serde_json::Value {
        self.pending.health(now)
    }

    /// The count of ticks the reader missed since start, when it grew
    /// since the last call (the health alert raises once per growth).
    pub fn ticks_missed_unreported(&self) -> Option<u64> {
        self.pending.missed_since_alert()
    }

    /// The strategies whose exit or stop waits to be acted on at `now`, and
    /// how long the oldest has waited (the slow-exit health alert).
    pub(crate) fn exit_wait(&self, now: chrono::DateTime<chrono::Utc>) -> pending::ExitWait {
        self.pending.exit_wait(now)
    }

    /// Add this run's instruments; subscribes only those newly referenced.
    pub async fn add_run(&self, run_id: i64, keys: &[Key]) {
        let fresh: Vec<Key> = {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            let mine = inner.runs.entry(run_id).or_default();
            let mut new_for_run = Vec::new();
            for k in keys {
                if !mine.contains(k) {
                    mine.push(k.clone());
                    new_for_run.push(k.clone());
                }
            }
            let mut fresh = Vec::new();
            for k in new_for_run {
                let n = inner.refs.entry(k.clone()).or_insert(0);
                *n += 1;
                if *n == 1 {
                    fresh.push(k);
                }
            }
            fresh
        };
        if let (Some(src), false) = (&self.source, fresh.is_empty()) {
            let taken = src.subscribe(&fresh).await;
            let orphans = self.record_taken(taken);
            if !orphans.is_empty() {
                src.unsubscribe(&orphans).await;
            }
        }
    }

    /// Remember what the source took; what no run holds any more (its run
    /// ended while the subscribe was answered) is returned, to be released.
    fn record_taken(&self, taken: Vec<Key>) -> Vec<Key> {
        let mut inner = self.inner.lock();
        let mut orphans = Vec::new();
        for k in taken {
            if inner.refs.contains_key(&k) {
                inner.held.insert(k);
            } else {
                orphans.push(k);
            }
        }
        orphans
    }

    /// Release this run's instruments; unsubscribes those no run holds.
    pub async fn remove_run(&self, run_id: i64) {
        let gone: Vec<Key> = {
            let mut inner = self.inner.lock();
            let keys = inner.runs.remove(&run_id).unwrap_or_default();
            let mut gone = Vec::new();
            for k in keys {
                if let Some(n) = inner.refs.get_mut(&k) {
                    *n -= 1;
                    if *n == 0 {
                        inner.refs.remove(&k);
                        inner.last_tick.remove(&k);
                        if inner.held.remove(&k) {
                            gone.push(k);
                        }
                    }
                }
            }
            gone
        };
        if let (Some(src), false) = (&self.source, gone.is_empty()) {
            src.unsubscribe(&gone).await;
        }
    }

    /// Release every subscription (shutdown).
    pub async fn release_all(&self) {
        let all: Vec<Key> = {
            let mut inner = self.inner.lock();
            inner.runs.clear();
            inner.last_tick.clear();
            inner.refs.clear();
            inner.held.drain().collect()
        };
        self.pending.clear();
        if let (Some(src), false) = (&self.source, all.is_empty()) {
            src.unsubscribe(&all).await;
        }
    }

    /// Instruments currently subscribed (hygiene tests).
    pub fn subscribed(&self) -> Vec<Key> {
        let mut v: Vec<Key> = self.inner.lock().refs.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn runs_tracked(&self) -> usize {
        self.inner.lock().runs.len()
    }

    /// Record a streamed price of a held instrument as fresh and read;
    /// false when no run holds it (the price is dropped).
    fn note_tick(&self, key: Key) -> bool {
        {
            let mut inner = self.inner.lock();
            if !inner.refs.contains_key(&key) {
                return false;
            }
            inner.last_tick.insert(key, Instant::now());
        }
        self.pending.note_read();
        true
    }

    fn stale(&self) -> Vec<Key> {
        let now = Instant::now();
        let inner = self.inner.lock();
        inner
            .refs
            .keys()
            .filter(|k| {
                inner
                    .last_tick
                    .get(*k)
                    .map(|t| now.saturating_duration_since(*t) >= STALE_AFTER)
                    .unwrap_or(true)
            })
            .take(POLL_BATCH_MAX)
            .cloned()
            .collect()
    }
}

/// Start the owned reader, evaluator and renewer (a no-op without a price
/// source).
pub fn start(module: &Arc<StrategyModule>) {
    let Some(source) = module.feed.source.clone() else {
        return;
    };
    spawn_reader(module, source.clone());
    spawn_evaluator(module, source.clone());
    module.spawn(keep_renewed(source, module.shutdown_token()));
}

/// Take again, every `POLL_EVERY` until `stop`, what `source` holds from a
/// broker session that ended (`PriceSource::renew`). A task of its own, so
/// a broker slow to answer a subscribe holds neither the ticks nor the
/// exits.
pub(crate) async fn keep_renewed(source: Arc<dyn PriceSource>, stop: CancellationToken) {
    let mut every = tokio::time::interval(POLL_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = every.tick() => source.renew().await,
        }
    }
}

/// Read every tick as it comes and decide the held ones; nothing here
/// waits on an action.
fn spawn_reader(module: &Arc<StrategyModule>, source: Arc<dyn PriceSource>) {
    let mut rx = source.ticks();
    let weak = Arc::downgrade(module);
    let token = module.shutdown_token();
    module.spawn(async move {
        loop {
            let ev = tokio::select! {
                _ = token.cancelled() => break,
                ev = rx.recv() => ev,
            };
            let Some(m) = weak.upgrade() else { break };
            match ev {
                Ok(ev) => {
                    if let FeedEvent::Tick(t) = ev.as_ref() {
                        let usable = t.ltp.is_finite() && t.ltp > 0.0;
                        if usable && m.feed.note_tick((t.symbol.clone(), t.exchange.clone())) {
                            m.decide_tick(&t.symbol, &t.exchange, t.ltp);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    m.feed.pending.note_lagged(n);
                    source.note_skipped(n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Act on what was decided whenever the reader leaves a decision, and
/// price stale symbols over REST.
fn spawn_evaluator(module: &Arc<StrategyModule>, source: Arc<dyn PriceSource>) {
    let weak = Arc::downgrade(module);
    let token = module.shutdown_token();
    let wake = module.feed.pending.wake.clone();
    module.spawn(async move {
        let mut poll = tokio::time::interval(POLL_EVERY);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = wake.notified() => {
                    let Some(m) = weak.upgrade() else { break };
                    m.act_on_decided().await;
                }
                _ = poll.tick() => {
                    let Some(m) = weak.upgrade() else { break };
                    let stale = m.feed.stale();
                    if stale.is_empty() {
                        continue;
                    }
                    for ((symbol, exchange), ltp) in source.poll(&stale).await {
                        if ltp.is_finite() && ltp > 0.0 {
                            m.process_tick(&symbol, &exchange, ltp).await;
                        }
                    }
                }
            }
        }
    });
}

/// The production source: the market-data feed and broker multiquotes.
///
/// A reference belongs to the broker session that took it (the bus's
/// session generation, as Flow's do, `flow::triggers::feed`). When a
/// session ends the feed forgets every reference
/// (`WebSocketManager::disconnect`), so one taken in an ended session is
/// never given back, where it would take another holder's in its place
/// (an 8765 client, a Flow node, the other of the strategies and the
/// scalping stops), and `renew` takes it again in the session running now.
/// The session is read before the feed is asked, so a reference taken
/// while sessions change counts as the old one's and is taken again: such
/// an instrument may stream on one reference too many until the next
/// session change.
pub struct FeedPrices {
    ctx: Weak<AppState>,
    feed: Arc<crate::websocket::WebSocketManager>,
    /// Who reads these prices, for the feed's skip counts.
    reader: crate::websocket::Reader,
    /// What this source holds, with the broker session each was taken in.
    held: Mutex<HashMap<Key, u64>>,
    /// One renewal at a time.
    renewing: tokio::sync::Mutex<()>,
}

impl FeedPrices {
    pub fn new(
        ctx: Weak<AppState>,
        feed: Arc<crate::websocket::WebSocketManager>,
        reader: crate::websocket::Reader,
    ) -> Self {
        Self {
            ctx,
            feed,
            reader,
            held: Mutex::new(HashMap::new()),
            renewing: tokio::sync::Mutex::new(()),
        }
    }

    /// The broker session the feed counts references in now.
    fn session(&self) -> Option<u64> {
        self.ctx.upgrade().map(|ctx| ctx.bus.session_generation())
    }

    /// The same, only while a broker is logged in: nothing streams before.
    fn live_session(&self) -> Option<u64> {
        let ctx = self.ctx.upgrade()?;
        ctx.get_broker_session()
            .map(|_| ctx.bus.session_generation())
    }

    /// Ask the feed for `keys`: each one the master resolves, with the
    /// feed's answer.
    async fn admit(&self, keys: &[Key]) -> Vec<(Key, Result<(), String>)> {
        let subs = self.subscriptions(keys);
        if subs.is_empty() {
            return Vec::new();
        }
        let asked: Vec<Key> = subs
            .iter()
            .map(|s| (s.symbol.clone(), s.exchange.clone()))
            .collect();
        asked.into_iter().zip(self.feed.admit(subs).await).collect()
    }

    /// Give the feed back references it counts for this source.
    async fn release(&self, keys: &[Key]) {
        let subs = self.subscriptions(keys);
        if !subs.is_empty() {
            let _ = self.feed.unsubscribe(subs).await;
        }
    }

    fn subscriptions(&self, keys: &[Key]) -> Vec<FeedSubscription> {
        let Some(ctx) = self.ctx.upgrade() else {
            return vec![];
        };
        keys.iter()
            .filter_map(|(s, e)| {
                let row = ctx.symbols.by_symbol(e, s)?;
                Some(FeedSubscription {
                    symbol: s.clone(),
                    exchange: e.clone(),
                    token: row.token.clone(),
                    brsymbol: row.br_symbol().to_string(),
                    brexchange: row.br_exchange().to_string(),
                    mode: FeedMode::Ltp,
                    depth: 5,
                })
            })
            .collect()
    }
}

#[async_trait]
impl PriceSource for FeedPrices {
    async fn subscribe(&self, keys: &[Key]) -> Vec<Key> {
        let Some(session) = self.session() else {
            return Vec::new();
        };
        let mut taken = Vec::with_capacity(keys.len());
        for (key, answer) in self.admit(keys).await {
            match answer {
                Ok(()) => taken.push(key),
                // The REST fallback keeps the run priced.
                Err(why) => tracing::warn!(
                    "{} on {} could not be streamed for a strategy: {}",
                    key.0,
                    key.1,
                    why
                ),
            }
        }
        let mut held = self.held.lock();
        for k in &taken {
            held.insert(k.clone(), session);
        }
        taken
    }

    /// Only what this session counts for this source goes back: the feed
    /// forgot a reference of an ended session, and giving one back would
    /// take another holder's (the web's old-socket cleanup reaching the
    /// adapter built for new credentials, 9512aa5fe).
    async fn unsubscribe(&self, keys: &[Key]) {
        let Some(session) = self.session() else {
            return;
        };
        let ours: Vec<Key> = {
            let mut held = self.held.lock();
            keys.iter()
                .filter(|k| held.remove(*k) == Some(session))
                .cloned()
                .collect()
        };
        self.release(&ours).await;
    }

    async fn renew(&self) {
        let Ok(_alone) = self.renewing.try_lock() else {
            return;
        };
        let Some(session) = self.live_session() else {
            return;
        };
        let lapsed: Vec<Key> = self
            .held
            .lock()
            .iter()
            .filter(|(_, taken_in)| **taken_in != session)
            .map(|(k, _)| k.clone())
            .collect();
        if lapsed.is_empty() {
            return;
        }
        let (mut renewed, mut extra) = (0usize, Vec::new());
        for (key, answer) in self.admit(&lapsed).await {
            if let Err(why) = answer {
                // Kept as it is: the next renewal asks again.
                tracing::debug!("{} on {} not streamed again yet: {}", key.0, key.1, why);
                continue;
            }
            let mut held = self.held.lock();
            match held.get_mut(&key) {
                Some(taken_in) if *taken_in != session => {
                    *taken_in = session;
                    renewed += 1;
                }
                // Given back while it was asked for, or already taken in
                // this session: one reference too many.
                _ => extra.push(key),
            }
        }
        if !extra.is_empty() && self.session() == Some(session) {
            self.release(&extra).await;
        }
        if renewed > 0 {
            tracing::info!(
                "{} instrument(s) the {:?} reader follows were subscribed again in the new broker session",
                renewed,
                self.reader
            );
        }
    }

    fn ticks(&self) -> broadcast::Receiver<MarketEvent> {
        self.feed.subscribe_ticks()
    }

    fn note_skipped(&self, n: u64) {
        self.feed.stats().note_skipped(self.reader, n);
    }

    async fn poll(&self, keys: &[Key]) -> Vec<(Key, f64)> {
        let Some(ctx) = self.ctx.upgrade() else {
            return vec![];
        };
        let Ok(h) = crate::services::core::broker_handle(&ctx) else {
            return vec![];
        };
        drop(ctx);
        let qk: Vec<QuoteKey> = keys
            .iter()
            .map(|(s, e)| QuoteKey::new(e.clone(), s.clone()))
            .collect();
        match h.broker.get_multiquotes(&h.auth, &qk).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|r| {
                    let q = r.data?;
                    Some(((r.symbol, r.exchange), q.ltp))
                })
                .collect(),
            Err(e) => {
                tracing::debug!("Strategy price poll failed: {}", e.code());
                vec![]
            }
        }
    }
}
