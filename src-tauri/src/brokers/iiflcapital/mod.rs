//! IIFL Capital adapter (web `broker/iiflcapital/**`).
//!
//! Bespoke REST at `https://api.iiflcapital.com/v1` (not the XTS `iifl`
//! member) plus an MQTT 3.1.1 bridge for market data and order updates.
//!
//! * Sign-in: OAuth redirect to `markets.iiflcapital.com`; the callback
//!   carries `authCode` and `clientId`, exchanged for a `userSession` JWT
//!   with `checkSum = sha256(clientId + authCode + secret)`.
//! * Every REST call: `Authorization: Bearer <userSession>`.
//! * Pacing (web `api/rate_limiter.py`): one process-wide slot every 125 ms
//!   for reads and a separate clock for order writes. Reads that meet a
//!   throttle reply are retried up to three times (`Retry-After` or 1/2/4 s);
//!   order writes are never resent, since IIFL's generic "try after some
//!   time" reply can arrive after the order was accepted.
//! * Feeds: the shared `WebSocketManager` speaks WebSocket only, so each feed
//!   runs a loopback relay (`mqtt_relay`) that owns the MQTT connection.

pub mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
pub mod mqtt_relay;
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
use mqtt_relay::MqttEndpoint;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

pub const BASE_URL: &str = "https://api.iiflcapital.com/v1";
pub const LOGIN_URL: &str = "https://markets.iiflcapital.com/";
pub const MQTT_HOST: &str = "bridge.iiflcapital.com";
pub const MQTT_PORT: u16 = 8883;

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
];

/// web `BrokerData.timeframe_map` (`api/data.py`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1 minute"),
    ("5m", "5 minutes"),
    ("10m", "10 minutes"),
    ("15m", "15 minutes"),
    ("30m", "30 minutes"),
    ("60m", "60 minutes"),
    ("1h", "60 minutes"),
    ("D", "1 day"),
    ("W", "weekly"),
    ("M", "monthly"),
];

/// web `rate_limiter.MIN_INTERVAL` (about 8 calls a second).
pub const MIN_INTERVAL: Duration = Duration::from_millis(125);
/// web `rate_limiter.MAX_RETRIES`.
pub const MAX_RETRIES: u32 = 3;
/// Longest server-requested wait a read sleeps out before giving up.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(10);

/// What a trader reads when IIFL answers an order write with a throttle
/// (web `order_api._UNCONFIRMED_WRITE_MESSAGE`).
pub const UNCONFIRMED_WRITE_MESSAGE: &str = "IIFL Capital did not confirm this request and asked for it to be tried again later. OpenAlgo did not send it again, because the first one may already have reached IIFL Capital. Check the order book before sending it again.";

pub struct IiflCapitalBroker {
    http: reqwest::Client,
    base_url: String,
    symbols: SymbolResolver,
    read_pacer: Pacer,
    order_pacer: Pacer,
    /// Wait base for read retries (1 s like the web; tests shorten it).
    retry_base: Duration,
    /// Wait base between master-contract download attempts (2 s).
    download_backoff: Duration,
    mqtt: MqttEndpoint,
}

impl IiflCapitalBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, BASE_URL)
    }

    /// Point the adapter at another host (tests run a local fake).
    pub fn with_base_url(symbols: SymbolResolver, base_url: impl Into<String>) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            symbols,
            read_pacer: Pacer::with_interval(MIN_INTERVAL),
            order_pacer: Pacer::with_interval(MIN_INTERVAL),
            retry_base: Duration::from_secs(1),
            download_backoff: Duration::from_secs(2),
            mqtt: MqttEndpoint::tls(MQTT_HOST, MQTT_PORT),
        }
    }

    /// Use another MQTT bridge (tests run a plain-TCP fake).
    pub fn with_mqtt(mut self, endpoint: MqttEndpoint) -> Self {
        self.mqtt = endpoint;
        self
    }

    /// Shorten retry and download backoffs (tests).
    pub fn with_backoff(mut self, retry_base: Duration, download_backoff: Duration) -> Self {
        self.retry_base = retry_base;
        self.download_backoff = download_backoff;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    fn bearer(auth: &AuthToken) -> Result<String> {
        let t = auth.raw().trim();
        if t.is_empty() {
            return Err(session_expired());
        }
        Ok(format!("Bearer {}", t))
    }

    /// One IIFL call. Returns the HTTP status and the JSON body (a body that
    /// is not JSON becomes `{"status":"error","message":<text>}` like the
    /// web). Reads (`GET`, and the market-data `POST`s flagged `read`) are
    /// retried on a throttle reply; writes never are.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        read: bool,
    ) -> Result<(StatusCode, Value)> {
        let bearer = Self::bearer(auth)?;
        let mut attempt = 0u32;
        loop {
            if read {
                self.read_pacer.acquire().await;
            } else {
                self.order_pacer.acquire().await;
            }
            let url = format!("{}{}", self.base_url, path);
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", &bearer)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok());
            let bytes = resp.bytes().await?;
            let data: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::json!({
                    "status": "error",
                    "message": String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).to_string()
                })
            });
            let message = mapping::message_of(&data).unwrap_or_default();
            if mapping::is_rate_limited(status.as_u16(), &message) {
                if !read {
                    tracing::warn!(
                        status = status.as_u16(),
                        "IIFL Capital answered an order write on {} with a throttle reply; not resending it",
                        path.split('?').next().unwrap_or("")
                    );
                    return Err(AppError::Broker(UNCONFIRMED_WRITE_MESSAGE.into()));
                }
                if attempt < MAX_RETRIES {
                    let wait = retry_after
                        .map(|s| Duration::from_secs_f64(s.max(0.05)))
                        .unwrap_or_else(|| self.retry_base * (1u32 << attempt));
                    if wait <= MAX_RETRY_WAIT {
                        tracing::warn!(
                            "IIFL Capital rate limited {}; retrying in {:?} (attempt {}/{})",
                            path.split('?').next().unwrap_or(""),
                            wait,
                            attempt + 1,
                            MAX_RETRIES
                        );
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                }
                return Err(AppError::Broker(
                    "IIFL Capital is limiting requests right now. Wait a moment and try again."
                        .into(),
                ));
            }
            if status == StatusCode::UNAUTHORIZED {
                tracing::warn!(
                    "IIFL Capital refused the session on {}",
                    path.split('?').next().unwrap_or("")
                );
                return Err(session_expired());
            }
            return Ok((status, data));
        }
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your IIFL Capital session has expired. Log in to IIFL Capital again.".into())
}

#[async_trait]
impl Broker for IiflCapitalBroker {
    fn id(&self) -> &'static str {
        "iiflcapital"
    }

    fn name(&self) -> &'static str {
        "IIFL Capital"
    }

    fn logo(&self) -> &'static str {
        "/logos/iiflcapital.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "authCode" }
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
            // The order-update stream, offered by
            // `IiflCapitalBroker::order_socket`, served through `Broker::create_order_feed`.
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

    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        data::get_multiquotes(self, auth, keys)
            .await
            .map_err(redact)
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

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let session = auth.raw().trim();
        if session.is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::IiflFeed::new(
            self.mqtt.clone(),
            session,
            self.symbols.clone(),
        )?))
    }
}

impl IiflCapitalBroker {
    /// Order and trade updates over a second, dedicated MQTT connection
    /// (web `streaming/iiflcapital_order_adapter.py`). The `Broker` trait has
    /// no order-feed factory on master, so this is an inherent method, like
    /// `UpstoxBroker::create_order_feed` and `AngelBroker::create_order_feed`.
    /// The client id comes from the session (`AuthToken::user_id`) or, when
    /// absent, from `GET /profile` on each connect.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let session = auth.raw().trim();
        if session.is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::IiflOrderFeed::new(
            self.mqtt.clone(),
            session,
            auth.user_id().map(str::to_string),
            self.base_url.clone(),
            self.symbols.clone(),
        )?))
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
