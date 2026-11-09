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
//!
//! Many brokers send a book and its price in separate packets, in either
//! order (Pocketful's snapquote and detailed data, Samco's quote2 and quote,
//! Dhan's 20 levels, Fyers' depth and symbol feeds): [`FrameWait`] joins
//! them, so a Depth node reads the traded price and not 0.

use crate::brokers::common::streaming::{
    FeedEvent, FeedMode, FeedSubscription, MarketEvent, NormalizedDepth, NormalizedTick, QuoteField,
};
use crate::feed::bridge::{depth_update, tick_update};
use crate::feed::protocol::market_data;
use crate::feed::source::{InstrumentKey, MarketUpdate, Mode, DEFAULT_DEPTH};
use crate::flow::contract::env::{FeedError, MarketFeed};
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Weak;
use std::time::Duration;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::Instant;

/// What the feed answers while no broker is logged in.
pub const NO_BROKER_SESSION: &str = "No broker is logged in, so there is no live market data feed.";

/// How long a Depth wait holds a book that came with no price for the
/// instrument's next priced tick. On a quiet contract none comes, and the
/// node must not sit out its timeout for it: the book then goes out with
/// ltp 0 (the 1.0.6 rule for a book before any price).
const PRICE_AFTER_BOOK: Duration = Duration::from_secs(2);

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

/// One wait's reading of the feed: what it has seen of its instrument.
struct FrameWait<'a> {
    key: &'a InstrumentKey,
    mode: Mode,
    broker: &'a str,
    /// The instrument's last tick with a traded price, in this wait.
    priced: Option<NormalizedTick>,
    /// The latest book with no price, held for one (a Depth wait only).
    held: Option<NormalizedDepth>,
}

impl<'a> FrameWait<'a> {
    fn new(key: &'a InstrumentKey, mode: Mode, broker: &'a str) -> Self {
        Self {
            key,
            mode,
            broker,
            priced: None,
            held: None,
        }
    }

    /// The 8765 frame `event` completes for this wait, if any.
    fn read(&mut self, event: &FeedEvent) -> Option<Value> {
        match event {
            FeedEvent::Tick(t)
                if t.symbol == self.key.symbol && t.exchange == self.key.exchange =>
            {
                self.read_tick(t)
            }
            FeedEvent::Depth(d)
                if d.symbol == self.key.symbol && d.exchange == self.key.exchange =>
            {
                self.read_book(d)
            }
            _ => None,
        }
    }

    fn read_tick(&mut self, t: &NormalizedTick) -> Option<Value> {
        if t.knows(QuoteField::Ltp) && t.ltp > 0.0 {
            self.priced = Some(t.clone());
            if let Some(book) = self.held.take() {
                return self.render(&self.book_update(&book));
            }
        }
        self.render(&tick_update(self.key.clone(), t)?)
    }

    /// A book with no price, and none known yet, is held for one, and never
    /// answers a lower wait (no LTP or Quote from a packet with no price).
    fn read_book(&mut self, d: &NormalizedDepth) -> Option<Value> {
        let update = self.book_update(d);
        if update.ltp > 0.0 {
            return self.render(&update);
        }
        if self.mode == Mode::Depth && !(d.buy.is_empty() && d.sell.is_empty()) {
            self.held = Some(d.clone());
        }
        None
    }

    fn book_update(&self, d: &NormalizedDepth) -> MarketUpdate {
        depth_update(
            self.key.clone(),
            d,
            self.priced.as_ref(),
            Default::default(),
        )
    }

    /// The frame of an update at this wait's mode, when it serves it: a
    /// Depth wait wants a book, so a full tick without one does not end it.
    fn render(&self, update: &MarketUpdate) -> Option<Value> {
        if update.mode < self.mode || (self.mode == Mode::Depth && update.depth.is_none()) {
            return None;
        }
        serde_json::from_str(&market_data(update, self.mode, DEFAULT_DEPTH, self.broker)).ok()
    }

    /// The held book as it stands, when no price came for it.
    fn held_frame(&mut self) -> Option<Value> {
        let book = self.held.take()?;
        self.render(&self.book_update(&book))
    }

    fn holds_a_book(&self) -> bool {
        self.held.is_some()
    }
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
        let instrument = InstrumentKey::new(symbol, exchange);
        let mut wait = FrameWait::new(&instrument, mode_of(mode).1, &broker);
        // The broker can take a while to answer a subscribe: what comes
        // meanwhile is read as it comes and only the newest is kept, so a
        // price kept waiting is not read afterwards as the current one. The
        // one `wait` reads on after the answer, so a book and a price either
        // side of it still join.
        let mut newest = None;
        let mut keep = |event: &MarketEvent| {
            if let Some(f) = wait.read(event).filter(|f| accept(f)) {
                newest = Some(f);
            }
        };
        let read_on = async {
            loop {
                match rx.recv().await {
                    Ok(event) => keep(&event),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        };
        tokio::select! {
            held = self.hold(&app, &key) => held?,
            () = read_on => {}
        }
        // What came just before the answer and has not been read yet.
        loop {
            match rx.try_recv() {
                Ok(event) => keep(&event),
                Err(TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        drop(app);
        if newest.is_some() {
            return Ok(newest);
        }
        let end = Instant::now() + timeout;
        // When the held book stops waiting for a price.
        let mut price_by: Option<Instant> = None;
        loop {
            // Here and not after a read: the book may have come while the
            // broker was answering.
            if wait.holds_a_book() {
                price_by.get_or_insert_with(|| Instant::now() + PRICE_AFTER_BOOK);
            }
            let until = price_by.map_or(end, |by| by.min(end));
            match tokio::time::timeout_at(until, rx.recv()).await {
                Ok(Ok(event)) => {
                    if let Some(f) = wait.read(&event).filter(|f| accept(f)) {
                        return Ok(Some(f));
                    }
                }
                Ok(Err(RecvError::Lagged(_))) => {}
                Ok(Err(RecvError::Closed)) => return Ok(wait.held_frame().filter(|f| accept(f))),
                Err(_) => {
                    if let Some(f) = wait.held_frame().filter(|f| accept(f)) {
                        return Ok(Some(f));
                    }
                    if Instant::now() >= end {
                        return Ok(None);
                    }
                    price_by = None;
                }
            }
        }
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
