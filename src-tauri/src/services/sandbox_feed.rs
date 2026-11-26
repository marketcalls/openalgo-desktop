//! Adapters that connect the sandbox engine to the live app:
//!
//! * [`MasterSymbols`]: the shared symbol master as a `SymbolSource`;
//! * [`LiveQuotes`]: the connected broker's quote API as a `QuoteSource`;
//! * [`FeedTicks`]: the market-data feed (the `WebSocketManager` broadcast)
//!   as a `TickSource`; the subscribes and releases it asks for wait in
//!   `calls` for its worker, which keeps what it holds in `held`, each
//!   reference with the broker session that took it.
//!
//! The engine never sees a broker; these are the only seams.

mod calls;
mod held;

use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::QuoteKey;
use crate::events::EventBus;
use crate::sandbox::{Quote, QuoteSource, SymbolKey, SymbolMeta, SymbolSource, Tick, TickSource};
use crate::state::AppState;
use crate::websocket::WebSocketManager;
use calls::{FeedCalls, Refused, WatchCmd};
use held::{key_of, Held};
use parking_lot::Mutex;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::broadcast;

/// The symbol master as the engine's instrument source.
pub struct MasterSymbols(pub SymbolResolver);

impl SymbolSource for MasterSymbols {
    fn lookup(&self, symbol: &str, exchange: &str) -> Option<SymbolMeta> {
        let g = self.0.snapshot();
        g.by_symbol(exchange, symbol).map(|r| SymbolMeta {
            lotsize: i64::from(r.lot_size.max(1)),
            // Crypto masters carry a multiplier (0.001 BTC per BTCUSD
            // contract); everything else is 1.
            contract_value: g
                .contract_value(exchange, &r.token)
                .map(crate::sandbox::types::dec_from_f64)
                .filter(|v| *v > Decimal::ZERO)
                .unwrap_or(Decimal::ONE),
            expiry: parse_oa_expiry(&r.expiry),
        })
    }
}

/// Quotes from the connected broker (none while no broker is connected).
pub struct LiveQuotes {
    ctx: Weak<AppState>,
}

impl LiveQuotes {
    pub fn new(ctx: Weak<AppState>) -> Self {
        Self { ctx }
    }
}

/// A broker quote as the engine's quote, stamped with the time it was
/// received (the broker's own timestamp is not relied on).
fn to_quote(q: &crate::brokers::types::Quote) -> Quote {
    Quote::from_f64(q.ltp, q.bid, q.ask, q.high, q.low).received(std::time::Instant::now())
}

#[async_trait::async_trait]
impl QuoteSource for LiveQuotes {
    async fn quote(&self, symbol: &str, exchange: &str) -> Option<Quote> {
        let ctx = self.ctx.upgrade()?;
        let h = super::core::broker_handle(&ctx).ok()?;
        drop(ctx);
        match h
            .broker
            .get_quote(&h.auth, &QuoteKey::new(exchange, symbol))
            .await
        {
            Ok(q) if q.ltp > 0.0 => Some(to_quote(&q)),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(
                    "Sandbox quote for {}:{} failed: {}",
                    exchange,
                    symbol,
                    e.code()
                );
                None
            }
        }
    }

    async fn quotes(&self, keys: &[SymbolKey]) -> HashMap<SymbolKey, Quote> {
        let mut out = HashMap::new();
        let Some(ctx) = self.ctx.upgrade() else {
            return out;
        };
        let Ok(h) = super::core::broker_handle(&ctx) else {
            return out;
        };
        drop(ctx);
        let qk: Vec<QuoteKey> = keys
            .iter()
            .map(|k| QuoteKey::new(k.exchange.clone(), k.symbol.clone()))
            .collect();
        match h.broker.get_multiquotes(&h.auth, &qk).await {
            Ok(rows) => {
                for r in rows {
                    if let Some(q) = r.data.filter(|q| q.ltp > 0.0) {
                        out.insert(SymbolKey::new(r.symbol, r.exchange), to_quote(&q));
                    }
                }
            }
            Err(e) => tracing::debug!("Sandbox multiquote failed: {}", e.code()),
        }
        out
    }
}

/// Bound of the tick channel handed to the engine.
pub const TICK_CAPACITY: usize = 4096;

/// How often the worker takes again what it held in an ended broker
/// session: as often as strategies and scalping stops do
/// (`strategy::tick_feed::FeedPrices::renew`).
const RENEW_EVERY: Duration = Duration::from_secs(2);

/// Live LTP ticks from the market-data feed. One forwarding task (owned by
/// the app context) turns feed events into sandbox ticks and applies the
/// engine's watch list to the feed; it ends when this adapter is dropped
/// (its queue of calls closes), releasing every subscription it made.
///
/// The engine's watch, unwatch and resubscribe never wait: each is queued
/// in the same hold of the watch list as the change that decided it, so
/// the calls reach the broker in the order they were decided (web
/// marketcalls/openalgo#2192).
///
/// The forwarder outlives broker sessions (it follows analyzer mode, not
/// the sign-in), so what it holds is tied to the session that took it
/// (`held`): after a new sign-in it gives back nothing it took before, and
/// takes it again in the new session.
pub struct FeedTicks {
    tx: broadcast::Sender<Tick>,
    calls: Arc<FeedCalls>,
    symbols: SymbolResolver,
    watched: Mutex<HashSet<SymbolKey>>,
}

impl FeedTicks {
    /// Start the forwarder on `ctx` (aborted on app shutdown).
    pub fn start(ctx: &AppState) -> Arc<Self> {
        Self::with_feed(ctx, ctx.websocket.clone())
    }

    pub fn with_feed(ctx: &AppState, feed: Arc<WebSocketManager>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(TICK_CAPACITY);
        let calls = Arc::new(FeedCalls::default());
        let me = Arc::new(Self {
            tx: tx.clone(),
            calls: calls.clone(),
            symbols: ctx.symbols.clone(),
            watched: Mutex::new(HashSet::new()),
        });
        let shutdown = ctx.shutdown.clone();
        ctx.spawn(forward(feed, tx, calls, ctx.bus.clone(), shutdown));
        me
    }

    fn subscription(&self, key: &SymbolKey) -> Option<FeedSubscription> {
        let row = self.symbols.by_symbol(&key.exchange, &key.symbol)?;
        Some(FeedSubscription {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            token: row.token.clone(),
            brsymbol: row.br_symbol().to_string(),
            brexchange: row.br_exchange().to_string(),
            mode: FeedMode::Ltp,
            depth: 5,
        })
    }

    /// Symbols currently watched (tests).
    pub fn watched(&self) -> HashSet<SymbolKey> {
        self.watched.lock().clone()
    }

    /// A subscribe the full queue turned away. The symbol stays on the
    /// watch list, so the engine asks for it again while its prices stay
    /// quiet (SB-01), and polling checks its orders meanwhile.
    fn subscribe_turned_away(&self, key: &SymbolKey) {
        tracing::warn!(
            "Sandbox could not ask to stream {}:{} now ({} requests turned away since sandbox \
             mode started); it asks again while the symbol's prices stay quiet, and checks \
             its orders by polling meanwhile",
            key.exchange,
            key.symbol,
            self.calls.turned_away()
        );
    }
}

impl Drop for FeedTicks {
    fn drop(&mut self) {
        self.calls.close();
    }
}

/// The forwarder: ticks read as they come, and the watch list applied
/// beside them (one task, both polled in turn).
async fn forward(
    feed: Arc<WebSocketManager>,
    tx: broadcast::Sender<Tick>,
    calls: Arc<FeedCalls>,
    sessions: Arc<EventBus>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let stop = shutdown.child_token();
    tokio::join!(
        read_ticks(&feed, tx, stop.clone()),
        apply_watches(&feed, &calls, &sessions, stop)
    );
}

/// Turn feed events into sandbox ticks as they arrive. Nothing here waits
/// on a subscribe: the broker can take seconds to answer one (review X5),
/// and ticks kept waiting would then be stamped as if just received, so an
/// old price would pass the stale check (SB-06).
async fn read_ticks(
    feed: &WebSocketManager,
    tx: broadcast::Sender<Tick>,
    stop: tokio_util::sync::CancellationToken,
) {
    let mut events = feed.subscribe_ticks();
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            ev = events.recv() => match ev {
                Ok(ev) => {
                    if let FeedEvent::Tick(t) = ev.as_ref() {
                        if t.ltp > 0.0 {
                            let _ = tx.send(Tick {
                                symbol: t.symbol.clone(),
                                exchange: t.exchange.clone(),
                                ltp: crate::sandbox::types::dec_from_f64(t.ltp),
                                received_at: std::time::Instant::now(),
                            });
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Sandbox tick forwarder lagged by {}", n);
                    feed.stats().note_skipped(crate::websocket::Reader::Sandbox, n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// Apply the engine's watch list to the feed, one call at a time and
/// oldest first, until the adapter is dropped or the app shuts down; the
/// ticks stop with it, nothing more is queued, and every subscription made
/// here in the session running then is released, on every exit path.
/// Between calls it takes again what it held in an ended broker session.
async fn apply_watches(
    feed: &WebSocketManager,
    calls: &FeedCalls,
    sessions: &EventBus,
    stop: tokio_util::sync::CancellationToken,
) {
    let mut held = Held::default();
    let mut renew = tokio::time::interval(RENEW_EVERY);
    renew.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let cmd = tokio::select! {
            _ = stop.cancelled() => break,
            _ = renew.tick() => {
                renew_lapsed(feed, &mut held, sessions.session_generation()).await;
                continue;
            }
            cmd = calls.next() => match cmd {
                Some(cmd) => cmd,
                None => break,
            },
        };
        apply(feed, &mut held, sessions.session_generation(), cmd).await;
    }
    stop.cancel();
    calls.close();
    let ours = held.into_current(sessions.session_generation());
    if !ours.is_empty() {
        let _ = feed.unsubscribe(ours).await;
    }
}

/// Make one call the engine decided. `session` is the broker session read
/// before the feed is asked, as `FeedPrices` reads it: a reference taken
/// while sessions change counts as the old one's and is taken again (one
/// reference too many until the next change), never given back for
/// another holder's.
async fn apply(feed: &WebSocketManager, held: &mut Held, session: u64, cmd: WatchCmd) {
    match cmd {
        WatchCmd::Watch(s) => {
            if !held.holds_in(&key_of(&s), session) {
                take(feed, held, session, s, "").await;
            }
        }
        WatchCmd::Unwatch(s) => {
            if let Some(sub) = held.release(&key_of(&s), session) {
                let _ = feed.unsubscribe(vec![sub]).await;
            }
        }
        WatchCmd::Resubscribe(s) => {
            if let Some(sub) = held.release(&key_of(&s), session) {
                let _ = feed.unsubscribe(vec![sub]).await;
            }
            take(feed, held, session, s, " again").await;
        }
    }
}

/// Subscribe `s` and note it as taken in `session`.
async fn take(
    feed: &WebSocketManager,
    held: &mut Held,
    session: u64,
    s: FeedSubscription,
    again: &str,
) {
    match feed.subscribe(vec![s.clone()]).await {
        Ok(()) => held.insert(s, session),
        Err(e) => tracing::warn!(
            "Sandbox could not stream {}:{}{}: {}",
            s.exchange,
            s.symbol,
            again,
            e.code()
        ),
    }
}

/// Take again in broker session `session` what was held in an ended one
/// (the feed forgot it with that session). What the feed will not take now
/// is let go: the symbol stays watched, so the engine's quiet-feed backoff
/// asks for it again (SB-01), and polling checks its orders meanwhile.
async fn renew_lapsed(feed: &WebSocketManager, held: &mut Held, session: u64) {
    let lapsed = held.lapsed(session);
    if lapsed.is_empty() {
        return;
    }
    let answers = feed.admit(lapsed.clone()).await;
    let mut renewed = 0usize;
    for (sub, answer) in lapsed.into_iter().zip(answers) {
        match answer {
            Ok(()) => {
                held.insert(sub, session);
                renewed += 1;
            }
            Err(why) => {
                tracing::debug!(
                    "Sandbox could not stream {}:{} in the new broker session yet: {}",
                    sub.exchange,
                    sub.symbol,
                    why
                );
                held.forget(&key_of(&sub));
            }
        }
    }
    if renewed > 0 {
        tracing::info!(
            "{} instrument(s) the sandbox follows were subscribed again in the new broker session",
            renewed
        );
    }
}

impl TickSource for FeedTicks {
    fn subscribe_ticks(&self) -> broadcast::Receiver<Tick> {
        self.tx.subscribe()
    }

    fn watch(&self, key: &SymbolKey) {
        let queued = {
            let mut watched = self.watched.lock();
            if !watched.insert(key.clone()) {
                return;
            }
            self.subscription(key)
                .map(|s| self.calls.push(WatchCmd::Watch(s)))
        };
        if queued == Some(Err(Refused::Full)) {
            self.subscribe_turned_away(key);
        }
    }

    fn unwatch(&self, key: &SymbolKey) {
        let queued = {
            let mut watched = self.watched.lock();
            if !watched.remove(key) {
                return;
            }
            self.subscription(key)
                .map(|s| self.calls.push(WatchCmd::Unwatch(s)))
        };
        if queued == Some(Err(Refused::Full)) {
            tracing::warn!(
                "Sandbox could not stop streaming {}:{} now; it stops when sandbox mode stops",
                key.exchange,
                key.symbol
            );
        }
    }

    fn resubscribe(&self, key: &SymbolKey) {
        let queued = {
            let watched = self.watched.lock();
            if !watched.contains(key) {
                return;
            }
            self.subscription(key)
                .map(|s| self.calls.push(WatchCmd::Resubscribe(s)))
        };
        if queued == Some(Err(Refused::Full)) {
            self.subscribe_turned_away(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::SymToken;

    fn row(symbol: &str, exchange: &str, lot: i32, expiry: &str) -> SymToken {
        SymToken {
            symbol: symbol.into(),
            brsymbol: symbol.into(),
            name: symbol.into(),
            exchange: exchange.into(),
            brexchange: exchange.into(),
            token: "1".into(),
            expiry: expiry.into(),
            strike: 0.0,
            lot_size: lot,
            instrument_type: "FUT".into(),
            tick_size: 0.05,
        }
    }

    /// Review X5: ticks go on being forwarded, stamped when they arrive,
    /// while a new symbol's subscribe waits for the broker. The forwarder
    /// stopped reading for that long, and stamped the old ticks with the
    /// time it got to them, so an old price passed the stale check.
    #[tokio::test]
    async fn ticks_are_forwarded_while_a_subscribe_waits_for_the_broker() {
        use crate::brokers::common::streaming::{NormalizedTick, Presubscribe};
        use std::time::Duration;
        let feed = Arc::new(WebSocketManager::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());
        let step: Presubscribe = {
            let (gate, entered) = (gate.clone(), entered.clone());
            Arc::new(move |subs: Vec<FeedSubscription>| {
                let (gate, entered) = (gate.clone(), entered.clone());
                futures_util::FutureExt::boxed(async move {
                    entered.notify_one();
                    gate.notified().await;
                    subs.into_iter().map(Ok).collect()
                })
            })
        };
        feed.set_presubscribe(Some(step));
        let (tx, mut ticks) = broadcast::channel(16);
        let calls = Arc::new(FeedCalls::default());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let sessions = Arc::new(EventBus::new());
        let task = tokio::spawn(forward(
            feed.clone(),
            tx,
            calls.clone(),
            sessions,
            shutdown.clone(),
        ));
        calls
            .push(WatchCmd::Watch(FeedSubscription {
                symbol: "SBIN".into(),
                exchange: "NSE".into(),
                token: "1".into(),
                brsymbol: "SBIN".into(),
                brexchange: "NSE".into(),
                mode: FeedMode::Ltp,
                depth: 5,
            }))
            .unwrap();
        entered.notified().await;

        feed.emit(Arc::new(FeedEvent::Tick(NormalizedTick {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            mode: 1,
            ltp: 1424.0,
            ..Default::default()
        })));
        let tick = tokio::time::timeout(Duration::from_secs(2), ticks.recv())
            .await
            .expect("forwarded while the subscribe waits")
            .unwrap();
        assert_eq!(tick.symbol, "RELIANCE");

        gate.notify_one();
        shutdown.cancel();
        task.await.unwrap();
    }

    fn ltp(symbol: &str) -> FeedSubscription {
        FeedSubscription {
            symbol: symbol.into(),
            exchange: "NSE".into(),
            token: "1".into(),
            brsymbol: symbol.into(),
            brexchange: "NSE".into(),
            mode: FeedMode::Ltp,
            depth: 5,
        }
    }

    /// The worker's last release gives back only what the broker session
    /// running then counts for it (integration review of 1.0.10): the feed
    /// forgot what it took before a new sign-in, and another holder took
    /// the symbol since. Its reference stays; the worker gave it back
    /// before, stopping that holder's prices. Whether or not the worker
    /// renewed its own meanwhile, one reference remains.
    #[tokio::test]
    async fn stopping_after_a_new_broker_session_leaves_another_holders_price() {
        use std::time::Duration;
        let feed = Arc::new(WebSocketManager::new());
        let sessions = Arc::new(EventBus::new());
        let (tx, _ticks) = broadcast::channel(16);
        let calls = Arc::new(FeedCalls::default());
        let shutdown = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(forward(
            feed.clone(),
            tx,
            calls.clone(),
            sessions.clone(),
            shutdown,
        ));
        calls.push(WatchCmd::Watch(ltp("SBIN"))).unwrap();
        for _ in 0..500 {
            if !feed.subscriptions().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(feed.subscriptions().len(), 1, "the sandbox took SBIN");

        sessions.begin_session();
        feed.disconnect().await.unwrap();
        sessions.begin_session();
        feed.subscribe(vec![ltp("SBIN")]).await.unwrap();
        calls.close();
        task.await.unwrap();

        assert_eq!(
            feed.subscriptions().len(),
            1,
            "the other holder's reference stays"
        );
    }

    #[test]
    fn master_symbols_give_lot_and_expiry() {
        let r = SymbolResolver::new();
        r.load(vec![row("NIFTY27OCT26FUT", "NFO", 65, "27-OCT-26")]);
        let m = MasterSymbols(r);
        let meta = m.lookup("NIFTY27OCT26FUT", "NFO").unwrap();
        assert_eq!(meta.lotsize, 65);
        assert_eq!(meta.expiry, chrono::NaiveDate::from_ymd_opt(2026, 10, 27));
        assert!(m.lookup("NOPE", "NFO").is_none());
    }
}
