//! Angel One SmartAPI adapter (web `broker/angel/**`).
//!
//! * The stored session token is `api_key:jwt`. Every secure REST call sends
//!   `Authorization: Bearer <jwt>` and the API key in `X-PrivateKey`, plus the
//!   web's fixed client headers (`X-UserType`, `X-SourceID`,
//!   `X-ClientLocalIP`, `X-ClientPublicIP`, `X-MACAddress`).
//! * Market-data calls are paced like the web's shared limiter (quotes one
//!   slot per 0.15 s, history one per 0.5 s) and retried on Angel's
//!   "exceeding access rate" rejection with backoff; order calls are never
//!   retried, so a slow answer cannot place an order twice.
//! * Books come back in OpenAlgo symbols (by token, as the web's
//!   `get_symbol`) with Angel's own lowercase statuses.
//! * The SmartStream feed authenticates with the raw JWT (no `Bearer`), the
//!   API key, the client code and the feed token issued at login.

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

use crate::brokers::common::de::string_lenient;
use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::{backoff_delay, Pacer};
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::time::Duration;

pub const BASE_URL: &str = "https://apiconnect.angelone.in";
pub const MASTER_CONTRACT_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

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
    Exchange::McxIndex,
];

/// web `BrokerData.timeframe_map`.
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

/// Pacing and retry category of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    /// Order placement / modification / cancel / GTT writes: never retried.
    Order,
    /// `market/v1/quote` (web 0.15 s spacing).
    Quote,
    /// `historical/v1/*` (web 0.5 s spacing).
    History,
    /// Books, funds, margin, GTT reads.
    Other,
}

/// Retries of a market-data call rejected for rate (web `max_retries=2`).
const RATE_RETRIES: u32 = 2;

pub struct AngelBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) master_url: String,
    pub(crate) feed_url: String,
    pub(crate) order_feed_url: String,
    symbols: SymbolResolver,
    quote_pacer: Pacer,
    history_pacer: Pacer,
    order_pacer: Pacer,
    other_pacer: Pacer,
}

impl AngelBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, BASE_URL)
    }

    /// Point the REST calls (and the master download) at another host; tests
    /// run a local fake SmartAPI.
    pub fn with_base_url(symbols: SymbolResolver, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let master_url = if base_url == BASE_URL {
            MASTER_CONTRACT_URL.to_string()
        } else {
            format!("{}/OpenAPI_File/files/OpenAPIScripMaster.json", base_url)
        };
        Self {
            http: http::client(),
            base_url,
            master_url,
            feed_url: streaming::WS_URL.to_string(),
            order_feed_url: streaming::ORDER_WS_URL.to_string(),
            symbols,
            quote_pacer: Pacer::with_interval(Duration::from_millis(150)),
            history_pacer: Pacer::with_interval(Duration::from_millis(500)),
            // SmartAPI order limit is 20/s; books and funds 10/s.
            order_pacer: Pacer::per_second(20.0),
            other_pacer: Pacer::per_second(10.0),
        }
    }

    /// Point the market-data and order-update sockets at other addresses
    /// (tests run a local fake server).
    pub fn with_feed_urls(
        mut self,
        feed_url: impl Into<String>,
        order_feed_url: impl Into<String>,
    ) -> Self {
        self.feed_url = feed_url.into();
        self.order_feed_url = order_feed_url.into();
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

    /// The web's fixed SmartAPI header block.
    pub(crate) fn request(
        &self,
        method: Method,
        path: &str,
        api_key: &str,
        jwt: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-UserType", "USER")
            .header("X-SourceID", "WEB")
            .header("X-ClientLocalIP", "CLIENT_LOCAL_IP")
            .header("X-ClientPublicIP", "CLIENT_PUBLIC_IP")
            .header("X-MACAddress", "MAC_ADDRESS")
            .header("X-PrivateKey", api_key);
        if let Some(t) = jwt {
            r = r.header("Authorization", format!("Bearer {}", t));
        }
        r
    }

    async fn pace(&self, cat: Category) {
        match cat {
            Category::Order => self.order_pacer.acquire().await,
            Category::Quote => self.quote_pacer.acquire().await,
            Category::History => self.history_pacer.acquire().await,
            Category::Other => self.other_pacer.acquire().await,
        }
    }

    /// One secure call returning the whole envelope (status false included),
    /// so callers that must inspect `errorcode` can.
    pub(crate) async fn call_env<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        cat: Category,
    ) -> Result<Envelope<T>> {
        let (api_key, jwt) = auth.pair().ok_or_else(session_expired)?;
        let retries = if matches!(cat, Category::Quote | Category::History) {
            RATE_RETRIES
        } else {
            0
        };
        let mut attempt = 0;
        loop {
            self.pace(cat).await;
            let mut req = self.request(method.clone(), path, api_key, Some(jwt));
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            match read_envelope::<T>(resp).await? {
                Reply::Ok(env) => return Ok(env),
                Reply::RateLimited if attempt < retries => {
                    let wait =
                        backoff_delay(attempt, Duration::from_millis(500), Duration::from_secs(4));
                    tracing::warn!(
                        "Angel One rate limit on {}; retry {} in {:?}",
                        path,
                        attempt + 1,
                        wait
                    );
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                Reply::RateLimited => {
                    return Err(AppError::Broker(
                        "Angel One is limiting requests right now. Wait a moment and try again."
                            .into(),
                    ))
                }
                Reply::Denied => return Err(session_expired()),
            }
        }
    }

    /// One secure call; `status: false` becomes a trader-facing error and the
    /// `data` member is returned.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        cat: Category,
    ) -> Result<Option<T>> {
        let env: Envelope<T> = self.call_env(method, path, auth, body, cat).await?;
        if !env.status {
            tracing::warn!(code = %env.errorcode, "Angel One refused {}: {}", path, env.message);
            return Err(angel_error(&env.errorcode, &env.message));
        }
        Ok(env.data)
    }
}

/// SmartAPI response envelope: `{status, message, errorcode, data}`.
#[derive(Debug, Deserialize)]
pub(crate) struct Envelope<T> {
    #[serde(default, deserialize_with = "status_lenient")]
    pub status: bool,
    #[serde(default, deserialize_with = "string_lenient")]
    pub message: String,
    #[serde(default, deserialize_with = "string_lenient")]
    pub errorcode: String,
    #[serde(default = "none")]
    pub data: Option<T>,
}

fn none<T>() -> Option<T> {
    None
}

/// `true` / `"true"` -> true (the web accepts both for modify).
fn status_lenient<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => s.eq_ignore_ascii_case("true"),
        _ => false,
    })
}

pub(crate) enum Reply<T> {
    Ok(Envelope<T>),
    /// HTTP 429, or 403 with Angel's plain-text "exceeding access rate".
    RateLimited,
    /// HTTP 401/403 that is not a rate limit (web: "Authentication failed").
    Denied,
}

/// Classify one SmartAPI answer (web `get_api_response`).
pub(crate) async fn read_envelope<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<Reply<T>> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::FORBIDDEN {
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        if status == StatusCode::TOO_MANY_REQUESTS
            || text.contains("exceeding")
            || text.contains("access rate")
            || text.contains("rate limit")
        {
            return Ok(Reply::RateLimited);
        }
    }
    match serde_json::from_slice::<Envelope<T>>(&bytes) {
        Ok(env) => Ok(Reply::Ok(env)),
        Err(e) => {
            if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
                tracing::warn!(status = status.as_u16(), "Angel One denied the request");
                return Ok(Reply::Denied);
            }
            let prefix: String = String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            tracing::warn!(
                status = status.as_u16(),
                "Unexpected response from Angel One ({}): {}",
                e,
                prefix
            );
            if status.is_server_error() {
                return Err(AppError::Broker(
                    "Angel One's servers are not responding normally. Try again shortly.".into(),
                ));
            }
            Err(AppError::Broker(
                "Angel One sent a response OpenAlgo could not read. Try again shortly.".into(),
            ))
        }
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Angel One session has expired. Log in to Angel One again.".into())
}

/// SmartAPI error code / message -> trader-facing error.
pub(crate) fn angel_error(code: &str, message: &str) -> AppError {
    match code {
        // Invalid / expired / missing token, session expired, and the GTT
        // engine's invalid client id / session id.
        "AG8001" | "AG8002" | "AG8003" | "AB1010" | "AB8050" | "AB8051" | "AB9003" | "AB9005" => {
            session_expired()
        }
        _ if message.trim().is_empty() => AppError::Broker("Angel One refused the request.".into()),
        _ => AppError::Broker(message.trim().to_string()),
    }
}

#[async_trait]
impl Broker for AngelBroker {
    fn id(&self) -> &'static str {
        "angel"
    }

    fn name(&self) -> &'static str {
        "Angel One"
    }

    fn logo(&self) -> &'static str {
        "/logos/angel.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["client_id", "password", "totp"],
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
            gtt: true,
            streaming: true,
            // The order-status socket is a separate connection.
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

    async fn get_order_book_tagged(&self, auth: &AuthToken) -> Result<Vec<TaggedOrder>> {
        orders::get_order_book_tagged(self, auth).await
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
        Ok(Box::new(streaming::AngelFeed::from_auth(
            &self.feed_url,
            auth,
            self.symbols.clone(),
        )?))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    async fn get_holdings_with_totals(&self, auth: &AuthToken) -> Result<HoldingsBook> {
        orders::get_holdings_with_totals(self, auth).await
    }
}

impl AngelBroker {
    /// The dedicated order-status socket (web `angel_order_adapter.py`),
    /// served through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (_, jwt) = auth.pair().ok_or_else(session_expired)?;
        Ok(Box::new(streaming::AngelOrderFeed::new(
            &self.order_feed_url,
            jwt,
            self.symbols.clone(),
        )))
    }
}
