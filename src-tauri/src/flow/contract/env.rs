//! What a node reads besides the [`super::client::FlowClient`]: the symbol
//! master, the strategy book, the live feed, outbound HTTP and the trading
//! calendar (the web's nodes import `SymToken`, `get_strategy_legs`,
//! `websocket_service`, `httpx` and `utils.trading_calendar` for these).
//!
//! Each is a trait. The node lanes implement the real ones over the app's
//! services and install them on the module ([`crate::flow::FlowModule::env`]);
//! tests and the golden replays give fakes built from a golden's `env`. A
//! context without one answers [`UNAVAILABLE`]-style errors, never a guess.

use crate::brokers::common::symbols::SymToken;
use async_trait::async_trait;
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// What a missing service answers.
pub const UNAVAILABLE: &str = "This service is not available to Flow right now.";

/// A symbol master lookup, the filters the web's `SymToken` queries use;
/// every set field must match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolQuery {
    pub symbol: Option<String>,
    /// The symbol starts with this (the web's `symbol_prefix_filter`).
    pub symbol_prefix: Option<String>,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub instrumenttype: Option<String>,
    /// At most this many rows.
    pub limit: Option<usize>,
}

/// The symbol master (lot sizes, strikes, expiries, underlyings).
pub trait SymbolMaster: Send + Sync {
    fn rows(&self, q: &SymbolQuery) -> Result<Vec<SymToken>, String>;
}

/// The strategy book: one strategy's legs, as the web's
/// `get_strategy_legs` lists them; `Err` with the trader sentence when the
/// book cannot be read.
pub trait StrategyBook: Send + Sync {
    fn legs(&self, strategy: &str) -> Result<Vec<Value>, String>;
}

/// Why the feed did not deliver (the web logs each differently).
#[derive(Debug, Clone, PartialEq)]
pub enum FeedError {
    /// The run's key names no user (`WebSocket: Invalid API key`).
    InvalidKey,
    /// No connection (`WebSocket connection failed: {error}`).
    Connect(String),
    /// The subscription (or its release) was refused
    /// (`WebSocket subscribe failed: {message}`, `Unsubscribe failed: ...`).
    Subscribe(String),
    /// The feed failed outright (the web's exception: `WebSocket error: {e}`,
    /// `Unsubscribe error: {e}`).
    Failed(String),
}

impl FeedError {
    /// The error's own text.
    pub fn text(&self) -> String {
        match self {
            FeedError::InvalidKey => "Invalid API key".into(),
            FeedError::Connect(m) | FeedError::Subscribe(m) | FeedError::Failed(m) => m.clone(),
        }
    }
}

/// The live market data feed the subscribe and price nodes read.
#[async_trait]
pub trait MarketFeed: Send + Sync {
    /// Subscribe `symbol` on `exchange` in `mode` (`LTP`, `Quote`, `Depth`)
    /// and wait up to `timeout` for the first frame `accept` takes;
    /// `Ok(None)` when none came in time. A subscription made is recorded by
    /// the caller for release.
    async fn first_frame(
        &self,
        symbol: &str,
        exchange: &str,
        mode: &str,
        timeout: Duration,
        accept: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
    ) -> Result<Option<Value>, FeedError>;

    /// Whether the run has a feed session to work with (the web's
    /// `verify_api_key` and `get_websocket_connection`): `InvalidKey` or
    /// `Connect` when not.
    async fn session(&self) -> Result<(), FeedError>;

    /// Give back one subscription: `Subscribe` when the feed refused,
    /// `Failed` when it failed.
    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> Result<(), FeedError>;
}

/// One outbound HTTP request, sent only to an address the guard vetted.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpCall {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    /// Connect here, never to a second lookup of the host.
    pub connect_to: SocketAddr,
}

/// What came back.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Outbound HTTP for the httpRequest node: name resolution (the guard
/// checks every address) and the request itself.
#[async_trait]
pub trait OutboundHttp: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
    async fn send(&self, call: HttpCall) -> Result<HttpReply, String>;
}

/// The trading calendar the calendar and market-hours checks read: one
/// year's holiday records, as `/api/v1/market/holidays` lists them.
pub trait TradingCalendar: Send + Sync {
    fn holidays(&self, year: i32) -> Result<Vec<Value>, String>;
}

/// The services a run's nodes read besides the client.
#[derive(Clone)]
pub struct NodeEnv {
    pub symbols: Arc<dyn SymbolMaster>,
    pub strategy_book: Arc<dyn StrategyBook>,
    pub feed: Arc<dyn MarketFeed>,
    pub http: Arc<dyn OutboundHttp>,
    pub calendar: Arc<dyn TradingCalendar>,
}

impl Default for NodeEnv {
    /// Every service unavailable: what a node sees until a lane installs
    /// the real one, and in a bare test context.
    fn default() -> Self {
        let none = Arc::new(Unavailable);
        Self {
            symbols: none.clone(),
            strategy_book: none.clone(),
            feed: none.clone(),
            http: none.clone(),
            calendar: none,
        }
    }
}

/// The stand-in for a service nobody installed.
struct Unavailable;

impl SymbolMaster for Unavailable {
    fn rows(&self, _: &SymbolQuery) -> Result<Vec<SymToken>, String> {
        Err(UNAVAILABLE.into())
    }
}

impl StrategyBook for Unavailable {
    fn legs(&self, _: &str) -> Result<Vec<Value>, String> {
        Err(UNAVAILABLE.into())
    }
}

#[async_trait]
impl MarketFeed for Unavailable {
    async fn first_frame(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Duration,
        _: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
    ) -> Result<Option<Value>, FeedError> {
        Err(FeedError::Connect(UNAVAILABLE.into()))
    }

    async fn session(&self) -> Result<(), FeedError> {
        Err(FeedError::Connect(UNAVAILABLE.into()))
    }

    async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> Result<(), FeedError> {
        Err(FeedError::Failed(UNAVAILABLE.into()))
    }
}

#[async_trait]
impl OutboundHttp for Unavailable {
    async fn resolve(&self, _: &str) -> Result<Vec<IpAddr>, String> {
        Err(UNAVAILABLE.into())
    }

    async fn send(&self, _: HttpCall) -> Result<HttpReply, String> {
        Err(UNAVAILABLE.into())
    }
}

impl TradingCalendar for Unavailable {
    fn holidays(&self, _: i32) -> Result<Vec<Value>, String> {
        Err(UNAVAILABLE.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A feed that owns its frames, as a real feed and a golden replay do.
    struct Ticks(Vec<Value>);

    #[async_trait]
    impl MarketFeed for Ticks {
        async fn first_frame(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Duration,
            accept: &(dyn for<'a> Fn(&'a Value) -> bool + Send + Sync),
        ) -> Result<Option<Value>, FeedError> {
            for frame in self.0.clone() {
                if accept(&frame) {
                    return Ok(Some(frame));
                }
            }
            Ok(None)
        }

        async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// The feed can hand `accept` frames it owns (the elided lifetime tied it
    /// to the call's own borrows).
    #[tokio::test]
    async fn a_feed_offers_its_own_frames_to_accept() {
        let feed = Ticks(vec![
            serde_json::json!({"ltp": 0}),
            serde_json::json!({"ltp": 812.5}),
        ]);
        let frame = feed
            .first_frame("SBIN", "NSE", "LTP", Duration::from_secs(1), &|f| {
                f["ltp"].as_f64().is_some_and(|p| p > 0.0)
            })
            .await;
        assert_eq!(frame, Ok(Some(serde_json::json!({"ltp": 812.5}))));
    }

    #[tokio::test]
    async fn a_service_nobody_installed_says_so() {
        let env = NodeEnv::default();
        assert_eq!(
            env.symbols.rows(&SymbolQuery::default()).err().as_deref(),
            Some(UNAVAILABLE)
        );
        assert_eq!(env.strategy_book.legs("s"), Err(UNAVAILABLE.into()));
        assert_eq!(env.calendar.holidays(2026), Err(UNAVAILABLE.into()));
        assert_eq!(
            env.http.resolve("example.com").await,
            Err(UNAVAILABLE.into())
        );
        let frame = env
            .feed
            .first_frame("SBIN", "NSE", "LTP", Duration::from_millis(1), &|_| true)
            .await;
        assert_eq!(frame, Err(FeedError::Connect(UNAVAILABLE.into())));
    }
}
