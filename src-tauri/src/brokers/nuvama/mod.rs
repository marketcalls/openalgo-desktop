//! Nuvama adapter on API Connect (web `broker/nuvama/**`, openalgo
//! ee123273b, #2461).
//!
//! What lives where:
//! * `session`: the packed session token and the header set every call
//!   carries; `auth`: the redirect sign-in (`request_id`) and the
//!   `loginvendor` / `logindata` exchange.
//! * `client`: the one REST path, with Nuvama's HTTP 222 error envelope.
//! * `mapping`: exchanges, index names, products, order types, sides and
//!   statuses; `books`: order, trade, position and holding rows.
//! * `orders`, `funds`, `data`: the endpoints; `master_contract`: the
//!   public `instruments.zip`.
//! * `streaming`: the market and order feeds; `tcp_relay`: the loopback
//!   relay to Nuvama's newline-delimited JSON socket.
//!
//! Credentials (Broker Configuration): the API key and secret of the API
//! Connect app, and the Nuvama account the sign-in must come back for (the
//! redirect carries no `state`).

mod auth;
mod books;
mod client;
mod data;
mod funds;
pub mod mapping;
pub mod master_contract;
mod orders;
pub mod session;
pub mod streaming;
pub mod tcp_relay;
#[cfg(test)]
mod tests;

use crate::brokers::common::http;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::ratelimit::{Pacer, ORDERS_PER_SECOND};
use crate::brokers::common::streaming::{BrokerFeed, OrderFeed};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use std::time::Duration;

/// Hosts (web `api/baseurl.py`).
pub const EQ_URL: &str = "https://nc.nuvamawealth.com/edelmw-eq/eq/";
pub const CONTENT_URL: &str = "https://nc.nuvamawealth.com/edelmw-content/content/";
pub const LOGIN_URL: &str = "https://nc.nuvamawealth.com/edelmw-login/login/";
pub const CONTRACT_URL: &str = "https://nc.nuvamawealth.com/app/toccontracts/instruments.zip";
/// The streaming socket: plain TCP, newline-delimited JSON. The host is the
/// one in Nuvama's docs (`ncst`); the SDK's `ndst` sends nothing.
pub const STREAM_HOST: &str = "ncst.nuvamawealth.com";
pub const STREAM_PORT: u16 = 9443;

/// web `plugin.json` supported_exchanges.
pub const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::Ncdex,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map`.
pub const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "M1"),
    ("3m", "M3"),
    ("5m", "M5"),
    ("15m", "M15"),
    ("30m", "M30"),
    ("1h", "H1"),
    ("D", "D1"),
];

/// Where the adapter reaches Nuvama (tests point it at local fakes).
#[derive(Debug, Clone)]
pub struct NuvamaUrls {
    pub eq: String,
    pub content: String,
    pub login: String,
    pub contract: String,
    pub stream_host: String,
    pub stream_port: u16,
}

impl Default for NuvamaUrls {
    fn default() -> Self {
        Self {
            eq: EQ_URL.into(),
            content: CONTENT_URL.into(),
            login: LOGIN_URL.into(),
            contract: CONTRACT_URL.into(),
            stream_host: STREAM_HOST.into(),
            stream_port: STREAM_PORT,
        }
    }
}

impl NuvamaUrls {
    /// Every REST host under one HTTP base, the stream on a local port
    /// (tests).
    pub fn rebased(http_base: &str, stream_port: u16) -> Self {
        let base = http_base.trim_end_matches('/');
        Self {
            eq: format!("{}/edelmw-eq/eq/", base),
            content: format!("{}/edelmw-content/content/", base),
            login: format!("{}/edelmw-login/login/", base),
            contract: format!("{}/app/toccontracts/instruments.zip", base),
            stream_host: "127.0.0.1".into(),
            stream_port,
        }
    }
}

/// The pacing lanes. Nuvama allows 3000 requests in 5 minutes per IP
/// across every route; the web keeps quotes and history to 8 a second so
/// orders always have room (`api/data.py` `_QUOTE_INTERVAL`). Orders take
/// their own lane at the exchanges' 10 a second (RATE-03; the web paces
/// none).
pub(crate) struct Pacers {
    data: Pacer,
    orders: Pacer,
}

impl Default for Pacers {
    fn default() -> Self {
        Self {
            data: Pacer::with_interval(Duration::from_millis(125)),
            orders: Pacer::per_second(ORDERS_PER_SECOND),
        }
    }
}

pub struct NuvamaBroker {
    http: reqwest::Client,
    urls: NuvamaUrls,
    symbols: SymbolResolver,
    pacers: Pacers,
}

impl NuvamaBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self::with_base_url(symbols, NuvamaUrls::default())
    }

    /// Point the adapter at other hosts (tests run a local fake Nuvama).
    pub fn with_base_url(symbols: SymbolResolver, urls: NuvamaUrls) -> Self {
        Self {
            http: http::client(),
            urls,
            symbols,
            pacers: Pacers::default(),
        }
    }

    pub(crate) fn resolver(&self) -> &SymbolResolver {
        &self.symbols
    }

    /// The master row of an OpenAlgo instrument.
    pub(crate) fn instrument(&self, key: &QuoteKey) -> Result<SymbolData> {
        self.symbols
            .by_symbol(&key.exchange, &key.symbol)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{} is not in the Nuvama master contract for {}. Download the master contract again and check the symbol.",
                    key.symbol, key.exchange
                ))
            })
    }
}

#[async_trait]
impl Broker for NuvamaBroker {
    fn id(&self) -> &'static str {
        "nuvama"
    }

    fn name(&self) -> &'static str {
        "Nuvama"
    }

    fn logo(&self) -> &'static str {
        "/logos/nuvama.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect {
            param: "request_id",
        }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            multiquotes_batch: false,
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

    /// The web's totals: the holding value only (RMS holdings carry no
    /// buy average).
    async fn get_holdings_with_totals(&self, auth: &AuthToken) -> Result<HoldingsBook> {
        orders::get_holdings_with_totals(self, auth).await
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        funds::get_funds(self, auth).await
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        data::get_quote(self, auth, key).await
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        data::get_market_depth(self, auth, key).await
    }

    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>> {
        data::get_history(self, auth, req).await
    }

    /// Nuvama's bars sit on a clock grid; the history service puts them on
    /// the exchange session's (web `_session_align`).
    fn history_on_session_grid(&self, req: &HistoryRequest) -> bool {
        crate::brokers::common::history::intraday_minutes(&req.interval).is_some()
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        master_contract::download(self).await
    }

    fn create_feed(&self, auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        let session = self.session(auth)?;
        Ok(Box::new(streaming::NuvamaFeed::new(&self.urls, &session)))
    }

    fn create_order_feed(&self, auth: &AuthToken) -> Result<OrderFeed> {
        let session = self.session(auth)?;
        Ok(OrderFeed::Socket(Box::new(
            streaming::NuvamaOrderFeed::new(&self.urls, &session, self.symbols.clone()),
        )))
    }
}
