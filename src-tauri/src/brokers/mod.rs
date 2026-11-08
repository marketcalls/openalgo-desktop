//! Broker adapters.
//!
//! Every adapter implements `Broker`: translation between the broker's
//! shapes and OpenAlgo's. Inputs are resolved OpenAlgo requests; outputs
//! (books, quotes, ticks) carry OpenAlgo symbols and exchanges. Optional
//! capabilities (margin, GTT, streaming) default to `AppError::Unsupported`.
//! The trait is object safe: the registry hands out `Arc<dyn Broker>`.

pub mod aliceblue;
pub mod angel;
pub mod arrow;
pub mod catalog;
pub mod common;
pub mod compositedge;
pub mod definedge;
pub mod deltaexchange;
pub mod dhan;
pub mod dhan_sandbox;
pub mod families;
pub mod firstock;
pub mod fivepaisa;
pub mod fivepaisaxts;
pub mod flattrade;
pub mod fyers;
pub mod groww;
pub mod hdfcsecurities;
pub mod hdfcsky;
pub mod ibulls;
pub mod iifl;
pub mod iiflcapital;
pub mod indmoney;
pub mod jainamxts;
pub mod kotak;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod motilal;
pub mod mstock;
pub mod nubra;
pub mod paytm;
pub mod pocketful;
pub mod rmoney;
pub mod rupeezy;
pub mod sahi;
pub mod samco;
// CI's native E2E test build (src/e2e.rs) and the tests of it.
#[cfg(any(test, feature = "test-support"))]
pub mod scripted;
pub mod setup_guide;
pub mod shoonya;
pub mod tradejini;
pub mod tradesmart;
pub mod types;
pub mod upstox;
pub mod wisdom;
pub mod zebu;
pub mod zerodha;

use crate::error::{AppError, Result};
use async_trait::async_trait;
use common::mapping::{Exchange, OrderStatus, PriceType, Product};
use common::streaming::{BrokerFeed, OrderFeed};
use common::symbols::SymbolResolver;
use std::collections::HashMap;
use std::sync::Arc;
use types::*;

/// The broker module contract (web `.claude/skills/broker-integration`).
#[async_trait]
pub trait Broker: Send + Sync {
    // ---- identity and capabilities ----

    /// Broker id, e.g. `zerodha`.
    fn id(&self) -> &'static str;
    /// Display name.
    fn name(&self) -> &'static str;
    /// Logo path served by the UI.
    fn logo(&self) -> &'static str;
    /// How the trader signs in.
    fn login_kind(&self) -> LoginKind;
    /// Exchanges the broker trades (web `plugin.json` `supported_exchanges`).
    fn supported_exchanges(&self) -> &'static [Exchange];
    /// Optional capabilities implemented by this adapter.
    fn capabilities(&self) -> Capabilities;
    /// OpenAlgo interval -> broker interval, in the order `/intervals` lists them.
    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)];

    /// Whether this broker needs TOTP on its login form.
    fn requires_totp(&self) -> bool {
        matches!(self.login_kind(), LoginKind::DirectTotp { fields } if fields.contains(&"totp"))
    }

    /// The symbol master this adapter resolves against, when it has one.
    /// Used by the default `close_all_positions`.
    fn symbols(&self) -> Option<&SymbolResolver> {
        None
    }

    /// The concrete adapter, for broker-specific sign-in helpers (the
    /// Definedge OTP request, the Samco static IP check). `None` unless the
    /// adapter opts in.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// API plans the broker sells with different request limits, for
    /// Broker Configuration: `(id, label, what it allows)`, the default
    /// (what applies until the trader picks one) first. Empty when the
    /// broker has one set of limits.
    fn rate_plans(&self) -> &'static [RatePlan] {
        &[]
    }

    /// Pace requests for the plan with this id (one of `rate_plans`);
    /// anything else is the default plan.
    fn set_rate_plan(&self, _plan: &str) {}

    /// The plan requests are paced for now, when the broker has plans.
    fn rate_plan(&self) -> Option<&'static str> {
        None
    }

    /// Requests counted today against the broker's daily caps, per request
    /// category, saved so a restart keeps the day's count (Fyers). Empty
    /// when the broker has no daily cap.
    fn daily_request_counts(&self) -> Vec<common::ratelimit::DailyCount> {
        Vec::new()
    }

    /// Count requests saved earlier today as already sent.
    fn restore_daily_request_counts(&self, _counts: &[common::ratelimit::DailyCount]) {}

    // ---- auth and session lifecycle ----

    /// Exchange login credentials (or an OAuth code) for a session token.
    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse>;

    /// A browser sign-in whose address needs a broker call first (Dhan's
    /// consent). `None` means the catalogue's authorize URL applies.
    /// `credentials.redirect_uri` carries the callback address.
    async fn begin_login(&self, _credentials: &BrokerCredentials) -> Result<Option<String>> {
        Ok(None)
    }

    /// A stored session is being used again without a new login (resume
    /// after the app restarted): rebuild per-login state the token does not
    /// carry (Kotak's UCC) from the stored broker credentials. Login secrets
    /// (password, TOTP, codes) are never set here.
    fn restore_session(&self, _credentials: &BrokerCredentials) {}

    /// Stop everything the adapter runs on its own (order pollers, cached
    /// per-session state). Called on broker logout, at the daily session
    /// boundary and on app shutdown; must be idempotent.
    async fn on_logout(&self) {}

    // ---- orders ----

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse>;

    /// A live order with the request's `strategy`, for an adapter whose web
    /// plugin sends it to the broker (Rupeezy's `order_identifier`). The
    /// order service's live placement calls this; the default sends the
    /// order without it.
    async fn place_order_with_strategy(
        &self,
        auth: &AuthToken,
        order: &ResolvedOrder,
        _strategy: &str,
    ) -> Result<OrderResponse> {
        self.place_order(auth, order).await
    }

    async fn modify_order(&self, auth: &AuthToken, order: &ResolvedModify)
        -> Result<OrderResponse>;

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse>;

    // ---- crypto: exact sizes and leverage ----

    /// Place a `CRYPTO` order whose size is carried exactly (it may be
    /// fractional). `order.quantity` holds the size truncated to whole
    /// units. Default: whole sizes go through `place_order`; fractional ones
    /// are refused (only crypto venues override this).
    async fn place_order_exact(
        &self,
        auth: &AuthToken,
        order: &ResolvedOrder,
        quantity: &CryptoQuantity,
    ) -> Result<OrderResponse> {
        match quantity.as_whole() {
            Some(units) => {
                let mut whole = order.clone();
                whole.quantity = units;
                self.place_order(auth, &whole).await
            }
            None => Err(AppError::Validation(
                "This broker accepts whole-number quantities only.".into(),
            )),
        }
    }

    /// Modify a `CRYPTO` order to an exact size (see `place_order_exact`).
    async fn modify_order_exact(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
        quantity: &CryptoQuantity,
    ) -> Result<OrderResponse> {
        match quantity.as_whole() {
            Some(units) => {
                let mut whole = order.clone();
                whole.quantity = units;
                self.modify_order(auth, &whole).await
            }
            None => Err(AppError::Validation(
                "This broker accepts whole-number quantities only.".into(),
            )),
        }
    }

    /// Whether the venue takes a per-instrument leverage before each order
    /// (web `plugin.json` `leverage_config`). The order service then applies
    /// the leverage saved on the Leverage page through `set_leverage`.
    fn leverage_config(&self) -> bool {
        false
    }

    /// Web `plugin.json` `broker_type`: `IN_stock`, or `crypto`.
    fn broker_type(&self) -> &'static str {
        "IN_stock"
    }

    /// Set the leverage used for new orders on one instrument.
    async fn set_leverage(
        &self,
        _auth: &AuthToken,
        _instrument: &SymbolData,
        _leverage: u32,
    ) -> Result<()> {
        Err(AppError::Unsupported("leverage"))
    }

    /// The leverage currently set on one instrument.
    async fn get_leverage(&self, _auth: &AuthToken, _instrument: &SymbolData) -> Result<f64> {
        Err(AppError::Unsupported("leverage"))
    }

    /// Cancel every `open` / `trigger pending` order (web
    /// `cancel_all_orders_api`). Default: order book, then one cancel each.
    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult> {
        let book = self.get_order_book(auth).await?;
        let mut result = CancelAllResult::default();
        for o in book {
            let pending = o
                .status
                .parse::<OrderStatus>()
                .map(OrderStatus::is_pending)
                .unwrap_or(false);
            if !pending {
                continue;
            }
            match self.cancel_order(auth, &o.order_id).await {
                Ok(_) => result.cancelled.push(o.order_id),
                Err(e) => {
                    tracing::warn!("Cancel of order {} failed: {}", o.order_id, e.code());
                    result.failed.push(o.order_id)
                }
            }
        }
        Ok(result)
    }

    /// Square off every open position with a MARKET order (web
    /// `close_all_positions`). Default: position book, one exit order each,
    /// resolved through `symbols()`.
    async fn close_all_positions(&self, auth: &AuthToken) -> Result<CloseAllResult> {
        let symbols = self
            .symbols()
            .ok_or(AppError::Unsupported("close_all"))?
            .clone();
        let positions = self.get_positions(auth).await?;
        let mut result = CloseAllResult::default();
        for p in positions.into_iter().filter(|p| p.quantity != 0) {
            let label = format!("{} ({})", p.symbol, p.exchange);
            let req = OrderRequest {
                symbol: p.symbol.clone(),
                exchange: p.exchange.clone(),
                side: if p.quantity > 0 { "SELL" } else { "BUY" }.to_string(),
                quantity: p.quantity.abs(),
                price: 0.0,
                order_type: PriceType::Market.as_str().to_string(),
                product: p.product.clone(),
                validity: "DAY".to_string(),
                trigger_price: None,
                disclosed_quantity: None,
                amo: false,
            };
            let placed = match ResolvedOrder::resolve(&req, &symbols) {
                Ok(order) => common::outcome::placing(self.place_order(auth, &order)).await,
                Err(e) => Err(e),
            };
            // An exit with no definite answer is reported with the advice to
            // check the order book, never as refused (LOG-08).
            result.record(&label, placed);
        }
        Ok(result)
    }

    /// Net quantity of one OpenAlgo symbol/exchange/product (web
    /// `get_open_position`), 0 when flat. Default: scan the position book.
    async fn get_open_position(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<i64> {
        let positions = self.get_positions(auth).await?;
        Ok(positions
            .iter()
            .find(|p| {
                p.symbol == symbol
                    && p.exchange == exchange.as_str()
                    && p.product == product.as_str()
            })
            .map(|p| i64::from(p.quantity))
            .unwrap_or(0))
    }

    /// `get_open_position` with the exact signed size: a crypto spot
    /// balance can be fractional (0.0005 BTC), which the whole-unit answer
    /// truncates. A live CRYPTO smart order decides on this. Default: the
    /// whole-unit answer.
    async fn get_open_position_exact(
        &self,
        auth: &AuthToken,
        symbol: &str,
        exchange: Exchange,
        product: Product,
    ) -> Result<rust_decimal::Decimal> {
        let units = self
            .get_open_position(auth, symbol, exchange, product)
            .await?;
        Ok(rust_decimal::Decimal::from(units))
    }

    // ---- books (OpenAlgo symbols, lowercase statuses) ----

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>>;
    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>>;
    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>>;

    /// The order book with the client tag each order was sent with, for
    /// adapters that tag their orders and read the tag back; `None` tags
    /// elsewhere. The order reconciler reads this once per pass to find an
    /// order whose placement had no definite answer (LOG-08).
    /// Whether this adapter tags every order with its own client tag and
    /// reads it back in `get_order_book_tagged`. Only then is a tag the way
    /// to find an unconfirmed order (LOG-08 P1-4); rows' contents never
    /// decide it.
    fn reports_order_tags(&self) -> bool {
        false
    }

    async fn get_order_book_tagged(&self, auth: &AuthToken) -> Result<Vec<TaggedOrder>> {
        Ok(self
            .get_order_book(auth)
            .await?
            .into_iter()
            .map(|order| TaggedOrder {
                order,
                client_tag: None,
            })
            .collect())
    }

    /// The order book with exact sizes (crypto). Default: the whole-unit
    /// book. Only venues whose `broker_type` is `crypto` are asked.
    async fn get_order_book_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Order>>> {
        Ok(self
            .get_order_book(auth)
            .await?
            .into_iter()
            .map(|o| {
                let q = i64::from(o.quantity);
                ExactRow::whole(o, q)
            })
            .collect())
    }

    /// The trade book with exact sizes (see `get_order_book_exact`).
    async fn get_trade_book_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Trade>>> {
        Ok(self
            .get_trade_book(auth)
            .await?
            .into_iter()
            .map(|t| {
                let q = i64::from(t.quantity);
                ExactRow::whole(t, q)
            })
            .collect())
    }

    /// Positions with exact signed sizes, fractional spot balances included
    /// (see `get_order_book_exact`).
    async fn get_positions_exact(&self, auth: &AuthToken) -> Result<Vec<ExactRow<Position>>> {
        Ok(self
            .get_positions(auth)
            .await?
            .into_iter()
            .map(|p| {
                let q = i64::from(p.quantity);
                ExactRow::whole(p, q)
            })
            .collect())
    }
    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>>;
    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds>;

    /// Holdings with the broker's own portfolio totals when it reports them
    /// (Angel's `totalholding`). Default: the holdings, no totals, and the
    /// caller computes them (web `calculate_portfolio_statistics`).
    async fn get_holdings_with_totals(&self, auth: &AuthToken) -> Result<HoldingsBook> {
        Ok(HoldingsBook {
            holdings: self.get_holdings(auth).await?,
            totals: None,
        })
    }

    /// Margin for a basket of legs (web `calculate_margin_api`).
    async fn calculate_margin(
        &self,
        _auth: &AuthToken,
        _legs: &[MarginLeg],
    ) -> Result<MarginResult> {
        Err(AppError::Unsupported("margin"))
    }

    // ---- market data ----

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote>;

    /// Quotes for many instruments, one entry per key in request order.
    /// Default: one `get_quote` per key; adapters with a batch endpoint
    /// override it.
    async fn get_multiquotes(
        &self,
        auth: &AuthToken,
        keys: &[QuoteKey],
    ) -> Result<Vec<QuoteResult>> {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            match self.get_quote(auth, k).await {
                Ok(q) => out.push(QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: Some(q),
                    error: None,
                }),
                Err(e) => out.push(QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some(e.client_message()),
                }),
            }
        }
        Ok(out)
    }

    /// Quotes for the legs of one option chain, in request order, saying
    /// which legs came back without their open interest. Default:
    /// `get_multiquotes`, whose open interest the adapter vouches for. An
    /// adapter whose batch quotes carry no open interest overrides it
    /// (Fyers: its native chain, checked against the requested expiry).
    async fn get_chain_quotes(
        &self,
        auth: &AuthToken,
        req: &ChainQuoteRequest,
    ) -> Result<ChainQuotes> {
        Ok(ChainQuotes {
            quotes: self.get_multiquotes(auth, &req.keys).await?,
            oi_missing: Default::default(),
        })
    }

    /// Five-level depth (padded with zero levels).
    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth>;

    /// Candles for `req`, epoch seconds, oldest first, with OI.
    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>>;

    /// Whether this broker's history for `req` can carry bars that start
    /// after the session close (Dhan appends one to index series after the
    /// close; web openalgo#2308). The history service then drops them
    /// against the market calendar, which an adapter cannot read. Default:
    /// no.
    fn history_trims_after_close(&self, _req: &HistoryRequest) -> bool {
        false
    }

    // ---- GTT (optional) ----

    async fn place_gtt(&self, _auth: &AuthToken, _req: &GttRequest) -> Result<GttResponse> {
        Err(AppError::Unsupported("gtt"))
    }

    async fn modify_gtt(
        &self,
        _auth: &AuthToken,
        _trigger_id: &str,
        _req: &GttRequest,
    ) -> Result<GttResponse> {
        Err(AppError::Unsupported("gtt"))
    }

    async fn cancel_gtt(&self, _auth: &AuthToken, _trigger_id: &str) -> Result<GttResponse> {
        Err(AppError::Unsupported("gtt"))
    }

    async fn get_gtt_book(
        &self,
        _auth: &AuthToken,
        _include_history: bool,
    ) -> Result<Vec<GttOrder>> {
        Err(AppError::Unsupported("gtt"))
    }

    // ---- master contract ----

    /// Download and normalise the broker's instrument list.
    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>>;

    /// The master with per-row extras (crypto `contract_value`). Default:
    /// `download_master_contract` with no extras.
    async fn download_master(&self, auth: &AuthToken) -> Result<MasterContract> {
        Ok(MasterContract::new(
            self.download_master_contract(auth).await?,
        ))
    }

    /// The instrument type of stored master rows this broker carries into a
    /// new download when part of it fails (Firstock: `INDEX`, web
    /// `get_existing_index_rows`). For `Some`, the master contract service
    /// reads those rows from the stored table, when it holds this broker's
    /// master, and downloads through `download_master_carrying`.
    fn carries_stored(&self) -> Option<&'static str> {
        None
    }

    /// `download_master` with the stored rows `carries_stored` names.
    /// Default: they are not needed.
    async fn download_master_carrying(
        &self,
        auth: &AuthToken,
        _stored: Vec<SymbolData>,
    ) -> Result<MasterContract> {
        self.download_master(auth).await
    }

    // ---- streaming ----

    /// A streaming adapter for the market-data feed.
    fn create_feed(&self, _auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Err(AppError::Unsupported("streaming"))
    }

    /// Where order updates come from: a dedicated socket, or a stream the
    /// adapter produces itself (an order-book poller it stops in
    /// `on_logout`). A broker whose market feed already carries order
    /// updates (Kite) leaves this unsupported.
    fn create_order_feed(&self, _auth: &AuthToken) -> Result<OrderFeed> {
        Err(AppError::Unsupported("order_feed"))
    }

    /// A separate socket for books deeper than the market feed's (Fyers 50
    /// levels, Dhan 20). Subscriptions asking for `levels` go to it.
    fn create_depth_feed(&self, _auth: &AuthToken, _levels: u8) -> Result<Box<dyn BrokerFeed>> {
        Err(AppError::Unsupported("depth_feed"))
    }

    /// Depth levels the feeds can stream for `exchange` (what a feed client
    /// may ask for). Default: the advertised `depth_levels`.
    fn feed_depth_levels(&self, _exchange: &str) -> Vec<u8> {
        self.capabilities().depth_levels.to_vec()
    }

    /// Why the live feed does not stream `exchange`, when it does not: a
    /// feed subscribe on it is refused with this, for the trader, instead
    /// of reaching a topic the broker never serves (Groww streams NSE and
    /// BSE cash, F&O and indices only, web openalgo#2341). Default: every
    /// exchange streams.
    fn feed_refusal(&self, _exchange: &str) -> Option<String> {
        None
    }

    /// The step the live feed awaits before it subscribes an instrument no
    /// consumer holds yet (`common::streaming::Presubscribe`): Samco looks
    /// a cold index's streaming id up over REST, as the web does when
    /// subscribing. Default: none, every subscription streams as the master
    /// describes it.
    fn feed_presubscribe(&self) -> Option<common::streaming::Presubscribe> {
        None
    }

    /// An update from the broker's order socket, completed before it is
    /// published: IndMoney's stream names an order it did not see placed
    /// by its id alone, and the web looks it up in the order book (bk-b
    /// RB-11). The relay awaits it within a deadline and holds no lock
    /// meanwhile. Default: as it came.
    async fn complete_order_update(
        &self,
        update: common::streaming::OrderUpdate,
    ) -> common::streaming::OrderUpdate {
        update
    }
}

/// One API plan of a broker (`Broker::rate_plans`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatePlan {
    pub id: &'static str,
    pub label: &'static str,
    /// The limits it allows, for the trader.
    pub hint: &'static str,
}

/// Broker credentials for authentication. `Debug` is redacted.
#[derive(Clone, Default, serde::Deserialize)]
pub struct BrokerCredentials {
    pub api_key: String,
    pub api_secret: Option<String>,
    pub client_id: Option<String>,
    pub password: Option<String>,
    pub totp: Option<String>,
    pub request_token: Option<String>,
    pub auth_code: Option<String>,
    /// Second key pair of the XTS family (market-data session).
    #[serde(default)]
    pub api_key_market: Option<String>,
    #[serde(default)]
    pub api_secret_market: Option<String>,
    /// The callback address the authorize URL was built with; brokers that
    /// check it on the code exchange (Upstox) repeat it byte for byte.
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

impl std::fmt::Debug for BrokerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerCredentials")
            .field("api_key", &"[REDACTED]")
            .field("client_id", &self.client_id.as_ref().map(|_| "[set]"))
            .finish_non_exhaustive()
    }
}

/// Authentication response from broker
#[derive(Clone)]
pub struct AuthResponse {
    pub auth_token: String,
    pub feed_token: Option<String>,
    pub user_id: String,
    pub user_name: Option<String>,
}

impl std::fmt::Debug for AuthResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthResponse")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// The adapters compiled into this build, sharing one symbol master.
pub struct BrokerRegistry {
    brokers: HashMap<String, Arc<dyn Broker>>,
    symbols: SymbolResolver,
    /// Why no adapter was built (BR-04: the broker HTTP client could not be
    /// configured); every sign-in reads it.
    refusal: Option<String>,
}

impl BrokerRegistry {
    /// Every production adapter, sharing a fresh symbol master; none when
    /// the broker HTTP client could not be configured (BR-04: fail closed).
    pub fn new() -> Self {
        Self::built_for(SymbolResolver::new(), common::http::shared())
    }

    /// Every adapter when there is a client to share, else none and the
    /// refusal every sign-in reads.
    pub(crate) fn built_for(symbols: SymbolResolver, client: Result<reqwest::Client>) -> Self {
        match client {
            Ok(_) => Self::with_symbols(symbols.clone(), Self::adapters(&symbols)),
            Err(e) => Self {
                brokers: HashMap::new(),
                symbols,
                refusal: Some(e.client_message()),
            },
        }
    }

    fn adapters(symbols: &SymbolResolver) -> Vec<Arc<dyn Broker>> {
        vec![
            Arc::new(angel::AngelBroker::new(symbols.clone())),
            Arc::new(zerodha::ZerodhaBroker::new(symbols.clone())),
            Arc::new(fyers::FyersBroker::new(symbols.clone())),
            Arc::new(upstox::UpstoxBroker::new(symbols.clone())),
            Arc::new(groww::GrowwBroker::new(symbols.clone())),
            Arc::new(dhan::DhanBroker::new(symbols.clone())),
            Arc::new(dhan_sandbox::broker(symbols.clone())),
            Arc::new(kotak::KotakBroker::new(symbols.clone())),
            Arc::new(fivepaisaxts::broker(symbols.clone())),
            Arc::new(jainamxts::broker(symbols.clone())),
            Arc::new(compositedge::broker(symbols.clone())),
            Arc::new(rmoney::broker(symbols.clone())),
            Arc::new(ibulls::broker(symbols.clone())),
            Arc::new(wisdom::broker(symbols.clone())),
            Arc::new(iifl::broker(symbols.clone())),
            Arc::new(iiflcapital::IiflCapitalBroker::new(symbols.clone())),
            Arc::new(shoonya::broker(symbols.clone())),
            Arc::new(flattrade::broker(symbols.clone())),
            Arc::new(tradesmart::broker(symbols.clone())),
            Arc::new(zebu::broker(symbols.clone())),
            Arc::new(firstock::FirstockBroker::new(symbols.clone())),
            Arc::new(fivepaisa::FivepaisaBroker::new(symbols.clone())),
            Arc::new(nubra::NubraBroker::new(symbols.clone())),
            Arc::new(tradejini::TradejiniBroker::new(symbols.clone())),
            Arc::new(indmoney::IndmoneyBroker::new(symbols.clone())),
            Arc::new(deltaexchange::DeltaBroker::new(symbols.clone())),
            Arc::new(arrow::ArrowBroker::new(symbols.clone())),
            Arc::new(pocketful::PocketfulBroker::new(symbols.clone())),
            Arc::new(paytm::PaytmBroker::new(symbols.clone())),
            Arc::new(hdfcsky::HdfcSkyBroker::new(symbols.clone())),
            Arc::new(hdfcsecurities::HdfcSecuritiesBroker::new(symbols.clone())),
            Arc::new(rupeezy::RupeezyBroker::new(symbols.clone())),
            Arc::new(sahi::SahiBroker::new(symbols.clone())),
            Arc::new(samco::SamcoBroker::new(symbols.clone())),
            Arc::new(aliceblue::AliceBlueBroker::new(symbols.clone())),
            Arc::new(definedge::DefinedgeBroker::new(symbols.clone())),
            Arc::new(mstock::MstockBroker::new(symbols.clone())),
            Arc::new(motilal::MotilalBroker::new(symbols.clone())),
        ]
    }

    /// Registry with exactly these adapters (tests use a mock broker).
    pub fn with(brokers: Vec<Arc<dyn Broker>>) -> Self {
        Self::with_symbols(SymbolResolver::new(), brokers)
    }

    /// Registry whose adapters were built against `symbols`.
    pub fn with_symbols(symbols: SymbolResolver, brokers: Vec<Arc<dyn Broker>>) -> Self {
        Self {
            brokers: brokers
                .into_iter()
                .map(|b| (b.id().to_string(), b))
                .collect(),
            symbols,
            refusal: None,
        }
    }

    /// The shared symbol master (the app context holds the same handle).
    pub fn symbols(&self) -> SymbolResolver {
        self.symbols.clone()
    }

    /// IDs of the adapters compiled into this build.
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.brokers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Get broker by ID
    pub fn get(&self, id: &str) -> Option<Arc<dyn Broker>> {
        self.brokers.get(id).cloned()
    }

    /// The adapter a sign-in uses, or what the trader reads when there is
    /// none: why no adapter could be built, else that this broker is not in
    /// this version.
    pub fn adapter(&self, id: &str) -> Result<Arc<dyn Broker>> {
        if let Some(why) = &self.refusal {
            return Err(AppError::Broker(why.clone()));
        }
        self.get(id).ok_or_else(|| {
            AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                setup_guide::display_name(id)
            ))
        })
    }

    /// List all available brokers
    pub fn list(&self) -> Vec<Arc<dyn Broker>> {
        self.brokers.values().cloned().collect()
    }
}

impl Default for BrokerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Lowercase a broker status the way the web does for unknown values.
pub(crate) fn lower_status(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_object_safe(_: &dyn Broker) {}

    #[test]
    fn registry_shares_one_symbol_master() {
        let reg = BrokerRegistry::new();
        assert_eq!(
            reg.ids(),
            [
                "aliceblue",
                "angel",
                "arrow",
                "compositedge",
                "definedge",
                "deltaexchange",
                "dhan",
                "dhan_sandbox",
                "firstock",
                "fivepaisa",
                "fivepaisaxts",
                "flattrade",
                "fyers",
                "groww",
                "hdfcsecurities",
                "hdfcsky",
                "ibulls",
                "iifl",
                "iiflcapital",
                "indmoney",
                "jainamxts",
                "kotak",
                "motilal",
                "mstock",
                "nubra",
                "paytm",
                "pocketful",
                "rmoney",
                "rupeezy",
                "sahi",
                "samco",
                "shoonya",
                "tradejini",
                "tradesmart",
                "upstox",
                "wisdom",
                "zebu",
                "zerodha"
            ]
        );
        let s = reg.symbols();
        s.load(vec![common::symbols::tests::row(
            "SBIN", "SBIN-EQ", "NSE", "1",
        )]);
        let z = reg.get("zerodha").unwrap();
        assert_object_safe(z.as_ref());
        assert_eq!(z.symbols().unwrap().len(), 1);
        assert_eq!(reg.get("angel").unwrap().symbols().unwrap().len(), 1);
    }

    /// BR-04: when the broker HTTP client cannot be configured no adapter is
    /// built, and a sign-in is refused with the reason, before any broker
    /// is contacted.
    #[tokio::test]
    async fn without_a_broker_client_every_sign_in_is_refused() {
        let unavailable = || Err(AppError::Broker(common::http::UNAVAILABLE.into()));
        let reg = BrokerRegistry::built_for(SymbolResolver::new(), unavailable());
        assert!(reg.ids().is_empty());
        let refused = reg.adapter("zerodha").err().map(|e| e.client_message());
        assert_eq!(refused.as_deref(), Some(common::http::UNAVAILABLE));
        let now = chrono::Utc::now();
        let t = crate::state::testing::build(
            BrokerRegistry::built_for(SymbolResolver::new(), unavailable()),
            now,
        );
        let e = crate::services::broker_auth_service::BrokerAuthService::start_oauth(
            &t.ctx, "zerodha", None,
        )
        .await
        .unwrap_err();
        assert_eq!(e.client_message(), common::http::UNAVAILABLE);
        // With a client, an unknown broker still reads as not in this version.
        let reg = BrokerRegistry::built_for(SymbolResolver::new(), common::http::shared());
        let e = reg
            .adapter("nosuchbroker")
            .err()
            .map(|e| e.client_message());
        assert!(e.is_some_and(|m| m.contains("not available in this version")));
    }
}
