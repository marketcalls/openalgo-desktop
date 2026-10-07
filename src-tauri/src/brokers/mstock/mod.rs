//! mStock (Mirae Asset) Type B adapter (web `broker/mstock/**`).
//!
//! Credentials, as on the web:
//! * API key: the mStock client code (login id).
//! * API secret: the mStock API key, sent as `X-PrivateKey` on every call
//!   (and as `API_KEY` on the market-data socket).
//!
//! Sign-in (`LoginKind::TwoStep`): `connect/login` with password and TOTP
//! returns a refresh token, then `session/verifytotp` with the same TOTP
//! returns the session `jwtToken` and the `feedToken`. The web runs both from
//! one form POST; so does `authenticate`.
//!
//! Stored session: `jwtToken:::privateKey`. The private key is needed on
//! every call and a resumed session is rebuilt from the stored token alone,
//! so it travels with the token (both are encrypted at rest; neither is ever
//! logged).
//!
//! Every REST call sends `X-Mirae-Version: 1`, `Authorization: Bearer <jwt>`
//! and `X-PrivateKey`. mStock answers HTTP 200 for business failures, so
//! success is the payload's `status` (`true` or `"true"` in any case), and a
//! payload can arrive wrapped in a one-element list (`order_api.py:26-45,
//! 288-291`).

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
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

/// REST base of the Type B API (`order_api.py:97`).
pub const BASE_URL: &str = "https://api.mstock.trade/openapi/typeb";
/// Index token annexure scraped for NSE and BSE indices
/// (`master_contract_db.py:200-205`).
pub const ANNEXURE_URL: &str = "https://tradingapi.mstock.com/docs/v1/Annexure/";
/// Market-data socket (`mstockwebsocket.py:73`).
pub const WS_URL: &str = "wss://ws.mstock.trade";

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map` (`data.py:114-125`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "ONE_MINUTE"),
    ("3m", "THREE_MINUTE"),
    ("5m", "FIVE_MINUTE"),
    ("10m", "TEN_MINUTE"),
    ("15m", "FIFTEEN_MINUTE"),
    ("30m", "THIRTY_MINUTE"),
    ("1h", "ONE_HOUR"),
    ("D", "ONE_DAY"),
];

/// The parsed session token. `Debug` is redacted.
#[derive(Clone)]
pub struct MstockSession {
    pub jwt: String,
    pub private_key: String,
}

impl std::fmt::Debug for MstockSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MstockSession([REDACTED])")
    }
}

impl MstockSession {
    /// `jwtToken:::privateKey`.
    pub fn parse(auth: &AuthToken) -> Result<Self> {
        let (jwt, key) = auth.raw().split_once(":::").ok_or_else(session_expired)?;
        let (jwt, key) = (jwt.trim(), key.trim());
        if jwt.is_empty() || key.is_empty() {
            return Err(session_expired());
        }
        Ok(Self {
            jwt: jwt.to_string(),
            private_key: key.to_string(),
        })
    }

    pub fn compose(&self) -> String {
        format!("{}:::{}", self.jwt, self.private_key)
    }
}

pub struct MstockBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) annexure_url: String,
    pub(crate) ws_url: String,
    pub(crate) symbols: SymbolResolver,
    /// mStock data APIs allow one request per second (`data.py:262-281,
    /// 594-605`): multiquote batches and history chunks share this pacer.
    pub(crate) data_pacer: Pacer,
    /// Budget of the one-shot depth socket (connect, login, first packet).
    pub(crate) depth_timeout: Duration,
    /// Fixed "today" (IST) for the history split; tests pin it.
    pub(crate) today: Option<NaiveDate>,
}

impl MstockBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, ANNEXURE_URL, WS_URL)
    }

    /// Point the REST base, the annexure page and the market-data socket at
    /// local fakes (tests).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        annexure_url: impl Into<String>,
        ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            annexure_url: annexure_url.into(),
            ws_url: ws_url.into(),
            symbols,
            data_pacer: Pacer::per_second(1.0),
            depth_timeout: Duration::from_secs(10),
            today: None,
        }
    }

    /// Replace the data-API pacing (tests shrink it).
    pub fn with_data_pacing(mut self, interval: Duration) -> Self {
        self.data_pacer = Pacer::with_interval(interval);
        self
    }

    /// Replace the depth socket budget.
    pub fn with_depth_timeout(mut self, timeout: Duration) -> Self {
        self.depth_timeout = timeout;
        self
    }

    /// Pin "today" (IST) for the history split (tests).
    pub fn with_today(mut self, today: NaiveDate) -> Self {
        self.today = Some(today);
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// Master row for an OpenAlgo symbol, or a trader-facing error.
    pub(crate) fn lookup(&self, key: &QuoteKey) -> Result<SymToken> {
        self.symbols.by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
    }

    pub(crate) fn today_ist(&self) -> NaiveDate {
        self.today.unwrap_or_else(|| {
            chrono::Utc::now()
                .with_timezone(&chrono_tz::Asia::Kolkata)
                .date_naive()
        })
    }

    /// The web's Type B header block.
    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        s: &MstockSession,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base_url, path))
            .header("X-Mirae-Version", "1")
            .header("Authorization", format!("Bearer {}", s.jwt))
            .header("X-PrivateKey", &s.private_key)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
    }

    /// One authenticated call. The body (a one-element list unwrapped, an
    /// empty body read as `{}`) is returned whatever its `status`; HTTP 401
    /// and 403 mean the session is gone.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let s = MstockSession::parse(auth)?;
        let mut req = self.request(method, path, &s);
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await?;
        read_payload(resp).await
    }
}

/// Read an mStock answer: 401/403 -> session expired, empty -> `{}`, a
/// one-element list unwrapped (web `get_api_response` / `place_order_api`).
pub(crate) async fn read_payload(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        tracing::warn!(status = status.as_u16(), "mStock refused the session");
        return Err(session_expired());
    }
    let bytes = resp.bytes().await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        if status.is_server_error() {
            return Err(unavailable());
        }
        return Ok(Value::Object(Default::default()));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => Ok(unwrap_list(v)),
        Err(e) => {
            let prefix: String = String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            tracing::warn!(
                status = status.as_u16(),
                "Unexpected response from mStock ({}): {}",
                e,
                prefix
            );
            if status == StatusCode::TOO_MANY_REQUESTS {
                return Err(AppError::Broker(
                    "mStock is limiting requests right now. Wait a moment and try again.".into(),
                ));
            }
            if status.is_server_error() {
                return Err(unavailable());
            }
            Err(AppError::Broker(
                "mStock sent a response OpenAlgo could not read. Try again shortly.".into(),
            ))
        }
    }
}

fn unavailable() -> AppError {
    AppError::Broker("mStock's servers are not responding normally. Try again shortly.".into())
}

/// `[x]` -> `x` (web: "API returned list, extracting first element").
pub fn unwrap_list(v: Value) -> Value {
    match v {
        Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
        other => other,
    }
}

/// web `is_success_payload`: `status` in `True, "true", "True", "TRUE"`.
pub fn is_success(v: &Value) -> bool {
    match v.get("status") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.as_str(), "true" | "True" | "TRUE"),
        _ => false,
    }
}

/// The payload's `message`, trimmed.
pub fn message(v: &Value) -> String {
    match v.get("message") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// A refusal with mStock's own message, or `fallback`.
pub fn refusal(v: &Value, fallback: &str) -> AppError {
    let msg = message(v);
    let lower = msg.to_ascii_lowercase();
    if lower.contains("invalid token")
        || lower.contains("token expired")
        || lower.contains("session expired")
        || lower.contains("unauthorized")
    {
        return session_expired();
    }
    if msg.is_empty() {
        AppError::Broker(fallback.to_string())
    } else {
        AppError::Broker(format!("mStock: {}", msg))
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth(
        "Your mStock session has expired. Log in to mStock again with your password and TOTP."
            .into(),
    )
}

#[async_trait]
impl Broker for MstockBroker {
    fn id(&self) -> &'static str {
        "mstock"
    }

    fn name(&self) -> &'static str {
        "mStock by Mirae Asset"
    }

    fn logo(&self) -> &'static str {
        "/logos/mstock.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::TwoStep {
            step1: &["password"],
            step2: &["totp"],
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
            // mStock has no order-update stream (web has none).
            order_feed: false,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn requires_totp(&self) -> bool {
        true
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
        let s = MstockSession::parse(auth)?;
        Ok(Box::new(streaming::MstockFeed::new(
            &self.ws_url,
            &s.jwt,
            &s.private_key,
        )))
    }
}
