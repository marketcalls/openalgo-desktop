//! 5paisa OpenAPI adapter (web `broker/fivepaisa/**`; audit Part D
//! "fivepaisa"). Not the XTS `fivepaisaxts` broker.
//!
//! Credentials as on the web: API key = `api_key:::user_id:::client_id`
//! (app key, the app's user id, the client code), API secret = the app's
//! `EncryKey`. The trader signs in with the 5paisa login email, PIN and
//! TOTP (`TOTPLogin`, then `GetAccessToken`).
//!
//! Every later call needs the app key (`head.key`) and the client code
//! (`body.ClientCode`, feed `Value1`), which the web re-reads from its
//! environment. The desktop adapter only receives the stored session, so the
//! stored token is `api_key:::client_code:::access_token` (the access token
//! is a JWT and never contains `:::`); `user_id` is the client code.
//!
//! Every REST body is `{"head": {"key": <api_key>}, "body": {..}}` with
//! `Authorization: bearer <access_token>`. Prices are rupees (no scaling).

pub mod auth;
pub mod data;
pub mod funds;
pub mod mapping;
pub mod master_contract;
pub mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::order_poll;
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// REST host (web `BASE_URL`).
pub const BASE_URL: &str = "https://Openapi.5paisa.com";
/// Scrip master (web uses the lowercase host here).
pub const MASTER_URL: &str =
    "https://openapi.5paisa.com/VendorsAPI/Service1.svc/ScripMaster/segment/all";

/// `plugin.json` supported_exchanges.
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

/// web `get_supported_intervals` -> Historical Candles interval code
/// (`map_interval`). `d` and `1d` are accepted as aliases of `D`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "1m"),
    ("5m", "5m"),
    ("10m", "10m"),
    ("15m", "15m"),
    ("30m", "30m"),
    ("1h", "60m"),
    ("D", "1d"),
];

pub(crate) const NAME: &str = "5paisa";

/// The stored session, split.
#[derive(Clone)]
pub struct Session {
    pub api_key: String,
    pub client_code: String,
    pub access_token: crate::security::secret::Secret,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("client_code", &self.client_code)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// `api_key:::client_code:::access_token`.
    pub fn encode(&self) -> String {
        format!(
            "{}:::{}:::{}",
            self.api_key,
            self.client_code,
            self.access_token.expose()
        )
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your 5paisa session has expired. Log in to 5paisa again.".into())
}

/// Split the stored token.
pub fn session(auth: &AuthToken) -> Result<Session> {
    let mut parts = auth.raw().splitn(3, ":::");
    let (Some(k), Some(c), Some(t)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(session_expired());
    };
    if k.trim().is_empty() || c.trim().is_empty() || t.trim().is_empty() {
        return Err(session_expired());
    }
    Ok(Session {
        api_key: k.to_string(),
        client_code: c.to_string(),
        access_token: t.into(),
    })
}

/// web `{"head": {"key": api_key}, "body": body}`.
pub fn envelope(api_key: &str, body: Value) -> Value {
    json!({"head": {"key": api_key}, "body": body})
}

/// `head.statusDescription == "Success"`.
pub fn head_success(v: &Value) -> bool {
    v.get("head")
        .and_then(|h| h.get("statusDescription"))
        .and_then(Value::as_str)
        == Some("Success")
}

/// `body.Message`, or the head's status description.
pub fn message(v: &Value) -> String {
    v.get("body")
        .and_then(|b| b.get("Message"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            v.get("head")
                .and_then(|h| h.get("statusDescription"))
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Cloning shares the HTTP client, pacer and symbol master (the order
/// poller holds a clone).
#[derive(Clone)]
pub struct FivepaisaBroker {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) master_url: String,
    /// Feed host override (tests); `None` picks the host from the token.
    pub(crate) feed_url: Option<String>,
    pub(crate) symbols: SymbolResolver,
    /// web multiquotes pause between batches (500 ms).
    pub(crate) batch_pause: Duration,
    pub(crate) pacer: Arc<Pacer>,
    /// Running order-update poller, aborted on stop or drop.
    pub(crate) poller: Arc<parking_lot::Mutex<Option<order_poll::OrderPoller>>>,
}

impl FivepaisaBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_urls(symbols, BASE_URL, MASTER_URL)
    }

    /// Point the adapter at a local fake (tests).
    pub fn with_urls(
        symbols: SymbolResolver,
        base_url: impl Into<String>,
        master_url: impl Into<String>,
    ) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            master_url: master_url.into(),
            feed_url: None,
            symbols,
            batch_pause: Duration::from_millis(500),
            pacer: Arc::new(Pacer::per_second(20.0)),
            poller: Arc::default(),
        }
    }

    /// Use this feed host instead of the one named by the token (tests).
    pub fn with_feed_url(mut self, url: impl Into<String>) -> Self {
        self.feed_url = Some(url.into());
        self
    }

    /// Shorter pause between multiquote batches (tests).
    pub fn with_batch_pause(mut self, d: Duration) -> Self {
        self.batch_pause = d;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// POST `{base}{path}` with the head/body envelope.
    pub(crate) async fn post(&self, path: &str, body: Value, s: &Session) -> Result<Value> {
        self.post_with(path, body, s, http::REQUEST_TIMEOUT).await
    }

    pub(crate) async fn post_with(
        &self,
        path: &str,
        body: Value,
        s: &Session,
        timeout: Duration,
    ) -> Result<Value> {
        self.pacer.acquire().await;
        let resp = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .timeout(timeout)
            .header(
                "Authorization",
                format!("bearer {}", s.access_token.expose()),
            )
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(envelope(&s.api_key, body).to_string())
            .send()
            .await?;
        Self::read(path, resp).await
    }

    /// GET `{base}{path}` (history).
    pub(crate) async fn get(&self, path: &str, s: &Session) -> Result<Value> {
        self.pacer.acquire().await;
        let resp = self
            .http
            .get(format!("{}{}", self.base_url, path))
            .header(
                "Authorization",
                format!("bearer {}", s.access_token.expose()),
            )
            .header("Content-Type", "application/json")
            .send()
            .await?;
        Self::read(path.split('?').next().unwrap_or(""), resp).await
    }

    async fn read(path: &str, resp: reqwest::Response) -> Result<Value> {
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            tracing::warn!(
                broker = "fivepaisa",
                "5paisa refused the session on {}",
                path
            );
            return Err(session_expired());
        }
        let (status, v): (_, Value) = http::read_json("fivepaisa", resp).await?;
        if !status.is_success() {
            tracing::warn!(
                broker = "fivepaisa",
                status = status.as_u16(),
                "5paisa answered {} with an error",
                path
            );
            let m = message(&v);
            return Err(AppError::Broker(if m.is_empty() {
                "5paisa refused the request. Try again shortly.".into()
            } else {
                m
            }));
        }
        Ok(v)
    }
}

#[async_trait]
impl Broker for FivepaisaBroker {
    fn id(&self) -> &'static str {
        "fivepaisa"
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn logo(&self) -> &'static str {
        "/logos/fivepaisa.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["userid", "pin", "totp"],
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
            // Order updates come from REST polling (`start_order_updates`),
            // not the web's unregistered OrderTradeConfirmations socket:
            // 5paisa allows one feed connection per token.
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

    /// Order updates come from polling the order book (5paisa evicts a
    /// second feed connection per token).
    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        Ok(OrderFeed::Stream(self.start_order_updates(
            auth,
            order_poll::DEFAULT_INTERVAL,
        )?))
    }

    /// Broker logout, the daily boundary and app shutdown stop the poller.
    async fn on_logout(&self) {
        self.stop_order_updates();
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let s = session(auth)?;
        let host = match &self.feed_url {
            Some(u) => u.clone(),
            None => streaming::feed_url(&streaming::redirect_server(s.access_token.expose()))
                .to_string(),
        };
        Ok(Box::new(streaming::FivepaisaFeed::new(
            &host,
            s.access_token.expose(),
            &s.client_code,
        )))
    }
}

impl FivepaisaBroker {
    /// Start polling the order book for order updates (web
    /// `PollingOrderUpdateAdapter`: fivepaisa is in `_POLLING_BROKERS`
    /// because 5paisa evicts a second feed connection per token). Replaces
    /// a running poller. The interval is clamped to 1..=60 s.
    pub fn start_order_updates(
        &self,
        auth: &AuthToken,
        interval: Duration,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::brokers::common::streaming::OrderUpdate>> {
        // The task's copy gets its own empty poller slot so it never keeps
        // this poller (and itself) alive.
        let mut core = self.clone();
        core.poller = Arc::default();
        let auth = auth.clone();
        let (poller, rx) = order_poll::OrderPoller::start(
            "fivepaisa",
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

    /// Stop the order-update poller (broker logout, session revocation).
    pub fn stop_order_updates(&self) {
        if let Some(p) = self.poller.lock().take() {
            p.stop();
        }
    }

    /// Whether an order-update poller is running.
    pub fn order_updates_running(&self) -> bool {
        self.poller.lock().as_ref().is_some_and(|p| p.is_running())
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
