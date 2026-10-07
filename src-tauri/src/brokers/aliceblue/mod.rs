//! AliceBlue adapter (web `broker/aliceblue/**`, the V2 "open-api").
//!
//! Sign-in is an OAuth-style redirect: the vendor login page
//! (`https://ant.aliceblueonline.com/?appcode=<appCode>`) returns to
//! `/aliceblue/callback?authCode=..&userId=..`; the catalogue hands the
//! adapter `userId:authCode`, and `SHA256(userId + authCode + apiSecret)` is
//! exchanged for the session JWT (`userSession`). The stored token is that
//! JWT alone; every REST call sends `Authorization: Bearer <JWT>`.
//!
//! AliceBlue has no REST quote API. Quotes, multiquotes and depth go through
//! the Noren market-data socket exactly as the web does (`data.rs`), over one
//! pooled connection that closes itself when idle. The live feed and the
//! order-update feed need REST calls before every connect (`createWsSess`,
//! `createWsToken`), so they run through the loopback relay
//! (`crate::brokers::upstox::relay`) until the streaming contract grows an
//! async prepare hook.

pub mod auth;
pub mod data;
pub mod funds;
pub mod mapping;
pub mod master_contract;
pub mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

/// Trading and session REST host (web `order_api.BASE_URL`).
pub const BASE_URL: &str = "https://a3.aliceblueonline.com";
/// Vendor login host (web `auth_api.py`).
pub const AUTH_URL: &str = "https://ant.aliceblueonline.com";
/// Contract master CSV directory (web `master_contract_db.py`).
pub const MASTER_URL: &str = "https://v2api.aliceblueonline.com/restpy/static/contract_master/V2";
/// Market-data sockets, primary then alternate (web `alicebluewebsocket.py:26-27`).
pub const WS_URLS: &[&str] = &[
    "wss://ws1.aliceblueonline.com/NorenWS/",
    "wss://ws2.aliceblueonline.com/NorenWS/",
];
/// Order Status Feed socket (web `aliceblue_order_adapter.py`).
pub const ORDER_WS_URL: &str = "wss://a3.aliceblueonline.com/open-api/order-notify/websocket";

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Bcd,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map`: AliceBlue serves 1-minute and daily
/// candles; every other intraday interval is resampled from 1 minute.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1"),
    ("3m", "1"),
    ("5m", "1"),
    ("10m", "1"),
    ("15m", "1"),
    ("30m", "1"),
    ("1h", "1"),
    ("D", "D"),
];

/// Every host the adapter talks to (tests point them at local fakes).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub base: String,
    pub auth: String,
    pub master: String,
    pub ws: Vec<String>,
    pub order_ws: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            base: BASE_URL.into(),
            auth: AUTH_URL.into(),
            master: MASTER_URL.into(),
            ws: WS_URLS.iter().map(|s| s.to_string()).collect(),
            order_ws: ORDER_WS_URL.into(),
        }
    }
}

/// Waits of the socket-backed quote calls (web `data.py`).
#[derive(Debug, Clone, Copy)]
pub struct QuoteTiming {
    /// Socket open + login budget (web waits up to 10 s for the login).
    pub connect: Duration,
    /// Single quote / depth wait (web sleeps 2 s).
    pub single: Duration,
    /// Pause before the retry of a failed single quote (web 1 s).
    pub retry_pause: Duration,
    /// Multiquote deadline per instrument, floor and ceiling
    /// (web `min(max(n * 0.08, 2), 20)` seconds).
    pub multi_per_symbol: Duration,
    pub multi_floor: Duration,
    pub multi_ceiling: Duration,
    /// Extra wait for stragglers (web 3 s).
    pub straggler: Duration,
    /// Poll period while waiting (web 50 ms).
    pub poll: Duration,
    /// Pooled socket closes after this long without a request.
    pub idle: Duration,
}

impl Default for QuoteTiming {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            single: Duration::from_secs(2),
            retry_pause: Duration::from_secs(1),
            multi_per_symbol: Duration::from_millis(80),
            multi_floor: Duration::from_secs(2),
            multi_ceiling: Duration::from_secs(20),
            straggler: Duration::from_secs(3),
            poll: Duration::from_millis(50),
            idle: Duration::from_secs(60),
        }
    }
}

/// Reads share AliceBlue's "all other requests" budget of 1800 per 15
/// minutes; the web keeps 50 in hand (`api/rate_limiter.py`). Orders are
/// not limited. State is one timestamp per request in the window, so it is
/// bounded by the budget itself.
#[derive(Debug)]
pub struct WindowLimiter {
    window: Duration,
    max: usize,
    stamps: tokio::sync::Mutex<VecDeque<Instant>>,
}

impl WindowLimiter {
    pub fn new(window: Duration, max: usize) -> Self {
        Self {
            window,
            max: max.max(1),
            stamps: tokio::sync::Mutex::new(VecDeque::new()),
        }
    }

    /// The web's budget: 1800 per 15 minutes minus a margin of 50.
    pub fn aliceblue() -> Self {
        Self::new(Duration::from_secs(15 * 60), 1800 - 50)
    }

    /// Wait until a request fits in the trailing window, then claim it.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut q = self.stamps.lock().await;
                let now = Instant::now();
                while q
                    .front()
                    .is_some_and(|t| now.duration_since(*t) >= self.window)
                {
                    q.pop_front();
                }
                if q.len() < self.max {
                    q.push_back(now);
                    return;
                }
                match q.front() {
                    Some(first) => self.window.saturating_sub(now.duration_since(*first)),
                    None => Duration::ZERO,
                }
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }

    /// Requests counted in the current window.
    pub async fn in_window(&self) -> usize {
        self.stamps.lock().await.len()
    }
}

pub struct AliceBlueBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) ep: Endpoints,
    pub(crate) symbols: SymbolResolver,
    pub(crate) reads: WindowLimiter,
    pub(crate) timing: QuoteTiming,
    pub(crate) quotes: data::QuotePool,
}

impl AliceBlueBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_endpoints(symbols, Endpoints::default())
    }

    /// Point every host at a fake (tests).
    pub fn with_endpoints(symbols: SymbolResolver, ep: Endpoints) -> Self {
        Self {
            http: http::client(),
            ep,
            symbols,
            reads: WindowLimiter::aliceblue(),
            timing: QuoteTiming::default(),
            quotes: data::QuotePool::default(),
        }
    }

    /// Shrink the socket waits (tests).
    pub fn with_quote_timing(mut self, timing: QuoteTiming) -> Self {
        self.timing = timing;
        self
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.ep
    }

    /// Bearer GET/POST to the trading host, returning the JSON body.
    pub(crate) async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        limited: bool,
    ) -> Result<(StatusCode, Value)> {
        if limited {
            self.reads.acquire().await;
        }
        let mut req = self
            .http
            .request(method, format!("{}{}", self.ep.base, path))
            .header("Authorization", format!("Bearer {}", auth.raw()))
            .header("Content-Type", "application/json");
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            tracing::warn!(
                status = status.as_u16(),
                "AliceBlue refused the session on a REST call"
            );
            return Err(session_expired());
        }
        http::read_json("aliceblue", resp).await
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth(
        "Your AliceBlue session has expired. Log in to AliceBlue again from the broker page."
            .into(),
    )
}

/// Text of a JSON value (`null` and missing are empty).
pub(crate) fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// AliceBlue's `"status": "Ok"`.
pub(crate) fn status_ok(v: &Value) -> bool {
    text(v.get("status")) == "Ok"
}

/// The broker's error, EC codes expanded, worded for a trader.
pub(crate) fn broker_error(v: &Value, fallback: &str) -> AppError {
    let raw = [text(v.get("message")), text(v.get("emsg"))]
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_default();
    if raw.is_empty() {
        return AppError::Broker(fallback.to_string());
    }
    let lower = raw.to_ascii_lowercase();
    if lower.contains("session") && (lower.contains("expired") || lower.contains("invalid"))
        || lower.contains("unauthorized")
    {
        return session_expired();
    }
    AppError::Broker(format!("AliceBlue: {}", mapping::describe(&raw)))
}

#[async_trait]
impl Broker for AliceBlueBroker {
    fn id(&self) -> &'static str {
        "aliceblue"
    }

    fn name(&self) -> &'static str {
        "Alice Blue"
    }

    fn logo(&self) -> &'static str {
        "/logos/aliceblue.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "authCode" }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: true,
            margin: false,
            gtt: false,
            streaming: true,
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn requires_totp(&self) -> bool {
        false
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(self, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        orders::place_order(self, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        orders::modify_order(self, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(self, auth, order_id).await
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(self, auth).await
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(self, auth).await
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(self, auth, symbol, exchange, product).await
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        orders::get_order_book(self, auth).await
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        orders::get_trade_book(self, auth).await
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        orders::get_positions(self, auth).await
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(self, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(self, auth, key).await
    }

    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        data::get_multiquotes(self, auth, keys).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(self, auth, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let ucc = auth::ucc(auth).ok_or_else(missing_ucc)?;
        Ok(Box::new(streaming::AliceBlueFeed::new(
            self.http.clone(),
            self.ep.clone(),
            auth.raw(),
            &ucc,
        )))
    }
}

impl AliceBlueBroker {
    /// The Order Status Feed socket (`createWsToken`, then
    /// `{"orderToken","userId"}`), through the loopback relay.
    pub fn create_order_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let ucc = auth::ucc(auth).ok_or_else(missing_ucc)?;
        Ok(Box::new(streaming::AliceBlueOrderFeed::new(
            self.http.clone(),
            self.ep.clone(),
            auth.raw(),
            &ucc,
            self.symbols.clone(),
        )))
    }

    /// Close the pooled quote socket (logout, session change).
    pub fn close_quote_socket(&self) {
        self.quotes.close();
    }
}

pub(crate) fn missing_ucc() -> AppError {
    AppError::Auth(
        "OpenAlgo could not read your AliceBlue client code from the session. Log in to AliceBlue again."
            .into(),
    )
}
