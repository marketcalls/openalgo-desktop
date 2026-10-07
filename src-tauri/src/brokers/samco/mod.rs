//! Samco Trade API v3.2 adapter (web `broker/samco/**`).
//!
//! Sign-in is server to server: the stored API key and secret of the
//! trader's Samco OAuth app are exchanged at `POST /session/token` for a
//! JWT session token, sent as `x-session-token` on every call (valid until
//! 08:00 IST the next day). The legacy user id / password / year-of-birth
//! flow was removed from the web in v3.2 and is not implemented.
//!
//! * Base URL `https://tradeapi.samco.in`; JSON bodies; HTTP 200 with
//!   `status: "Failure"` for business errors.
//! * Market-data reads retry 429 and 5xx with 1/2/4 s back-off; 403 means
//!   the session is no longer accepted (web `api/data.py`).
//! * Samco only documents L and SL order types, so MARKET and SL-M are sent
//!   as protected limits (web `mapping/transform_data.py`).
//! * Tokens are Samco's `<scripCode>_<segment>` (`41015_NFO`), which is also
//!   the multiQuote and streaming join key.

pub mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::redact;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub use auth::{
    ip_status_error, IpStatus, DASHBOARD_URL, IP_STATUS_NOT_CONNECTED, IP_STATUS_NOT_LOGGED_IN,
};

pub const BASE_URL: &str = "https://tradeapi.samco.in";
/// Consolidated scrip master (web `master_contract_download`).
pub const SCRIP_MASTER_URL: &str = "https://developers.stocknote.com/doc/ScripMaster.csv";
/// Broadcast feed.
pub const WS_URL: &str = "wss://stream.samco.in";

/// web `plugin.json` supported_exchanges.
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

/// web `BrokerData.timeframe_map`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1"),
    ("5m", "5"),
    ("10m", "10"),
    ("15m", "15"),
    ("30m", "30"),
    ("1h", "60"),
    ("D", "DAY"),
];

/// multiQuote symbols per request (web `BATCH_SIZE`).
pub const MULTIQUOTE_BATCH: usize = 25;

/// Most index listing ids kept (Samco lists 68 indices).
pub const LISTING_CACHE_CAP: usize = 256;

/// Index `listingId`s (`-23` for NIFTY) learnt from `/quote/indexQuote`,
/// keyed by OpenAlgo `(exchange, symbol)`. The streaming feed subscribes
/// indices by listing id; the REST index-quote path fills this. Bounded:
/// cleared when full (it only ever holds the index list).
#[derive(Debug, Default)]
pub struct ListingIds {
    map: HashMap<(String, String), String>,
}

impl ListingIds {
    pub fn insert(&mut self, exchange: &str, symbol: &str, listing_id: &str) {
        if self.map.len() >= LISTING_CACHE_CAP
            && !self
                .map
                .contains_key(&(exchange.to_string(), symbol.to_string()))
        {
            self.map.clear();
        }
        self.map.insert(
            (exchange.to_string(), symbol.to_string()),
            listing_id.to_string(),
        );
    }

    pub fn get(&self, exchange: &str, symbol: &str) -> Option<String> {
        self.map
            .get(&(exchange.to_string(), symbol.to_string()))
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

pub struct SamcoBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) master_url: String,
    pub(crate) ws_url: String,
    pub(crate) symbols: SymbolResolver,
    /// First back-off step for 429 / 5xx retries (1 s on the web).
    pub(crate) retry_base: Duration,
    /// Pause between multiQuote batches (0.2 s on the web).
    pub(crate) batch_delay: Duration,
    /// Fixed "today" for tests; IST today otherwise.
    pub(crate) today: Option<NaiveDate>,
    pub(crate) listing_ids: Arc<Mutex<ListingIds>>,
}

impl SamcoBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, SCRIP_MASTER_URL, WS_URL)
    }

    /// Point the REST host, the scrip master and the feed at fakes.
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        master_url: impl Into<String>,
        ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            master_url: master_url.into(),
            ws_url: ws_url.into(),
            symbols,
            retry_base: Duration::from_secs(1),
            batch_delay: Duration::from_millis(200),
            today: None,
            listing_ids: Arc::new(Mutex::new(ListingIds::default())),
        }
    }

    /// Shrink retry back-off and batch pacing (tests).
    pub fn with_timing(mut self, retry_base: Duration, batch_delay: Duration) -> Self {
        self.retry_base = retry_base;
        self.batch_delay = batch_delay;
        self
    }

    /// Pin "today" (tests).
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn today(&self) -> NaiveDate {
        self.today.unwrap_or_else(|| {
            chrono::Utc::now()
                .with_timezone(&chrono_tz::Asia::Kolkata)
                .date_naive()
        })
    }

    /// The listing id cache the feed shares.
    pub fn listing_ids(&self) -> Arc<Mutex<ListingIds>> {
        self.listing_ids.clone()
    }

    fn token(auth: &AuthToken) -> Result<&str> {
        let t = auth.raw().trim();
        if t.is_empty() {
            return Err(session_expired());
        }
        Ok(t)
    }

    /// One call, no retry (web `order_api.get_api_response`). `path` may
    /// carry a query string.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let token = Self::token(auth)?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Accept", "application/json")
            .header("x-session-token", token);
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.map_err(redact::http)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(redact::http)?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(session_expired());
        }
        if bytes.iter().all(u8::is_ascii_whitespace) {
            // web: an empty body reads as `{}`.
            return Ok((status, Value::Object(Default::default())));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => Ok((status, v)),
            Err(_) => {
                tracing::warn!(
                    status = status.as_u16(),
                    "Samco sent a response that is not JSON for {}",
                    path.split('?').next().unwrap_or("")
                );
                if status == StatusCode::FORBIDDEN {
                    return Err(ip_or_session_refused());
                }
                Ok((status, Value::Object(Default::default())))
            }
        }
    }

    /// A market-data call with the web's retry policy: 403 is an auth
    /// failure, 429 and 5xx are retried up to three times (1, 2, 4 s).
    pub(crate) async fn data_call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        const MAX_RETRIES: u32 = 3;
        let token = Self::token(auth)?;
        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .request(method.clone(), format!("{}{}", self.base_url, path))
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .header("x-session-token", token);
            if let Some(b) = body {
                req = req.body(b.to_string());
            }
            let resp = req.send().await.map_err(redact::http)?;
            let status = resp.status();
            if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
                tracing::warn!(
                    status = status.as_u16(),
                    "Samco refused the session for a data call"
                );
                return Err(session_expired());
            }
            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt < MAX_RETRIES {
                let delay = self.retry_base.saturating_mul(1u32 << attempt);
                tracing::warn!(
                    status = status.as_u16(),
                    attempt = attempt + 1,
                    "Samco asked us to slow down or failed; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                return Err(AppError::Broker(
                    "Samco is limiting requests right now. Wait a moment and try again.".into(),
                ));
            }
            if status.is_server_error() {
                let bytes = resp.bytes().await.unwrap_or_default();
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                tracing::error!(
                    status = status.as_u16(),
                    msg_id = %mapping::text(v.get("msgId")),
                    server_time = %mapping::text(v.get("serverTime")),
                    "Samco server error persisted after retries"
                );
                return Err(AppError::Broker(
                    "Samco's servers are not responding normally. Try again shortly.".into(),
                ));
            }
            let (_, v): (StatusCode, Value) = http::read_json("samco", resp)
                .await
                .map_err(redact::redact)?;
            return Ok(v);
        }
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Samco session has expired. Connect to Samco again.".into())
}

pub(crate) fn ip_or_session_refused() -> AppError {
    AppError::Auth(format!(
        "Samco refused the request. Either your session has expired (connect to Samco again) or this computer's IP address is not registered as a static IP at {}.",
        DASHBOARD_URL
    ))
}

/// `status == "Success"`.
pub fn is_success(v: &Value) -> bool {
    v.get("status").and_then(Value::as_str) == Some("Success")
}

/// The error a `Failure` body stands for.
pub fn samco_error(status: StatusCode, v: &Value, fallback: &str) -> AppError {
    let msg = mapping::text(v.get("statusMessage"));
    let lower = msg.to_ascii_lowercase();
    if status == StatusCode::UNAUTHORIZED
        || lower.contains("session expired")
        || lower.contains("invalid session")
        || lower.contains("session token")
    {
        return session_expired();
    }
    if status == StatusCode::FORBIDDEN {
        return ip_or_session_refused();
    }
    if msg.is_empty() {
        AppError::Broker(fallback.to_string())
    } else {
        AppError::Broker(format!("Samco: {}", msg))
    }
}

/// Python `urllib.parse.quote(s)` (safe `/`).
pub fn url_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "_.-~/".contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[async_trait]
impl Broker for SamcoBroker {
    fn id(&self) -> &'static str {
        "samco"
    }

    fn name(&self) -> &'static str {
        "Samco"
    }

    fn logo(&self) -> &'static str {
        "/logos/samco.svg"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::ApiKeySecret
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

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let token = Self::token(auth)?;
        Ok(Box::new(streaming::SamcoFeed::new(
            &self.ws_url,
            token,
            self.listing_ids.clone(),
        )))
    }
}

impl SamcoBroker {
    /// Static IP diagnostic behind the web's `GET /samco/ip-status`.
    pub async fn ip_status(&self, auth: &AuthToken) -> Result<IpStatus> {
        auth::ip_status(self, auth).await
    }

    /// Learn an index's streaming `listingId` (web
    /// `get_index_listing_id`), caching it for the feed.
    pub async fn index_listing_id(&self, auth: &AuthToken, key: &QuoteKey) -> Result<String> {
        data::index_listing_id(self, auth, key).await
    }
}
