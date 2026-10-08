//! Motilal Oswal (MOFSL) adapter (web `broker/motilal/**`).
//!
//! Credentials, as on the web (`api/baseurl.py`):
//! * API key: the App API Key, sent as the `ApiKey` header on every call
//!   and used as the salt of the login hash `sha256(password + apikey)`.
//! * API secret (optional): the App API Secret, sent as `apisecretkey`; it
//!   enables the `getaccesstoken` step, whose token rides as `accesstoken`.
//! * The client code is not a credential: it is the user id typed on the
//!   login form, and travels as `vendorinfo`, as the dealer `clientcode`
//!   and in the market-data login packet.
//!
//! Sign-in is one form (`userid`, `password`, `dob`, optional `totp`). The
//! stored session must be usable on its own (session resume calls
//! `get_funds` with nothing but the stored string), so it carries
//! everything the headers need:
//! `AuthToken:::accesstoken:::clientcode:::apikey:::apisecret` (empty parts
//! allowed for the optional ones). It is encrypted at rest and never logged.
//!
//! Every REST call is a POST with the web's common header set; business
//! failures come back as HTTP 200 with `status: FAILURE` and an
//! `errorcode` (`MO8001` invalid token and friends).

pub mod auth;
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
use crate::brokers::common::redact;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use crate::security::Secret;
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub use data::{history_from_quote, index_quote_from_rows, quote_from_ltp_data};
pub use funds::funds_from_rows;

/// web `PRODUCTION_HOST`.
pub const BASE_URL: &str = "https://openapi.motilaloswal.com";
/// web `PRODUCTION_WS_FEED` (binary broadcast feed).
pub const FEED_WS_URL: &str = "wss://ws1feed.motilaloswal.com/jwebsocket/jwebsocket";
/// web `PRODUCTION_WS_TRADE` (JSON order stream).
pub const TRADE_WS_URL: &str = "wss://openapi.motilaloswal.com/ws";

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

/// web `BrokerData.timeframe_map`: Motilal has no historical API, only
/// today's daily bar from the live quote.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[("D", "D")];

/// web endpoint table (`api/baseurl.py` `ENDPOINTS`).
pub mod paths {
    pub const AUTH_DIRECT: &str = "/rest/login/v7/authdirectapi";
    pub const ACCESS_TOKEN: &str = "/rest/login/v1/getaccesstoken";
    pub const PLACE: &str = "/rest/trans/v2/placeorder";
    pub const MODIFY: &str = "/rest/trans/v5/modifyorder";
    pub const CANCEL: &str = "/rest/trans/v2/cancelorder";
    pub const ORDER_BOOK: &str = "/rest/book/v5/getorderbook";
    pub const TRADE_BOOK: &str = "/rest/book/v4/gettradebook";
    pub const ORDER_DETAIL: &str = "/rest/book/v5/getorderdetailbyuniqueorderid";
    pub const POSITIONS: &str = "/rest/book/v4/getposition";
    pub const HOLDINGS: &str = "/rest/report/v3/getdpholding";
    pub const MARGIN_DETAIL: &str = "/rest/report/v3/getreportmargindetail";
    pub const LTP: &str = "/rest/report/v3/getltpdata";
    pub const INDEX_LTP: &str = "/rest/report/v3/getindexltpdata";
    pub const SCRIP_MASTER: &str = "/getscripmastercsv";
    pub const INDEX_MASTER: &str = "/getindexdatacsv";
}

/// The parsed stored session. `Debug` is redacted.
#[derive(Clone)]
pub struct MotilalSession {
    pub auth_token: Secret,
    pub access_token: Option<Secret>,
    pub client_code: String,
    pub api_key: Secret,
    pub api_secret: Option<Secret>,
}

impl std::fmt::Debug for MotilalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MotilalSession")
            .field("client_code", &self.client_code)
            .finish_non_exhaustive()
    }
}

fn opt_secret(s: &str) -> Option<Secret> {
    let s = s.trim();
    (!s.is_empty()).then(|| Secret::new(s))
}

impl MotilalSession {
    /// `AuthToken:::accesstoken:::clientcode:::apikey[:::apisecret]`.
    pub fn parse(auth: &AuthToken) -> Result<Self> {
        let parts: Vec<&str> = auth.raw().split(":::").collect();
        if parts.len() < 4 {
            return Err(session_expired());
        }
        let auth_token = parts[0].trim();
        let api_key = parts[3].trim();
        if auth_token.is_empty() || api_key.is_empty() {
            return Err(session_expired());
        }
        let mut client_code = parts[2].trim().to_string();
        if client_code.is_empty() {
            client_code = auth.user_id().unwrap_or_default().to_string();
        }
        Ok(Self {
            auth_token: Secret::new(auth_token),
            access_token: opt_secret(parts[1]),
            client_code,
            api_key: Secret::new(api_key),
            api_secret: parts.get(4).and_then(|s| opt_secret(s)),
        })
    }

    pub fn compose(&self) -> String {
        format!(
            "{}:::{}:::{}:::{}:::{}",
            self.auth_token.expose(),
            self.access_token.as_ref().map(|s| s.expose()).unwrap_or(""),
            self.client_code,
            self.api_key.expose(),
            self.api_secret.as_ref().map(|s| s.expose()).unwrap_or("")
        )
    }
}

/// How long the one-shot market-data socket waits (web `data.py`).
#[derive(Debug, Clone, Copy)]
pub struct FeedTimings {
    /// Connect plus the first (login) reply (web: 10 s).
    pub connect: Duration,
    /// Depth wait (web `_DEPTH_WAIT_SECONDS`).
    pub depth_wait: Duration,
    /// Index depth wait (web: fixed 3 s sleep).
    pub index_wait: Duration,
    /// Multiquote wait: `min(max(n * per_symbol, min), max)` (web 0.1 s, 2 s, 5 s).
    pub multi_per_symbol: Duration,
    pub multi_min: Duration,
    pub multi_max: Duration,
    /// Pause between multiquote batches (web `RATE_LIMIT_DELAY`).
    pub batch_pause: Duration,
}

impl Default for FeedTimings {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            depth_wait: Duration::from_secs(3),
            index_wait: Duration::from_secs(3),
            multi_per_symbol: Duration::from_millis(100),
            multi_min: Duration::from_secs(2),
            multi_max: Duration::from_secs(5),
            batch_pause: Duration::from_millis(100),
        }
    }
}

pub struct MotilalBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) feed_ws_url: String,
    pub(crate) trade_ws_url: String,
    pub(crate) symbols: SymbolResolver,
    /// web `_dealer_mode`: latched the first time Motilal answers MO1062.
    pub(crate) dealer_mode: AtomicBool,
    /// web `_index_exchange_field`: the spelling getindexltpdata accepts.
    pub(crate) index_field: Mutex<Option<&'static str>>,
    /// web `_FEED_WAITERS_MAX`: at most eight one-shot feed reads at once.
    pub(crate) feed_gate: Arc<Semaphore>,
    pub(crate) timings: FeedTimings,
}

impl MotilalBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, FEED_WS_URL, TRADE_WS_URL)
    }

    /// Point every host at a local fake (tests).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        feed_ws_url: impl Into<String>,
        trade_ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            feed_ws_url: feed_ws_url.into(),
            trade_ws_url: trade_ws_url.into(),
            symbols,
            dealer_mode: AtomicBool::new(false),
            index_field: Mutex::new(None),
            feed_gate: Arc::new(Semaphore::new(8)),
            timings: FeedTimings::default(),
        }
    }

    /// Replace the feed waits (tests).
    pub fn with_timings(mut self, timings: FeedTimings) -> Self {
        self.timings = timings;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    pub(crate) fn dealer(&self) -> bool {
        self.dealer_mode.load(Ordering::Relaxed)
    }

    /// The web's common header set (`get_common_headers`).
    pub(crate) fn headers(
        &self,
        rb: reqwest::RequestBuilder,
        api_key: &str,
        api_secret: Option<&str>,
        vendor_info: &str,
        auth: Option<(&str, Option<&str>)>,
    ) -> reqwest::RequestBuilder {
        let mut rb = rb
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("User-Agent", "MOSL/V.1.1.0")
            .header("ApiKey", api_key)
            .header("ClientLocalIp", "127.0.0.1")
            .header("ClientPublicIp", "127.0.0.1")
            .header("MacAddress", "00:00:00:00:00:00")
            .header("SourceId", "WEB")
            .header("vendorinfo", vendor_info)
            .header("osname", "Windows 10")
            .header("osversion", "10.0.19041")
            .header("devicemodel", "AHV")
            .header("manufacturer", "DELL")
            .header("productname", "OpenAlgo")
            .header("productversion", "1.0.0")
            .header("browsername", "Chrome")
            .header("browserversion", "120.0");
        if let Some(secret) = api_secret.filter(|s| !s.is_empty()) {
            rb = rb.header("apisecretkey", secret);
        }
        if let Some((token, access)) = auth {
            rb = rb.header("Authorization", token);
            if let Some(a) = access.filter(|a| !a.is_empty()) {
                rb = rb.header("accesstoken", a);
            }
        }
        rb
    }

    /// POST an authenticated call and return the raw envelope (any status).
    pub(crate) async fn post_raw(
        &self,
        s: &MotilalSession,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value)> {
        let rb = self.http.post(format!("{}{}", self.base_url, path));
        let rb = self.headers(
            rb,
            s.api_key.expose(),
            s.api_secret.as_ref().map(|x| x.expose()),
            &s.client_code,
            Some((
                s.auth_token.expose(),
                s.access_token.as_ref().map(|x| x.expose()),
            )),
        );
        let rb = match body {
            Some(b) => rb.body(b.to_string()),
            None => rb,
        };
        let resp = rb.send().await.map_err(redact::http)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(redact::http)?;
        if bytes.is_empty() {
            tracing::warn!(
                status = status.as_u16(),
                "Motilal Oswal sent an empty response to {}",
                path
            );
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(session_expired());
            }
            return Err(AppError::Broker(
                "Motilal Oswal sent an empty response. Try again shortly.".into(),
            ));
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => Ok((status, v)),
            Err(_) => {
                tracing::warn!(
                    status = status.as_u16(),
                    "Motilal Oswal sent a response that is not JSON for {}",
                    path
                );
                if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    return Err(session_expired());
                }
                Err(AppError::Broker(
                    "Motilal Oswal sent a response OpenAlgo could not read. Try again shortly."
                        .into(),
                ))
            }
        }
    }

    /// POST an authenticated call; anything but `status: SUCCESS` is an error.
    pub(crate) async fn post(
        &self,
        s: &MotilalSession,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, v) = self.post_raw(s, path, body).await?;
        if is_success(&v) {
            return Ok(v);
        }
        Err(motilal_error(
            status,
            &v,
            "Motilal Oswal could not complete the request.",
        ))
    }
}

/// web: `status == "SUCCESS"`.
pub fn is_success(v: &Value) -> bool {
    v.get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("SUCCESS"))
}

/// Error code of an envelope (`errorcode`, sometimes `errorCode`).
pub fn error_code(v: &Value) -> String {
    mapping::vs(v, "errorcode")
        .or_else(|| mapping::vs(v, "errorCode"))
        .unwrap_or_default()
        .to_ascii_uppercase()
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Motilal Oswal session has expired. Log in to Motilal Oswal again.".into())
}

/// The trader-facing error for a FAILURE envelope (web doc 31 codes).
pub fn motilal_error(status: StatusCode, v: &Value, fallback: &str) -> AppError {
    let code = error_code(v);
    let msg = mapping::vs(v, "message").unwrap_or_default();
    tracing::warn!(
        status = status.as_u16(),
        code = %code,
        "Motilal Oswal refused the request: {}",
        msg
    );
    let lower = msg.to_ascii_lowercase();
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || matches!(code.as_str(), "MO8001" | "MO8002" | "MO1001")
        || lower.contains("invalid token")
        || lower.contains("token expired")
        || lower.contains("session expired")
    {
        return session_expired();
    }
    match code.as_str() {
        "MO2035" => AppError::Broker(
            "Motilal Oswal does not accept orders from this computer's internet address. Register it as a static IP with Motilal Oswal and try again."
                .into(),
        ),
        "MO2012" => AppError::Broker(
            "Motilal Oswal did not recognise the client code for this session. Log in to Motilal Oswal again."
                .into(),
        ),
        _ if msg.is_empty() => AppError::Broker(fallback.to_string()),
        _ => AppError::Broker(format!("Motilal Oswal: {}", msg)),
    }
}

#[async_trait]
impl Broker for MotilalBroker {
    fn id(&self) -> &'static str {
        "motilal"
    }

    fn name(&self) -> &'static str {
        "Motilal Oswal"
    }

    fn logo(&self) -> &'static str {
        "/logos/motilal.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["userid", "password", "dob", "totp"],
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
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn requires_totp(&self) -> bool {
        // TOTP is optional on the web form (blank asks Motilal for an OTP,
        // which OpenAlgo does not support yet).
        false
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

    async fn calculate_margin(
        &self,
        _auth: &AuthToken,
        _legs: &[MarginLeg],
    ) -> Result<MarginResult> {
        // web margin_api.py: Motilal publishes no margin calculator (501).
        Err(AppError::Unsupported("margin"))
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

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = MotilalSession::parse(auth)?;
        Ok(Box::new(streaming::MotilalFeed::new(
            &self.feed_ws_url,
            &s.client_code,
        )))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Socket(self.order_socket(auth)?))
    }
}

impl MotilalBroker {
    /// The order-update socket (web `streaming/motilal_order_adapter.py`),
    /// served through `Broker::create_order_feed`.
    pub fn order_socket(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = MotilalSession::parse(auth)?;
        if s.client_code.is_empty() {
            return Err(AppError::Auth(
                "The Motilal Oswal client code for this session is missing. Log in to Motilal Oswal again."
                    .into(),
            ));
        }
        Ok(Box::new(streaming::MotilalOrderFeed::new(
            &self.trade_ws_url,
            &s,
            self.symbols.clone(),
        )))
    }
}
