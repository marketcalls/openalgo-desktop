//! Groww Trade API adapter (web `broker/groww/**`).
//!
//! * REST base `https://api.groww.in`, every call `Authorization: Bearer
//!   <token>` and `X-API-VERSION: 1.0`; responses are `{"status":
//!   "SUCCESS", "payload": {...}}` or `{"status": "FAILURE", "error":
//!   {"code", "message"}}`.
//! * Every call is paced per Groww API type and retried on HTTP 429
//!   (`rate_limiter.rs`, web #2194).
//! * The stored session token is the raw Groww access token (no prefix).
//! * Market data streams over NATS-on-WebSocket with protobuf payloads
//!   (`streaming.rs`, on the shared feed manager); order updates are a REST
//!   poll of the order book (`common::order_poll`), as on the web.

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
pub mod nkeys;
mod orders;
pub mod proto;
pub mod rate_limiter;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::order_poll;
use crate::brokers::common::redact::url_safe_error;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed, OrderUpdate};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const BASE_URL: &str = "https://api.groww.in";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map`: OpenAlgo interval -> Groww
/// `candle_interval` (Groww backtesting "Get Historical Candle Data").
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1minute"),
    ("2m", "2minute"),
    ("3m", "3minute"),
    ("5m", "5minute"),
    ("10m", "10minute"),
    ("15m", "15minute"),
    ("30m", "30minute"),
    ("1h", "1hour"),
    ("4h", "4hour"),
    ("D", "1day"),
    ("W", "1week"),
];

pub(crate) use rate_limiter::ApiType as Category;
use rate_limiter::{paced, Attempt, GrowwLimiter};

/// Everything a call needs; cheap to clone so the order poller task can own
/// one.
#[derive(Clone)]
pub(crate) struct GrowwCore {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: Arc<str>,
    pub(crate) symbols: SymbolResolver,
    limiter: Arc<GrowwLimiter>,
}

/// A Groww response: HTTP status and the parsed body (`Null` when the body
/// was not JSON).
pub(crate) struct Reply {
    pub status: reqwest::StatusCode,
    pub body: Value,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
            && self.body.get("status").and_then(Value::as_str) == Some("SUCCESS")
    }

    pub fn payload(&self) -> &Value {
        self.body.get("payload").unwrap_or(&Value::Null)
    }

    /// Groww error text from `error.message`, `message` or
    /// `errors[0].message`.
    pub fn error_message(&self) -> String {
        error_message(&self.body)
    }
}

pub(crate) fn error_message(body: &Value) -> String {
    fn s(v: Option<&Value>) -> Option<&str> {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
    s(body.pointer("/error/message"))
        .or_else(|| s(body.get("message")))
        .or_else(|| s(body.pointer("/errors/0/message")))
        .or_else(|| s(body.get("error")))
        .unwrap_or("")
        .to_string()
}

impl GrowwCore {
    pub(crate) fn new(symbols: SymbolResolver, base_url: &str) -> Self {
        Self {
            http: http::client(),
            base_url: Arc::from(base_url.trim_end_matches('/')),
            symbols,
            limiter: Arc::new(GrowwLimiter::default()),
        }
    }

    /// One Groww call, paced by its API type and retried on HTTP 429; any
    /// other HTTP status comes back as a `Reply`, except 401 (and a 403
    /// about the token), which mean the session is gone. Any other 403
    /// keeps Groww's reason, as the web shows it.
    pub(crate) async fn send(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
    ) -> Result<Reply> {
        let token = auth.raw();
        if token.trim().is_empty() {
            return Err(session_expired());
        }
        let r = self
            .request(
                method,
                path_and_query,
                &format!("Bearer {}", token),
                body,
                category,
            )
            .await?;
        let path = path_and_query.split('?').next().unwrap_or("");
        if r.status == reqwest::StatusCode::UNAUTHORIZED
            || (r.status == reqwest::StatusCode::FORBIDDEN && refuses_session(&r))
        {
            tracing::warn!(
                status = r.status.as_u16(),
                "Groww refused the session on {}",
                path
            );
            return Err(session_expired());
        }
        if !r.status.is_success() {
            tracing::warn!(
                status = r.status.as_u16(),
                "Groww refused {}: {}",
                path,
                r.error_message()
            );
        }
        Ok(r)
    }

    /// The paced, retried HTTP exchange behind every Groww call, with the
    /// headers Groww requires (01-introduction).
    pub(crate) async fn request(
        &self,
        method: Method,
        path_and_query: &str,
        authorization: &str,
        body: Option<&Value>,
        category: Category,
    ) -> Result<Reply> {
        let url = format!("{}{}", self.base_url, path_and_query);
        paced(&self.limiter, category, || {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .timeout(http::REQUEST_TIMEOUT)
                .header("Authorization", authorization)
                .header("Accept", "application/json")
                .header("X-API-VERSION", "1.0");
            if let Some(b) = body {
                req = req.json(b);
            }
            async move {
                let resp = req.send().await?;
                let status = resp.status();
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let bytes = resp.bytes().await?;
                let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                Ok(Attempt {
                    status: status.as_u16(),
                    retry_after,
                    value: Reply { status, body },
                })
            }
        })
        .await
    }

    /// Like `send`, but only a `SUCCESS` envelope is a success; its payload
    /// is returned.
    pub(crate) async fn call(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
        category: Category,
    ) -> Result<Value> {
        let r = self
            .send(method, path_and_query, auth, body, category)
            .await?;
        if r.is_success() {
            return Ok(r.payload().clone());
        }
        Err(groww_error(&r))
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Groww session has expired. Log in to Groww again.".into())
}

/// Whether a 403 is about the session (no reason given, or one naming the
/// token or session) rather than a refusal of this request with its own
/// reason (an IP or permission check), which keeps Groww's words.
fn refuses_session(r: &Reply) -> bool {
    let m = r.error_message().to_ascii_lowercase();
    m.is_empty()
        || [
            "token",
            "session",
            "unauthori",
            "authenticat",
            "expired",
            "login",
        ]
        .iter()
        .any(|w| m.contains(w))
}

/// A Groww call that failed in transit -> trader-facing error. For an
/// order call the order may have reached Groww, so the trader is told to
/// check the order book before sending it again (web
/// `direct_place_order_api`). Other errors (a refused session, OpenAlgo's
/// own pacing refusal) pass through unchanged.
pub(crate) fn in_transit(e: AppError, what: &str) -> AppError {
    match e {
        AppError::Http(ref err) => {
            tracing::warn!(
                "Groww {} request failed in transit: {}",
                what,
                url_safe_error(err)
            );
            AppError::Broker(match what {
                "place" | "modify" | "cancel" => format!(
                    "Could not reach Groww to {} the order. Check the order book before retrying.",
                    what
                ),
                "login" => {
                    "Could not reach Groww to log in. Check your connection and try again.".into()
                }
                _ => "Could not reach Groww. Check your connection and try again.".into(),
            })
        }
        other => other,
    }
}

/// A non-success Groww reply -> trader-facing error.
pub(crate) fn groww_error(r: &Reply) -> AppError {
    if r.status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return AppError::Broker(
            "Groww is limiting requests right now. Wait a moment and try again.".into(),
        );
    }
    if r.status.is_server_error() {
        return AppError::Broker(
            "Groww's servers are not responding normally. Try again shortly.".into(),
        );
    }
    let m = r.error_message();
    if m.is_empty() {
        AppError::Broker("Groww refused the request.".into())
    } else {
        AppError::Broker(m)
    }
}

pub struct GrowwBroker {
    core: GrowwCore,
    feed: streaming::FeedEndpoints,
    poller: parking_lot::Mutex<Option<order_poll::OrderPoller>>,
}

impl GrowwBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, BASE_URL)
    }

    /// Point the adapter at another host (tests run a local fake Groww).
    pub fn with_base_url(symbols: SymbolResolver, base_url: impl AsRef<str>) -> Self {
        Self {
            core: GrowwCore::new(symbols, base_url.as_ref()),
            feed: streaming::FeedEndpoints::default(),
            poller: parking_lot::Mutex::new(None),
        }
    }

    /// Override the socket-token and NATS socket addresses (tests).
    pub fn with_feed_endpoints(mut self, socket_token_url: &str, ws_url: &str) -> Self {
        self.feed = streaming::FeedEndpoints {
            socket_token_url: socket_token_url.to_string(),
            ws_url: ws_url.to_string(),
        };
        self
    }

    /// Start polling the order book for order updates (web
    /// `PollingOrderUpdateAdapter`; Groww has no order socket). Replaces a
    /// running poller. The interval is clamped to 1..=60 s.
    pub fn start_order_updates(
        &self,
        auth: &AuthToken,
        interval: Duration,
    ) -> Result<mpsc::Receiver<OrderUpdate>> {
        let core = self.core.clone();
        let auth = auth.clone();
        let (poller, rx) = order_poll::OrderPoller::start(
            "groww",
            interval,
            move || {
                let (core, auth) = (core.clone(), auth.clone());
                async move { orders::get_order_book(&core, &auth).await }
            },
            order_poll::session_ends_on_auth,
        )?;
        // Dropping the old poller aborts its task.
        *self.poller.lock() = Some(poller);
        Ok(rx)
    }

    /// Stop the order-update poller (`Broker::on_logout` calls this on
    /// broker logout, session revocation and app shutdown; dropping the
    /// broker also stops it).
    pub fn stop_order_updates(&self) {
        if let Some(p) = self.poller.lock().take() {
            p.stop();
        }
    }

    pub fn order_updates_running(&self) -> bool {
        self.poller.lock().as_ref().is_some_and(|p| p.is_running())
    }
}

#[async_trait]
impl Broker for GrowwBroker {
    fn id(&self) -> &'static str {
        "groww"
    }

    fn name(&self) -> &'static str {
        "Groww"
    }

    fn logo(&self) -> &'static str {
        "/logos/groww.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["totp", "password"],
        }
    }

    /// TOTP is one of three ways in (approval checksum and a pasted access
    /// token need none), so the form must not insist on it.
    fn requires_totp(&self) -> bool {
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
            // Order updates come from the REST poller, not a socket.
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.core.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        auth::authenticate(&self.core, credentials).await
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        orders::place_order(&self.core, auth, order).await
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        orders::modify_order(&self.core, auth, order).await
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        orders::cancel_order(&self.core, auth, order_id, None).await
    }

    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        orders::cancel_all_orders(&self.core, auth).await
    }

    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        orders::close_all_positions(&self.core, auth).await
    }

    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        orders::get_open_position(&self.core, auth, symbol, exchange, product).await
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        orders::get_order_book(&self.core, auth).await
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        orders::get_trade_book(&self.core, auth).await
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        orders::get_positions(&self.core, auth).await
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        orders::get_holdings(&self.core, auth).await
    }

    /// Holdings valued at the live price where Groww priced them, else at
    /// the average (web `calculate_portfolio_statistics`).
    async fn get_holdings_with_totals(&self, auth: &AuthToken) -> Result<HoldingsBook> {
        orders::get_holdings_book(&self.core, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(&self.core, auth).await
    }

    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult> {
        funds::calculate_margin(&self.core, auth, legs).await
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(&self.core, auth, key).await
    }

    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        data::get_multiquotes(&self.core, auth, keys).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(&self.core, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(&self.core, auth, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(&self.core).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::GrowwFeed::new(
            self.core.http.clone(),
            auth.raw(),
            self.feed.clone(),
        )))
    }

    /// The order-book poller (web `PollingOrderUpdateAdapter`, 5 s); it
    /// replaces a running one and stops in `on_logout`.
    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        if auth.raw().trim().is_empty() {
            return Err(session_expired());
        }
        Ok(OrderFeed::Stream(
            self.start_order_updates(auth, order_poll::DEFAULT_INTERVAL)?,
        ))
    }

    async fn on_logout(&self) {
        self.stop_order_updates();
    }
}
