//! HDFC Securities InvestRight adapter (web `broker/hdfcsecurities/**`).
//!
//! * Sign-in: browser redirect to `/oapi/v1/login?api_key=..`, back with a
//!   request token exchanged at `/oapi/v1/access-token` (body `apiSecret`).
//! * The stored session is `api_key:access_token`. Every call sends the
//!   token as a bare `Authorization` header (no `Bearer`), the mandatory
//!   `User-Agent` and `api_key` as a query parameter. URLs carry the API
//!   key, so they are never logged.
//! * Instruments are addressed by three fields (parent exchange,
//!   `instrument_segment`, `security_id` = brsymbol); market data uses the
//!   segment code and `exch_security_id` (the master token).
//! * REST market data is `/fetch-ltp` only (LTP and previous close). OHLC,
//!   volume, OI and the five-level book come from a short-lived feed
//!   snapshot. There is no history and no margin API.

mod auth;
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
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::Value;

pub const BASE_URL: &str = "https://developer.hdfcsec.com";
/// Public security master (no auth).
pub const MASTER_URL: &str = "https://developer.hdfcsec.com/oapi/v1/security-master";
/// Market-data feed (protobuf `GenericDTO` frames).
pub const WS_URL: &str = "wss://developer.hdfcsec.com/wsapi/v1/session";
/// InvestRight rejects requests without a browser User-Agent (web
/// `api/baseurl.py` `USER_AGENT`).
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36";

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
];

/// InvestRight publishes no candle API; the web's `timeframe_map` is empty.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[];

/// Hosts the adapter talks to (tests point them at a local fake).
#[derive(Debug, Clone)]
pub struct Urls {
    pub base: String,
    pub master: String,
    pub ws: String,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            base: BASE_URL.into(),
            master: MASTER_URL.into(),
            ws: WS_URL.into(),
        }
    }
}

pub struct HdfcSecuritiesBroker {
    http: reqwest::Client,
    urls: Urls,
    symbols: SymbolResolver,
}

/// `api_key` and access token from the stored `api_key:access_token`.
pub(crate) struct Session<'a> {
    pub api_key: &'a str,
    pub token: &'a str,
}

impl HdfcSecuritiesBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, Urls::default())
    }

    /// Point the adapter at another REST host, master URL and feed URL.
    pub fn with_base_url(
        symbols: SymbolResolver,
        base: impl Into<String>,
        master: impl Into<String>,
        ws: impl Into<String>,
    ) -> Self {
        Self::with_urls(
            symbols,
            Urls {
                base: base.into(),
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
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn session(auth: &AuthToken) -> Result<Session<'_>> {
        auth.pair()
            .map(|(api_key, token)| Session { api_key, token })
            .ok_or_else(session_expired)
    }

    /// Send one authenticated InvestRight request.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<reqwest::Response> {
        let s = Self::session(auth)?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.urls.base, path))
            .query(&[("api_key", s.api_key)])
            .header("Authorization", s.token)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send().await.map_err(|e| {
            // reqwest errors embed the URL, which carries the API key.
            tracing::warn!(
                "HDFC Securities request to {} failed: {}",
                path,
                e.without_url()
            );
            AppError::Broker(
                "Could not reach HDFC Securities. Check your internet connection and try again."
                    .into(),
            )
        })
    }

    /// Decode a response; a refused session becomes `session_expired`.
    pub(crate) async fn decode(path: &str, resp: reqwest::Response) -> Result<(StatusCode, Value)> {
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            tracing::warn!(status = status.as_u16(), "HDFC Securities refused {}", path);
            return Err(session_expired());
        }
        http::read_json("hdfcsecurities", resp)
            .await
            .map_err(redact)
    }

    /// One authenticated call: HTTP status and decoded JSON body; envelope
    /// interpretation is left to the caller.
    pub(crate) async fn request(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let resp = self.send(method, path, auth, body).await?;
        Self::decode(path, resp).await
    }

    /// Like `request`, but insists on the `{"status": "success"}` envelope
    /// and returns its `data`.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, v) = self.request(method, path, auth, body).await?;
        if v.get("status").and_then(Value::as_str) == Some("success") {
            return Ok(v.get("data").cloned().unwrap_or(Value::Null));
        }
        tracing::warn!(
            status = status.as_u16(),
            "HDFC Securities refused {}: {}",
            path,
            error_message(&v)
        );
        Err(broker_error(&v))
    }
}

/// The message of an InvestRight error body (`message` or `error`).
pub(crate) fn error_message(v: &Value) -> String {
    ["message", "error"]
        .iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// InvestRight error body -> trader-facing error.
pub(crate) fn broker_error(v: &Value) -> AppError {
    let m = error_message(v);
    let lower = m.to_ascii_lowercase();
    if lower.contains("invalid credentials")
        || lower.contains("token") && (lower.contains("expired") || lower.contains("invalid"))
    {
        return session_expired();
    }
    if m.is_empty() {
        AppError::Broker("HDFC Securities refused the request.".into())
    } else {
        AppError::Broker(m)
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth(
        "Your HDFC Securities session has expired. Log in to HDFC Securities again.".into(),
    )
}

#[async_trait]
impl Broker for HdfcSecuritiesBroker {
    fn id(&self) -> &'static str {
        "hdfcsecurities"
    }

    fn name(&self) -> &'static str {
        "HDFC Securities"
    }

    fn logo(&self) -> &'static str {
        "/logos/hdfcsecurities.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "request_token",
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
        // web `get_history` raises: an empty answer would read as "no trades".
        Err(AppError::Unsupported("history"))
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = Self::session(auth)?;
        Ok(Box::new(streaming::HdfcSecuritiesFeed::new(
            &self.urls.ws,
            s.api_key,
            s.token,
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
