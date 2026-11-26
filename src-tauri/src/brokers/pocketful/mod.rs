//! Pocketful adapter (web `broker/pocketful/**`, rebuilt on Pocketful's new
//! API in marketcalls/openalgo#2467).
//!
//! * Every REST call goes to `https://algo.pocketful.in` through
//!   [`client`] (both documented header forms, both answer envelopes).
//! * Sign-in ([`auth`]) is OAuth 2.0 on the same host: `/oauth2/auth`, then
//!   the code exchange at `/oauth2/token` with HTTP Basic client auth and
//!   `GET /api/v1/user/trading_info` for the trading account, which is the
//!   session's user id. Every call carries that client id; a session
//!   without one is expired, never a call with an empty account.
//! * Order and trade updates and the market feed: [`streaming`]. Order
//!   updates come on a socket of their own (`streaming::order_feed`, web
//!   `pocketful_order_adapter.py`), published as `FeedEvent::OrderUpdate`.

pub mod auth;
pub mod client;
mod data;
mod funds;
mod gtt;
mod history;
pub mod mapping;
mod margin;
pub mod market;
pub mod master_contract;
mod orders;
mod pricing;
#[cfg(test)]
mod rest_tests;
pub mod streaming;
#[cfg(test)]
mod tests;
pub mod zip;

pub use auth::redirect_uri;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::{Pacer, ORDERS_PER_SECOND};
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use std::time::Duration;

/// The REST and OAuth host (web `baseurl.BASE_URL`).
pub const BASE_URL: &str = "https://algo.pocketful.in";
/// The feed host; the feed adds its own path (`/ws/v1/feeds`).
pub const WS_URL: &str = "wss://algo.pocketful.in";
/// The public exchange-wise contract (web `CONTRACT_URL`).
pub const MASTER_URL: &str =
    "https://algo.pocketful.in/api/v1/contract/Compact?info=download&exchanges=NSE,NFO,BSE,BFO,MCX";

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
    /// REST and OAuth base, e.g. `https://algo.pocketful.in`.
    pub rest: String,
    /// WebSocket base, e.g. `wss://algo.pocketful.in`.
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
    /// Data and account calls, at the web's 8 a second.
    pacer: Pacer,
    /// Places, modifies and cancels, apart from the data calls so none
    /// queues ahead of an exit (RATE-02).
    order_pacer: Pacer,
    /// The first wait before a rate-limited data request is sent again
    /// (web `_RATE_LIMIT_BACKOFF`); tests shorten it.
    rate_limit_backoff: Duration,
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
            pacer: Pacer::with_interval(client::DATA_INTERVAL),
            order_pacer: Pacer::per_second(ORDERS_PER_SECOND),
            rate_limit_backoff: client::RATE_LIMIT_BACKOFF,
        }
    }

    /// The same adapter, waiting `first` before repeating a rate-limited
    /// data request (doubled per retry). Tests use a few milliseconds.
    pub fn with_rate_limit_backoff(mut self, first: Duration) -> Self {
        self.rate_limit_backoff = first;
        self
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// The bare access token every request carries.
    fn access_token(auth: &AuthToken) -> Result<&str> {
        let t = auth.raw().trim();
        if t.is_empty() {
            Err(session_expired())
        } else {
            Ok(t)
        }
    }

    /// The trading client id every call carries: the account the sign-in
    /// stored with the session (web `baseurl.client_id`). A session without
    /// one (an older build's, or a damaged row) is treated as expired, so
    /// the trader is told to log in again before anything is sent.
    pub(crate) fn client_id(&self, auth: &AuthToken) -> Result<String> {
        match auth.user_id().map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => Ok(id.to_string()),
            None => {
                tracing::warn!("The Pocketful session carries no client id");
                Err(session_expired())
            }
        }
    }
}

/// What a GTT or margin request answers when the session carries no
/// client id (web `gtt_api.NO_CLIENT_ID`): refused before anything is sent.
const NO_CLIENT_ID: &str =
    "Your Pocketful session carries no client id. Log in to Pocketful again and retry.";

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Pocketful session has expired. Log in to Pocketful again.".into())
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
            history: true,
            // One REST request per symbol: no route takes several.
            multiquotes_batch: false,
            margin: true,
            gtt: true,
            streaming: true,
            order_feed: true,
            depth_levels: &[5],
        }
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        history::TIMEFRAME_MAP
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
        margin::calculate_margin(self, auth, legs).await
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

    /// The derivatives quote's open interest is undocumented: a leg whose
    /// answer lacks it is reported, never read as zero.
    async fn get_chain_quotes(
        &self,
        auth: &AuthToken,
        req: &ChainQuoteRequest,
    ) -> Result<ChainQuotes> {
        data::get_chain_quotes(self, auth, req).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        history::get_history(self, auth, req).await
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
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

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let token = Self::access_token(auth)?;
        let client_id = self.client_id(auth)?;
        Ok(Box::new(streaming::PocketfulFeed::new(
            &self.urls.ws,
            &client_id,
            token,
            self.symbols.clone(),
        )))
    }

    /// web `create_pocketful_order_adapter`: the session's client id and
    /// bare access token, on the market feed's host.
    fn create_order_feed(
        &self,
        auth: &AuthToken,
    ) -> Result<crate::brokers::common::streaming::OrderFeed> {
        let token = Self::token(auth)?;
        let client_id = auth
            .user_id()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(session_expired)?;
        Ok(crate::brokers::common::streaming::OrderFeed::Socket(
            Box::new(streaming::order_feed::PocketfulOrderFeed::new(
                &self.urls.ws,
                client_id,
                token,
                self.symbols.clone(),
            )),
        ))
    }
}

/// Transport errors carry the request URL, and with it query credentials
/// (the account id). Strip it before the error can reach a log or a
/// caller; every other error passes through unchanged.
pub(crate) fn redact(e: AppError) -> AppError {
    match e {
        AppError::Http(h) => AppError::Http(Box::new(h.without_url())),
        other => other,
    }
}
