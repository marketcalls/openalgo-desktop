//! Nubra Trading API V3 adapter (web `broker/nubra/**`).
//!
//! * Sign-in (web `authenticate_broker_totp`): the stored API key is the
//!   registered mobile number and the secret the MPIN; the form carries a
//!   TOTP. `POST /totp/login` -> `auth_token`, then `POST /verifypin` ->
//!   `session_token`, which is the stored session. The phone-OTP flow's
//!   steps are public in `auth` for a future two-form route.
//! * Every authenticated call sends `Authorization: Bearer <session>`,
//!   `Accept: application/json` and `x-device-id: OPENALGO` (the id the
//!   login bound the session to). HTTP 440 means the session expired and is
//!   never retried; 429 is retried with 1 s, 2 s backoff (3 attempts).
//! * Prices travel as integer paise in both directions.

pub mod auth;
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
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

pub const BASE_URL: &str = "https://api.nubra.io";
/// Market-data socket (web `api/nubrawebsocket.py` `WS_URL`).
pub const MARKET_WS_URL: &str = "wss://api.nubra.io/apibatch/ws";
/// Order-update socket used when `/userinfo` does not name one (web
/// `NUBRA_ORDER_WS_URL_DEFAULT`).
pub const ORDER_WS_FALLBACK: &str = "wss://uatapi.nubra.io/ws";
/// The device id OpenAlgo presents (web `baseurl.DEVICE_ID`).
pub const DEVICE_ID: &str = "OPENALGO";
/// Nubra's session-expired status (web `SESSION_EXPIRED_STATUS`).
pub const SESSION_EXPIRED_STATUS: u16 = 440;

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map`, in its order.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1s", "1s"),
    ("1m", "1m"),
    ("2m", "2m"),
    ("3m", "3m"),
    ("5m", "5m"),
    ("15m", "15m"),
    ("30m", "30m"),
    ("1h", "1h"),
    ("D", "1d"),
    ("W", "1w"),
    ("M", "1mt"),
];

/// Attempts for a call that keeps answering 429.
const MAX_ATTEMPTS: u32 = 3;

pub struct NubraBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) market_ws_url: String,
    pub(crate) order_ws_fallback: String,
    symbols: SymbolResolver,
    /// Sequential loops (cancel-all, close-all) at 10 ops/s.
    pub(crate) loop_pacer: Pacer,
    /// Historical data: 60 requests a minute.
    pub(crate) history_pacer: Pacer,
    /// 429 backoff base (1 s; tests shorten it).
    pub(crate) retry_base: Duration,
    /// How long a one-shot feed snapshot waits (web 2 s).
    pub(crate) snapshot_wait: Duration,
}

impl NubraBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, MARKET_WS_URL)
    }

    /// Point the adapter at other hosts (tests run local fakes).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        market_ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            market_ws_url: market_ws_url.into(),
            order_ws_fallback: ORDER_WS_FALLBACK.to_string(),
            symbols,
            loop_pacer: Pacer::per_second(10.0),
            history_pacer: Pacer::per_second(1.0),
            retry_base: Duration::from_secs(1),
            snapshot_wait: Duration::from_secs(2),
        }
    }

    /// Shorter waits for tests (429 backoff, history pacing, snapshots).
    pub fn with_fast_timings(mut self) -> Self {
        self.retry_base = Duration::from_millis(5);
        self.history_pacer = Pacer::per_second(1000.0);
        self.loop_pacer = Pacer::per_second(1000.0);
        self.snapshot_wait = Duration::from_millis(400);
        self
    }

    /// Order-update socket used when `/userinfo` names none.
    pub fn with_order_ws_fallback(mut self, url: impl Into<String>) -> Self {
        self.order_ws_fallback = url.into();
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// One authenticated call. Returns `(status, body)`; 440 becomes the
    /// session-expired error, 429 is retried. A body that is not JSON
    /// becomes `Value::Null` (callers decide from the status).
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let token = auth.raw();
        if token.trim().is_empty() {
            return Err(session_expired());
        }
        let url = self.url(path);
        let mut attempt = 0;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Accept", "application/json")
                .header("x-device-id", DEVICE_ID);
            if let Some(b) = body {
                req = req
                    .header("Content-Type", "application/json")
                    .body(b.to_string());
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            if status == 429 {
                attempt += 1;
                if attempt < MAX_ATTEMPTS {
                    let delay = self.retry_base * 2u32.pow(attempt - 1);
                    tracing::warn!(
                        "Nubra is limiting requests on {}; retrying in {:?}",
                        path.split('?').next().unwrap_or(""),
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(AppError::Broker(
                    "Nubra is limiting requests right now. Wait a moment and try again.".into(),
                ));
            }
            if status == SESSION_EXPIRED_STATUS {
                tracing::warn!(
                    "Nubra session expired on {}",
                    path.split('?').next().unwrap_or("")
                );
                return Err(session_expired());
            }
            let bytes = resp.bytes().await?;
            let v = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            return Ok((status, v));
        }
    }

    /// A call whose success is a 2xx with a JSON body; anything else is a
    /// broker error carrying Nubra's `error` / `message`.
    pub(crate) async fn get_ok(&self, path: &str, auth: &AuthToken) -> Result<Value> {
        let (status, v) = self.call(Method::GET, path, auth, None).await?;
        if !(200..300).contains(&status) || v.get("error").is_some_and(|e| !e.is_null()) {
            tracing::warn!(
                status,
                "Nubra refused {}",
                path.split('?').next().unwrap_or("")
            );
            return Err(refused(&v, status));
        }
        Ok(v)
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Nubra session has expired. Log in to Nubra again.".into())
}

/// Nubra's reason (V3 `error`, else `message`).
pub(crate) fn error_text(v: &Value) -> Option<String> {
    ["error", "message"].iter().find_map(|k| {
        v.get(*k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

pub(crate) fn refused(v: &Value, status: u16) -> AppError {
    match error_text(v) {
        Some(m) => AppError::Broker(m),
        None if status >= 500 => AppError::Broker(
            "Nubra's servers are not responding normally. Try again shortly.".into(),
        ),
        None => AppError::Broker("Nubra refused the request.".into()),
    }
}

#[async_trait]
impl Broker for NubraBroker {
    fn id(&self) -> &'static str {
        "nubra"
    }

    fn name(&self) -> &'static str {
        "Nubra"
    }

    fn logo(&self) -> &'static str {
        "/logos/nubra.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp { fields: &["totp"] }
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
            // The order-update stream, offered by
            // `NubraBroker::create_order_feed` (as Upstox and Kotak do).
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
        auth::authenticate(self, credentials).await.map_err(redact)
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        orders::place_order(self, auth, order).await.map_err(redact)
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        orders::modify_order(self, auth, order)
            .await
            .map_err(redact)
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(self, auth, order_id)
            .await
            .map_err(redact)
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(self, auth).await.map_err(redact)
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(self, auth)
            .await
            .map_err(redact)
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(self, auth, symbol, exchange, product)
            .await
            .map_err(redact)
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        orders::get_order_book(self, auth).await.map_err(redact)
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        orders::get_trade_book(self, auth).await.map_err(redact)
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        orders::get_positions(self, auth).await.map_err(redact)
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(self, auth).await.map_err(redact)
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await.map_err(redact)
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(self, auth, legs)
            .await
            .map_err(redact)
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(self, auth, key).await.map_err(redact)
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, auth, key)
            .await
            .map_err(redact)
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(self, auth, req).await.map_err(redact)
    }

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth).await.map_err(redact)
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::NubraFeed::new(
            &self.market_ws_url,
            auth.raw(),
            self.symbols.clone(),
        )))
    }
}

impl NubraBroker {
    /// The order-update stream (web `streaming/nubra_order_adapter.py`).
    /// Not part of the `Broker` trait yet (upstox and angel expose theirs
    /// the same way). The socket URL comes from `GET /userinfo` on every
    /// (re)connect, which a `BrokerFeed` cannot do synchronously, so the
    /// feed runs behind the loopback relay (`upstox::relay`).
    pub fn create_order_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::NubraOrderFeed::new(
            streaming::OrderUpstream {
                http: self.http.clone(),
                base_url: self.base_url.clone(),
                fallback_url: self.order_ws_fallback.clone(),
                session: crate::security::Secret::new(auth.raw()),
            },
            self.symbols.clone(),
        )))
    }
}

/// Transport errors carry the request URL. None of this adapter's REST URLs
/// carries a credential, but an error is still stripped of its URL before it
/// can reach a log or a caller (the OAuth batch pattern); every other error
/// passes through unchanged.
pub(crate) fn redact(e: AppError) -> AppError {
    match e {
        AppError::Http(h) => AppError::Http(Box::new(h.without_url())),
        other => other,
    }
}
