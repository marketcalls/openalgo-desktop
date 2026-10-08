//! Kotak Neo adapter (web `broker/kotak/**`).
//!
//! Credentials, as on the web:
//! * API key: the UCC (Kotak unique client code), used verbatim.
//! * API secret: the Neo API access token from the Kotak developer portal,
//!   sent verbatim (no `Bearer`) as `Authorization` on login, market data
//!   and scrip-master calls.
//!
//! Sign-in is two steps (`LoginKind::TwoStep`): mobile + TOTP
//! (`tradeApiLogin`), then MPIN (`tradeApiValidate`). The stored session is
//! the web's composite `token:::sid:::baseUrl:::accessToken:::dataCenter`;
//! readers take the first four parts positionally and treat a missing fifth
//! as unknown, so four-part tokens keep working.
//!
//! Trading calls go to the dynamic `baseUrl` with `Auth`, `Sid` and
//! `neo-fin-key: neotradeapi`; writes are `application/x-www-form-urlencoded`
//! bodies of `jData=<percent-encoded JSON>`, every value a string.

pub mod auth;
mod data;
mod funds;
pub mod hsm;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

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
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub use data::{
    depth_from_quote, history_chunk_days, history_timestamp, index_candidates, kotak_segment,
    match_multiquotes, normalize_candles, quote_from_row, repair_candles,
};
pub use funds::{funds_from_limits, margin_body, parse_margin};

/// Login host (not the dynamic `baseUrl`).
pub const LOGIN_BASE_URL: &str = "https://mis.kotaksecurities.com";
/// Feed host lookup per data centre.
pub const FEED_CONFIG_URL: &str =
    "https://lapi.kotaksecurities.com/5config/config?appVersion=1.0.0&platform=api&environment=prod";
/// Scrip-master hosts tried after the session `baseUrl`.
pub const SCRIP_MASTER_FALLBACK_BASES: &[&str] = &[
    "https://cis.kotaksecurities.com",
    "https://neo-gw.kotaksecurities.com",
];
/// Dated scrip-master CDN used when every file-paths call fails.
pub const SCRIP_MASTER_CDN: &str = "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod";

/// web `plugin.json` supported_exchanges (BCD is mapped but not offered).
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

/// web `BrokerData.timeframe_map` (`60m` is an accepted alias of `1h`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1min"),
    ("3m", "3min"),
    ("5m", "5min"),
    ("10m", "10min"),
    ("15m", "15min"),
    ("30m", "30min"),
    ("1h", "60min"),
    ("60m", "60min"),
    ("D", "D"),
    ("W", "W"),
];

/// The parsed session token.
#[derive(Clone)]
pub struct KotakSession {
    pub token: String,
    pub sid: String,
    pub base_url: String,
    pub access_token: String,
    pub data_center: String,
}

impl std::fmt::Debug for KotakSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KotakSession")
            .field("base_url", &self.base_url)
            .field("data_center", &self.data_center)
            .finish_non_exhaustive()
    }
}

impl KotakSession {
    /// `token:::sid:::baseUrl:::accessToken[:::dataCenter]`.
    pub fn parse(auth: &AuthToken) -> Result<Self> {
        let parts: Vec<&str> = auth.raw().split(":::").collect();
        if parts.len() < 4 {
            return Err(session_expired());
        }
        let base_url = parts[2].trim().trim_end_matches('/').to_string();
        if !base_url.starts_with("http") {
            // web: "Kotak auth token missing baseUrl. Please re-login".
            return Err(session_expired());
        }
        if parts[0].trim().is_empty() || parts[1].trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Self {
            token: parts[0].trim().to_string(),
            sid: parts[1].trim().to_string(),
            base_url,
            access_token: parts[3].trim().to_string(),
            data_center: parts
                .get(4)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        })
    }

    pub fn compose(&self) -> String {
        format!(
            "{}:::{}:::{}:::{}:::{}",
            self.token, self.sid, self.base_url, self.access_token, self.data_center
        )
    }
}

pub struct KotakBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) login_base_url: String,
    pub(crate) feed_config_url: String,
    /// Scrip-master hosts tried after the session's own `baseUrl`.
    pub(crate) scrip_fallback_bases: Vec<String>,
    pub(crate) scrip_cdn: String,
    pub(crate) symbols: SymbolResolver,
    /// Neo rejects quote calls on concurrency, not rate: at most four in
    /// flight (web `QUOTES_MAX_INFLIGHT`).
    pub(crate) quotes_gate: Arc<Semaphore>,
    /// History is paced at one request per second (web).
    pub(crate) history_pacer: Pacer,
    /// Base of the retry back-off ladders (quotes 0.5 s, history 1 s on the
    /// web); tests shrink it.
    pub(crate) retry_base: Duration,
    /// The market-data feed URL resolved for the last login's data centre:
    /// one entry, replaced on every login.
    pub(crate) feed_url: Arc<Mutex<Option<(String, String)>>>,
    /// The UCC of the last login, which SFeed wants as its `user` (the web
    /// reads `BROKER_API_KEY`; without it the web sends `neome`).
    pub(crate) ucc: Mutex<Option<String>>,
}

impl KotakBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, LOGIN_BASE_URL, FEED_CONFIG_URL)
    }

    /// Point the login host and the feed-config service at a local fake (the
    /// trading host comes from the session's `baseUrl`).
    pub fn with_urls(
        symbols: SymbolResolver,
        login_base_url: impl Into<String>,
        feed_config_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            login_base_url: login_base_url.into(),
            feed_config_url: feed_config_url.into(),
            scrip_fallback_bases: SCRIP_MASTER_FALLBACK_BASES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            scrip_cdn: SCRIP_MASTER_CDN.to_string(),
            symbols,
            quotes_gate: Arc::new(Semaphore::new(4)),
            history_pacer: Pacer::per_second(1.0),
            retry_base: Duration::from_secs(1),
            feed_url: Arc::new(Mutex::new(None)),
            ucc: Mutex::new(None),
        }
    }

    /// Replace the scrip-master fallbacks (tests).
    pub fn with_scrip_master_fallbacks(mut self, bases: Vec<String>, cdn: String) -> Self {
        self.scrip_fallback_bases = bases;
        self.scrip_cdn = cdn;
        self
    }

    /// Shrink the retry back-off (tests).
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    /// Replace the history pacing (Neo's one request a second by default).
    pub fn with_history_pacing(mut self, interval: Duration) -> Self {
        self.history_pacer = Pacer::with_interval(interval);
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    fn trading_request(
        &self,
        method: Method,
        s: &KotakSession,
        path: &str,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", s.base_url, path))
            .header("accept", "application/json")
            .header("Sid", &s.sid)
            .header("Auth", &s.token)
            .header("neo-fin-key", "neotradeapi")
    }

    /// A trading GET (books).
    pub(crate) async fn trading_get(
        &self,
        s: &KotakSession,
        path: &str,
    ) -> Result<(StatusCode, Value)> {
        let resp = self.trading_request(Method::GET, s, path).send().await?;
        http::read_json("kotak", resp).await
    }

    /// A trading POST with a `jData` form body.
    pub(crate) async fn trading_post(
        &self,
        s: &KotakSession,
        path: &str,
        jdata: &Value,
    ) -> Result<(StatusCode, Value)> {
        let resp = self
            .trading_request(Method::POST, s, path)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(jdata_body(jdata))
            .send()
            .await?;
        http::read_json("kotak", resp).await
    }
}

/// Python `urllib.parse.quote(s, safe=safe)`: unreserved characters and
/// `safe` stay, everything else is percent-encoded byte by byte.
pub fn py_quote(s: &str, safe: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "_.-~".contains(c) || safe.contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// `jData=` + `quote(json.dumps(obj))` (web `order_api`).
pub fn jdata_body(v: &Value) -> String {
    format!("jData={}", py_quote(&v.to_string(), "/"))
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth(
        "Your Kotak session has expired. Log in to Kotak again with TOTP and MPIN.".into(),
    )
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Kotak `stat` is `Ok` (case-insensitive on positions).
pub fn stat_ok(v: &Value) -> bool {
    text(v.get("stat")).eq_ignore_ascii_case("ok")
}

/// The error a `Not_Ok` / fault body stands for.
pub fn kotak_error(status: StatusCode, v: &Value, fallback: &str) -> AppError {
    let msg = [
        text(v.get("emsg")),
        text(v.get("errMsg")),
        text(v.get("message")),
        v.get("fault")
            .map(|f| text(f.get("message")))
            .unwrap_or_default(),
    ]
    .into_iter()
    .find(|m| !m.is_empty())
    .unwrap_or_default();
    let lower = msg.to_ascii_lowercase();
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || lower.contains("invalid session")
        || lower.contains("session expired")
        || lower.contains("invalid token")
        || lower.contains("unauthorized")
        || lower.contains("invalid credentials")
    {
        return session_expired();
    }
    if msg.is_empty() {
        AppError::Broker(fallback.to_string())
    } else {
        AppError::Broker(format!("Kotak: {}", msg))
    }
}

#[async_trait]
impl Broker for KotakBroker {
    fn id(&self) -> &'static str {
        "kotak"
    }

    fn name(&self) -> &'static str {
        "Kotak Neo"
    }

    fn logo(&self) -> &'static str {
        "/logos/kotak.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::TwoStep {
            step1: &["mobile", "totp"],
            step2: &["mpin"],
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

    /// The SFeed client. Its host depends on the session's data centre and
    /// is resolved in `prepare` when this run has not looked it up yet (a
    /// resumed session), so streaming works without a fresh login.
    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = KotakSession::parse(auth)?;
        let url = streaming::cached_feed_url(self, &s.data_center);
        let feed = streaming::KotakFeed::new(&url, &s.sid, self.ucc_hint(), self.symbols.clone());
        let feed = if self.has_feed_url(&s.data_center) {
            feed
        } else {
            feed.with_lookup(
                self.http.clone(),
                self.feed_config_url.clone(),
                s.data_center.clone(),
                self.feed_url.clone(),
            )
        };
        Ok(Box::new(feed))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    /// The UCC (the stored API key) is what the SFeed `user` field carries;
    /// a resumed session has not been through `authenticate` in this run.
    fn restore_session(&self, credentials: &BrokerCredentials) {
        let ucc = credentials.api_key.trim();
        if !ucc.is_empty() {
            *self.ucc.lock() = Some(ucc.to_string());
        }
    }

    async fn on_logout(&self) {
        *self.feed_url.lock() = None;
    }
}

impl KotakBroker {
    /// The UCC the SFeed `user` field carries.
    fn ucc_hint(&self) -> String {
        self.ucc.lock().clone().unwrap_or_default()
    }

    /// The legacy HSM market feed, for a data centre the config service
    /// still routes to source `hs` (none does today).
    pub fn create_hsm_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = KotakSession::parse(auth)?;
        Ok(Box::new(hsm::KotakHsmFeed::new(
            hsm::DEFAULT_HSM_URL,
            &s.token,
            &s.sid,
        )))
    }

    fn has_feed_url(&self, data_center: &str) -> bool {
        self.feed_url
            .lock()
            .as_ref()
            .is_some_and(|(dc, _)| dc == data_center)
    }

    /// The order-update socket (`wss://<baseUrl host>/realtime`), served
    /// through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = KotakSession::parse(auth)?;
        Ok(Box::new(streaming::KotakOrderFeed::new(
            &s,
            self.symbols.clone(),
        )))
    }
}
