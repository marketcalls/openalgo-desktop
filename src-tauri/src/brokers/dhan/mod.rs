//! Dhan v2 adapter (web `broker/dhan/**`), also driving `dhan_sandbox`.
//!
//! Credentials, as on the web:
//! * API key: `client_id:::api_key` (Dhan client id, then the app id). A
//!   bare value is read as the client id.
//! * API secret: the Dhan app secret (consent flow). For `dhan_sandbox` it is
//!   the sandbox access token itself.
//!
//! Stored session token: `client_id:::access_token`. The web keeps the
//! access token alone and re-reads the client id from `BROKER_API_KEY` or the
//! `user_id` column on every call; the desktop adapters only ever see the
//! token, so the client id travels with it. A token without `:::` is read as
//! a bare access token with no client id (reads still work; orders, margin
//! and data calls then report the missing client id).
//!
//! Every call sends `access-token`; `client-id` is added where the web adds
//! it (place order, margin, GTT writes, all market data, and every call on
//! the sandbox). Market-data calls are paced like the web (`/v2/charts/*` at
//! 0.2 s, `/v2/marketfeed/*` at 1.1 s). The shared HTTP client speaks
//! HTTP/1.1 only, which is what Dhan's Forever Order endpoints need.

pub mod auth;
mod data;
mod funds;
mod gtt;
pub mod mapping;
pub mod master_contract;
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
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

pub use data::{
    adjust_dates, daily_timestamp, instrument_type as history_instrument_type, intraday_chunks,
    multiquote_bodies, parse_chart, quote_entry, to_depth, to_quote,
};
pub(crate) use data::{data_call, ist_today};
pub use funds::{funds_from_limit, parse_basket_margin, parse_single_margin};
pub use gtt::{map_gtt_book, modify_gtt_body, place_gtt_body};

pub const BASE_URL: &str = "https://api.dhan.co";
pub const AUTH_BASE_URL: &str = "https://auth.dhan.co";
pub const MASTER_URL: &str = "https://images.dhan.co/api-data/api-scrip-master.csv";

/// web `plugin.json` supported_exchanges (+ NCO, which the master emits and
/// the order maps accept).
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Bcd,
    Exchange::Mcx,
    Exchange::Nco,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map` (there is no `60m` key; the hour is `1h`).
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1"),
    ("5m", "5"),
    ("15m", "15"),
    ("25m", "25"),
    ("1h", "60"),
    ("D", "D"),
];

/// Live Dhan or the Dhan sandbox (web `broker/dhan_sandbox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Live,
    Sandbox,
}

/// Pacing category of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    /// Orders, books, funds, margin, GTT: no client-side pacing (web).
    Trade,
    /// `/v2/charts/*`: 5 per second.
    Data,
    /// `/v2/marketfeed/*`: 1 per second.
    Quote,
}

/// The parsed session token.
#[derive(Clone)]
pub struct DhanSession {
    pub client_id: Option<String>,
    pub access_token: String,
}

impl std::fmt::Debug for DhanSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhanSession")
            .field("client_id", &self.client_id)
            .field("access_token", &"[REDACTED]")
            .finish()
    }
}

impl DhanSession {
    /// Parse a stored token (`client_id:::access_token`, or a bare token).
    pub fn parse(auth: &AuthToken) -> Result<Self> {
        let raw = auth.raw().trim();
        let (cid, token) = match raw.split_once(":::") {
            Some((c, t)) => (Some(c.trim()), t.trim()),
            None => (None, raw),
        };
        if token.is_empty() {
            return Err(session_expired());
        }
        Ok(Self {
            client_id: cid.filter(|c| !c.is_empty()).map(str::to_string),
            access_token: token.to_string(),
        })
    }

    /// The client id, required by this call.
    pub fn require_client_id(&self) -> Result<&str> {
        self.client_id.as_deref().ok_or_else(|| {
            AppError::Validation(
                "Your Dhan client ID is missing. Enter the API key as client_id:::api_key in Profile, Broker Configuration, then log in to Dhan again."
                    .into(),
            )
        })
    }
}

/// `client_id:::api_key` -> (client id, app id). A bare value is the client
/// id (web `data.py` fallback).
pub fn split_api_key(api_key: &str) -> (Option<String>, Option<String>) {
    let key = api_key.trim();
    match key.split_once(":::") {
        Some((c, k)) => (
            Some(c.trim().to_string()).filter(|s| !s.is_empty()),
            Some(k.trim().to_string()).filter(|s| !s.is_empty()),
        ),
        None if key.is_empty() => (None, None),
        None => (Some(key.to_string()), None),
    }
}

pub struct DhanBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) variant: Variant,
    pub(crate) base_url: String,
    pub(crate) auth_base_url: String,
    pub(crate) master_url: String,
    pub(crate) symbols: SymbolResolver,
    data_pacer: Pacer,
    quote_pacer: Pacer,
    /// Base delay of the web's retry ladders (805 and history chunks back off
    /// 2, 4, 8 s; the sandbox 429 ladder is a quarter of that). Tests shrink
    /// it so a retry path runs in milliseconds.
    pub(crate) retry_base: Duration,
}

impl DhanBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::build(Variant::Live, symbols, BASE_URL, AUTH_BASE_URL, MASTER_URL)
    }

    /// The Dhan sandbox (`dhan_sandbox`).
    pub fn sandbox(symbols: SymbolResolver) -> Self {
        Self::build(
            Variant::Sandbox,
            symbols,
            crate::brokers::dhan_sandbox::BASE_URL,
            AUTH_BASE_URL,
            MASTER_URL,
        )
    }

    /// Point every host (REST, auth, scrip master) at a local fake.
    pub fn with_urls(
        variant: Variant,
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        auth_base_url: impl Into<String>,
        master_url: impl Into<String>,
    ) -> Self {
        Self::build(variant, symbols, base_url, auth_base_url, master_url)
    }

    fn build(
        variant: Variant,
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        auth_base_url: impl Into<String>,
        master_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            variant,
            base_url: base_url.into(),
            auth_base_url: auth_base_url.into(),
            master_url: master_url.into(),
            symbols,
            data_pacer: Pacer::with_interval(Duration::from_millis(200)),
            quote_pacer: Pacer::with_interval(Duration::from_millis(1100)),
            retry_base: Duration::from_secs(2),
        }
    }

    /// Shrink the retry back-off (tests).
    pub fn with_retry_base(mut self, base: Duration) -> Self {
        self.retry_base = base;
        self
    }

    /// Replace the market-data pacing (tests run a local fake at full
    /// speed; the defaults are Dhan's 0.2 s and 1.1 s).
    pub fn with_pacing(mut self, data: Duration, quote: Duration) -> Self {
        self.data_pacer = Pacer::with_interval(data);
        self.quote_pacer = Pacer::with_interval(quote);
        self
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn is_sandbox(&self) -> bool {
        self.variant == Variant::Sandbox
    }

    fn label(&self) -> &'static str {
        match self.variant {
            Variant::Live => "dhan",
            Variant::Sandbox => "dhan_sandbox",
        }
    }

    /// One Dhan REST call. `client_id` is sent as the `client-id` header when
    /// given (and always on the sandbox when the session has one). Returns
    /// the HTTP status and the decoded body; a body that is not JSON becomes
    /// a trader-facing error.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        session: &DhanSession,
        body: Option<&Value>,
        client_id: Option<&str>,
        category: Category,
    ) -> Result<(StatusCode, Value)> {
        if self.variant == Variant::Live {
            match category {
                Category::Data => self.data_pacer.acquire().await,
                Category::Quote => self.quote_pacer.acquire().await,
                Category::Trade => {}
            }
        }
        let cid = client_id.or(if self.is_sandbox() {
            session.client_id.as_deref()
        } else {
            None
        });
        let url = format!("{}{}", self.base_url, path);
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("access-token", &session.access_token)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json");
            if let Some(c) = cid {
                req = req.header("client-id", c);
            }
            if let Some(b) = body {
                req = req.body(b.to_string());
            }
            let resp = req.send().await?;
            // Sandbox: no pacing, a 429 retries three times (0.5, 1, 2 s).
            if self.is_sandbox() && resp.status() == StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                tokio::time::sleep(self.retry_base / 4 * (1 << attempt)).await;
                attempt += 1;
                continue;
            }
            return http::read_json::<Value>(self.label(), resp).await;
        }
    }

    /// Like `send`, but a Dhan error body (any status) becomes an error.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        session: &DhanSession,
        body: Option<&Value>,
        client_id: Option<&str>,
        category: Category,
    ) -> Result<Value> {
        let (status, v) = self
            .send(method, path, session, body, client_id, category)
            .await?;
        if let Some(e) = dhan_error(&v) {
            tracing::warn!(
                broker = self.label(),
                status = status.as_u16(),
                "Dhan refused {}: {}",
                path.split('?').next().unwrap_or(""),
                e.code()
            );
            return Err(e);
        }
        if !status.is_success() {
            tracing::warn!(
                broker = self.label(),
                status = status.as_u16(),
                "Dhan answered {} with an error status",
                path
            );
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(session_expired());
            }
            return Err(AppError::Broker(
                "Dhan refused the request. Try again shortly.".into(),
            ));
        }
        Ok(v)
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Dhan session has expired. Log in to Dhan again.".into())
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Dhan error envelopes (web `order_api.get_api_response`):
/// `{"status":"failed"|"error","data":{"<code>":"<message>"}}` or
/// `{"errorType","errorCode","errorMessage"}`. `None` when the body is not an
/// error.
pub fn dhan_error(v: &Value) -> Option<AppError> {
    let obj = v.as_object()?;
    let status = text(obj.get("status")).to_ascii_lowercase();
    let mut code = text(obj.get("errorCode"));
    let mut message = text(obj.get("errorMessage"));
    let error_type = text(obj.get("errorType"));
    if status == "failed" || status == "error" {
        if let Some(Value::Object(d)) = obj.get("data") {
            if let Some((k, m)) = d.iter().next() {
                code = k.clone();
                message = text(Some(m));
            }
        }
        if message.is_empty() {
            message = text(obj.get("errors")).trim_matches('"').to_string();
        }
    } else if error_type.is_empty() && code.is_empty() {
        return None;
    }
    Some(error_for(&error_type, &code, &message))
}

/// Dhan error code -> trader-facing error (web `data.py` error mapping).
pub fn error_for(error_type: &str, code: &str, message: &str) -> AppError {
    match (error_type, code) {
        ("Invalid_Authentication", _) | (_, "DH-901") | (_, "401") | (_, "807") | (_, "808")
        | (_, "809") => session_expired(),
        (_, "805") | (_, "DH-904") => AppError::Broker(
            "Dhan is limiting requests right now. Wait a moment and try again.".into(),
        ),
        (_, "806") => AppError::Broker(
            "Dhan market data is not active on your account. Subscribe to Dhan's Data APIs, then try again."
                .into(),
        ),
        (_, "810") => AppError::Auth(
            "Dhan did not accept your client ID. Check the API key (client_id:::api_key) in Profile, Broker Configuration."
                .into(),
        ),
        (_, "820") | (_, "821") => AppError::Broker(
            "Dhan market data subscription is required for this. Subscribe to Dhan's Data APIs, then try again."
                .into(),
        ),
        _ if message.is_empty() => AppError::Broker("Dhan refused the request.".into()),
        _ => AppError::Broker(format!("Dhan: {}", message)),
    }
}

#[async_trait]
impl Broker for DhanBroker {
    fn id(&self) -> &'static str {
        self.label()
    }

    fn name(&self) -> &'static str {
        match self.variant {
            Variant::Live => "Dhan",
            Variant::Sandbox => "Dhan Sandbox",
        }
    }

    fn logo(&self) -> &'static str {
        "/logos/dhan.svg"
    }

    fn login_kind(&self) -> LoginKind {
        match self.variant {
            // Consent redirect; the callback carries `tokenId`. A pasted
            // access token goes through the same `authenticate`.
            Variant::Live => LoginKind::Redirect { param: "tokenId" },
            // web: the sandbox access token is the API secret.
            Variant::Sandbox => LoginKind::AccessToken,
        }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        match self.variant {
            Variant::Live => Capabilities {
                history: true,
                multiquotes_batch: true,
                margin: true,
                gtt: true,
                streaming: true,
                order_feed: true,
                depth_levels: &[5, 20],
            },
            Variant::Sandbox => Capabilities {
                history: true,
                multiquotes_batch: false,
                margin: true,
                gtt: false,
                streaming: false,
                order_feed: false,
                depth_levels: &[5],
            },
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
        if self.is_sandbox() {
            return Err(AppError::Unsupported("gtt"));
        }
        gtt::place_gtt(self, auth, req).await
    }

    async fn modify_gtt(
        &self,
        auth: &AuthToken,
        trigger_id: &str,
        req: &GttRequest,
    ) -> Result<GttResponse> {
        if self.is_sandbox() {
            return Err(AppError::Unsupported("gtt"));
        }
        gtt::modify_gtt(self, auth, trigger_id, req).await
    }

    async fn cancel_gtt(&self, auth: &AuthToken, trigger_id: &str) -> Result<GttResponse> {
        if self.is_sandbox() {
            return Err(AppError::Unsupported("gtt"));
        }
        gtt::cancel_gtt(self, auth, trigger_id).await
    }

    async fn get_gtt_book(&self, auth: &AuthToken, include_history: bool) -> Result<Vec<GttOrder>> {
        if self.is_sandbox() {
            return Err(AppError::Unsupported("gtt"));
        }
        gtt::get_gtt_book(self, auth, include_history).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if self.is_sandbox() {
            // web: the sandbox feed is a synthetic tick generator, not Dhan.
            return Err(AppError::Unsupported("streaming"));
        }
        let s = DhanSession::parse(auth)?;
        let cid = s.require_client_id()?.to_string();
        Ok(Box::new(streaming::DhanFeed::new(
            &s.access_token,
            &cid,
            self.symbols.clone(),
        )))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }

    fn create_depth_feed(&self, auth: &AuthToken, levels: u8) -> Result<Box<dyn BrokerFeed>> {
        if levels != 20 {
            return Err(AppError::Unsupported("depth_feed"));
        }
        self.create_depth20_feed(auth)
    }

    /// 20-level books stream for NSE and NFO only (web Dhan 20-depth).
    fn feed_depth_levels(&self, exchange: &str) -> Vec<u8> {
        if !self.is_sandbox() && matches!(exchange, "NSE" | "NFO") {
            vec![5, 20]
        } else {
            vec![5]
        }
    }

    async fn begin_login(&self, credentials: &BrokerCredentials) -> Result<Option<String>> {
        if self.is_sandbox() {
            return Ok(None);
        }
        auth::login_url(self, credentials).await.map(Some)
    }
}

impl DhanBroker {
    /// The 20-level depth socket (NSE and NFO only), a second connection on
    /// Dhan, served through `Broker::create_depth_feed(_, 20)`.
    pub fn create_depth20_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if self.is_sandbox() {
            return Err(AppError::Unsupported("streaming"));
        }
        let s = DhanSession::parse(auth)?;
        let cid = s.require_client_id()?.to_string();
        Ok(Box::new(streaming::Dhan20DepthFeed::new(
            &s.access_token,
            &cid,
            self.symbols.clone(),
        )))
    }

    /// The order-update socket (`wss://api-order-update.dhan.co`), served
    /// through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if self.is_sandbox() {
            return Err(AppError::Unsupported("streaming"));
        }
        let s = DhanSession::parse(auth)?;
        let cid = s.require_client_id()?.to_string();
        Ok(Box::new(streaming::DhanOrderFeed::new(
            &s.access_token,
            &cid,
            self.symbols.clone(),
        )))
    }
}
