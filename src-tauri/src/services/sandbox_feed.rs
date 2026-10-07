//! Adapters that connect the sandbox engine to the live app:
//!
//! * [`MasterSymbols`]: the shared symbol master as a `SymbolSource`;
//! * [`LiveQuotes`]: the connected broker's quote API as a `QuoteSource`;
//! * [`FeedTicks`]: the market-data feed (the `WebSocketManager` broadcast)
//!   as a `TickSource`.
//!
//! The engine never sees a broker; these are the only seams.

use crate::brokers::common::master_contract::parse_oa_expiry;
use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::QuoteKey;
use crate::sandbox::{Quote, QuoteSource, SymbolKey, SymbolMeta, SymbolSource, Tick, TickSource};
use crate::state::AppState;
use crate::websocket::WebSocketManager;
use parking_lot::Mutex;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use tokio::sync::{broadcast, mpsc};

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

fn to_quote(q: &crate::brokers::types::Quote) -> Quote {
    Quote::from_f64(q.ltp, q.bid, q.ask, q.high, q.low)
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
/// Bound of the watch/unwatch command channel.
const CMD_CAPACITY: usize = 1024;

enum WatchCmd {
    Watch(FeedSubscription),
    Unwatch(FeedSubscription),
}

/// Live LTP ticks from the market-data feed. One forwarding task (owned by
/// the app context) turns feed events into sandbox ticks and applies the
/// engine's watch list to the feed; it ends when this adapter is dropped
/// (the command channel closes), releasing every subscription it made.
pub struct FeedTicks {
    tx: broadcast::Sender<Tick>,
    cmd: mpsc::Sender<WatchCmd>,
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
        let (cmd, rx) = mpsc::channel(CMD_CAPACITY);
        let me = Arc::new(Self {
            tx: tx.clone(),
            cmd,
            symbols: ctx.symbols.clone(),
            watched: Mutex::new(HashSet::new()),
        });
        let shutdown = ctx.shutdown.clone();
        ctx.spawn(forward(feed, tx, rx, shutdown));
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
}

async fn forward(
    feed: Arc<WebSocketManager>,
    tx: broadcast::Sender<Tick>,
    mut rx: mpsc::Receiver<WatchCmd>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut events = feed.subscribe_ticks();
    let mut mine: HashMap<SymbolKey, FeedSubscription> = HashMap::new();
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            cmd = rx.recv() => match cmd {
                Some(WatchCmd::Watch(s)) => {
                    let key = SymbolKey::new(s.symbol.clone(), s.exchange.clone());
                    if let std::collections::hash_map::Entry::Vacant(slot) = mine.entry(key) {
                        match feed.subscribe(vec![s.clone()]).await {
                            Ok(()) => {
                                slot.insert(s);
                            }
                            Err(e) => tracing::warn!(
                                "Sandbox could not stream {}:{}: {}",
                                s.exchange,
                                s.symbol,
                                e.code()
                            ),
                        }
                    }
                }
                Some(WatchCmd::Unwatch(s)) => {
                    let key = SymbolKey::new(s.symbol.clone(), s.exchange.clone());
                    if let Some(sub) = mine.remove(&key) {
                        let _ = feed.unsubscribe(vec![sub]).await;
                    }
                }
                None => break,
            },
            ev = events.recv() => match ev {
                Ok(ev) => {
                    if let FeedEvent::Tick(t) = ev.as_ref() {
                        if t.ltp > 0.0 {
                            let _ = tx.send(Tick {
                                symbol: t.symbol.clone(),
                                exchange: t.exchange.clone(),
                                ltp: crate::sandbox::types::dec_from_f64(t.ltp),
                            });
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!("Sandbox tick forwarder lagged by {}", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    // Release everything this forwarder subscribed, on every exit path.
    if !mine.is_empty() {
        let subs: Vec<FeedSubscription> = mine.into_values().collect();
        let _ = feed.unsubscribe(subs).await;
    }
}

impl TickSource for FeedTicks {
    fn subscribe_ticks(&self) -> broadcast::Receiver<Tick> {
        self.tx.subscribe()
    }

    fn watch(&self, key: &SymbolKey) {
        if !self.watched.lock().insert(key.clone()) {
            return;
        }
        if let Some(s) = self.subscription(key) {
            if self.cmd.try_send(WatchCmd::Watch(s)).is_err() {
                // The engine's polling fallback covers a dropped watch.
                tracing::debug!("Sandbox watch queue full");
                self.watched.lock().remove(key);
            }
        }
    }

    fn unwatch(&self, key: &SymbolKey) {
        if !self.watched.lock().remove(key) {
            return;
        }
        if let Some(s) = self.subscription(key) {
            let _ = self.cmd.try_send(WatchCmd::Unwatch(s));
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
