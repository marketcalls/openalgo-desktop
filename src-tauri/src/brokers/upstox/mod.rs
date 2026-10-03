//! Upstox API adapter (web `broker/upstox/**`).
//!
//! * Session token: the raw Upstox `access_token`, sent as
//!   `Authorization: Bearer <token>`.
//! * Hosts: order place / modify / cancel use the v3 API on the
//!   low-latency `api-hft.upstox.com`; books are v2, quotes, history, funds
//!   and GTT are v3 on `api.upstox.com`.
//! * `SymToken.token` holds the Upstox instrument key (`NSE_EQ|INE...`),
//!   `brexchange` the Upstox segment.
//! * Pacing: the web's two rolling-window budgets (order, standard); read
//!   endpoints retry a rate-limit refusal up to three times, mutations never.

mod auth;
mod data;
mod funds;
mod gtt;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod pacing;
pub mod proto;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use pacing::WindowLimiter;
use reqwest::{Method, StatusCode};
use serde_json::Value;

pub const API_URL: &str = "https://api.upstox.com";
pub const HFT_URL: &str = "https://api-hft.upstox.com";
pub const MASTER_URL: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/complete.json.gz";

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
    Exchange::GlobalIndex,
];

/// web `BrokerData.timeframe_map` (unit, interval) flattened to
/// `unit/interval`; the order is the web's.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "minutes/1"),
    ("2m", "minutes/2"),
    ("3m", "minutes/3"),
    ("5m", "minutes/5"),
    ("10m", "minutes/10"),
    ("15m", "minutes/15"),
    ("30m", "minutes/30"),
    ("60m", "minutes/60"),
    ("1h", "hours/1"),
    ("2h", "hours/2"),
    ("3h", "hours/3"),
    ("4h", "hours/4"),
    ("D", "days/1"),
    ("W", "weeks/1"),
    ("M", "months/1"),
];

/// Where the adapter sends requests (tests point every host at a local
/// fake Upstox).
#[derive(Debug, Clone)]
pub struct Urls {
    /// `https://api.upstox.com`: books, quotes, history, funds, GTT, feeds.
    pub api: String,
    /// `https://api-hft.upstox.com`: v3 place / modify / cancel.
    pub hft: String,
    /// The gzip JSON instrument master.
    pub master: String,
}

impl Urls {
    pub fn production() -> Self {
        Self {
            api: API_URL.into(),
            hft: HFT_URL.into(),
            master: MASTER_URL.into(),
        }
    }

    /// Every host on one base URL (tests); the master is `/master.json.gz`.
    pub fn local(base: &str) -> Self {
        Self {
            api: base.into(),
            hft: base.into(),
            master: format!("{}/master.json.gz", base),
        }
    }
}

/// Rate-limit category of a call (web `_rate_limit_category`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    /// Place / modify / cancel / GTT (8/s, 475/min, 1900/30min). Never
    /// retried on a rate-limit refusal.
    Order,
    /// Everything else (45/s, 475/min, 1900/30min); retried up to three
    /// times.
    Standard,
}

pub struct UpstoxBroker {
    http: reqwest::Client,
    urls: Urls,
    symbols: SymbolResolver,
    order_limiter: WindowLimiter,
    standard_limiter: WindowLimiter,
}

impl UpstoxBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, Urls::production())
    }

    /// Point the adapter at other hosts (tests run a local fake Upstox).
    pub fn with_urls(symbols: SymbolResolver, urls: Urls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            order_limiter: WindowLimiter::order(),
            standard_limiter: WindowLimiter::standard(),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// Bearer header for a stored token. Upstox tokens are long JWTs; the
    /// web's feed treats anything under 10 characters as no token at all.
    fn bearer(auth: &AuthToken) -> Result<String> {
        let t = auth.raw().trim();
        if t.len() < 10 {
            return Err(session_expired());
        }
        Ok(format!("Bearer {}", t))
    }

    /// One Upstox call. Returns the HTTP status and the JSON body as sent;
    /// a standard-category rate-limit refusal (HTTP 429 or `UDAPI10005`) is
    /// retried with the web's backoff.
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
        extra_headers: &[(&'static str, &'static str)],
    ) -> Result<(StatusCode, Value)> {
        let bearer = Self::bearer(auth)?;
        let mut attempt = 0;
        loop {
            match category {
                Category::Order => self.order_limiter.acquire().await,
                Category::Standard => self.standard_limiter.acquire().await,
            }
            let mut req = self
                .http
                .request(method.clone(), url)
                .header("Authorization", &bearer)
                .header("Accept", "application/json");
            for (k, v) in extra_headers {
                req = req.header(*k, *v);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let status = resp.status();
            let bytes = resp.bytes().await?;
            let parsed: Option<Value> = serde_json::from_slice(&bytes).ok();
            let limited = status == StatusCode::TOO_MANY_REQUESTS
                || parsed
                    .as_ref()
                    .and_then(mapping::error_code)
                    .is_some_and(|c| c == pacing::RATE_LIMIT_CODE);
            if limited && category == Category::Standard && attempt < pacing::MAX_RETRIES {
                let delay = pacing::retry_delay(retry_after.as_deref(), attempt);
                tracing::warn!(
                    "Upstox rate limit on {}; retrying in {:?} (attempt {}/{})",
                    path_of(url),
                    delay,
                    attempt + 1,
                    pacing::MAX_RETRIES
                );
                attempt += 1;
                tokio::time::sleep(delay).await;
                continue;
            }
            return match parsed {
                Some(v) => Ok((status, v)),
                None => {
                    tracing::warn!(
                        status = status.as_u16(),
                        "Upstox sent a non-JSON response for {}",
                        path_of(url)
                    );
                    Err(non_json_error(status))
                }
            };
        }
    }

    /// `send` and unwrap the success envelope's `data`.
    pub(crate) async fn call(
        &self,
        method: Method,
        url: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
    ) -> Result<Value> {
        let (status, v) = self.send(method, url, auth, body, category, &[]).await?;
        envelope_data(status, v, url)
    }

    pub(crate) fn api(&self, path: &str) -> String {
        format!("{}{}", self.urls.api, path)
    }

    pub(crate) fn hft(&self, path: &str) -> String {
        format!("{}{}", self.urls.hft, path)
    }
}

/// Path of a URL for logs (never the query: it may carry ids).
fn path_of(url: &str) -> &str {
    let no_query = url.split('?').next().unwrap_or(url);
    match no_query.find("://") {
        Some(i) => match no_query[i + 3..].find('/') {
            Some(j) => &no_query[i + 3 + j..],
            None => "/",
        },
        None => no_query,
    }
}

fn non_json_error(status: StatusCode) -> AppError {
    if status == StatusCode::UNAUTHORIZED {
        return session_expired();
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return AppError::Broker(
            "Upstox is limiting requests right now. Wait a moment and try again.".into(),
        );
    }
    if status.is_server_error() {
        return AppError::Broker(
            "Upstox's servers are not responding normally. Try again shortly.".into(),
        );
    }
    AppError::Broker("Upstox sent a response OpenAlgo could not read. Try again shortly.".into())
}

/// `data` of a success envelope, or the trader-facing error.
pub(crate) fn envelope_data(status: StatusCode, v: Value, url: &str) -> Result<Value> {
    if v.get("status").and_then(Value::as_str) == Some("success") {
        return Ok(v.get("data").cloned().unwrap_or(Value::Null));
    }
    let err = upstox_error(status, &v);
    tracing::warn!(
        status = status.as_u16(),
        code = mapping::error_code(&v).unwrap_or_default(),
        "Upstox refused {}: {}",
        path_of(url),
        err.client_message()
    );
    Err(err)
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Upstox session has expired. Log in to Upstox again.".into())
}

/// Upstox error body -> trader-facing error.
pub(crate) fn upstox_error(status: StatusCode, body: &Value) -> AppError {
    let code = mapping::error_code(body).unwrap_or_default();
    if status == StatusCode::UNAUTHORIZED || code == "UDAPI100050" {
        return session_expired();
    }
    if status == StatusCode::TOO_MANY_REQUESTS || code == pacing::RATE_LIMIT_CODE {
        return AppError::Broker(
            "Upstox is limiting requests right now. Wait a moment and try again.".into(),
        );
    }
    match mapping::error_text(body).or_else(|| {
        body.get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .map(str::to_string)
    }) {
        Some(m) => AppError::Broker(m),
        None => AppError::Broker("Upstox refused the request.".into()),
    }
}

#[async_trait]
impl Broker for UpstoxBroker {
    fn id(&self) -> &'static str {
        "upstox"
    }

    fn name(&self) -> &'static str {
        "Upstox"
    }

    fn logo(&self) -> &'static str {
        "/logos/upstox.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "code" }
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

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Self::bearer(auth)?;
        Ok(Box::new(streaming::UpstoxFeed::new(
            streaming::Authorizer::market(self.http.clone(), &self.urls.api, auth.raw()),
            self.symbols.clone(),
        )))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }
}

impl UpstoxBroker {
    /// The portfolio order-update stream (web `upstox_order_adapter.py`),
    /// served through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Self::bearer(auth)?;
        Ok(Box::new(streaming::UpstoxOrderFeed::new(
            streaming::Authorizer::orders(self.http.clone(), &self.urls.api, auth.raw()),
            &self.urls.api,
            self.symbols.clone(),
        )))
    }
}
