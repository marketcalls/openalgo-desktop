//! Delta Exchange India adapter (web `broker/deltaexchange/**`), the only
//! crypto venue: `broker_type: crypto`, exchange `CRYPTO`, leverage set per
//! instrument before each order (`leverage_config: true`).
//!
//! * REST base `https://api.india.delta.exchange`. Private calls are signed
//!   per request with HMAC-SHA256 (`auth.rs`); the stored session is
//!   `api_key:api_secret`. Market data and the product list are public.
//! * A weighted quota (10000 units per 5 minutes, two buckets) paces every
//!   call (`ratelimit.rs`); 429s are retried on the reset Delta names.
//! * Every book returns OpenAlgo crypto symbols (`BTCUSDFUT`,
//!   `BTC27NOV2662000CE`, `BTCINR`) resolved through the product master.
//! * Order sizes are contracts for derivatives (whole) and units for spot
//!   (fractional allowed), carried exactly through `place_order_exact`.

pub mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod ratelimit;
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
use ratelimit::{Bucket, Quota};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

pub const BASE_URL: &str = "https://api.india.delta.exchange";
/// Public market-data socket (no auth; `ticker`, `ob_l2`).
pub const WS_PUBLIC_URL: &str = "wss://public-socket.india.delta.exchange";
/// `brexchange` of every master row (web `master_contract_db.py`).
pub const BREXCHANGE: &str = "DELTAIN";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[Exchange::Crypto];

/// web `BrokerData.TIMEFRAME_MAP`, in its order (aliases `D`, `W`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1m"),
    ("3m", "3m"),
    ("5m", "5m"),
    ("15m", "15m"),
    ("30m", "30m"),
    ("1h", "1h"),
    ("2h", "2h"),
    ("4h", "4h"),
    ("6h", "6h"),
    ("1d", "1d"),
    ("D", "1d"),
    ("1w", "1w"),
    ("W", "1w"),
];

/// Wall clock in epoch seconds (signing timestamps, "today", history caps).
pub type Clock = fn() -> i64;

fn system_clock() -> i64 {
    chrono::Utc::now().timestamp()
}

pub struct DeltaBroker {
    http: reqwest::Client,
    base_url: String,
    ws_url: String,
    symbols: SymbolResolver,
    quota: Quota,
    clock: Clock,
    /// Pause between transient-error retries of public calls (web 0.3 s).
    transient_delay: Duration,
    /// Whether 429 waits get random jitter (off in tests).
    jitter: bool,
}

impl DeltaBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, WS_PUBLIC_URL)
    }

    /// Point the adapter at other hosts (tests run a local fake Delta).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            ws_url: ws_url.into(),
            symbols,
            quota: Quota::default(),
            clock: system_clock,
            transient_delay: Duration::from_millis(300),
            jitter: true,
        }
    }

    /// Fixed clock (tests).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Smaller quota and no retry pauses (tests).
    pub fn with_quota(mut self, quota: Quota) -> Self {
        self.quota = quota;
        self
    }

    /// No jitter and no transient-retry pause (tests).
    pub fn without_delays(mut self) -> Self {
        self.transient_delay = Duration::ZERO;
        self.jitter = false;
        self
    }

    pub fn quota(&self) -> &Quota {
        &self.quota
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Draw `weight` units from `bucket`, waiting for the window when the
    /// budget is spent (web `consume`), but never past `budget`. Runs before
    /// signing.
    pub(crate) async fn consume(
        &self,
        bucket: Bucket,
        path: &str,
        method: &Method,
        budget: Duration,
    ) -> Result<()> {
        let weight = ratelimit::endpoint_weight(path, method.as_str());
        let mut probed = false;
        let mut slept = 0;
        loop {
            let wait = match self.quota.try_take(bucket, weight) {
                Ok(()) => return Ok(()),
                Err(w) => w,
            };
            // The public bucket asks the exchange where its fixed window
            // really stands before parking anyone (3 units, unauthenticated
            // so it reports the IP allowance).
            if bucket == Bucket::Public && !probed {
                probed = true;
                if let Some((used, left)) = self.server_quota().await {
                    self.quota.set_server(bucket, used, left);
                    continue;
                }
            }
            if wait > ratelimit::MAX_WAIT.min(budget) || slept >= ratelimit::MAX_SLEEPS {
                tracing::warn!(
                    ?bucket,
                    "Delta Exchange quota spent; window resets in {}s",
                    wait.as_secs()
                );
                return Err(rate_limited());
            }
            tracing::warn!(
                ?bucket,
                "Delta Exchange quota spent; waiting {:.1}s for the window",
                wait.as_secs_f64()
            );
            tokio::time::sleep(wait).await;
            slept += 1;
        }
    }

    /// `GET /v2/rate_limits/quota`: `(units used, time to reset)`.
    async fn server_quota(&self) -> Option<(u32, Duration)> {
        let url = format!("{}/v2/rate_limits/quota", self.base_url);
        let resp = self
            .http
            .get(&url)
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        let used = v.get("current_quota").and_then(num)? as u32;
        let left_ms = v
            .get("remaining_time_in_milliseconds")
            .and_then(num)
            .unwrap_or(0.0);
        tracing::info!(
            "Delta Exchange quota check: {} units used, resets in {}s",
            used,
            (left_ms / 1000.0).round()
        );
        Some((used, Duration::from_secs_f64((left_ms / 1000.0).max(0.0))))
    }

    fn jittered(&self, d: Duration) -> Duration {
        if self.jitter {
            d + Duration::from_secs_f64(rand::random::<f64>() * 0.5)
        } else {
            d
        }
    }

    /// One signed call (web `get_api_response`). `params` become the sorted,
    /// unencoded query string that is both signed and sent; `body` is sent
    /// verbatim as signed. Returns the envelope's `result`.
    pub(crate) async fn signed<T: DeserializeOwned>(
        &self,
        auth: &AuthToken,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<T> {
        let env = self
            .signed_envelope(auth, method, path, params, body)
            .await?;
        decode_result(env, path)
    }

    /// Like `signed`, but hands back the whole envelope.
    pub(crate) async fn signed_envelope(
        &self,
        auth: &AuthToken,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Envelope> {
        let (key, secret) = auth::credentials(auth)?;
        let query = auth::query_string(params);
        let body_text = body.map(Value::to_string).unwrap_or_default();
        let url = format!("{}{}{}", self.base_url, path, query);
        let mut attempt = 0;
        // An order write waits at most ORDER_WAIT_CAP in all (12-U2).
        let cap = if ratelimit::is_order_write(method.as_str(), path) {
            ratelimit::ORDER_WAIT_CAP
        } else {
            Duration::MAX
        };
        let started = tokio::time::Instant::now();
        loop {
            // Quota first, then sign: consume() can wait, and a signature
            // older than 5 seconds is refused ("SignatureExpired").
            self.consume(
                Bucket::Private,
                path,
                &method,
                cap.saturating_sub(started.elapsed()),
            )
            .await?;
            let ts = self.now().to_string();
            let sig = auth::signature(secret, method.as_str(), &ts, path, &query, &body_text);
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("api-key", key)
                .header("timestamp", &ts)
                .header("signature", sig)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            if !body_text.is_empty() {
                req = req.body(body_text.clone());
            }
            let resp = req.send().await?;
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                self.quota
                    .note_429(Bucket::Private, ratelimit::quota_reset(resp.headers()));
                if attempt >= ratelimit::MAX_RETRIES {
                    tracing::warn!("Delta Exchange kept refusing {} with 429", path);
                    return Err(rate_limited());
                }
                let wait = self.jittered(ratelimit::retry_delay(resp.headers(), attempt));
                if started.elapsed().saturating_add(wait) > cap {
                    tracing::warn!(
                        "Delta Exchange rate-limited {}; the order is refused rather than sent {:.0}s late",
                        path,
                        wait.as_secs_f64()
                    );
                    return Err(rate_limited());
                }
                tracing::warn!(
                    "Delta Exchange rate-limited {} (attempt {}/{}); retrying in {:.1}s",
                    path,
                    attempt + 1,
                    ratelimit::MAX_RETRIES,
                    wait.as_secs_f64()
                );
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }
            return read_envelope(resp, path).await;
        }
    }

    /// One public GET (web `_public_get`): no auth, the public bucket,
    /// transient network errors retried twice, 429s retried while the
    /// server's requested wait is short. Returns the envelope's `result`.
    pub(crate) async fn public<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<T> {
        let env = self.public_envelope(path, params).await?;
        decode_result(env, path)
    }

    pub(crate) async fn public_envelope(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Envelope> {
        let url = format!("{}{}{}", self.base_url, path, auth::query_string(params));
        let mut attempt = 0;
        loop {
            self.consume(Bucket::Public, path, &Method::GET, Duration::MAX)
                .await?;
            let sent = self
                .http
                .get(&url)
                .header("Accept", "application/json")
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(e) if attempt < ratelimit::PUBLIC_RETRIES && transient(&e) => {
                    tracing::warn!(
                        "Transient network error on Delta Exchange {} (attempt {}); retrying",
                        path,
                        attempt + 1
                    );
                    tokio::time::sleep(self.transient_delay).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                self.quota
                    .note_429(Bucket::Public, ratelimit::quota_reset(resp.headers()));
                let requested = ratelimit::server_requested_delay(resp.headers());
                if attempt >= ratelimit::PUBLIC_RETRIES
                    || requested.is_some_and(|r| r > ratelimit::MAX_WAIT)
                {
                    tracing::warn!("Delta Exchange rate-limited {}; not retrying", path);
                    return Err(rate_limited());
                }
                tokio::time::sleep(ratelimit::retry_delay(resp.headers(), attempt)).await;
                attempt += 1;
                continue;
            }
            return read_envelope(resp, path).await;
        }
    }
}

fn transient(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Delta's response envelope: `{success, result, error, meta}`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub error: Value,
    #[serde(default)]
    pub meta: Value,
}

async fn read_envelope(resp: reqwest::Response, path: &str) -> Result<Envelope> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let parsed = serde_json::from_slice::<Envelope>(&bytes);
    match (status, parsed) {
        (StatusCode::UNAUTHORIZED, parsed) => {
            let code = parsed
                .ok()
                .map(|e| error_code(&e.error))
                .unwrap_or_default();
            tracing::warn!("Delta Exchange refused the signature on {}: {}", path, code);
            Err(AppError::Auth(
                "Delta Exchange rejected the API key or signature. Check the API key and secret on the broker page, and that your computer's clock is correct."
                    .into(),
            ))
        }
        (StatusCode::FORBIDDEN, parsed) if parsed.as_ref().map_or(true, |e| e.error.is_null()) => {
            tracing::warn!("Delta Exchange answered 403 on {}", path);
            Err(AppError::Broker(
                "Delta Exchange blocked this request. Whitelist this computer's IP address for the API key in your Delta Exchange account, then try again."
                    .into(),
            ))
        }
        (_, Ok(env)) => {
            if env.success {
                Ok(env)
            } else {
                Err(envelope_error(&env.error, status, path))
            }
        }
        (_, Err(e)) => {
            let prefix: String = String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            tracing::warn!(
                status = status.as_u16(),
                "Unexpected Delta Exchange response on {} ({}): {}",
                path,
                e,
                prefix
            );
            Err(AppError::Broker(if status.is_server_error() {
                "Delta Exchange's servers are not responding normally. Try again shortly.".into()
            } else {
                "Delta Exchange sent a response OpenAlgo could not read. Try again shortly.".into()
            }))
        }
    }
}

fn error_code(err: &Value) -> String {
    match err {
        Value::String(s) => s.clone(),
        Value::Object(_) => err
            .get("code")
            .map(|c| match c {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// `error.message`, else `error.code` (web `error.get("message") or
/// error.get("code")`), as a trader-facing broker error.
pub(crate) fn envelope_error(err: &Value, status: StatusCode, path: &str) -> AppError {
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let code = error_code(err);
    tracing::warn!(
        status = status.as_u16(),
        "Delta Exchange refused {}: {}",
        path,
        if code.is_empty() { "unknown" } else { &code }
    );
    if matches!(
        code.as_str(),
        "SignatureExpired" | "expired_signature" | "Signature Expired"
    ) {
        return AppError::Broker(
            "Delta Exchange refused the request because the signature arrived late. Check that your computer's clock is set automatically, then try again."
                .into(),
        );
    }
    match message.or_else(|| (!code.is_empty()).then(|| code.replace('_', " "))) {
        Some(m) => AppError::Broker(m),
        None => AppError::Broker("Delta Exchange refused the request.".into()),
    }
}

fn decode_result<T: DeserializeOwned>(env: Envelope, path: &str) -> Result<T> {
    serde_json::from_value(env.result).map_err(|e| {
        tracing::warn!("Unexpected Delta Exchange result on {}: {}", path, e);
        AppError::Broker(
            "Delta Exchange sent a response OpenAlgo could not read. Try again shortly.".into(),
        )
    })
}

pub(crate) fn rate_limited() -> AppError {
    AppError::Broker(
        "Delta Exchange's request allowance is used up for the next few minutes. Wait a moment and try again."
            .into(),
    )
}

#[async_trait]
impl Broker for DeltaBroker {
    fn id(&self) -> &'static str {
        "deltaexchange"
    }

    fn name(&self) -> &'static str {
        "Delta Exchange"
    }

    fn logo(&self) -> &'static str {
        "/logos/deltaexchange.svg"
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
            multiquotes_batch: false,
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

    fn leverage_config(&self) -> bool {
        true
    }

    fn broker_type(&self) -> &'static str {
        "crypto"
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(self, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        let q = CryptoQuantity::whole(order.quantity);
        orders::place_order(self, auth, order, &q).await
    }

    async fn place_order_exact(
        &self,
        auth: &AuthToken,
        order: &ResolvedOrder,
        quantity: &CryptoQuantity,
    ) -> Result<OrderResponse> {
        orders::place_order(self, auth, order, quantity).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        let q = CryptoQuantity::whole(order.quantity);
        orders::modify_order(self, auth, order, &q).await
    }

    async fn modify_order_exact(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
        quantity: &CryptoQuantity,
    ) -> Result<OrderResponse> {
        orders::modify_order(self, auth, order, quantity).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(self, auth, order_id).await
    }

    async fn set_leverage(
        &self,
        auth: &AuthToken,
        instrument: &SymbolData,
        leverage: u32,
    ) -> Result<()> {
        orders::set_leverage(self, auth, instrument, leverage).await
    }

    async fn get_leverage(&self, auth: &AuthToken, instrument: &SymbolData) -> Result<f64> {
        orders::get_leverage(self, auth, instrument).await
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
        _product: Product,
    ) -> Result<i64> {
        orders::get_open_position(self, auth, symbol, exchange).await
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

    async fn get_order_book_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Order>>> {
        orders::get_order_book_exact(self, auth).await
    }

    async fn get_trade_book_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Trade>>> {
        orders::get_trade_book_exact(self, auth).await
    }

    async fn get_positions_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Position>>> {
        orders::get_positions_exact(self, auth).await
    }

    async fn get_holdings(&self, _auth: &AuthToken) -> Result<Vec<Holding>> {
        // Delta has no demat holdings; spot balances appear as positions.
        Ok(Vec::new())
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(self, auth, legs).await
    }

    async fn get_quote(&self, _auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(self, key).await
    }

    async fn get_market_depth(&self, _auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, key).await
    }

    async fn get_history(&self, _auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(self, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        Ok(master_contract::download(self).await?.rows)
    }

    async fn download_master(&self, _auth: &AuthToken) -> Result<MasterContract> {
        master_contract::download(self).await
    }

    fn create_feed(&self, _auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Ok(Box::new(streaming::DeltaFeed::new(&self.ws_url)))
    }
}
