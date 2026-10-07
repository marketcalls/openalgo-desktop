//! Definedge Securities adapter (web `broker/definedge/**`, the "Integrate"
//! API with a Noren-lineage WebSocket).
//!
//! Credentials, as on the web:
//! * API key: the Integrate `api_token`.
//! * API secret: the Integrate `api_secret`.
//!
//! Sign-in is OTP based (`LoginKind::TwoStep`): step one
//! (`GET <signin>/login/<api_token>` with an `api_secret` header) sends an
//! OTP to the registered mobile/email and returns an `otp_token`; step two
//! posts `{otp_token, otp, ac = sha256(otp_token + otp + api_secret)}` to
//! `<signin>/token` and returns `api_session_key`, `susertoken` and `uid`.
//! The stored session is the web's composite
//! `api_session_key:::susertoken:::api_token`; the feed token is the
//! `susertoken` and the user id the `uid`.
//!
//! REST calls carry `Authorization: <api_session_key>` (raw, no `Bearer`).
//! Requests are paced at one every 0.1 s per host (trading and data hosts
//! pace independently) and a 429 on a read is retried up to three times
//! with 1/2/4 s back-off (or the server's `Retry-After`), like the web's
//! `api/rate_limiter.py`. Order writes are not retried on 429: the broker
//! may already have accepted them.

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
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub use data::{
    candle_epoch, depth_from, history_chunks, parse_history_csv, quote_from, resample,
    session_open_minute, HistoryWindow, TIMEFRAME_MAP,
};
pub use funds::{funds_from_limits, margin_positions, parse_margin};

/// Production hosts (web `api/baseurl.py`, `master_contract_db.py`,
/// `streaming/definedge_websocket.py`).
pub const SIGNIN_URL: &str = "https://signin.definedgesecurities.com/auth/realms/debroking/dsbpkc";
pub const TRADE_URL: &str = "https://integrate.definedgesecurities.com/dart/v1";
pub const DATA_URL: &str = "https://data.definedgesecurities.com/sds";
pub const MASTER_URL: &str = "https://app.definedgesecurities.com/public/allmaster.zip";
pub const WS_URL: &str = "wss://trade.definedgesecurities.com/NorenWSTRTP/";

/// How long an OTP token from step one stays usable.
pub const OTP_TTL: Duration = Duration::from_secs(600);

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

/// Every host the adapter talks to; tests point them at a local fake.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub signin: String,
    pub trade: String,
    pub data: String,
    pub master: String,
    pub ws: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            signin: SIGNIN_URL.into(),
            trade: TRADE_URL.into(),
            data: DATA_URL.into(),
            master: MASTER_URL.into(),
            ws: WS_URL.into(),
        }
    }
}

/// The parsed session token (`api_session_key:::susertoken:::api_token`).
#[derive(Clone)]
pub struct DefinedgeSession {
    pub api_session_key: String,
    pub susertoken: String,
    pub api_token: String,
}

impl std::fmt::Debug for DefinedgeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DefinedgeSession([REDACTED])")
    }
}

impl DefinedgeSession {
    pub fn parse(auth: &AuthToken) -> Result<Self> {
        let parts: Vec<&str> = auth.raw().split(":::").collect();
        if parts.len() != 3 || parts[0].trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Self {
            api_session_key: parts[0].trim().to_string(),
            susertoken: parts[1].trim().to_string(),
            api_token: parts[2].trim().to_string(),
        })
    }

    pub fn compose(&self) -> String {
        format!(
            "{}:::{}:::{}",
            self.api_session_key, self.susertoken, self.api_token
        )
    }
}

/// An OTP token waiting for the trader's OTP (one slot, replaced on every
/// send, cleared on success or expiry).
pub(crate) struct PendingOtp {
    pub token: Secret,
    pub sent_at: Instant,
}

/// Bound of the open-interest backfill cache (web `_OI_CACHE_MAXSIZE`).
pub const OI_CACHE_MAX: usize = 4096;
/// Lifetime of one backfilled OI value (web `_OI_CACHE_TTL`).
pub const OI_CACHE_TTL: Duration = Duration::from_secs(60);

pub struct DefinedgeBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) urls: Endpoints,
    pub(crate) symbols: SymbolResolver,
    /// One pacing clock per host (web `_last_call_time[bucket]`).
    pub(crate) trade_pacer: Pacer,
    pub(crate) data_pacer: Pacer,
    /// Base of the 429 back-off (1 s on the web); tests shrink it.
    pub(crate) retry_base: Duration,
    /// Base of the history chunk retry (0.5 s on the web).
    pub(crate) chunk_retry_base: Duration,
    pub(crate) pending_otp: Mutex<Option<PendingOtp>>,
    /// `(segment, token) -> (oi, fetched)`, bounded by `OI_CACHE_MAX`.
    pub(crate) oi_cache: Mutex<HashMap<(String, String), (i64, Instant)>>,
}

impl DefinedgeBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_endpoints(symbols, Endpoints::default())
    }

    /// Point every host at a local fake (tests).
    pub fn with_endpoints(symbols: SymbolResolver, urls: Endpoints) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            trade_pacer: Pacer::with_interval(Duration::from_millis(100)),
            data_pacer: Pacer::with_interval(Duration::from_millis(100)),
            retry_base: Duration::from_secs(1),
            chunk_retry_base: Duration::from_millis(500),
            pending_otp: Mutex::new(None),
            oi_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Shrink pacing and retry delays (tests).
    pub fn with_fast_timing(mut self) -> Self {
        self.trade_pacer = Pacer::with_interval(Duration::from_millis(1));
        self.data_pacer = Pacer::with_interval(Duration::from_millis(1));
        self.retry_base = Duration::from_millis(5);
        self.chunk_retry_base = Duration::from_millis(5);
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// Whether an unexpired OTP token is waiting for the trader's OTP.
    pub fn otp_pending(&self) -> bool {
        self.pending_otp
            .lock()
            .as_ref()
            .is_some_and(|p| p.sent_at.elapsed() < OTP_TTL)
    }

    fn pacer_for(&self, url: &str) -> &Pacer {
        if url.starts_with(&self.urls.data) {
            &self.data_pacer
        } else {
            &self.trade_pacer
        }
    }

    /// One paced request with the web's 429 handling. `order` writes are
    /// returned as answered (no retry). Returns the status and body text.
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &str,
        session_key: &str,
        body: Option<&Value>,
        order: bool,
    ) -> Result<(StatusCode, String)> {
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0u32;
        loop {
            self.pacer_for(url).acquire().await;
            let mut req = self
                .http
                .request(method.clone(), url)
                .header("Authorization", session_key);
            if let Some(b) = body {
                req = req
                    .header("Content-Type", "application/json")
                    .body(b.to_string());
            }
            let resp = req.send().await?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok());
            let text = resp.text().await?;
            if status != StatusCode::TOO_MANY_REQUESTS || order || attempt >= MAX_RETRIES {
                return Ok((status, text));
            }
            let delay = match retry_after {
                Some(s) if s.is_finite() => Duration::from_secs_f64(s.clamp(0.05, 60.0)),
                _ => self.retry_base.saturating_mul(1u32 << attempt),
            };
            tracing::warn!(
                broker = "definedge",
                "Rate limited by Definedge; retry {}/{}",
                attempt + 1,
                MAX_RETRIES
            );
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    /// A trading-host call whose body must be JSON. HTTP failures become
    /// trader-facing errors (401/403: session expired).
    pub(crate) async fn trade_json(
        &self,
        s: &DefinedgeSession,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.urls.trade, path);
        let (status, text) = self
            .send(method, &url, &s.api_session_key, body, false)
            .await?;
        http_json(status, &text)
    }
}

/// Interpret a REST answer: auth failures, rate limits and outages become
/// trader-facing errors; any other body must be JSON.
pub(crate) fn http_json(status: StatusCode, text: &str) -> Result<Value> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(session_expired());
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::Broker(
            "Definedge is limiting requests right now. Wait a moment and try again.".into(),
        ));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v) if status.is_success() => Ok(v),
        Ok(v) => Err(broker_error(
            &v,
            "Definedge could not complete the request.",
        )),
        Err(_) => {
            tracing::warn!(
                broker = "definedge",
                status = status.as_u16(),
                "Unreadable response from Definedge"
            );
            if status.is_server_error() {
                Err(AppError::Broker(
                    "Definedge's servers are not responding normally. Try again shortly.".into(),
                ))
            } else {
                Err(AppError::Broker(
                    "Definedge sent a response OpenAlgo could not read. Try again shortly.".into(),
                ))
            }
        }
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth(
        "Your Definedge session has expired. Log in to Definedge again with a fresh OTP.".into(),
    )
}

/// Text of a JSON field (numbers rendered), empty when absent.
pub(crate) fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// A body field as f64 (numbers or numeric strings, 0 otherwise).
pub(crate) fn num(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A body field as i64 (Python `int(float(x))`).
pub(crate) fn int(v: &Value, k: &str) -> i64 {
    match v.get(k) {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => {
            let t = s.trim();
            t.parse::<i64>()
                .unwrap_or_else(|_| t.parse::<f64>().unwrap_or(0.0) as i64)
        }
        _ => 0,
    }
}

/// `stat == "Ok"` or `status == "SUCCESS"` (the two success dialects).
pub(crate) fn is_success(v: &Value) -> bool {
    text(v, "stat") == "Ok" || text(v, "status") == "SUCCESS"
}

/// The error a failure body stands for (`emsg`, then `message`).
pub(crate) fn broker_error(v: &Value, fallback: &str) -> AppError {
    let msg = [text(v, "emsg"), text(v, "message")]
        .into_iter()
        .find(|m| !m.is_empty())
        .unwrap_or_default();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("session expired")
        || lower.contains("invalid session")
        || lower.contains("unauthorized")
        || lower.contains("invalid token")
    {
        return session_expired();
    }
    if msg.is_empty() {
        AppError::Broker(fallback.to_string())
    } else {
        AppError::Broker(format!("Definedge: {}", msg))
    }
}

#[async_trait]
impl Broker for DefinedgeBroker {
    fn id(&self) -> &'static str {
        "definedge"
    }

    fn name(&self) -> &'static str {
        "Definedge Securities"
    }

    fn logo(&self) -> &'static str {
        "/logos/definedge.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::TwoStep {
            step1: &[],
            step2: &["otp"],
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
        let (uid, token) = streaming::feed_identity(auth)?;
        Ok(Box::new(streaming::DefinedgeFeed::new(
            &self.urls.ws,
            &uid,
            &token,
        )))
    }
}

impl DefinedgeBroker {
    /// The order-update socket (same NorenWSTRTP host, `{"t":"o"}` after the
    /// connect frame).
    pub fn create_order_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (uid, token) = streaming::feed_identity(auth)?;
        Ok(Box::new(streaming::DefinedgeOrderFeed::new(
            &self.urls.ws,
            &uid,
            &token,
            self.symbols.clone(),
        )))
    }

    /// Security information for an instrument (web `get_security_info`).
    pub async fn security_info(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
        data::security_info(self, auth, key).await
    }
}
