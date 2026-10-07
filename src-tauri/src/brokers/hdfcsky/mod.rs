//! HDFC Sky adapter (web `broker/hdfcsky/**`).
//!
//! * Session token: `api_key:access_token`. The app `api_key` travels as a
//!   query parameter on every call; account calls also want `client_id`,
//!   which is the access token's JWT `sub` claim.
//! * Headers: `Authorization: <access_token>` (no `Bearer`), a mandatory
//!   browser `User-Agent`, `Accept: application/json`.
//! * URLs carry the API key, so they are never logged, and transport errors
//!   are stripped of their URL before they travel.
//! * Merchant keys may only place LIMIT orders: MARKET goes out as LIMIT and
//!   SL-M as SL, each with a Market Price Protection buffer.

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod proto;
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
use base64::Engine;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub const BASE_URL: &str = "https://developer.hdfcsky.com";
/// Public security master: a ZIP holding `CompactScrip.csv`.
pub const MASTER_URL: &str = "https://hdfcsky.com/api/v1/contract/Compact?info=download";
pub const WS_URL: &str = "wss://developer.hdfcsky.com/wsapi/v1/session";
/// The documentation's sample User-Agent; requests without one are refused.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData._interval_spec` keys (the chart API serves MINUTE and
/// DAY; the rest is resampled).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1m"),
    ("3m", "3m"),
    ("5m", "5m"),
    ("10m", "10m"),
    ("15m", "15m"),
    ("30m", "30m"),
    ("1h", "1h"),
    ("D", "D"),
    ("W", "W"),
    ("M", "M"),
];

/// Bound of the index chart-symbol cache.
const CHART_CACHE_MAX: usize = 1024;

/// Hosts the adapter talks to (tests point them at a local fake).
#[derive(Debug, Clone)]
pub struct Urls {
    pub base: String,
    pub master: String,
    pub ws: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            base: BASE_URL.into(),
            master: MASTER_URL.into(),
            ws: WS_URL.into(),
        }
    }
}

pub struct HdfcSkyBroker {
    http: reqwest::Client,
    urls: Urls,
    symbols: SymbolResolver,
    /// `(exchange, symbol)` -> the chart symbol form that returns candles
    /// (indices answer to one of two forms). Bounded.
    chart_symbols: Mutex<HashMap<(String, String), String>>,
    /// Retry and pacing delays (shortened in tests).
    pub(crate) delays: Delays,
}

/// Web sleeps and backoffs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Delays {
    /// Between multiquote batches and history chunks (0.15 s).
    pub pace: Duration,
    /// First 429 backoff, doubled per attempt (0.5 s).
    pub backoff: Duration,
}

impl Default for Delays {
    fn default() -> Self {
        Self {
            pace: Duration::from_millis(150),
            backoff: Duration::from_millis(500),
        }
    }
}

/// The parts of a stored session.
pub(crate) struct Session {
    pub api_key: String,
    pub token: String,
    pub client_id: String,
}

impl HdfcSkyBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, Urls::default())
    }

    /// Point the adapter at other hosts: REST base, master URL, feed URL.
    pub fn with_base_url(
        symbols: SymbolResolver,
        base: impl Into<String>,
        master: impl Into<String>,
        ws: impl Into<String>,
    ) -> Self {
        let mut b = Self::with_urls(
            symbols,
            Urls {
                base: base.into(),
                master: master.into(),
                ws: ws.into(),
            },
        );
        // Local fakes need no real pacing.
        b.delays = Delays {
            pace: Duration::from_millis(1),
            backoff: Duration::from_millis(5),
        };
        b
    }

    pub fn with_urls(symbols: SymbolResolver, urls: Urls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            chart_symbols: Mutex::new(HashMap::new()),
            delays: Delays::default(),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn cached_chart_symbol(&self, exchange: &str, symbol: &str) -> Option<String> {
        self.chart_symbols
            .lock()
            .get(&(exchange.to_string(), symbol.to_string()))
            .cloned()
    }

    pub(crate) fn remember_chart_symbol(&self, exchange: &str, symbol: &str, chart: &str) {
        let mut m = self.chart_symbols.lock();
        if m.len() >= CHART_CACHE_MAX {
            m.clear();
        }
        m.insert(
            (exchange.to_string(), symbol.to_string()),
            chart.to_string(),
        );
    }

    /// Split the stored `api_key:access_token` and read the client id.
    pub(crate) fn session(auth: &AuthToken) -> Result<Session> {
        let (api_key, token) = auth.pair().ok_or_else(session_expired)?;
        let client_id = client_id_from_jwt(token)
            .or_else(|| auth.user_id().map(str::to_string))
            .unwrap_or_default();
        Ok(Session {
            api_key: api_key.to_string(),
            token: token.to_string(),
            client_id,
        })
    }

    /// One authenticated call. Returns the HTTP status and the JSON body;
    /// a refused token becomes the session-expired error.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        query: &[(&str, String)],
        with_client_id: bool,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let sess = Self::session(auth)?;
        let mut q: Vec<(&str, String)> = vec![("api_key", sess.api_key.clone())];
        if with_client_id && !sess.client_id.is_empty() {
            q.push(("client_id", sess.client_id.clone()));
        }
        q.extend(query.iter().cloned());
        let mut req = self
            .http
            .request(method, format!("{}{}", self.urls.base, path))
            .query(&q)
            .header("Authorization", sess.token.as_str())
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        // The URL carries the API key: never let it reach an error or a log.
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::from(e.without_url()))?;
        let (status, v): (_, Value) = http::read_json("hdfcsky", resp).await?;
        if status == StatusCode::UNAUTHORIZED || is_invalid_credentials(&v) {
            tracing::warn!(status = status.as_u16(), "HDFC Sky refused the session");
            return Err(session_expired());
        }
        Ok((status, v))
    }

    /// `send` that also requires `status == "success"` in the envelope.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        query: &[(&str, String)],
        with_client_id: bool,
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, v) = self
            .send(method, path, auth, query, with_client_id, body)
            .await?;
        if v.get("status").and_then(Value::as_str) == Some("success") {
            return Ok(v);
        }
        let msg = message_of(&v);
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Sky refused {}: {}",
            path.split('/').take(4).collect::<Vec<_>>().join("/"),
            msg
        );
        Err(broker_error(&msg))
    }
}

/// `message` / `error` of an error envelope (`error` may be an object).
pub(crate) fn message_of(v: &Value) -> String {
    if let Some(m) = v.get("message").and_then(Value::as_str) {
        if !m.trim().is_empty() {
            return m.trim().to_string();
        }
    }
    match v.get("error") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Object(o)) => o
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

fn is_invalid_credentials(v: &Value) -> bool {
    v.get("error")
        .and_then(Value::as_str)
        .map(|e| e.to_ascii_lowercase().contains("invalid credentials"))
        .unwrap_or(false)
}

pub(crate) fn broker_error(message: &str) -> AppError {
    if message.trim().is_empty() {
        AppError::Broker("HDFC Sky refused the request.".into())
    } else {
        AppError::Broker(message.trim().to_string())
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your HDFC Sky session has expired. Log in to HDFC Sky again.".into())
}

/// The account id in the access token's JWT `sub` (or `client_id`) claim.
/// The token is only read, not verified.
pub fn client_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    ["sub", "client_id"]
        .iter()
        .find_map(|k| match claims.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        })
}

#[async_trait]
impl Broker for HdfcSkyBroker {
    fn id(&self) -> &'static str {
        "hdfcsky"
    }

    fn name(&self) -> &'static str {
        "HDFC Sky"
    }

    fn logo(&self) -> &'static str {
        "/logos/hdfcsky.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "request_token",
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
            order_feed: false,
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

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let sess = Self::session(auth)?;
        Ok(Box::new(streaming::HdfcSkyFeed::new(
            &self.urls.ws,
            &sess.api_key,
            &sess.token,
        )))
    }
}
