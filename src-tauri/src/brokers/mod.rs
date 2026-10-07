//! Broker adapters.
//!
//! Every adapter implements `Broker`: translation between the broker's
//! shapes and OpenAlgo's. Inputs are resolved OpenAlgo requests; outputs
//! (books, quotes, ticks) carry OpenAlgo symbols and exchanges. Optional
//! capabilities (margin, GTT, streaming) default to `AppError::Unsupported`.
//! The trait is object safe: the registry hands out `Arc<dyn Broker>`.

pub mod angel;
pub mod arrow;
pub mod catalog;
pub mod common;
pub mod compositedge;
pub mod deltaexchange;
pub mod dhan;
pub mod dhan_sandbox;
pub mod families;
pub mod firstock;
pub mod fivepaisaxts;
pub mod flattrade;
pub mod fyers;
pub mod groww;
pub mod hdfcsecurities;
pub mod hdfcsky;
pub mod ibulls;
pub mod iifl;
pub mod jainamxts;
pub mod kotak;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod paytm;
pub mod pocketful;
pub mod rmoney;
pub mod shoonya;
pub mod tradesmart;
pub mod types;
pub mod upstox;
pub mod wisdom;
pub mod zebu;
pub mod zerodha;

use crate::error::{AppError, Result};
use async_trait::async_trait;
use common::mapping::{Exchange, OrderStatus, PriceType, Product};
use common::streaming::BrokerFeed;
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

    // ---- auth ----

    /// Exchange login credentials (or an OAuth code) for a session token.
    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse>;

    // ---- orders ----

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse>;

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
            let outcome = match ResolvedOrder::resolve(&req, &symbols) {
                Ok(order) => self.place_order(auth, &order).await,
                Err(e) => Err(e),
            };
            match outcome {
                Ok(r) if !r.order_id.is_empty() => result.placed.push(r.order_id),
                Ok(_) => result.failed.push(format!("{}: order was refused", label)),
                Err(e) => result
                    .failed
                    .push(format!("{}: {}", label, e.client_message())),
            }
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

    // ---- books (OpenAlgo symbols, lowercase statuses) ----

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>>;
    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>>;
    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>>;

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

    /// Five-level depth (padded with zero levels).
    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth>;

    /// Candles for `req`, epoch seconds, oldest first, with OI.
    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>>;

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

    // ---- streaming ----

    /// A streaming adapter for the market-data feed.
    fn create_feed(&self, _auth: &AuthToken) -> Result<Box<dyn BrokerFeed>> {
        Err(AppError::Unsupported("streaming"))
    }
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
}

impl BrokerRegistry {
    /// Every production adapter, sharing a fresh symbol master.
    pub fn new() -> Self {
        let symbols = SymbolResolver::new();
        let brokers: Vec<Arc<dyn Broker>> = vec![
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
            Arc::new(shoonya::broker(symbols.clone())),
            Arc::new(flattrade::broker(symbols.clone())),
            Arc::new(tradesmart::broker(symbols.clone())),
            Arc::new(zebu::broker(symbols.clone())),
            Arc::new(firstock::FirstockBroker::new(symbols.clone())),
            Arc::new(deltaexchange::DeltaBroker::new(symbols.clone())),
            Arc::new(arrow::ArrowBroker::new(symbols.clone())),
            Arc::new(pocketful::PocketfulBroker::new(symbols.clone())),
            Arc::new(paytm::PaytmBroker::new(symbols.clone())),
            Arc::new(hdfcsky::HdfcSkyBroker::new(symbols.clone())),
            Arc::new(hdfcsecurities::HdfcSecuritiesBroker::new(symbols.clone())),
        ];
        Self::with_symbols(symbols, brokers)
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
                "angel",
                "arrow",
                "compositedge",
                "deltaexchange",
                "dhan",
                "dhan_sandbox",
                "firstock",
                "fivepaisaxts",
                "flattrade",
                "fyers",
                "groww",
                "hdfcsecurities",
                "hdfcsky",
                "ibulls",
                "iifl",
                "jainamxts",
                "kotak",
                "paytm",
                "pocketful",
                "rmoney",
                "shoonya",
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
}
