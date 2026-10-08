//! Fyers API v3 adapter (web `broker/fyers/**`).
//!
//! The session token is `app_id:access_token`, sent verbatim as
//! `Authorization` on every REST call (web `f"{BROKER_API_KEY}:{AUTH_TOKEN}"`).
//! Fyers caps every endpoint together at 10 requests per second per app, so
//! one pacer spaces all calls 125 ms apart (web `api/rate_limiter.py`) and a
//! 429 is retried up to three times, honouring `X-Retry-After-Ms` /
//! `Retry-After`.
//!
//! Streaming: the HSM market-data socket (`streaming::HsmFeed`, no
//! `Authorization` header, `hsm_key` from the JWT), the 50-level TBT depth
//! socket (`streaming::TbtFeed`, protobuf) and the order-update socket
//! (`streaming::OrderFeed`).

mod auth;
mod data;
mod funds;
mod gtt;
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
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

pub use auth::{app_id_hash, decode_jwt_claims, JwtClaims};

/// REST host (web `https://api-t1.fyers.in`).
pub const API_BASE: &str = "https://api-t1.fyers.in";
/// Master-contract host.
pub const PUBLIC_BASE: &str = "https://public.fyers.in/sym_details";

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

/// web `BrokerData.timeframe_map`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("5s", "5S"),
    ("10s", "10S"),
    ("15s", "15S"),
    ("30s", "30S"),
    ("45s", "45S"),
    ("1m", "1"),
    ("2m", "2"),
    ("3m", "3"),
    ("5m", "5"),
    ("10m", "10"),
    ("15m", "15"),
    ("20m", "20"),
    ("30m", "30"),
    ("1h", "60"),
    ("2h", "120"),
    ("4h", "240"),
    ("D", "1D"),
];

/// web `rate_limiter.MIN_INTERVAL` (8 requests per second, under the cap).
pub const MIN_INTERVAL: Duration = Duration::from_millis(125);
/// web `rate_limiter.MAX_RETRIES` for HTTP 429.
pub const MAX_RETRIES: u32 = 3;
/// Longest broker-requested 429 wait we honour before giving up.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(10);

/// Endpoints, overridable so tests can run against a local fake Fyers.
#[derive(Debug, Clone)]
pub struct FyersUrls {
    pub api: String,
    pub public: String,
    pub hsm: String,
    pub tbt: String,
    pub order_ws: String,
}

impl Default for FyersUrls {
    fn default() -> Self {
        Self {
            api: API_BASE.into(),
            public: PUBLIC_BASE.into(),
            hsm: streaming::HSM_URL.into(),
            tbt: streaming::TBT_URL.into(),
            order_ws: streaming::ORDER_WS_URL.into(),
        }
    }
}

pub struct FyersBroker {
    http: reqwest::Client,
    urls: FyersUrls,
    symbols: SymbolResolver,
    /// One budget for every endpoint (Fyers counts them together).
    pacer: Pacer,
    /// Base of the 429 and history-chunk retry backoff (web 1 s / 2 s).
    retry_base: Duration,
    funds_cache: funds::FundsCache,
}

impl FyersBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, FyersUrls::default())
    }

    /// Point the adapter at other hosts (tests run a local fake Fyers).
    pub fn with_urls(symbols: SymbolResolver, urls: FyersUrls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            pacer: Pacer::with_interval(MIN_INTERVAL),
            retry_base: Duration::from_secs(1),
            funds_cache: funds::FundsCache::default(),
        }
    }

    /// Shorter retry backoff (tests).
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn urls(&self) -> &FyersUrls {
        &self.urls
    }

    /// The master row for an OpenAlgo symbol, or a trader-facing error.
    pub(crate) fn lookup(&self, key: &QuoteKey) -> Result<SymToken> {
        self.symbols.by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
    }

    /// Validate the stored token shape (`app_id:access_token`).
    fn auth_header(auth: &AuthToken) -> Result<&str> {
        match auth.pair() {
            Some(_) => Ok(auth.raw()),
            None => Err(session_expired()),
        }
    }

    /// One paced Fyers call with the web's 429 retry. Returns the HTTP status
    /// and the JSON body whatever `s` says.
    pub(crate) async fn raw(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let header = Self::auth_header(auth)?;
        let url = format!("{}{}", self.urls.api, path_and_query);
        let mut attempt = 0;
        loop {
            self.pacer.acquire().await;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", header)
                .header("Content-Type", "application/json");
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            if resp.status() == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                let delay = retry_delay(resp.headers(), attempt, self.retry_base);
                tracing::warn!(
                    "Fyers rate limited {}; retrying in {:?} (attempt {}/{})",
                    path_and_query.split('?').next().unwrap_or(""),
                    delay,
                    attempt + 1,
                    MAX_RETRIES
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            return http::read_json("fyers", resp).await;
        }
    }

    /// A call whose success is `s == "ok"`; anything else becomes a
    /// trader-facing error.
    pub(crate) async fn call(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, v) = self.raw(method, path_and_query, auth, body).await?;
        if is_ok(&v) {
            return Ok(v);
        }
        let (code, message) = code_message(&v);
        tracing::warn!(
            status = status.as_u16(),
            code,
            "Fyers refused {}: {}",
            path_and_query.split('?').next().unwrap_or(""),
            message
        );
        Err(fyers_error(status.as_u16(), code, &message))
    }
}

/// `s == "ok"` (Fyers also answers `"OK"` on modify).
pub(crate) fn is_ok(v: &Value) -> bool {
    v.get("s")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("ok"))
}

/// `(code, message)` of a Fyers body.
pub(crate) fn code_message(v: &Value) -> (i64, String) {
    let code = match v.get("code") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    };
    let message = v
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    (code, message)
}

/// web `retry_delay_from_headers`: `X-Retry-After-Ms`, then `Retry-After`,
/// then `base * 2^attempt`; capped.
pub(crate) fn retry_delay(
    headers: &reqwest::header::HeaderMap,
    attempt: u32,
    base: Duration,
) -> Duration {
    let read = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
    };
    let d = if let Some(ms) = read("x-retry-after-ms") {
        Duration::from_secs_f64((ms / 1000.0).max(0.05))
    } else if let Some(s) = read("retry-after") {
        Duration::from_secs_f64(s.max(0.05))
    } else {
        base.saturating_mul(1u32 << attempt.min(8))
    };
    d.min(MAX_RETRY_WAIT)
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Fyers session has expired. Log in to Fyers again.".into())
}

/// Fyers error body -> trader-facing error. Codes -8 / -15 / -16 / -17 and
/// HTTP 401 mean the access token is no longer valid.
pub(crate) fn fyers_error(status: u16, code: i64, message: &str) -> AppError {
    if status == 401 || matches!(code, -8 | -15 | -16 | -17) {
        return session_expired();
    }
    if status == 429 || code == 429 {
        return AppError::Broker(
            "Fyers is limiting requests right now. Wait a moment and try again.".into(),
        );
    }
    if message.is_empty() {
        AppError::Broker("Fyers refused the request.".into())
    } else {
        AppError::Broker(message.to_string())
    }
}

#[async_trait]
impl Broker for FyersBroker {
    fn id(&self) -> &'static str {
        "fyers"
    }

    fn name(&self) -> &'static str {
        "Fyers"
    }

    fn logo(&self) -> &'static str {
        "/logos/fyers.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "auth_code" }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: true,
            margin: true,
            gtt: true,
            streaming: true,
            // Order updates on their own socket; 50-level books on the TBT
            // socket for NSE and NFO (`feed_depth_levels`).
            order_feed: true,
            depth_levels: &[5, 50],
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

    async fn place_gtt(&self, auth: &AuthToken, req: &GttRequest) -> Result<GttResponse> {
        gtt::place_gtt(self, auth, req).await
    }

    async fn modify_gtt(
        &self,
        auth: &AuthToken,
        trigger_id: &str,
        req: &GttRequest,
    ) -> Result<GttResponse> {
        gtt::modify_gtt(self, auth, trigger_id, req).await
    }

    async fn cancel_gtt(&self, auth: &AuthToken, trigger_id: &str) -> Result<GttResponse> {
        gtt::cancel_gtt(self, auth, trigger_id).await
    }

    async fn get_gtt_book(&self, auth: &AuthToken, include_history: bool) -> Result<Vec<GttOrder>> {
        gtt::get_gtt_book(self, auth, include_history).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::HsmFeed::new(
            &self.urls.hsm,
            auth,
            self.symbols.clone(),
        )?))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    /// The 50-level TBT socket; its address is looked up (web
    /// `_get_tbt_url`) before every connect.
    fn create_depth_feed(&self, auth: &AuthToken, levels: u8) -> Result<Box<dyn BrokerFeed>> {
        if levels != 50 {
            return Err(AppError::Unsupported("depth_feed"));
        }
        let feed = streaming::TbtFeed::new(&self.urls.tbt, auth, self.symbols.clone())?
            .with_lookup(
                self.http.clone(),
                format!("{}/indus/home/tbtws", self.urls.api),
            );
        Ok(Box::new(feed))
    }

    fn feed_depth_levels(&self, exchange: &str) -> Vec<u8> {
        if streaming::TBT_EXCHANGES.contains(&exchange) {
            vec![5, 50]
        } else {
            vec![5]
        }
    }
}

impl FyersBroker {
    /// The order-update socket (`wss://socket.fyers.in/trade/v3`), served
    /// through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::OrderFeed::new(
            &self.urls.order_ws,
            auth,
            self.symbols.clone(),
        )?))
    }

    /// The 50-level TBT depth socket at a known URL (see `tbt_socket_url`).
    pub fn create_depth50_feed(&self, auth: &AuthToken, url: &str) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::TbtFeed::new(
            url,
            auth,
            self.symbols.clone(),
        )?))
    }

    /// web `_get_tbt_url`: ask Fyers for the TBT socket address, falling back
    /// to the documented default.
    pub async fn tbt_socket_url(&self, auth: &AuthToken) -> String {
        match self.raw(Method::GET, "/indus/home/tbtws", auth, None).await {
            Ok((status, v)) if status.is_success() => v
                .pointer("/data/socket_url")
                .and_then(Value::as_str)
                .filter(|s| s.starts_with("wss://") || s.starts_with("ws://"))
                .map(str::to_string)
                .unwrap_or_else(|| self.urls.tbt.clone()),
            Ok(_) => self.urls.tbt.clone(),
            Err(e) => {
                tracing::warn!("Fyers TBT address lookup failed: {}", e.code());
                self.urls.tbt.clone()
            }
        }
    }
}
