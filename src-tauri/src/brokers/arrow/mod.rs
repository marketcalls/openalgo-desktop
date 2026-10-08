//! Arrow adapter (web `broker/arrow/**`).
//!
//! * Sign-in is a Kite-style redirect: the `request-token` is exchanged for
//!   a JWT with `SHA256("appID:appSecret:request-token")`.
//! * The stored session token is `appID:jwt`. Every REST call carries the two
//!   custom headers `appID: <appID>` and `token: <jwt>` (never `Bearer`).
//! * REST lives on `edge.arrow.trade`, candles on `historical-api.arrow.trade`,
//!   the binary market feed on `ds.arrow.trade` and order updates on
//!   `order-updates.arrow.trade`.
//! * Quote and candle prices are paise (divide by 100); book prices are
//!   rupees.

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

pub use auth::checksum;
pub use data::{format_quote, parse_candles, to_depth};
pub use funds::{funds_from_limits, margin_bodies};

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

pub const REST_URL: &str = "https://edge.arrow.trade";
pub const HISTORY_URL: &str = "https://historical-api.arrow.trade";
pub const WS_MARKET_URL: &str = "wss://ds.arrow.trade";
pub const WS_ORDERS_URL: &str = "wss://order-updates.arrow.trade";

/// `plugin.json` supported_exchanges.
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

/// web `BrokerData.timeframe_map` (`api/data.py:59-73`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "min"),
    ("3m", "3min"),
    ("5m", "5min"),
    ("10m", "10min"),
    ("15m", "15min"),
    ("30m", "30min"),
    ("1h", "hour"),
    ("2h", "2hours"),
    ("3h", "3hours"),
    ("4h", "4hours"),
    ("D", "day"),
    ("W", "week"),
    ("M", "month"),
];

/// Bound on the per-token index quote-name caches (the index list is about
/// two hundred rows; this only guards against unbounded growth).
const INDEX_CACHE_MAX: usize = 2048;

/// Hosts the adapter talks to (tests point all four at a local fake).
#[derive(Debug, Clone)]
pub struct ArrowUrls {
    pub rest: String,
    pub history: String,
    pub ws_market: String,
    pub ws_orders: String,
}

impl Default for ArrowUrls {
    fn default() -> Self {
        Self {
            rest: REST_URL.into(),
            history: HISTORY_URL.into(),
            ws_market: WS_MARKET_URL.into(),
            ws_orders: WS_ORDERS_URL.into(),
        }
    }
}

impl ArrowUrls {
    /// Every host on one base (tests).
    pub fn rebased(http_base: &str, ws_base: &str) -> Self {
        Self {
            rest: http_base.into(),
            history: http_base.into(),
            ws_market: ws_base.into(),
            ws_orders: ws_base.into(),
        }
    }
}

/// Pacing category of a call.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Category {
    Order,
    Quote,
    History,
    Other,
}

pub struct ArrowBroker {
    http: reqwest::Client,
    urls: ArrowUrls,
    symbols: SymbolResolver,
    /// Arrow allows 10 requests/s; the web spaces quote batches and history
    /// chunks 0.15 s apart (`data.py:241-251`).
    order_pacer: Pacer,
    quote_pacer: Pacer,
    history_pacer: Pacer,
    other_pacer: Pacer,
    /// token -> the symbol Arrow's INDEX quote endpoint accepted.
    index_names: Mutex<HashMap<String, String>>,
    /// tokens every candidate was refused for.
    index_unsupported: Mutex<HashSet<String>>,
}

impl ArrowBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, ArrowUrls::default())
    }

    /// Point the adapter at other hosts (tests run a local fake Arrow).
    pub fn with_base_url(symbols: SymbolResolver, urls: ArrowUrls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            order_pacer: Pacer::per_second(10.0),
            quote_pacer: Pacer::with_interval(Duration::from_millis(150)),
            history_pacer: Pacer::with_interval(Duration::from_millis(150)),
            other_pacer: Pacer::per_second(10.0),
            index_names: Mutex::new(HashMap::new()),
            index_unsupported: Mutex::new(HashSet::new()),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn urls(&self) -> &ArrowUrls {
        &self.urls
    }

    pub(crate) fn cached_index_name(&self, token: &str) -> Option<String> {
        self.index_names.lock().get(token).cloned()
    }

    pub(crate) fn remember_index_name(&self, token: &str, name: &str) {
        let mut m = self.index_names.lock();
        if m.len() >= INDEX_CACHE_MAX {
            m.clear();
        }
        m.insert(token.to_string(), name.to_string());
    }

    pub(crate) fn index_refused(&self, token: &str) -> bool {
        self.index_unsupported.lock().contains(token)
    }

    pub(crate) fn mark_index_refused(&self, token: &str) {
        let mut s = self.index_unsupported.lock();
        if s.len() >= INDEX_CACHE_MAX {
            s.clear();
        }
        s.insert(token.to_string());
    }

    /// `(appID, jwt)` from the stored `appID:jwt`.
    pub(crate) fn credentials(auth: &AuthToken) -> Result<(&str, &str)> {
        auth.pair().ok_or_else(session_expired)
    }

    async fn pace(&self, cat: Category) {
        match cat {
            Category::Order => self.order_pacer.acquire().await,
            Category::Quote => self.quote_pacer.acquire().await,
            Category::History => self.history_pacer.acquire().await,
            Category::Other => self.other_pacer.acquire().await,
        }
    }

    /// Send one authenticated request; the body (if any) goes as JSON.
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        cat: Category,
    ) -> Result<reqwest::Response> {
        let (app_id, jwt) = Self::credentials(auth)?;
        self.pace(cat).await;
        let mut req = self
            .http
            .request(method, url)
            .header("appID", app_id)
            .header("token", jwt);
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send().await.map_err(|e| redact(e.into()))
    }

    /// Send and decode a JSON answer, with the HTTP status.
    pub(crate) async fn call_raw(
        &self,
        method: Method,
        url: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        cat: Category,
    ) -> Result<(StatusCode, Value)> {
        let resp = self.send(method, url, auth, body, cat).await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            tracing::warn!(status = status.as_u16(), "Arrow refused the session");
            return Err(session_expired());
        }
        http::read_json::<Value>("arrow", resp)
            .await
            .map_err(redact)
    }

    /// Send and unwrap the `{status, data}` envelope: `data` on success
    /// (or when the envelope carries no status, as the web accepts), a
    /// trader-facing error otherwise.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        cat: Category,
    ) -> Result<Value> {
        let url = format!("{}{}", self.urls.rest, path);
        let (status, v) = self.call_raw(method, &url, auth, body, cat).await?;
        envelope(status, v, path)
    }
}

/// `{status, data, message}` -> `data`.
pub(crate) fn envelope(status: StatusCode, mut v: Value, what: &str) -> Result<Value> {
    let st = v.get("status").and_then(Value::as_str).map(str::to_string);
    let ok = match st.as_deref() {
        Some("success") => true,
        None => status.is_success(),
        Some(_) => false,
    };
    if ok && status.is_success() {
        return Ok(v.get_mut("data").map(Value::take).unwrap_or(Value::Null));
    }
    let msg = message_of(&v);
    tracing::warn!(
        status = status.as_u16(),
        "Arrow refused {}: {}",
        what.split('?').next().unwrap_or(""),
        msg
    );
    Err(arrow_error(&msg))
}

/// The broker's `message` text, if any.
pub(crate) fn message_of(v: &Value) -> String {
    v.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Arrow session has expired. Log in to Arrow again.".into())
}

/// Broker message -> trader-facing error.
pub(crate) fn arrow_error(message: &str) -> AppError {
    let lower = message.to_ascii_lowercase();
    if [
        "invalid token",
        "token expired",
        "session expired",
        "unauthorized",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        return session_expired();
    }
    if message.is_empty() {
        AppError::Broker("Arrow refused the request.".into())
    } else {
        AppError::Broker(message.to_string())
    }
}

#[async_trait]
impl Broker for ArrowBroker {
    fn id(&self) -> &'static str {
        "arrow"
    }

    fn name(&self) -> &'static str {
        "Arrow"
    }

    fn logo(&self) -> &'static str {
        "/logos/arrow.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "request-token",
        }
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
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
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

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(self, auth, legs).await
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

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth).await
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (app_id, jwt) = Self::credentials(auth)?;
        Ok(Box::new(streaming::ArrowFeed::new(
            &self.urls.ws_market,
            app_id,
            jwt,
        )))
    }
}

impl ArrowBroker {
    /// The order-update stream (web `streaming/arrow_order_adapter.py`), a
    /// second socket, served through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (app_id, jwt) = Self::credentials(auth)?;
        Ok(Box::new(streaming::ArrowOrderFeed::new(
            &self.urls.ws_orders,
            app_id,
            jwt,
            self.symbols.clone(),
        )))
    }
}

/// Transport errors carry the request URL, and with it query credentials
/// (API key, account id). Strip it before the error can reach a log or a
/// caller; every other error passes through unchanged.
pub(crate) fn redact(e: AppError) -> AppError {
    match e {
        AppError::Http(h) => AppError::Http(Box::new(h.without_url())),
        other => other,
    }
}
