//! The sandbox execution task (web `websocket_execution_engine.py`,
//! `execution_thread.py`, the square-off scheduler thread): one owned tokio
//! task per running engine.
//!
//! It selects over:
//! * the tick stream: a tick for a watched symbol evaluates that symbol's
//!   resting orders and GTT legs and (throttled by `mtm_update_interval`)
//!   marks its open positions to market;
//! * engine commands from the order path (`Watch`, `Rebuild`,
//!   `ConfigChanged`), on a bounded channel; each acts on what the
//!   database says when it is handled, since a command can arrive after
//!   one sent for a later commit (a `Watch` is checked again);
//! * a one-second clock that runs the day schedule's due jobs;
//! * the polling fallback every `order_check_interval` seconds, which
//!   quotes only the symbols whose own feed is stale: no usable tick for
//!   `stale_feed_after` (30 s) since the last one or since watching began;
//! * a one-minute maintenance pass (GTT reclaim and expiry, watch-set
//!   rebuild).
//!
//! Freshness is kept per symbol, from ticks that passed the watched and
//! positive-price checks, so a live feed for one symbol (or ticks for
//! symbols the sandbox does not need) never hides another symbol's stalled
//! feed. A watched symbol that stays stale is re-subscribed with a backoff
//! that doubles up to [`RESUBSCRIBE_CAP`]. (The web keeps one global
//! timestamp and never re-subscribes.)
//!
//! The watch set is keyed by symbol and holds only symbols with a resting
//! order, a pending GTT leg or an open position; a symbol leaves the set (and
//! the feed is told to stop streaming it) when nothing needs it any more.
//! Every per-symbol map is keyed by the watch set and pruned with it.
//! Stopping is graceful: the task is cancelled, joined, and every watched
//! symbol is released.

use super::core::{blocking, Core, EngineCmd, ENGINE_CMD_CAPACITY};
use super::execution;
use super::gtt;
use super::positions;
use super::quotes::{Tick, TickSource};
use super::types::SymbolKey;
use super::SandboxInner;
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

/// A running engine: drop-safe, but [`EngineHandle::stop`] is the graceful
/// path.
pub struct EngineHandle {
    cancel: CancellationToken,
    join: Option<JoinHandle<()>>,
    pub(crate) ticks: Arc<dyn TickSource>,
}

impl EngineHandle {
    /// Cancel the task and wait for it to finish its current step.
    pub async fn stop(mut self) {
        self.cancel.cancel();
        if let Some(j) = self.join.take() {
            if tokio::time::timeout(Duration::from_secs(10), j)
                .await
                .is_err()
            {
                tracing::warn!("Sandbox engine did not stop within 10 s");
            }
        }
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().map(|j| j.is_finished()).unwrap_or(true)
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(j) = self.join.take() {
            j.abort();
        }
    }
}

/// Symbols that need ticks right now.
pub(crate) fn needed_symbols(conn: &rusqlite::Connection) -> rusqlite::Result<HashSet<SymbolKey>> {
    let mut out = HashSet::new();
    for sql in [
        "SELECT DISTINCT symbol, exchange FROM sandbox_orders WHERE order_status IN ('open','trigger pending')",
        "SELECT DISTINCT g.symbol, g.exchange FROM sandbox_gtt g JOIN sandbox_gtt_legs l ON l.gtt_id = g.gtt_id
           WHERE g.gtt_status = 'active' AND l.leg_status = 'pending'",
        "SELECT DISTINCT symbol, exchange FROM sandbox_positions WHERE quantity != 0",
    ] {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map([], |r| Ok(SymbolKey::new(r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            out.insert(r?);
        }
    }
    Ok(out)
}

fn symbol_needed(conn: &rusqlite::Connection, key: &SymbolKey) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM sandbox_orders WHERE symbol = ?1 AND exchange = ?2
              AND order_status IN ('open','trigger pending'))
         + (SELECT COUNT(*) FROM sandbox_positions WHERE symbol = ?1 AND exchange = ?2 AND quantity != 0)
         + (SELECT COUNT(*) FROM sandbox_gtt g JOIN sandbox_gtt_legs l ON l.gtt_id = g.gtt_id
              WHERE g.symbol = ?1 AND g.exchange = ?2 AND g.gtt_status = 'active' AND l.leg_status = 'pending')",
        params![key.symbol, key.exchange],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Does anything still need ticks for `key`? A read that fails answers yes:
/// a symbol watched a little longer costs a subscription, one dropped too
/// early costs fills, and the next rebuild settles it either way.
async fn still_needed(core: &Arc<Core>, key: &SymbolKey) -> bool {
    let k = key.clone();
    blocking(core, move |c| {
        Ok(c.db.with_conn(|conn| symbol_needed(conn, &k))?)
    })
    .await
    .unwrap_or(true)
}

/// Longest wait between re-subscriptions of a symbol whose feed stays
/// stale.
pub const RESUBSCRIBE_CAP: Duration = Duration::from_secs(300);

/// Re-subscription schedule of one stale symbol.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    attempts: u32,
    next: Instant,
}

/// Wait after the `attempts`-th re-subscription: `base` doubling, capped.
fn resubscribe_delay(base: Duration, attempts: u32) -> Duration {
    let base = base.max(Duration::from_secs(1));
    base.saturating_mul(1u32 << attempts.min(16))
        .min(RESUBSCRIBE_CAP)
}

#[derive(Default)]
struct State {
    watched: HashSet<SymbolKey>,
    /// Last MTM write per watched symbol (bounded by the watch set).
    last_mtm: HashMap<SymbolKey, Instant>,
    /// When each watched symbol last had a usable tick, or began being
    /// watched (bounded by the watch set).
    last_seen: HashMap<SymbolKey, Instant>,
    /// Re-subscription backoff of watched symbols whose feed is stale
    /// (bounded by the watch set; cleared by the symbol's next tick).
    resubscribe: HashMap<SymbolKey, Backoff>,
}

impl State {
    fn watch(&mut self, ticks: &dyn TickSource, key: &SymbolKey) {
        if self.watched.insert(key.clone()) {
            ticks.watch(key);
            self.last_seen.insert(key.clone(), Instant::now());
        }
    }

    fn forget(&mut self, ticks: &dyn TickSource, key: &SymbolKey) {
        if self.watched.remove(key) {
            ticks.unwatch(key);
        }
        self.last_mtm.remove(key);
        self.last_seen.remove(key);
        self.resubscribe.remove(key);
    }

    fn sync_watch(&mut self, ticks: &dyn TickSource, wanted: HashSet<SymbolKey>) {
        let gone: Vec<SymbolKey> = self.watched.difference(&wanted).cloned().collect();
        for k in &gone {
            self.forget(ticks, k);
        }
        for k in &wanted {
            self.watch(ticks, k);
        }
    }

    /// A usable tick for a watched symbol arrived at `at`.
    fn seen(&mut self, key: &SymbolKey, at: Instant) {
        let latest = self.last_seen.get(key).map(|t| (*t).max(at)).unwrap_or(at);
        self.last_seen.insert(key.clone(), latest);
        self.resubscribe.remove(key);
    }

    /// Watched symbols whose feed is live at `now`.
    fn fresh(&self, stale_after: Duration, now: Instant) -> HashSet<SymbolKey> {
        self.last_seen
            .iter()
            .filter(|(k, t)| {
                self.watched.contains(*k) && now.saturating_duration_since(**t) <= stale_after
            })
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Re-subscribe each watched symbol that is not fresh, when its backoff
    /// allows.
    fn resubscribe_stale(
        &mut self,
        ticks: &dyn TickSource,
        fresh: &HashSet<SymbolKey>,
        stale_after: Duration,
        now: Instant,
    ) {
        for k in self.watched.iter().filter(|k| !fresh.contains(*k)) {
            let b = self.resubscribe.entry(k.clone()).or_insert(Backoff {
                attempts: 0,
                next: now,
            });
            if now < b.next {
                continue;
            }
            tracing::info!(
                "Sandbox feed for {}:{} has been quiet; subscribing again",
                k.exchange,
                k.symbol
            );
            ticks.resubscribe(k);
            b.next = now + resubscribe_delay(stale_after, b.attempts);
            b.attempts = b.attempts.saturating_add(1);
        }
    }

    fn release_all(&mut self, ticks: &dyn TickSource) {
        for k in self.watched.drain() {
            ticks.unwatch(&k);
        }
        self.last_mtm.clear();
        self.last_seen.clear();
        self.resubscribe.clear();
    }
}

/// Start the engine task.
pub(crate) fn spawn(
    inner: Weak<SandboxInner>,
    core: Arc<Core>,
    ticks: Arc<dyn TickSource>,
) -> EngineHandle {
    let cancel = CancellationToken::new();
    let (tx, rx) = mpsc::channel(ENGINE_CMD_CAPACITY);
    *core.engine_tx.write() = Some(tx);
    let rx_ticks = ticks.subscribe_ticks();
    let join = tokio::spawn(run(
        inner,
        core,
        ticks.clone(),
        rx_ticks,
        rx,
        cancel.clone(),
    ));
    EngineHandle {
        cancel,
        join: Some(join),
        ticks,
    }
}

async fn rebuild(core: &Arc<Core>, state: &mut State, ticks: &dyn TickSource) {
    match blocking(core, |c| Ok(c.db.with_conn(needed_symbols)?)).await {
        Ok(wanted) => state.sync_watch(ticks, wanted),
        Err(e) => tracing::warn!(
            "Sandbox engine could not rebuild its watch list: {}",
            e.message
        ),
    }
}

/// The order path's `Watch`: watch the symbol if it still needs ticks now.
/// A place reads its order as resting and only then sends the command, and
/// nothing stops a cancel from committing in between: `orders::cancel`
/// (cancel all among its callers) takes no position lock and sends its
/// `Rebuild` ahead of the `Watch`, and the auto square-off cancels inside
/// this task. Acting on the stale `Watch` then left the symbol watched and
/// subscribed with nothing needing it until the next maintenance rebuild
/// (web 398f614cc: never the symbol left subscribed). Read here, after
/// every command sent before this one was handled, the answer is current:
/// a cancel committed after this read sends a `Rebuild` that comes next.
async fn watch_if_needed(
    core: &Arc<Core>,
    state: &mut State,
    ticks: &dyn TickSource,
    key: SymbolKey,
) {
    if !state.watched.contains(&key) && still_needed(core, &key).await {
        state.watch(ticks, &key);
    }
}

async fn on_tick(core: &Arc<Core>, state: &mut State, ticks: &dyn TickSource, tick: Tick) {
    let key = tick.key();
    if !state.watched.contains(&key) || tick.ltp <= rust_decimal::Decimal::ZERO {
        return;
    }
    // Only a usable tick for a symbol the sandbox needs counts as that
    // symbol's feed being live, as of when it was received.
    state.seen(&key, Instant::from_std(tick.received_at));
    let q = tick.quote();
    let filled = match execution::on_price(core, &key, &q).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "Sandbox tick processing for {} failed: {}",
                key.symbol,
                e.message
            );
            0
        }
    };
    let fired = match gtt::on_price(core, &key, &q).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "Sandbox GTT evaluation for {} failed: {}",
                key.symbol,
                e.message
            );
            0
        }
    };
    let interval = match core.config() {
        Ok(c) => c.mtm_update_interval,
        Err(_) => 5,
    };
    if interval > 0 {
        let due = state
            .last_mtm
            .get(&key)
            .map(|t| t.elapsed() >= Duration::from_secs(interval))
            .unwrap_or(true);
        if due {
            state.last_mtm.insert(key.clone(), Instant::now());
            let k = key.clone();
            let ltp = tick.ltp;
            if let Err(e) = blocking(core, move |c| {
                Ok(c.db.with_tx(|tx| positions::mtm_from_tick(tx, c, &k, ltp))?)
            })
            .await
            {
                tracing::debug!("Sandbox MTM for {} skipped: {}", key.symbol, e.message);
            }
        }
    }
    if (filled > 0 || fired > 0) && !still_needed(core, &key).await {
        state.forget(ticks, &key);
    }
}

async fn run(
    inner: Weak<SandboxInner>,
    core: Arc<Core>,
    ticks: Arc<dyn TickSource>,
    rx_ticks: broadcast::Receiver<Tick>,
    mut cmds: mpsc::Receiver<EngineCmd>,
    cancel: CancellationToken,
) {
    let mut state = State::default();
    rebuild(&core, &mut state, ticks.as_ref()).await;
    let mut rx: Option<broadcast::Receiver<Tick>> = Some(rx_ticks);

    let mut clock_tick = tokio::time::interval(Duration::from_secs(1));
    clock_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let check = core
        .config()
        .map(|c| c.order_check_interval)
        .unwrap_or(5)
        .max(1);
    let mut fallback = tokio::time::interval(Duration::from_secs(check));
    fallback.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut maintenance = tokio::time::interval(Duration::from_secs(60));
    maintenance.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let stale_after = core.opts.stale_feed_after;
    tracing::info!("Sandbox engine started");

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            msg = async {
                match rx.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match msg {
                    Ok(tick) => on_tick(&core, &mut state, ticks.as_ref(), tick).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Sandbox engine missed {} ticks; checking resting orders now", n);
                        if let Err(e) = execution::poll_once(&core).await {
                            tracing::warn!("Sandbox poll after missed ticks failed: {}", e.message);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::warn!("Sandbox tick stream closed; relying on the polling fallback");
                        rx = None;
                    }
                }
            }
            Some(cmd) = cmds.recv() => match cmd {
                EngineCmd::Watch(k) => watch_if_needed(&core, &mut state, ticks.as_ref(), k).await,
                EngineCmd::Rebuild => rebuild(&core, &mut state, ticks.as_ref()).await,
                EngineCmd::ConfigChanged => {
                    if let Some(i) = inner.upgrade() {
                        i.invalidate_schedule();
                    }
                    let check = core.config().map(|c| c.order_check_interval).unwrap_or(5).max(1);
                    fallback = tokio::time::interval(Duration::from_secs(check));
                    fallback.set_missed_tick_behavior(MissedTickBehavior::Delay);
                }
            },
            _ = clock_tick.tick() => {
                let Some(i) = inner.upgrade() else { break };
                if let Err(e) = i.run_due_jobs_inner().await {
                    tracing::warn!("Sandbox scheduled job failed: {}", e.message);
                }
            }
            _ = fallback.tick() => {
                // Quote only what no live feed covers: watched symbols gone
                // quiet, and resting orders or GTT legs on symbols not
                // (yet) watched.
                let now = Instant::now();
                let fresh = state.fresh(stale_after, now);
                state.resubscribe_stale(ticks.as_ref(), &fresh, stale_after, now);
                match execution::poll_stale(&core, &fresh).await {
                    Ok(s) if s.filled > 0 || s.gtt_fired > 0 => {
                        tracing::info!("Sandbox polling fallback: {:?}", s);
                        rebuild(&core, &mut state, ticks.as_ref()).await;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("Sandbox polling fallback failed: {}", e.message),
                }
            }
            _ = maintenance.tick() => {
                if let Err(e) = gtt::maintain(&core).await {
                    tracing::warn!("Sandbox GTT maintenance failed: {}", e.message);
                }
                rebuild(&core, &mut state, ticks.as_ref()).await;
            }
        }
    }
    state.release_all(ticks.as_ref());
    *core.engine_tx.write() = None;
    tracing::info!("Sandbox engine stopped");
}

#[cfg(test)]
mod tests;
