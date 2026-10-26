//! Fyers API v3 adapter (web `broker/fyers/**`).
//!
//! The session token is `app_id:access_token`, sent verbatim as
//! `Authorization` on every REST call (web `f"{BROKER_API_KEY}:{AUTH_TOKEN}"`).
//! Every call passes its API category's budget for the trader's plan
//! (`ratelimit`: per second, per minute and per day), waiting at most until
//! its deadline; a throttle answer pauses the category for the wait Fyers
//! names and is retried within the same deadline, or refused unretried.
//!
//! Streaming: the HSM market-data socket (`streaming::HsmFeed`, no
//! `Authorization` header, `hsm_key` from the JWT), the 50-level TBT depth
//! socket (`streaming::TbtFeed`, protobuf) and the order-update socket
//! (`streaming::OrderFeed`).

mod auth;
pub mod chain;
mod data;
mod funds;
mod gtt;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod ratelimit;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::DailyCount;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use ratelimit::{Category, FyersLimiter, FyersPlan, Op};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::{Duration, SystemTime};

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
    /// Request budgets per API category for the trader's plan.
    limiter: FyersLimiter,
    /// Base of the throttle and history-chunk retry backoff (web 1 s / 2 s).
    retry_base: Duration,
    funds_cache: funds::FundsCache,
    /// The native option chain's `timestamp` per underlying and expiry.
    expiry_timestamps: chain::ExpiryTimestamps,
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
            limiter: FyersLimiter::new(FyersPlan::Standard),
            retry_base: Duration::from_secs(1),
            funds_cache: funds::FundsCache::default(),
            expiry_timestamps: chain::ExpiryTimestamps::default(),
        }
    }

    /// Shorter retry backoff (tests).
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    /// Other request budgets (tests).
    pub fn with_limiter(mut self, limiter: FyersLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    pub fn limiter(&self) -> &FyersLimiter {
        &self.limiter
    }

    pub(crate) fn expiry_timestamps(&self) -> &chain::ExpiryTimestamps {
        &self.expiry_timestamps
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

    /// One Fyers call as its own logical request ([`Op::single`]).
    pub(crate) async fn raw(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        self.raw_in(&mut Op::single(), method, path_and_query, auth, body)
            .await
    }

    /// One budgeted Fyers send within the logical request `op`. Returns
    /// the HTTP status and the JSON body whatever `s` says.
    ///
    /// The send waits for its category's budget until `op`'s deadline, or
    /// is refused before sending. A throttle answer (HTTP 429, or a body
    /// with code 429) pauses the category for the wait Fyers names, in
    /// full; the send is repeated after it when the pause ends before the
    /// deadline and `op` has retries left, refused unretried when the pause
    /// would outlast the deadline, and handed back as answered when the
    /// retries are spent. Only a throttle answer is repeated: it means the
    /// request was not taken. A send that fails or times out is never
    /// repeated here.
    pub(crate) async fn raw_in(
        &self,
        op: &mut Op,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let header = Self::auth_header(auth)?;
        let url = format!("{}{}", self.urls.api, path_and_query);
        let category = Category::of(&method, path_and_query);
        let endpoint = path_and_query.split('?').next().unwrap_or("");
        let mut attempt = 0u32;
        loop {
            self.limiter.acquire(category, op).await?;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", header)
                .header("Content-Type", "application/json");
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let headers = resp.headers().clone();
            let (status, v) = if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                // A throttle notice need not be JSON; its status says it.
                let bytes = resp.bytes().await.unwrap_or_default();
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                )
            } else {
                http::read_json("fyers", resp).await?
            };
            if status != StatusCode::TOO_MANY_REQUESTS && !is_throttle_body(&v) {
                return Ok((status, v));
            }
            let delay =
                ratelimit::retry_delay(&headers, attempt, self.retry_base, SystemTime::now());
            let until = tokio::time::Instant::now().checked_add(delay);
            if let Some(u) = until {
                self.limiter.cool_down(category, u);
            }
            if op.retries_left == 0 {
                tracing::warn!("Fyers is still limiting {} after the retries", endpoint);
                return Ok((status, v));
            }
            if until.is_none_or(|u| u > op.deadline) {
                tracing::warn!(
                    "Fyers asked to wait {:?} before retrying {}; past this request's deadline, not retried",
                    delay,
                    endpoint
                );
                // The web raises `BrokerBusyError` here: 429.
                return Err(crate::brokers::common::outcome::busy(
                    ratelimit::SLOW_DOWN_MESSAGE,
                ));
            }
            op.retries_left -= 1;
            attempt += 1;
            tracing::warn!(
                "Fyers rate limited {}; retrying in {:?} ({} retries left)",
                endpoint,
                delay,
                op.retries_left
            );
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
        self.call_in(&mut Op::single(), method, path_and_query, auth, body)
            .await
    }

    /// [`call`](Self::call) within the logical request `op`.
    pub(crate) async fn call_in(
        &self,
        op: &mut Op,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, v) = self.raw_in(op, method, path_and_query, auth, body).await?;
        if status.is_server_error() {
            return Err(http::server_error("fyers", status));
        }
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
        // Fyers' explicit failure (`s: error`, a negative code) is a
        // definite refusal (LOG-08); a 2xx without one and a 5xx leave the
        // order unknown.
        let failed = v
            .get("s")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("error"))
            || code < 0;
        Err(crate::brokers::common::outcome::not_accepted_err(
            status.as_u16(),
            failed,
            fyers_error(status.as_u16(), code, &message),
        ))
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

/// A body that says Fyers is throttling (code 429 without success).
pub(crate) fn is_throttle_body(v: &Value) -> bool {
    !is_ok(v) && code_message(v).0 == 429
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

    /// The funds cached under the session's token go with the session
    /// (KEY-01: every cache is cleared at logout).
    async fn on_logout(&self) {
        self.funds_cache.clear();
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

    fn rate_plans(&self) -> &'static [crate::brokers::RatePlan] {
        ratelimit::PLANS
    }

    fn set_rate_plan(&self, plan: &str) {
        self.limiter.set_plan(FyersPlan::parse(plan));
    }

    fn rate_plan(&self) -> Option<&'static str> {
        Some(self.limiter.plan().id())
    }

    fn daily_request_counts(&self) -> Vec<DailyCount> {
        self.limiter.daily_counts()
    }

    fn restore_daily_request_counts(&self, counts: &[DailyCount]) {
        self.limiter.restore_daily_counts(counts);
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

    async fn get_chain_quotes(
        &self,
        auth: &AuthToken,
        req: &ChainQuoteRequest,
    ) -> Result<ChainQuotes> {
        chain::get_chain_quotes(self, auth, req).await
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
}
