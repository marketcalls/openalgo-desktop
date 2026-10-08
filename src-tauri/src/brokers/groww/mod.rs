//! Groww Trade API adapter (web `broker/groww/**`).
//!
//! * REST base `https://api.groww.in`, every call `Authorization: Bearer
//!   <token>`; responses are `{"status": "SUCCESS", "payload": {...}}`.
//! * The stored session token is the raw Groww access token (no prefix).
//! * Market data streams over NATS-on-WebSocket with protobuf payloads
//!   (`streaming.rs`, on the shared feed manager); order updates are a REST
//!   poll of the order book (`order_poller.rs`), as on the web.

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
pub mod nkeys;
pub mod order_poller;
mod orders;
pub mod proto;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed, OrderUpdate};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const BASE_URL: &str = "https://api.groww.in";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map` (`interval_in_minutes`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1"),
    ("5m", "5"),
    ("10m", "10"),
    ("1h", "60"),
    ("4h", "240"),
    ("D", "1440"),
    ("W", "10080"),
];

/// Pacing category of a call.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Category {
    Order,
    /// Per-symbol live quotes (web overlay spacing 0.25 s).
    Live,
    /// OHLC batches (web 0.2 s between batches).
    Ohlc,
    Other,
}

#[derive(Debug)]
pub(crate) struct Pacers {
    order: Pacer,
    live: Pacer,
    ohlc: Pacer,
    other: Pacer,
}

impl Default for Pacers {
    fn default() -> Self {
        Self {
            // Groww publishes 15 orders/s and 10 live-data calls/s; the web
            // spaces quote calls at 0.25 s and OHLC batches at 0.2 s.
            order: Pacer::per_second(10.0),
            live: Pacer::with_interval(Duration::from_millis(250)),
            ohlc: Pacer::with_interval(Duration::from_millis(200)),
            other: Pacer::per_second(10.0),
        }
    }
}

/// Everything a call needs; cheap to clone so the order poller task can own
/// one.
#[derive(Clone)]
pub(crate) struct GrowwCore {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Arc<str>,
    pub(crate) symbols: SymbolResolver,
    pacers: Arc<Pacers>,
}

/// A Groww response: HTTP status and the parsed body (`Null` when the body
/// was not JSON).
pub(crate) struct Reply {
    pub status: reqwest::StatusCode,
    pub body: Value,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
            && self.body.get("status").and_then(Value::as_str) == Some("SUCCESS")
    }

    pub fn payload(&self) -> &Value {
        self.body.get("payload").unwrap_or(&Value::Null)
    }

    /// Groww error text from `error.message`, `message` or
    /// `errors[0].message`.
    pub fn error_message(&self) -> String {
        error_message(&self.body)
    }
}

pub(crate) fn error_message(body: &Value) -> String {
    fn s(v: Option<&Value>) -> Option<&str> {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
    s(body.pointer("/error/message"))
        .or_else(|| s(body.get("message")))
        .or_else(|| s(body.pointer("/errors/0/message")))
        .or_else(|| s(body.get("error")))
        .unwrap_or("")
        .to_string()
}

impl GrowwCore {
    pub(crate) fn new(symbols: SymbolResolver, base_url: &str) -> Self {
        Self {
            http: http::client(),
            base_url: Arc::from(base_url.trim_end_matches('/')),
            symbols,
            pacers: Arc::new(Pacers::default()),
        }
    }

    async fn pace(&self, c: Category) {
        match c {
            Category::Order => self.pacers.order.acquire().await,
            Category::Live => self.pacers.live.acquire().await,
            Category::Ohlc => self.pacers.ohlc.acquire().await,
            Category::Other => self.pacers.other.acquire().await,
        }
    }

    /// One Groww call; any HTTP status comes back as a `Reply` except 401 /
    /// 403, which mean the session is gone.
    pub(crate) async fn send(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
        api_version: bool,
    ) -> Result<Reply> {
        let token = auth.raw();
        if token.trim().is_empty() {
            return Err(session_expired());
        }
        self.pace(category).await;
        let url = format!("{}{}", self.base_url, path_and_query);
        let mut req = self
            .http
            .request(method, &url)
            .timeout(http::REQUEST_TIMEOUT)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/json");
        if api_version {
            req = req.header("X-API-VERSION", "1.0");
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let path = path_and_query.split('?').next().unwrap_or("");
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            tracing::warn!(
                status = status.as_u16(),
                "Groww refused the session on {}",
                path
            );
            return Err(session_expired());
        }
        if !status.is_success() {
            tracing::warn!(
                status = status.as_u16(),
                "Groww refused {}: {}",
                path,
                error_message(&body)
            );
        }
        Ok(Reply { status, body })
    }

    /// Like `send`, but only a `SUCCESS` envelope is a success; its payload
    /// is returned.
    pub(crate) async fn call(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
    ) -> Result<Value> {
        let r = self
            .send(method, path_and_query, auth, body, category, false)
            .await?;
        if r.is_success() {
            return Ok(r.payload().clone());
        }
        Err(groww_error(&r))
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Groww session has expired. Log in to Groww again.".into())
}

/// A non-success Groww reply -> trader-facing error.
pub(crate) fn groww_error(r: &Reply) -> AppError {
    if r.status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return AppError::Broker(
            "Groww is limiting requests right now. Wait a moment and try again.".into(),
        );
    }
    if r.status.is_server_error() {
        return AppError::Broker(
            "Groww's servers are not responding normally. Try again shortly.".into(),
        );
    }
    let m = r.error_message();
    if m.is_empty() {
        AppError::Broker("Groww refused the request.".into())
    } else {
        AppError::Broker(m)
    }
}

pub struct GrowwBroker {
    core: GrowwCore,
    feed: streaming::FeedEndpoints,
    poller: parking_lot::Mutex<Option<order_poller::OrderPoller>>,
}

impl GrowwBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, BASE_URL)
    }

    /// Point the adapter at another host (tests run a local fake Groww).
    pub fn with_base_url(symbols: SymbolResolver, base_url: impl AsRef<str>) -> Self {
        Self {
            core: GrowwCore::new(symbols, base_url.as_ref()),
            feed: streaming::FeedEndpoints::default(),
            poller: parking_lot::Mutex::new(None),
        }
    }

    /// Override the socket-token and NATS socket addresses (tests).
    pub fn with_feed_endpoints(mut self, socket_token_url: &str, ws_url: &str) -> Self {
        self.feed = streaming::FeedEndpoints {
            socket_token_url: socket_token_url.to_string(),
            ws_url: ws_url.to_string(),
        };
        self
    }

    /// Start polling the order book for order updates (web
    /// `PollingOrderUpdateAdapter`; Groww has no order socket). Replaces a
    /// running poller. The interval is clamped to 1..=60 s.
    pub fn start_order_updates(
        &self,
        auth: &AuthToken,
        interval: Duration,
    ) -> Result<mpsc::Receiver<OrderUpdate>> {
        let (poller, rx) =
            order_poller::OrderPoller::start(self.core.clone(), auth.clone(), interval)?;
        // Dropping the old poller aborts its task.
        *self.poller.lock() = Some(poller);
        Ok(rx)
    }

    /// Stop the order-update poller (`Broker::on_logout` calls this on
    /// broker logout, session revocation and app shutdown; dropping the
    /// broker also stops it).
    pub fn stop_order_updates(&self) {
        if let Some(p) = self.poller.lock().take() {
            p.stop();
        }
    }

    pub fn order_updates_running(&self) -> bool {
        self.poller.lock().as_ref().is_some_and(|p| p.is_running())
    }
}

#[async_trait]
impl Broker for GrowwBroker {
    fn id(&self) -> &'static str {
        "groww"
    }

    fn name(&self) -> &'static str {
        "Groww"
    }

    fn logo(&self) -> &'static str {
        "/logos/groww.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["totp", "password"],
        }
    }

    /// TOTP is one of three ways in (approval checksum and a pasted access
    /// token need none), so the form must not insist on it.
    fn requires_totp(&self) -> bool {
        false
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: true,
            margin: true,
            gtt: false,
            streaming: true,
            // Order updates come from the REST poller, not a socket.
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.core.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(&self.core, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        orders::place_order(&self.core, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        orders::modify_order(&self.core, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(&self.core, auth, order_id, None).await
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(&self.core, auth).await
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(&self.core, auth, symbol, exchange, product).await
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        orders::get_order_book(&self.core, auth).await
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        orders::get_trade_book(&self.core, auth).await
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        orders::get_positions(&self.core, auth).await
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(&self.core, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(&self.core, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(&self.core, auth, legs).await
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(&self.core, auth, key).await
    }

    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        data::get_multiquotes(&self.core, auth, keys).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(&self.core, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(&self.core, auth, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(&self.core).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::GrowwFeed::new(
            self.core.http.clone(),
            auth.raw(),
            self.feed.clone(),
        )))
    }

    /// The order-book poller (web `PollingOrderUpdateAdapter`, 5 s); it
    /// replaces a running one and stops in `on_logout`.
    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(OrderFeed::Stream(self.start_order_updates(
            auth,
            order_poller::DEFAULT_INTERVAL,
        )?))
    }

    async fn on_logout(&self) {
        self.stop_order_updates();
    }
}
