//! The market feed the subscribe nodes read in the app (the web's
//! `_get_websocket_data` over its websocket proxy client): the broker
//! session's market-data manager (`crate::websocket::WebSocketManager`),
//! which counts references per instrument and mode.
//!
//! Flow holds at most one reference per symbol, exchange and mode, for all
//! its workflows together: the per-workflow record
//! (`contract::subscriptions`) gives a subscription back to the feed only
//! when no workflow holds it any more, so one reference is all Flow ever
//! needs. A reference belongs to the broker session that took it: a new
//! session (the manager forgot every reference when the old one ended)
//! takes a new one.
//!
//! A frame is the one an app on the 8765 feed receives for the same
//! update (`feed::protocol::market_data`), so a node reads what the web's
//! node read from its websocket client: `symbol`, `exchange`, numeric
//! `mode`, and the values under `data` (the book as `depth.buy` /
//! `depth.sell`). The wait is on the manager's broadcast with a timeout and
//! the receiver is dropped on every path.

use crate::brokers::common::streaming::{FeedEvent, FeedMode, FeedSubscription};
use crate::feed::bridge::{depth_update, tick_update};
use crate::feed::protocol::market_data;
use crate::feed::source::{InstrumentKey, Mode, DEFAULT_DEPTH};
use crate::flow::contract::env::{FeedError, MarketFeed};
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Weak;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// What the feed answers while no broker is logged in.
pub const NO_BROKER_SESSION: &str = "No broker is logged in, so there is no live market data feed.";

type Key = (String, String, String);

/// The app's feed for Flow.
pub struct AppFeed {
    app: Weak<AppState>,
    /// The references Flow holds, with the broker session (the bus's
    /// session generation) each was taken in.
    held: Mutex<HashMap<Key, u64>>,
}

fn mode_of(mode: &str) -> (FeedMode, Mode) {
    match mode {
        "LTP" => (FeedMode::Ltp, Mode::Ltp),
        "Depth" => (FeedMode::Depth, Mode::Depth),
        _ => (FeedMode::Quote, Mode::Quote),
    }
}

impl AppFeed {
    pub fn new(app: Weak<AppState>) -> Self {
        Self {
            app,
            held: Mutex::new(HashMap::new()),
        }
    }

    /// The app, signed in with a broker session and a feed that can stream.
    fn live(&self) -> Result<std::sync::Arc<AppState>, FeedError> {
        let app = self
            .app
            .upgrade()
            .ok_or_else(|| FeedError::Connect("OpenAlgo is closing".into()))?;
        if !app.security.is_unlocked() || app.get_broker_session().is_none() {
            return Err(FeedError::Connect(NO_BROKER_SESSION.into()));
        }
        if let Some(reason) =
            crate::feed::source::MarketDataSource::unavailable(app.bridge.as_ref())
        {
            return Err(FeedError::Connect(reason));
        }
        Ok(app)
    }

    /// The manager subscription for a symbol, from the symbol master.
    fn subscription(
        app: &AppState,
        symbol: &str,
        exchange: &str,
        mode: FeedMode,
    ) -> Option<FeedSubscription> {
        let row = app.symbols.by_symbol(exchange, symbol)?;
        Some(FeedSubscription {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            token: row.token.clone(),
            brsymbol: row.br_symbol().to_string(),
            brexchange: row.br_exchange().to_string(),
            mode,
            depth: DEFAULT_DEPTH,
        })
    }

    /// Take Flow's reference unless it holds one in this session. The
    /// claim is written before the manager is asked, so two runs reaching
    /// the same symbol at once take one reference between them.
    async fn hold(&self, app: &AppState, key: &Key) -> Result<(), FeedError> {
        let session = app.bus.session_generation();
        {
            let mut held = self.held.lock();
            if held.get(key) == Some(&session) {
                return Ok(());
            }
            held.insert(key.clone(), session);
        }
        let (symbol, exchange, mode) = key;
        let taken = match Self::subscription(app, symbol, exchange, mode_of(mode).0) {
            None => Err(FeedError::Subscribe(format!(
                "Token not found for {} on {}",
                symbol, exchange
            ))),
            Some(sub) => app
                .websocket
                .subscribe(vec![sub])
                .await
                .map_err(|e| FeedError::Subscribe(e.client_message())),
        };
        if taken.is_err() {
            self.held.lock().remove(key);
        }
        taken
    }
}

/// The 8765 frame for `event` at `mode`, when it is the instrument's and
/// carries that mode.
fn frame(event: &FeedEvent, key: &InstrumentKey, mode: Mode, broker: &str) -> Option<Value> {
    let update = match event {
        FeedEvent::Tick(t) if t.symbol == key.symbol && t.exchange == key.exchange => {
            tick_update(key.clone(), t)
        }
        FeedEvent::Depth(d) if d.symbol == key.symbol && d.exchange == key.exchange => {
            depth_update(key.clone(), d, None)
        }
        _ => return None,
    };
    if update.mode < mode {
        return None;
    }
    serde_json::from_str(&market_data(&update, mode, DEFAULT_DEPTH, broker)).ok()
}

#[async_trait]
impl MarketFeed for AppFeed {
    async fn first_frame(
        &self,
        symbol: &str,
        exchange: &str,
        mode: &str,
        timeout: Duration,
        accept: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
    ) -> Result<Option<Value>, FeedError> {
        let app = self.live()?;
        let broker = app
            .get_broker_session()
            .map(|s| s.broker_id)
            .unwrap_or_default();
        // Listen before subscribing, so the first frame is not missed.
        let mut rx = app.websocket.subscribe_ticks();
        let key: Key = (symbol.to_string(), exchange.to_string(), mode.to_string());
        self.hold(&app, &key).await?;
        drop(app);
        let instrument = InstrumentKey::new(symbol, exchange);
        let wanted = mode_of(mode).1;
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let Some(f) = frame(&event, &instrument, wanted, &broker) {
                            if accept(&f) {
                                return Some(f);
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return None,
                }
            }
        };
        Ok(tokio::time::timeout(timeout, wait).await.ok().flatten())
    }

    async fn session(&self) -> Result<(), FeedError> {
        self.live().map(|_| ())
    }

    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> Result<(), FeedError> {
        let key: Key = (symbol.to_string(), exchange.to_string(), mode.to_string());
        let Some(session) = self.held.lock().remove(&key) else {
            // Flow holds no reference: nothing of Flow's to give back, and
            // another owner's (the trading terminal, the sandbox) stays.
            return Ok(());
        };
        let Some(app) = self.app.upgrade() else {
            return Ok(());
        };
        if session != app.bus.session_generation() {
            // Taken in a broker session that has ended: the manager has
            // already forgotten it.
            return Ok(());
        }
        let Some(sub) = Self::subscription(&app, symbol, exchange, mode_of(mode).0) else {
            return Ok(());
        };
        if let Err(e) = app.websocket.unsubscribe(vec![sub]).await {
            self.held.lock().insert(key, session);
            return Err(FeedError::Failed(e.client_message()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
