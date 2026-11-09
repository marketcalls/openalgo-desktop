//! Sahi Direct API adapter (web `broker/sahi/**`).
//!
//! * Sign-in: the saved API key id (`pk_...`) and secret (`sahi_sk_...`)
//!   are exchanged for the day's Bearer token on the auth host
//!   (`POST /api/v1/apikeys/token`); no redirect, no TOTP. The trader must
//!   approve the key once per trading day in Sahi's API Dashboard, and the
//!   token lasts until 06:00 IST. The stored session token is the access
//!   token alone.
//! * Everything else is on the API host under `/v1`: prices in integer
//!   paise, quantities in units, times in epoch milliseconds.
//! * Instruments are named by Sahi's `instrument_id`, stored as the master's
//!   `token`; orders take the scrip master's `symbol` plus
//!   `exchange_segment` (the master's `brsymbol` and `brexchange`).
//! * Each endpoint has its own per-second and per-minute budget
//!   (`ratelimit.rs`); a 429 pauses the category for its Retry-After.
//! * Order writes must come from the static IP registered with Sahi
//!   (otherwise 403 `IP_NOT_ALLOWED`).
//! * Sahi's CloudFront refuses requests without a User-Agent, so every call
//!   names itself.
//!
//! The scrip master also lists MCX and NCOM contracts, but Sahi rejects
//! commodity orders and its tick stream has no commodity segment, so only
//! NSE, BSE, NFO and BFO are loaded (as on the web).

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod ratelimit;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::common::{http, outcome};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use ratelimit::{Category, Op, SahiLimiter};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::SystemTime;
use tokio::time::Instant;

pub const AUTH_BASE_URL: &str = "https://apis.sahi.com";
pub const API_BASE_URL: &str = "https://direct.sahi.com";
pub const WS_URL: &str = "wss://direct.sahi.com/v1/stream/marketdata";
pub const SCRIP_MASTER_URL: &str = "https://assets.sahi.com/external/instruments.csv";

/// What every call names itself as (CloudFront answers 403 without one).
pub const USER_AGENT: &str = "openalgo-sahi/1.0";

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1m"),
    ("3m", "3m"),
    ("5m", "5m"),
    ("15m", "15m"),
    ("30m", "30m"),
    ("1h", "1h"),
    ("D", "1d"),
];

/// One request to a Sahi host.
struct Call<'a> {
    category: Category,
    url: String,
    bearer: Option<&'a str>,
    method: Method,
    query: &'a [(&'a str, String)],
    body: Option<&'a Value>,
}

/// A decoded Sahi answer.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: StatusCode,
    pub body: Value,
}

pub struct SahiBroker {
    http: reqwest::Client,
    base_url: String,
    auth_url: String,
    master_url: String,
    ws_url: String,
    symbols: SymbolResolver,
    limiter: SahiLimiter,
}

impl SahiBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(
            symbols,
            API_BASE_URL,
            AUTH_BASE_URL,
            SCRIP_MASTER_URL,
            WS_URL,
        )
    }

    /// Point every host at a local fake (tests).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        auth_url: impl Into<String>,
        master_url: impl Into<String>,
        ws_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            auth_url: auth_url.into(),
            master_url: master_url.into(),
            ws_url: ws_url.into(),
            symbols,
            limiter: SahiLimiter::default(),
        }
    }

    /// Other request limits (tests run a fake at full speed).
    pub fn with_limiter(mut self, limiter: SahiLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// One call to the API host with the session's Bearer token.
    pub(crate) async fn send(
        &self,
        auth: &AuthToken,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        op: &Op,
    ) -> Result<Reply> {
        let token = auth.raw().trim();
        if token.is_empty() {
            return Err(session_expired());
        }
        let call = Call {
            category: Category::of(&method, path),
            url: format!("{}{}", self.base_url, path),
            bearer: Some(token),
            method,
            query,
            body,
        };
        self.send_to(&call, op).await
    }

    /// One paced call, repeating only a throttle answer (Sahi decides a 429
    /// before the order reaches risk checks, so nothing was placed).
    async fn send_to(&self, call: &Call<'_>, op: &Op) -> Result<Reply> {
        let Call {
            category,
            url,
            bearer,
            method,
            query,
            body,
        } = call;
        let category = *category;
        let mut retries_left = op.retries_left;
        let mut attempt = 0;
        loop {
            self.limiter.acquire(category, op).await?;
            let mut req = self
                .http
                .request(method.clone(), url.as_str())
                .header(reqwest::header::ACCEPT, "application/json")
                .header(reqwest::header::USER_AGENT, USER_AGENT);
            if let Some(t) = bearer {
                req = req.bearer_auth(t);
            }
            if !query.is_empty() {
                req = req.query(*query);
            }
            if let Some(b) = body {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(b.to_string());
            }
            let resp = req.send().await?;
            let status = resp.status();
            if status == StatusCode::TOO_MANY_REQUESTS && retries_left > 0 {
                let delay = crate::brokers::fyers::ratelimit::retry_delay(
                    resp.headers(),
                    attempt,
                    ratelimit::BASE_BACKOFF,
                    SystemTime::now(),
                );
                let until = Instant::now().checked_add(delay).unwrap_or(op.deadline);
                self.limiter.cool_down(category, until);
                if until > op.deadline {
                    tracing::warn!(
                        "Sahi asked to wait {:?}, past the request's deadline",
                        delay
                    );
                    return Err(outcome::busy(
                        "Sahi asked OpenAlgo to slow down for longer than a request can be held. This request was not retried. Try again in a minute.",
                    ));
                }
                retries_left -= 1;
                attempt += 1;
                continue;
            }
            let bytes = resp.bytes().await?;
            return Ok(Reply {
                status,
                body: decode(status, &bytes)?,
            });
        }
    }

    /// A read: the body of a 2xx answer, else Sahi's error.
    pub(crate) async fn get(
        &self,
        auth: &AuthToken,
        path: &str,
        query: &[(&str, String)],
        op: &Op,
    ) -> Result<Value> {
        let r = self.send(auth, Method::GET, path, query, None, op).await?;
        if r.status.is_success() {
            return Ok(r.body);
        }
        Err(error_for(r.status, &r.body, path))
    }
}

/// The body of an answer: `null` when empty (a 204), else JSON. A body
/// that is not JSON becomes a trader-facing error the way
/// `http::read_json` words it.
fn decode(status: StatusCode, bytes: &[u8]) -> Result<Value> {
    if status.is_server_error() {
        outcome::note_server_error();
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(bytes).map_err(|e| {
        tracing::warn!(
            broker = "sahi",
            status = status.as_u16(),
            "Unexpected response from Sahi ({})",
            e
        );
        let unreadable = "Sahi sent a response OpenAlgo could not read. Try again shortly.";
        if status == StatusCode::TOO_MANY_REQUESTS {
            outcome::refusal("Sahi is limiting requests right now. Wait a moment and try again.")
        } else if status.is_client_error() {
            outcome::refusal(unreadable)
        } else if status.is_server_error() {
            AppError::Broker(http::SERVER_ERROR.into())
        } else {
            AppError::Broker(unreadable.into())
        }
    })
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Sahi session has expired. Log in to Sahi again.".into())
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Sahi's error envelope `{code, message}` as a trader-facing error (web
/// `_error_message`). The code and status go to the log once, here.
pub(crate) fn error_for(status: StatusCode, body: &Value, path: &str) -> AppError {
    let code = text(body, "code").unwrap_or("");
    tracing::warn!(
        broker = "sahi",
        status = status.as_u16(),
        code,
        "Sahi refused {}",
        path.split('?').next().unwrap_or("")
    );
    if status.is_server_error() && code != "UNDER_MAINTENANCE" {
        return http::server_error("sahi", status);
    }
    AppError::Broker(match code {
        "IP_NOT_ALLOWED" => "Sahi refused the order because it did not come from your registered static IP. Check the IP in Sahi web, Profile, API Dashboard, and that OpenAlgo runs on that computer.".into(),
        "UNAUTHORIZED" => return session_expired(),
        _ if status == StatusCode::UNAUTHORIZED => return session_expired(),
        "FORBIDDEN" => "Your Sahi API key does not have permission for this. Create a key with Full access in the Sahi API Dashboard and save it on the Broker Configuration page.".into(),
        "RATE_LIMITED" => "Sahi is limiting requests right now. Wait a few seconds and try again.".into(),
        _ if status == StatusCode::TOO_MANY_REQUESTS => "Sahi is limiting requests right now. Wait a few seconds and try again.".into(),
        "UNDER_MAINTENANCE" => "Sahi's margin calculator is closed for daily maintenance until 07:00 IST.".into(),
        _ => match text(body, "message") {
            Some(m) => format!("Sahi: {}", m),
            None => "Sahi did not accept the request.".into(),
        },
    })
}

#[async_trait]
impl Broker for SahiBroker {
    fn id(&self) -> &'static str {
        "sahi"
    }

    fn name(&self) -> &'static str {
        "Sahi"
    }

    fn logo(&self) -> &'static str {
        "/logos/sahi.svg"
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
            multiquotes_batch: true,
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

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(&self.http, &self.master_url).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let token = auth.raw().trim();
        if token.is_empty() {
            return Err(session_expired());
        }
        Ok(Box::new(streaming::SahiFeed::new(
            token.to_string(),
            self.symbols.clone(),
            self.ws_url.clone(),
        )))
    }

    fn feed_refusal(&self, exchange: &str) -> Option<String> {
        streaming::refusal(exchange)
    }
}
