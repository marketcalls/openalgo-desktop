//! INDmoney adapter on the INDstocks API (web `broker/indmoney/**`).
//!
//! * Host `https://api.indstocks.com`; every call sends the raw access token
//!   as `Authorization: <token>` (no `Bearer`).
//! * The stored API key is the static INDstocks Client ID (`x-api-key` on
//!   `/generate/token`); the optional stored secret is a pasted 24-hour
//!   access token.
//! * Envelope `{"status": "success"|"error"|"failure", "data", "message" |
//!   "error": {"msg"}}`; market endpoints may answer `{"success": false}`.
//! * Pacing per documented category at 80% headroom (orders 8/s, data 4/s,
//!   quotes 4/s, everything else 12/s), 429 retried up to three times with
//!   `Retry-After` or 1/2/4 s, never for order writes (web
//!   `api/rate_limiter.py`).

mod auth;
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
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BASE_URL: &str = "https://api.indstocks.com";
pub const PRICES_WS_URL: &str = "wss://ws-prices.indstocks.com/api/v1/ws/prices";
pub const ORDERS_WS_URL: &str = "wss://ws-order-updates.indstocks.com/api/v1/ws/trades";

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map` (no sub-minute intervals).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1minute"),
    ("2m", "2minute"),
    ("3m", "3minute"),
    ("4m", "4minute"),
    ("5m", "5minute"),
    ("10m", "10minute"),
    ("15m", "15minute"),
    ("30m", "30minute"),
    ("1h", "60minute"),
    ("2h", "120minute"),
    ("3h", "180minute"),
    ("4h", "240minute"),
    ("D", "1day"),
    ("W", "1week"),
    ("M", "1month"),
];

/// Retries of a 429 on reads (web `MAX_RETRIES`).
const MAX_RETRIES: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long an unquotable scrip code is skipped (web `_BAD_SCRIP_TTL`).
pub(crate) const BAD_SCRIP_TTL: Duration = Duration::from_secs(300);
/// Bound on the unquotable-scrip cache.
pub(crate) const BAD_SCRIP_MAX: usize = 2048;
/// Bound on the order-id map shared with the order feed.
pub(crate) const ORDER_ID_MAX: usize = 4096;

/// Documented rate-limit category of a request (web `classify`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Order,
    Data,
    Quote,
    NonTrading,
}

const ORDER_WRITE_PATHS: &[&str] = &[
    "/order",
    "/order/modify",
    "/order/cancel",
    "/smart/order",
    "/smart/order/modify",
    "/smart/order/cancel",
];

pub fn classify(path: &str, method: &Method) -> Bucket {
    let p = path.split('?').next().unwrap_or("");
    let p = p.trim_end_matches('/');
    let p = if p.is_empty() { "/" } else { p };
    if p.starts_with("/market/quotes") {
        Bucket::Quote
    } else if p.starts_with("/market/historical") || p.starts_with("/market/instruments") {
        Bucket::Data
    } else if *method == Method::POST && ORDER_WRITE_PATHS.contains(&p) {
        Bucket::Order
    } else if p.starts_with("/margin") {
        Bucket::Data
    } else {
        Bucket::NonTrading
    }
}

/// Wait before retrying a 429 (web `retry_delay`).
pub fn retry_delay(retry_after: Option<&str>, attempt: u32) -> Duration {
    if let Some(v) = retry_after.and_then(|s| s.trim().parse::<f64>().ok()) {
        if v.is_finite() {
            return Duration::from_secs_f64(v.clamp(0.05, MAX_BACKOFF.as_secs_f64()));
        }
    }
    Duration::from_secs(1u64 << attempt.min(5)).min(MAX_BACKOFF)
}

/// One HTTP answer: status, the body as JSON (`Null` when it is not JSON)
/// and a short prefix of the raw text for error classification.
#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub json: Value,
    pub text: String,
}

impl Reply {
    pub fn ok(&self) -> bool {
        self.status == 200 || self.status == 201
    }
}

/// Bare numeric order id -> canonical `EQ-`/`DRV-`/`GTT-` id, filled from
/// placements and order books, read by the order-update feed (web
/// `_canonical_order_id`). Bounded: the oldest entries go first.
#[derive(Debug, Default)]
pub struct OrderIdMap {
    map: HashMap<String, String>,
    order: std::collections::VecDeque<String>,
}

impl OrderIdMap {
    pub fn remember(&mut self, canonical: &str) {
        let Some((_, suffix)) = canonical.split_once('-') else {
            return;
        };
        if suffix.is_empty() {
            return;
        }
        if self
            .map
            .insert(suffix.to_string(), canonical.to_string())
            .is_none()
        {
            self.order.push_back(suffix.to_string());
            while self.order.len() > ORDER_ID_MAX {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    /// The canonical id for a stream id (unchanged when already prefixed or
    /// unknown).
    pub fn canonical(&self, raw: &str) -> String {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains('-') {
            return raw.to_string();
        }
        self.map
            .get(raw)
            .cloned()
            .unwrap_or_else(|| raw.to_string())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Hosts the adapter talks to (tests point them at local fakes).
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub prices_ws: String,
    pub orders_ws: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: BASE_URL.to_string(),
            prices_ws: PRICES_WS_URL.to_string(),
            orders_ws: ORDERS_WS_URL.to_string(),
        }
    }
}

pub struct IndmoneyBroker {
    http: reqwest::Client,
    urls: Endpoints,
    symbols: SymbolResolver,
    order_pacer: Pacer,
    data_pacer: Pacer,
    quote_pacer: Pacer,
    other_pacer: Pacer,
    /// Scrip codes that poison a `/market/quotes/full` batch, with the time
    /// they were found (bounded, expiring).
    bad_scrips: Mutex<HashMap<String, Instant>>,
    order_ids: Arc<Mutex<OrderIdMap>>,
}

impl IndmoneyBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_endpoints(symbols, Endpoints::default())
    }

    /// Point the adapter at other hosts (tests run local fakes).
    pub fn with_endpoints(symbols: SymbolResolver, urls: Endpoints) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            order_pacer: Pacer::per_second(8.0),
            data_pacer: Pacer::per_second(4.0),
            quote_pacer: Pacer::per_second(4.0),
            other_pacer: Pacer::per_second(12.0),
            bad_scrips: Mutex::new(HashMap::new()),
            order_ids: Arc::new(Mutex::new(OrderIdMap::default())),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.urls.api, path)
    }

    pub(crate) fn remember_order_ids<'a>(&self, ids: impl IntoIterator<Item = &'a str>) {
        let mut m = self.order_ids.lock();
        for id in ids {
            m.remember(id);
        }
    }

    pub(crate) fn order_id_map(&self) -> Arc<Mutex<OrderIdMap>> {
        self.order_ids.clone()
    }

    async fn pace(&self, bucket: Bucket) {
        match bucket {
            Bucket::Order => self.order_pacer.acquire().await,
            Bucket::Data => self.data_pacer.acquire().await,
            Bucket::Quote => self.quote_pacer.acquire().await,
            Bucket::NonTrading => self.other_pacer.acquire().await,
        }
    }

    /// One paced INDstocks call (web `rate_limited_request`). Non-2xx
    /// answers come back as a `Reply`; only transport failures are errors.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        token: &str,
    ) -> Result<Reply> {
        let bucket = classify(path, &method);
        let retries = if bucket == Bucket::Order {
            0
        } else {
            MAX_RETRIES
        };
        let url = self.url(path);
        let mut attempt = 0;
        loop {
            self.pace(bucket).await;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", token)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            if !query.is_empty() {
                req = req.query(query);
            }
            if let Some(b) = body {
                req = req.body(b.to_string());
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            if status == 429 && attempt < retries {
                let delay = retry_delay(
                    resp.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()),
                    attempt,
                );
                tracing::warn!(
                    "INDmoney rate limit on {} {}; retry {}/{} in {:.2}s",
                    method,
                    path,
                    attempt + 1,
                    retries,
                    delay.as_secs_f64()
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            if status == 429 && bucket == Bucket::Order {
                tracing::error!(
                    "INDmoney rate limit on {} {}: an order write is never retried",
                    method,
                    path
                );
            }
            let bytes = resp.bytes().await?;
            let json = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
            let text: String = String::from_utf8_lossy(&bytes[..bytes.len().min(2000)]).into();
            return Ok(Reply { status, json, text });
        }
    }

    /// Account / order-book call with the web `order_api.get_api_response`
    /// envelope rules: success unwraps `data`, failures become errors.
    pub(crate) async fn account_call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        auth: &AuthToken,
    ) -> Result<Value> {
        let r = self.send(method, path, query, body, token(auth)?).await?;
        unwrap_account(path, &r)
    }
}

/// web `order_api.get_api_response` result handling.
pub(crate) fn unwrap_account(path: &str, r: &Reply) -> Result<Value> {
    if r.status == 401 || r.status == 403 {
        tracing::warn!(status = r.status, "INDmoney refused {}", path);
        return Err(session_expired());
    }
    if r.status == 429 {
        return Err(AppError::Broker(
            "INDmoney is limiting requests right now. Wait a moment and try again.".into(),
        ));
    }
    if !r.ok() {
        tracing::warn!(status = r.status, "INDmoney error on {}", path);
        return Err(AppError::Broker(
            error_message(&r.json).unwrap_or_else(|| "INDmoney refused the request.".into()),
        ));
    }
    if r.json.is_null() {
        return Err(AppError::Broker(
            "INDmoney sent a response OpenAlgo could not read. Try again shortly.".into(),
        ));
    }
    if r.json.get("success") == Some(&Value::Bool(false)) {
        return Err(AppError::Broker(
            error_message(&r.json).unwrap_or_else(|| "INDmoney refused the request.".into()),
        ));
    }
    match r.json.get("status").and_then(Value::as_str) {
        Some("error") | Some("failure") => Err(AppError::Broker(
            error_message(&r.json).unwrap_or_else(|| "INDmoney refused the request.".into()),
        )),
        Some("success") if r.json.get("data").is_some() => Ok(r.json["data"].clone()),
        _ => Ok(r.json.clone()),
    }
}

/// The broker's message: `error.msg` on a failure envelope, else `message`
/// or a string `error`.
pub(crate) fn error_message(v: &Value) -> Option<String> {
    let s = |x: &Value| {
        x.as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    if v.get("status").and_then(Value::as_str) == Some("failure") {
        if let Some(m) = v.get("error").and_then(|e| e.get("msg")).and_then(s) {
            return Some(m);
        }
    }
    v.get("message")
        .and_then(s)
        .or_else(|| v.get("error").and_then(s))
        .or_else(|| v.get("error").and_then(|e| e.get("msg")).and_then(s))
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your INDmoney session has expired. Log in to INDmoney again.".into())
}

/// The stored access token.
pub(crate) fn token(auth: &AuthToken) -> Result<&str> {
    let t = auth.raw().trim();
    if t.is_empty() {
        return Err(session_expired());
    }
    Ok(t)
}

#[async_trait]
impl Broker for IndmoneyBroker {
    fn id(&self) -> &'static str {
        "indmoney"
    }

    fn name(&self) -> &'static str {
        "INDmoney"
    }

    fn logo(&self) -> &'static str {
        "/logos/indmoney.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["mpin", "totp"],
        }
    }

    fn requires_totp(&self) -> bool {
        // A pasted access token signs in without one.
        false
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
            // Offered by `IndmoneyBroker::create_order_feed`; the trait has no
            // order-feed factory yet.
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

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::IndmoneyFeed::new(
            &self.urls.prices_ws,
            token(auth)?,
        )))
    }
}

impl IndmoneyBroker {
    /// The order-update stream (web `indmoney_order_adapter.py`) as a feed
    /// for a second `WebSocketManager`. The shared `Broker` trait has no
    /// order-feed factory yet, so it is offered here (as Upstox and Angel
    /// do). Bare numeric stream ids are mapped to the canonical `EQ-`/`DRV-`
    /// ids this adapter has seen in placements and order books.
    pub fn create_order_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::IndmoneyOrderFeed::new(
            &self.urls.orders_ws,
            token(auth)?,
            self.order_id_map(),
        )))
    }
}
