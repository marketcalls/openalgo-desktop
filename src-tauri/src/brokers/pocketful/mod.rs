//! Pocketful adapter (web `broker/pocketful/**`).
//!
//! * Sign-in is OAuth2 authorization code with HTTP Basic client auth
//!   (`POST /oauth2/token`), then `GET /api/v1/user/trading_info` for the
//!   trading `client_id`, which almost every call carries as a query or
//!   body parameter. The session token is the bare access token, sent as
//!   `Authorization: Bearer`.
//! * Quotes and depth have no REST endpoint: the web subscribes on the
//!   market-data socket and waits for the instrument's packet. `data.rs`
//!   does the same with a short-lived, bounded connection.
//! * No history API and no margin calculator (web `timeframe_map = {}`,
//!   `margin_api.py` raises).
//! * Order and trade updates arrive on the market-data socket (modes 50 and
//!   51) and are published as `FeedEvent::OrderUpdate`.

mod auth;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod streaming;
#[cfg(test)]
mod tests;
pub mod zip;

pub use auth::redirect_uri;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::Pacer;
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const BASE_URL: &str = "https://trade.pocketful.in";
pub const WS_URL: &str = "wss://trade.pocketful.in";
/// web `download_csv_pocketful_data`.
pub const MASTER_URL: &str =
    "https://trade.pocketful.in/api/v1/contract/Compact?info=download&exchanges=NSE,NFO,BSE,BFO,MCX";

/// `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// Hosts the adapter talks to (tests point them at a local fake).
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// REST and OAuth base, e.g. `https://trade.pocketful.in`.
    pub rest: String,
    /// WebSocket base, e.g. `wss://trade.pocketful.in`.
    pub ws: String,
    /// Master-contract ZIP URL.
    pub master: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            rest: BASE_URL.into(),
            ws: WS_URL.into(),
            master: MASTER_URL.into(),
        }
    }
}

pub struct PocketfulBroker {
    http: reqwest::Client,
    urls: Endpoints,
    symbols: SymbolResolver,
    /// The client id fetched for the current token when the session did not
    /// carry one: one entry keyed by a digest of the token (bounded).
    client_id_cache: Mutex<Option<([u8; 32], String)>>,
    pacer: Pacer,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl PocketfulBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_endpoints(symbols, Endpoints::default())
    }

    /// Point the adapter at other hosts (tests run a local fake Pocketful).
    pub fn with_base_url(
        symbols: SymbolResolver,
        rest: impl Into<String>,
        ws: impl Into<String>,
        master: impl Into<String>,
    ) -> Self {
        Self::with_endpoints(
            symbols,
            Endpoints {
                rest: rest.into(),
                ws: ws.into(),
                master: master.into(),
            },
        )
    }

    pub fn with_endpoints(symbols: SymbolResolver, urls: Endpoints) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            client_id_cache: Mutex::new(None),
            pacer: Pacer::per_second(10.0),
        }
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

    /// One REST call. Answers the decoded JSON body when Pocketful says
    /// `status: success`; anything else becomes a trader-facing error.
    pub(crate) async fn call(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Value> {
        let token = Self::token(auth)?;
        self.pacer.acquire().await;
        let url = format!("{}{}", self.urls.rest, path_and_query);
        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(token)
            .header("Content-Type", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| redact(e.into()))?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            tracing::warn!(status = status.as_u16(), "Pocketful refused the session");
            return Err(session_expired());
        }
        let (status, v): (_, Value) = http::read_json("pocketful", resp).await.map_err(redact)?;
        if status.is_success() && v.get("status").and_then(Value::as_str) == Some("success") {
            return Ok(v);
        }
        let path = path_and_query.split('?').next().unwrap_or("");
        let message = mapping::text(&v, "message");
        tracing::warn!(
            status = status.as_u16(),
            "Pocketful refused {}: {}",
            path,
            message
        );
        Err(broker_error(&message))
    }

    /// The trading client id: from the session, else `trading_info`
    /// (web `get_client_id`), cached for this token.
    pub(crate) async fn client_id(&self, auth: &AuthToken) -> Result<String> {
        if let Some(id) = auth.user_id().map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(id.to_string());
        }
        let d = digest(Self::token(auth)?);
        if let Some((cached, id)) = self.client_id_cache.lock().as_ref() {
            if *cached == d {
                return Ok(id.clone());
            }
        }
        let v = self
            .call(Method::GET, "/api/v1/user/trading_info", auth, None)
            .await?;
        let id = mapping::text(&v["data"], "client_id");
        if id.is_empty() {
            return Err(AppError::Broker(
                "Pocketful did not return your trading account id. Log in to Pocketful again."
                    .into(),
            ));
        }
        *self.client_id_cache.lock() = Some((d, id.clone()));
        Ok(id)
    }

    /// The client id without a network call (session, then cache).
    fn known_client_id(&self, auth: &AuthToken) -> Option<String> {
        if let Some(id) = auth.user_id().map(str::trim).filter(|s| !s.is_empty()) {
            return Some(id.to_string());
        }
        let d = digest(auth.raw().trim());
        self.client_id_cache
            .lock()
            .as_ref()
            .filter(|(cached, _)| *cached == d)
            .map(|(_, id)| id.clone())
    }
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Pocketful session has expired. Log in to Pocketful again.".into())
}

/// Pocketful error message -> trader-facing error.
pub(crate) fn broker_error(message: &str) -> AppError {
    let m = message.trim();
    if m.is_empty() {
        AppError::Broker("Pocketful refused the request.".into())
    } else if m.to_ascii_lowercase().contains("permission") {
        AppError::Broker(
            "Your Pocketful app does not have permission for this. Check the app's scopes on the Pocketful developer console."
                .into(),
        )
    } else {
        AppError::Broker(m.to_string())
    }
}

#[async_trait]
impl Broker for PocketfulBroker {
    fn id(&self) -> &'static str {
        "pocketful"
    }

    fn name(&self) -> &'static str {
        "Pocketful"
    }

    fn logo(&self) -> &'static str {
        "/logos/pocketful.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "code" }
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
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        // web `BrokerData.timeframe_map = {}`: no history API.
        &[]
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
        // web `get_history` answers success without candles ("Pocketful does
        // not support historical data API"); no rows is the closest shape.
        Ok(Vec::new())
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let token = Self::token(auth)?;
        // `create_feed` is synchronous, so the client id must already be
        // known: from the session (stored at login) or an earlier REST call.
        let client_id = self.known_client_id(auth).ok_or_else(session_expired)?;
        Ok(Box::new(streaming::PocketfulFeed::new(
            &self.urls.ws,
            &client_id,
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
