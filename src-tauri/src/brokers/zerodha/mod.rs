//! Zerodha Kite Connect v3 adapter (web `broker/zerodha/**`).
//!
//! The session token is `api_key:access_token`, sent as
//! `Authorization: token api_key:access_token` with `X-Kite-Version: 3`.

mod auth;
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
use crate::brokers::common::streaming::BrokerFeed;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use mapping::KiteEnvelope;
use reqwest::Method;
use serde::de::DeserializeOwned;

pub const BASE_URL: &str = "https://api.kite.trade";

/// `plugin.json` supported_exchanges (+ BCD, which the master maps).
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
    Exchange::McxIndex,
    Exchange::GlobalIndex,
];

/// web `BrokerData.timeframe_map`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "minute"),
    ("3m", "3minute"),
    ("5m", "5minute"),
    ("10m", "10minute"),
    ("15m", "15minute"),
    ("30m", "30minute"),
    ("60m", "60minute"),
    ("1h", "60minute"),
    ("D", "day"),
];

/// Request body encodings Kite uses.
pub(crate) enum Body<'a> {
    None,
    Form(&'a [(&'a str, String)]),
    Json(&'a serde_json::Value),
}

pub struct ZerodhaBroker {
    http: reqwest::Client,
    base_url: String,
    symbols: SymbolResolver,
    /// Kite limits: orders 10/s, quotes 1/s, historical 3/s, others 10/s.
    order_pacer: Pacer,
    quote_pacer: Pacer,
    history_pacer: Pacer,
    other_pacer: Pacer,
}

impl ZerodhaBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, BASE_URL)
    }

    /// Point the adapter at another host (tests run a local fake Kite).
    pub fn with_base_url(symbols: SymbolResolver, base_url: impl Into<String>) -> Self {
        Self {
            http: http::client(),
            base_url: base_url.into(),
            symbols,
            order_pacer: Pacer::per_second(10.0),
            quote_pacer: Pacer::per_second(1.0),
            history_pacer: Pacer::per_second(3.0),
            other_pacer: Pacer::per_second(10.0),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// Validate the stored token shape before using it.
    fn auth_header(auth: &AuthToken) -> Result<String> {
        match auth.pair() {
            Some(_) => Ok(format!("token {}", auth.raw())),
            None => Err(session_expired()),
        }
    }

    /// One Kite call. Returns the decoded envelope on success; Kite error
    /// envelopes become trader-facing errors.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Body<'_>,
        pacer: Category,
    ) -> Result<T> {
        let env: KiteEnvelope<T> = self
            .call_raw(method, path_and_query, auth, body, pacer)
            .await?;
        env.data
            .ok_or_else(|| AppError::Broker("Zerodha returned no data for this request.".into()))
    }

    /// Like `call`, but hands back the whole envelope.
    pub(crate) async fn call_raw<T: DeserializeOwned>(
        &self,
        method: Method,
        path_and_query: &str,
        auth: &AuthToken,
        body: Body<'_>,
        pacer: Category,
    ) -> Result<KiteEnvelope<T>> {
        match pacer {
            Category::Order => self.order_pacer.acquire().await,
            Category::Quote => self.quote_pacer.acquire().await,
            Category::History => self.history_pacer.acquire().await,
            Category::Other => self.other_pacer.acquire().await,
        }
        let url = format!("{}{}", self.base_url, path_and_query);
        let mut req = self
            .http
            .request(method, &url)
            .header("X-Kite-Version", "3")
            .header("Authorization", Self::auth_header(auth)?);
        req = match body {
            Body::None => req,
            Body::Form(f) => req.form(f),
            Body::Json(v) => req.json(v),
        };
        let resp = req.send().await?;
        let (status, env): (_, KiteEnvelope<T>) = http::read_json("zerodha", resp).await?;
        if env.status == "success" {
            return Ok(env);
        }
        tracing::warn!(
            status = status.as_u16(),
            error_type = %env.error_type,
            "Zerodha refused {}: {}",
            path_and_query.split('?').next().unwrap_or(""),
            env.message
        );
        Err(kite_error(&env.error_type, &env.message))
    }
}

/// Pacing category of a call.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Category {
    Order,
    Quote,
    History,
    Other,
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Zerodha session has expired. Log in to Zerodha again.".into())
}

/// Kite error envelope -> trader-facing error.
pub(crate) fn kite_error(error_type: &str, message: &str) -> AppError {
    match error_type {
        "TokenException" => session_expired(),
        "PermissionException" => AppError::Broker(
            "Your Kite Connect app does not have permission for this. Check the app's subscription on the Kite developer console."
                .into(),
        ),
        "NetworkException" => AppError::Broker(
            "Zerodha could not reach the exchange. Try again in a moment.".into(),
        ),
        _ if message.trim().is_empty() => {
            AppError::Broker("Zerodha refused the request.".into())
        }
        _ => AppError::Broker(message.trim().to_string()),
    }
}

#[async_trait]
impl Broker for ZerodhaBroker {
    fn id(&self) -> &'static str {
        "zerodha"
    }

    fn name(&self) -> &'static str {
        "Zerodha"
    }

    fn logo(&self) -> &'static str {
        "/logos/zerodha.svg"
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
            history: true,
            multiquotes_batch: true,
            margin: true,
            gtt: true,
            streaming: true,
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

    async fn get_order_book_tagged(&self, auth: &AuthToken) -> Result<Vec<TaggedOrder>> {
        orders::get_order_book_tagged(self, auth).await
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

    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self, auth).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let (api_key, access_token) = auth.pair().ok_or_else(session_expired)?;
        Ok(Box::new(streaming::KiteFeed::new(
            api_key,
            access_token,
            self.symbols.clone(),
        )))
    }
}
