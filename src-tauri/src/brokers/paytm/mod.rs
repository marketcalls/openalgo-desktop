//! Paytm Money adapter (web `broker/paytm/**`).
//!
//! * Sign-in: the redirect returns `requestToken`, exchanged at
//!   `/accounts/v2/gettoken` with the API key and secret. The stored session
//!   is the `access_token` (REST, header `x-jwt-token`); the
//!   `public_access_token` is the feed token for the market-data socket.
//! * Instruments are addressed by `security_id` (the master `token`) and the
//!   parent exchange (`NFO` -> `NSE`, `BFO` -> `BSE`) with a segment flag.
//! * No history API and no margin calculator on Paytm Money.

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
use mapping::PaytmEnvelope;
use reqwest::Method;
use std::time::Duration;

pub const BASE_URL: &str = "https://developer.paytmmoney.com";
pub const MASTER_URL: &str = "https://developer.paytmmoney.com/data/v1/scrips/security_master.csv";

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map = {}`: Paytm Money has no history API.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[];

/// web `get_api_response(max_retries=3, retry_delay=2)`.
pub const READ_ATTEMPTS: u32 = 3;
pub const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Hosts the adapter talks to (tests point them at a local fake).
#[derive(Debug, Clone)]
pub struct Urls {
    pub api: String,
    pub master: String,
    pub ws: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            api: BASE_URL.to_string(),
            master: MASTER_URL.to_string(),
            ws: streaming::WS_URL.to_string(),
        }
    }
}

pub struct PaytmBroker {
    http: reqwest::Client,
    urls: Urls,
    symbols: SymbolResolver,
    retry_delay: Duration,
    /// Paytm Money allows 10 order requests per second.
    order_pacer: Pacer,
    other_pacer: Pacer,
}

impl PaytmBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, Urls::default())
    }

    /// Point the adapter at other hosts (tests run a local fake Paytm).
    pub fn with_base_url(
        symbols: SymbolResolver,
        api: impl Into<String>,
        master: impl Into<String>,
        ws: impl Into<String>,
    ) -> Self {
        Self::with_urls(
            symbols,
            Urls {
                api: api.into(),
                master: master.into(),
                ws: ws.into(),
            },
        )
    }

    pub fn with_urls(symbols: SymbolResolver, urls: Urls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            retry_delay: RETRY_DELAY,
            order_pacer: Pacer::per_second(10.0),
            other_pacer: Pacer::per_second(10.0),
        }
    }

    /// Delay between read retries on a 5xx (2 s on the web).
    pub fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    fn token(auth: &AuthToken) -> Result<&str> {
        let t = auth.raw().trim();
        if t.is_empty() {
            Err(session_expired())
        } else {
            Ok(t)
        }
    }

    async fn send_once(
        &self,
        method: &Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&serde_json::Value>,
    ) -> Result<(reqwest::StatusCode, PaytmEnvelope)> {
        let url = format!("{}{}", self.urls.api, path);
        let mut req = self
            .http
            .request(method.clone(), &url)
            .header("x-jwt-token", Self::token(auth)?)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.map_err(|e| redact(e.into()))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| redact(e.into()))?;
        let env = serde_json::from_slice::<PaytmEnvelope>(&bytes).unwrap_or_default();
        Ok((status, env))
    }

    /// One Paytm call. Reads (GET) are retried on a 5xx or a network error
    /// like the web (`max_retries=3`, 2 s apart); writes are sent once so an
    /// order is never duplicated. A non-success envelope becomes a
    /// trader-facing error.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&serde_json::Value>,
    ) -> Result<PaytmEnvelope> {
        let is_read = method == Method::GET;
        let attempts = if is_read { READ_ATTEMPTS } else { 1 };
        let leaf = path.split('?').next().unwrap_or("");
        let mut last_err = None;
        for attempt in 0..attempts {
            if is_read {
                self.other_pacer.acquire().await;
            } else {
                self.order_pacer.acquire().await;
            }
            match self.send_once(&method, path, auth, body).await {
                Ok((status, _)) if status.is_server_error() => {
                    tracing::warn!(
                        status = status.as_u16(),
                        attempt = attempt + 1,
                        "Paytm Money server error on {}",
                        leaf
                    );
                    last_err = Some(AppError::Broker(
                        "Paytm Money's servers are not responding normally. Try again shortly."
                            .into(),
                    ));
                }
                Ok((status, env)) => {
                    if status.is_success() && env.is_success() {
                        return Ok(env);
                    }
                    let msg = env.error_message();
                    tracing::warn!(
                        status = status.as_u16(),
                        "Paytm Money refused {}: {}",
                        leaf,
                        msg
                    );
                    return Err(paytm_error(status.as_u16(), &msg));
                }
                Err(e) => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        "Paytm Money request to {} failed: {}",
                        leaf,
                        e.code()
                    );
                    last_err = Some(e);
                }
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(self.retry_delay).await;
            }
        }
        Err(last_err.unwrap_or_else(|| {
            AppError::Broker("Paytm Money did not answer. Try again shortly.".into())
        }))
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Paytm Money session has expired. Log in to Paytm Money again.".into())
}

/// HTTP status + broker message -> trader-facing error.
pub(crate) fn paytm_error(status: u16, message: &str) -> AppError {
    let m = message.trim();
    match status {
        401 | 403 => session_expired(),
        429 => AppError::Broker(
            "Paytm Money is limiting requests right now. Wait a moment and try again.".into(),
        ),
        _ if m.is_empty() => AppError::Broker("Paytm Money refused the request.".into()),
        _ => AppError::Broker(m.to_string()),
    }
}

#[async_trait]
impl Broker for PaytmBroker {
    fn id(&self) -> &'static str {
        "paytm"
    }

    fn name(&self) -> &'static str {
        "Paytm Money"
    }

    fn logo(&self) -> &'static str {
        "/logos/paytm.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "requestToken",
        }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: false,
            multiquotes_batch: true,
            margin: false,
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

    async fn get_history(&self, _auth: &AuthToken, _req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history()
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        // The socket authenticates with the public access token (web
        // `feed_token`); a session stored without one falls back to the
        // REST token, as the web's login does.
        let token = auth
            .feed()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(auth.raw());
        if token.trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::PaytmFeed::with_url(
            &self.urls.ws,
            token,
            self.symbols.clone(),
        )))
    }
}

/// Transport errors carry the request URL, and with it query credentials
/// (API key, account id). Strip it before the error can reach a log or a
/// caller; every other error passes through unchanged.
pub(crate) fn redact(e: AppError) -> AppError {
    match e {
        AppError::Http(h) => AppError::Http(Box::new(h.without_url())),
        other => other,
    }
}
