//! Tradejini CubePlus API v2 adapter (web `broker/tradejini/**`).
//!
//! Sign-in is the individual-app token service: the stored API key plus
//! the trader's CubePlus PIN and TOTP give an access token. Every later
//! call sends `Authorization: Bearer <api_key>:<access_token>`, so the
//! session token stored by OpenAlgo Desktop is `api_key:access_token`.
//! OMS bodies are form-encoded. Quotes, multiquotes and depth have no REST
//! endpoint: they open a short-lived NxtradStream socket, read what they
//! need, and close it on every path.

mod auth;
pub mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
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
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

pub const BASE_URL: &str = "https://api.tradejini.com/v2";

/// web `plugin.json` supported_exchanges.
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

/// web `BrokerData.timeframe_map` (Tradejini serves 1, 5 and 30 minute
/// bars; the request takes the minute count).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[("1m", "1"), ("5m", "5"), ("30m", "30")];

/// Waits of the short-lived quote/depth socket (web `api/data.py`).
#[derive(Debug, Clone, Copy)]
pub struct WsTimings {
    /// Socket open budget (web: 15 x 1 s).
    pub connect: Duration,
    /// Pause before a single quote subscribes (web: 3 s).
    pub quote_settle: Duration,
    /// Pause before a multiquote batch subscribes (web: 2 s).
    pub multi_settle: Duration,
    /// Single quote: look for a complete quote in steps of this long
    /// (web `_QUOTE_STEP_SECONDS`) ...
    pub quote_step: Duration,
    /// ... at most this many steps (web `_QUOTE_STEPS`).
    pub quote_steps: u32,
    /// Depth: wait for the first book (web `_DEPTH_WAIT_SECONDS`).
    pub depth_wait: Duration,
    /// Multiquote wait: `clamp(n * per_symbol, min, max)` (web 0.05 s, 2 s,
    /// 10 s).
    pub multi_per_symbol: Duration,
    pub multi_min: Duration,
    pub multi_max: Duration,
}

impl Default for WsTimings {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(15),
            quote_settle: Duration::from_secs(3),
            multi_settle: Duration::from_secs(2),
            quote_step: Duration::from_secs(1),
            quote_steps: 40,
            depth_wait: Duration::from_secs(20),
            multi_per_symbol: Duration::from_millis(50),
            multi_min: Duration::from_secs(2),
            multi_max: Duration::from_secs(10),
        }
    }
}

pub struct TradejiniBroker {
    http: reqwest::Client,
    base_url: String,
    stream_url: String,
    symbols: SymbolResolver,
    timings: WsTimings,
}

/// Request body of an OMS call.
pub(crate) enum Body<'a> {
    None,
    Form(&'a [(&'a str, String)]),
}

impl TradejiniBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, streaming::STREAM_URL)
    }

    /// Point the adapter at other hosts (tests run a local fake Tradejini:
    /// `base_url` replaces `https://api.tradejini.com/v2`, `stream_url`
    /// replaces `wss://api.tradejini.com/v2.1/stream`).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        stream_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            stream_url: stream_url.into(),
            symbols,
            timings: WsTimings::default(),
        }
    }

    /// Shorter socket waits (tests).
    pub fn with_timings(mut self, timings: WsTimings) -> Self {
        self.timings = timings;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// `api_key:access_token` from the stored session.
    pub(crate) fn pair(auth: &AuthToken) -> Result<(&str, &str)> {
        auth.pair().ok_or_else(session_expired)
    }

    /// One OMS / market-data REST call. Returns the envelope for `ok` and
    /// `no-data`; error envelopes become trader-facing errors.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        auth: &AuthToken,
        body: Body<'_>,
    ) -> Result<Value> {
        Self::pair(auth)?;
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .http
            .request(method, &url)
            .header("Authorization", format!("Bearer {}", auth.raw()))
            .header("Accept", "application/json");
        if !query.is_empty() {
            req = req.query(query);
        }
        req = match body {
            Body::None => req.header("Content-Type", "application/x-www-form-urlencoded"),
            Body::Form(f) => req.form(f),
        };
        let resp = req.send().await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            tracing::warn!("Tradejini refused {}: unauthorized", path);
            return Err(session_expired());
        }
        let (_, v): (_, Value) = http::read_json("tradejini", resp).await?;
        match v.get("s").and_then(Value::as_str) {
            Some("ok") | Some("no-data") => Ok(v),
            _ => {
                let msg = mapping::envelope_error(&v);
                tracing::warn!(
                    status = status.as_u16(),
                    "Tradejini refused {}: {}",
                    path,
                    msg.as_deref().unwrap_or("no message")
                );
                Err(AppError::Broker(
                    msg.unwrap_or_else(|| "Tradejini refused the request.".into()),
                ))
            }
        }
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Tradejini session has expired. Log in to Tradejini again.".into())
}

#[async_trait]
impl Broker for TradejiniBroker {
    fn id(&self) -> &'static str {
        "tradejini"
    }

    fn name(&self) -> &'static str {
        "Tradejini"
    }

    fn logo(&self) -> &'static str {
        "/logos/tradejini.svg"
    }

    fn login_kind(&self) -> LoginKind {
        // CubePlus login PIN and the TOTP (web form fields `password`,
        // `twofa`; `twofatype` defaults to totp).
        LoginKind::DirectTotp {
            fields: &["password", "totp"],
        }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
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

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await.map_err(redact)
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (api_key, access_token) = Self::pair(auth)?;
        Ok(Box::new(streaming::TradejiniFeed::new(
            &self.stream_url,
            api_key,
            access_token,
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
