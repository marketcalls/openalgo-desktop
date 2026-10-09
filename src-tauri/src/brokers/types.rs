//! Request and response types shared by every broker adapter.
//!
//! Inputs arrive in OpenAlgo vocabulary (`OrderRequest`, `QuoteKey`) and
//! are resolved once against the symbol master (`ResolvedOrder`) before an
//! adapter sees them. Outputs (`Order`, `Trade`, `Position`, `Holding`) are
//! already normalised to OpenAlgo symbols, exchanges and lowercase order
//! statuses, so every consumer reads one vocabulary.

use super::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use super::common::symbols::{SymToken, SymbolResolver};
use crate::error::{AppError, Result};
use crate::security::Secret;
use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// One master-contract row; the name the adapters have always used.
pub type SymbolData = SymToken;

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A broker session token as stored after login. `raw` is whatever the
/// adapter's `authenticate` returned (Kite: `api_key:access_token`; Fyers:
/// `app_id:access_token`; Angel: `api_key:jwt`). `Debug` never prints it.
#[derive(Clone)]
pub struct AuthToken {
    raw: Secret,
    feed: Option<Secret>,
    user_id: Option<String>,
}

impl AuthToken {
    pub fn new(raw: impl Into<String>) -> Self {
        Self {
            raw: Secret::new(raw),
            feed: None,
            user_id: None,
        }
    }

    pub fn with_feed(mut self, feed: Option<impl Into<String>>) -> Self {
        self.feed = feed.map(Secret::new);
        self
    }

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// The stored token, verbatim.
    pub fn raw(&self) -> &str {
        self.raw.expose()
    }

    /// Feed token, for brokers that issue a separate one.
    pub fn feed(&self) -> Option<&str> {
        self.feed.as_ref().map(|s| s.expose())
    }

    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// `key:secret` split at the first colon (Kite, Fyers, Angel).
    pub fn pair(&self) -> Option<(&str, &str)> {
        self.raw
            .expose()
            .split_once(':')
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
    }
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthToken")
            .field("raw", &"[REDACTED]")
            .field("feed", &self.feed.as_ref().map(|_| "[REDACTED]"))
            .field("user_id", &self.user_id)
            .finish()
    }
}

/// How the trader signs in to a broker (drives the login form).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoginKind {
    /// OAuth redirect back to `/<broker>/callback`; `param` carries the code.
    Redirect { param: &'static str },
    /// In-app form with these fields (client id, password/PIN, TOTP).
    DirectTotp { fields: &'static [&'static str] },
    /// Two forms in sequence.
    TwoStep {
        step1: &'static [&'static str],
        step2: &'static [&'static str],
    },
    /// The trader pastes an access token.
    AccessToken,
    /// API key and secret only (HMAC-signed APIs).
    ApiKeySecret,
}

/// Optional capabilities an adapter implements. Services check these before
/// calling an optional method, so an absent capability is reported as such
/// instead of being discovered by an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub history: bool,
    /// `get_multiquotes` uses a batch endpoint (not the per-symbol default).
    pub multiquotes_batch: bool,
    pub margin: bool,
    pub gtt: bool,
    pub streaming: bool,
    pub order_feed: bool,
    /// Depth levels the REST depth call and the feed can return.
    pub depth_levels: &'static [u8],
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            history: false,
            multiquotes_batch: false,
            margin: false,
            gtt: false,
            streaming: false,
            order_feed: false,
            depth_levels: &[5],
        }
    }
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

/// Place-order request in OpenAlgo vocabulary (as received by `/api/v1`).
#[derive(Debug, Clone, Deserialize)]
pub struct OrderRequest {
    pub symbol: String,
    pub exchange: String,
    /// `BUY` / `SELL`.
    pub side: String,
    pub quantity: i32,
    pub price: f64,
    /// `MARKET`, `LIMIT`, `SL`, `SL-M`.
    pub order_type: String,
    /// `CNC`, `NRML`, `MIS`.
    pub product: String,
    /// `DAY`, `IOC`.
    pub validity: String,
    pub trigger_price: Option<f64>,
    pub disclosed_quantity: Option<i32>,
    pub amo: bool,
}

/// Modify-order request in OpenAlgo vocabulary. The web's `/modifyorder`
/// requires every field; brokers such as Angel need symbol and product.
#[derive(Debug, Clone, Deserialize)]
pub struct ModifyOrderRequest {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub product: String,
    pub pricetype: String,
    pub quantity: i32,
    pub price: f64,
    pub trigger_price: f64,
    pub disclosed_quantity: i32,
}

fn parse_const<T: std::str::FromStr>(value: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|e| AppError::Validation(e.to_string()))
}

fn instrument(symbols: &SymbolResolver, symbol: &str, exchange: Exchange) -> Result<SymToken> {
    symbols.by_symbol(exchange.as_str(), symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            symbol, exchange
        ))
    })
}

/// An order validated against the OpenAlgo constants and resolved against
/// the symbol master. Adapters receive only this.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOrder {
    pub symbol: String,
    pub exchange: Exchange,
    pub action: Action,
    /// OpenAlgo units (MCX included: one CRUDEOIL lot is 100).
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: PriceType,
    pub product: Product,
    pub validity: Validity,
    pub disclosed_quantity: i64,
    pub amo: bool,
    /// Master-contract row for the instrument.
    pub instrument: SymToken,
}

impl ResolvedOrder {
    pub fn resolve(order: &OrderRequest, symbols: &SymbolResolver) -> Result<Self> {
        let exchange: Exchange = parse_const(&order.exchange)?;
        let instrument = instrument(symbols, &order.symbol, exchange)?;
        Ok(Self {
            symbol: order.symbol.clone(),
            exchange,
            action: parse_const(&order.side)?,
            quantity: i64::from(order.quantity),
            price: order.price,
            trigger_price: order.trigger_price.unwrap_or(0.0),
            pricetype: parse_const(&order.order_type)?,
            product: parse_const(&order.product)?,
            validity: if order.validity.is_empty() {
                Validity::Day
            } else {
                parse_const(&order.validity)?
            },
            disclosed_quantity: i64::from(order.disclosed_quantity.unwrap_or(0)),
            amo: order.amo,
            instrument,
        })
    }

    pub fn brsymbol(&self) -> &str {
        self.instrument.br_symbol()
    }

    pub fn token(&self) -> &str {
        &self.instrument.token
    }

    pub fn brexchange(&self) -> &str {
        self.instrument.br_exchange()
    }
}

/// A modify request validated and resolved like `ResolvedOrder`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModify {
    pub order_id: String,
    pub symbol: String,
    pub exchange: Exchange,
    pub action: Action,
    pub product: Product,
    pub pricetype: PriceType,
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub disclosed_quantity: i64,
    pub instrument: SymToken,
}

impl ResolvedModify {
    pub fn resolve(
        order_id: &str,
        m: &ModifyOrderRequest,
        symbols: &SymbolResolver,
    ) -> Result<Self> {
        let exchange: Exchange = parse_const(&m.exchange)?;
        let instrument = instrument(symbols, &m.symbol, exchange)?;
        Ok(Self {
            order_id: order_id.to_string(),
            symbol: m.symbol.clone(),
            exchange,
            action: parse_const(&m.action)?,
            product: parse_const(&m.product)?,
            pricetype: parse_const(&m.pricetype)?,
            quantity: i64::from(m.quantity),
            price: m.price,
            trigger_price: m.trigger_price,
            disclosed_quantity: i64::from(m.disclosed_quantity),
            instrument,
        })
    }

    pub fn brsymbol(&self) -> &str {
        self.instrument.br_symbol()
    }

    pub fn token(&self) -> &str {
        &self.instrument.token
    }
}

/// An exact order size for the `CRYPTO` exchange.
///
/// Every other exchange trades whole units and keeps the integer `quantity`
/// of `ResolvedOrder` / `ResolvedModify`. Crypto spot sizes may be
/// fractional (0.0005 BTC), so the order services hand crypto orders to
/// `Broker::place_order_exact` / `modify_order_exact` with this value,
/// parsed from the request text without a float round trip. It cannot be
/// built for any exchange but `CRYPTO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoQuantity(Decimal);

impl CryptoQuantity {
    /// Parse a positive size (JSON number or numeric string) for `exchange`.
    /// Refused for every exchange except `CRYPTO`.
    pub fn parse(exchange: Exchange, value: &serde_json::Value) -> Result<Self> {
        if exchange != Exchange::Crypto {
            return Err(AppError::Validation(format!(
                "Fractional quantities are only accepted on CRYPTO, not on {}.",
                exchange
            )));
        }
        let text = match value {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        };
        let bad = || AppError::Validation("Quantity must be a positive number.".into());
        let d = text
            .parse::<Decimal>()
            .or_else(|_| Decimal::from_scientific(&text))
            .map_err(|_| bad())?;
        if d <= Decimal::ZERO {
            return Err(bad());
        }
        Ok(Self(d.normalize()))
    }

    /// A whole-unit size (crypto derivatives, a whole position to close).
    pub fn whole(units: i64) -> Self {
        Self(Decimal::from(units))
    }

    /// An exact size taken from a broker's own decimal text (close-all of a
    /// spot balance). `None` unless positive.
    pub fn from_decimal(d: Decimal) -> Option<Self> {
        (d > Decimal::ZERO).then(|| Self(d.normalize()))
    }

    pub fn as_decimal(&self) -> Decimal {
        self.0
    }

    /// The size as whole units, when it has no fractional part.
    pub fn as_whole(&self) -> Option<i64> {
        if self.0.fract().is_zero() {
            self.0.to_i64()
        } else {
            None
        }
    }

    pub fn is_whole(&self) -> bool {
        self.as_whole().is_some()
    }

    /// Whole units toward zero: the integer `quantity` of the resolved order
    /// that travels alongside an exact size.
    pub fn truncated(&self) -> i64 {
        self.0.trunc().to_i64().unwrap_or(0)
    }
}

impl std::fmt::Display for CryptoQuantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A book row with its exact `CRYPTO` size. The shared rows carry whole
/// units; a crypto spot balance or fill can be fractional (0.0005 BTC), so
/// crypto venues also report the exact size, which the book services use
/// for `CRYPTO` rows only. Signed for positions (negative = short).
#[derive(Debug, Clone, PartialEq)]
pub struct ExactRow<T> {
    pub row: T,
    pub quantity: Decimal,
}

impl<T> ExactRow<T> {
    /// A row whose whole-unit quantity is already exact.
    pub fn whole(row: T, units: i64) -> Self {
        Self {
            row,
            quantity: Decimal::from(units),
        }
    }
}

/// Broker acknowledgement of an order operation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OrderResponse {
    pub order_id: String,
    pub message: Option<String>,
}

/// Result of cancelling every open order (web `cancel_all_orders_api`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CancelAllResult {
    pub cancelled: Vec<String>,
    pub failed: Vec<String>,
}

/// Result of squaring off every open position.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CloseAllResult {
    /// Order ids of the exit orders the broker accepted.
    pub placed: Vec<String>,
    /// `SYMBOL (EXCHANGE): reason` for each position still open.
    pub failed: Vec<String>,
}

impl CloseAllResult {
    /// The web's message for this outcome.
    pub fn message(&self) -> String {
        if self.failed.is_empty() {
            if self.placed.is_empty() {
                "No Open Positions Found".to_string()
            } else {
                "All Open Positions SquaredOff".to_string()
            }
        } else {
            format!(
                "{} position(s) could not be squared off and are still open: {}",
                self.failed.len(),
                self.failed.join("; ")
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Books (always OpenAlgo symbols, lowercase statuses)
// ---------------------------------------------------------------------------

/// Order-book row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub exchange_order_id: Option<String>,
    /// OpenAlgo symbol.
    pub symbol: String,
    /// OpenAlgo exchange.
    pub exchange: String,
    /// `BUY` / `SELL`.
    pub side: String,
    pub quantity: i32,
    pub filled_quantity: i32,
    pub pending_quantity: i32,
    pub price: f64,
    pub trigger_price: f64,
    pub average_price: f64,
    /// OpenAlgo price type.
    pub order_type: String,
    /// OpenAlgo product.
    pub product: String,
    /// Lowercase OpenAlgo status (`open`, `trigger pending`, `complete`,
    /// `rejected`, `cancelled`, broker extras lowercased).
    pub status: String,
    pub validity: String,
    pub order_timestamp: String,
    pub exchange_timestamp: Option<String>,
    pub rejection_reason: Option<String>,
    /// The broker's order tag, on brokers whose web book rows carry one
    /// (Kotak `GuiOrdId` as `order_tag`, web #2145); `None` elsewhere, and
    /// then the row has no such key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_tag: Option<String>,
}

/// Trade-book row (web `transform_tradebook_data`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    pub order_id: String,
    pub trade_id: String,
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub side: String,
    pub quantity: i32,
    pub average_price: f64,
    pub trade_value: f64,
    pub timestamp: String,
    /// As `Order::order_tag`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_tag: Option<String>,
}

impl From<Trade> for Order {
    /// For consumers that still render trades through the order shape.
    fn from(t: Trade) -> Self {
        Order {
            order_id: t.order_id,
            exchange_order_id: None,
            symbol: t.symbol,
            exchange: t.exchange,
            side: t.side,
            quantity: t.quantity,
            filled_quantity: t.quantity,
            pending_quantity: 0,
            price: t.average_price,
            trigger_price: 0.0,
            average_price: t.average_price,
            order_type: String::new(),
            product: t.product,
            status: "complete".to_string(),
            validity: "DAY".to_string(),
            order_timestamp: t.timestamp,
            exchange_timestamp: None,
            rejection_reason: None,
            order_tag: t.order_tag,
        }
    }
}

/// Net position row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub overnight_quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub buy_quantity: i32,
    pub buy_value: f64,
    pub sell_quantity: i32,
    pub sell_value: f64,
}

/// Holding row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Holding {
    pub symbol: String,
    pub exchange: String,
    /// Always `CNC` (demat holdings).
    pub product: String,
    pub isin: Option<String>,
    pub quantity: i32,
    pub t1_quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub close_price: f64,
    pub pnl: f64,
    pub pnl_percentage: f64,
    pub current_value: f64,
}

/// Holdings plus the broker's own totals, when it reports them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoldingsBook {
    pub holdings: Vec<Holding>,
    pub totals: Option<PortfolioStats>,
}

impl HoldingsBook {
    /// The broker's totals, else computed from the rows.
    pub fn statistics(&self) -> PortfolioStats {
        self.totals
            .unwrap_or_else(|| PortfolioStats::from_holdings(&self.holdings))
    }
}

/// Portfolio totals (web `calculate_portfolio_statistics`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct PortfolioStats {
    pub totalholdingvalue: f64,
    pub totalinvvalue: f64,
    pub totalprofitandloss: f64,
    pub totalpnlpercentage: f64,
}

impl PortfolioStats {
    pub fn from_holdings(h: &[Holding]) -> Self {
        let value: f64 = h.iter().map(|x| x.ltp * f64::from(x.quantity)).sum();
        let inv: f64 = h
            .iter()
            .map(|x| x.average_price * f64::from(x.quantity))
            .sum();
        let pnl: f64 = h.iter().map(|x| x.pnl).sum();
        Self {
            totalholdingvalue: value,
            totalinvvalue: inv,
            totalprofitandloss: pnl,
            totalpnlpercentage: if inv != 0.0 { pnl / inv * 100.0 } else { 0.0 },
        }
    }
}

/// Funds. The web `/funds` fields are `available_cash` (availablecash),
/// `collateral`, `m2m_unrealized`, `m2m_realized`, `utilised_debits`; the rest
/// are extras for the dashboard.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Funds {
    pub available_cash: f64,
    pub used_margin: f64,
    pub total_margin: f64,
    pub opening_balance: f64,
    pub payin: f64,
    pub payout: f64,
    pub span: f64,
    pub exposure: f64,
    pub collateral: f64,
    #[serde(default)]
    pub m2m_unrealized: f64,
    #[serde(default)]
    pub m2m_realized: f64,
    #[serde(default)]
    pub utilised_debits: f64,
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

/// An instrument in OpenAlgo terms. Replaces the old `(String, String)`
/// tuple whose order was ambiguous (audit 0.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QuoteKey {
    pub exchange: String,
    pub symbol: String,
}

impl QuoteKey {
    pub fn new(exchange: impl Into<String>, symbol: impl Into<String>) -> Self {
        Self {
            exchange: exchange.into(),
            symbol: symbol.into(),
        }
    }
}

/// Quote (web `/quotes` data; `close` is the previous close).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    pub exchange: String,
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_qty: i64,
    pub ask_qty: i64,
    pub oi: i64,
    pub change: f64,
    pub change_percent: f64,
    pub timestamp: String,
}

/// One entry of a multiquotes answer: data or a per-symbol error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuoteResult {
    pub symbol: String,
    pub exchange: String,
    pub data: Option<Quote>,
    pub error: Option<String>,
}

/// Depth level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i64,
    pub orders: i64,
}

/// Market depth (web `/depth` data), padded to the requested levels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MarketDepth {
    pub symbol: String,
    pub exchange: String,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
    pub ltp: f64,
    pub ltq: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub prev_close: f64,
    pub volume: i64,
    pub oi: i64,
    pub total_buy_qty: i64,
    pub total_sell_qty: i64,
}

/// History request: an instrument, an OpenAlgo interval key from the
/// broker's `timeframe_map`, and an inclusive date range.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRequest {
    pub key: QuoteKey,
    pub interval: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

/// One candle; `timestamp` is epoch seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Candle {
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: i64,
}

/// A downloaded master contract: the `SymToken` rows plus, for venues that
/// quote one (crypto), each row's contract multiplier keyed by token (web
/// `SymToken.contract_value`). Indian brokers leave the map empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MasterContract {
    pub rows: Vec<SymToken>,
    pub contract_values: std::collections::HashMap<String, f64>,
}

impl MasterContract {
    pub fn new(rows: Vec<SymToken>) -> Self {
        Self {
            rows,
            contract_values: Default::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Margin
// ---------------------------------------------------------------------------

/// One leg of a margin request (web `/margin` `positions[]`).
#[derive(Debug, Clone, PartialEq)]
pub struct MarginLeg {
    pub key: QuoteKey,
    pub action: Action,
    pub quantity: i64,
    pub product: Product,
    pub pricetype: PriceType,
    pub price: f64,
    pub trigger_price: f64,
}

/// Web `/margin` data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct MarginResult {
    pub total_margin_required: f64,
    pub span_margin: f64,
    pub exposure_margin: f64,
}

// ---------------------------------------------------------------------------
// GTT
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GttTriggerType {
    /// One trigger, one order.
    Single,
    /// Stop-loss and target legs; whichever triggers first cancels the other.
    Oco,
}

/// Place or modify a GTT (web flat GTT payload).
#[derive(Debug, Clone, PartialEq)]
pub struct GttRequest {
    pub key: QuoteKey,
    pub trigger_type: GttTriggerType,
    pub action: Action,
    pub product: Product,
    pub quantity: i64,
    pub pricetype: PriceType,
    /// SINGLE: limit price.
    pub price: f64,
    /// SINGLE: trigger.
    pub trigger_price: f64,
    /// OCO: stop-loss trigger and limit.
    pub triggerprice_sl: f64,
    pub stoploss: f64,
    /// OCO: target trigger and limit.
    pub triggerprice_tg: f64,
    pub target: f64,
    /// Fetched from the broker when absent.
    pub last_price: Option<f64>,
}

/// GTT acknowledgement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GttResponse {
    pub trigger_id: String,
}

/// One leg of a GTT in the book.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GttLeg {
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub pricetype: String,
    pub product: String,
}

/// GTT book row (web `map_gtt_book`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GttOrder {
    pub trigger_id: String,
    pub trigger_type: String,
    pub status: String,
    pub symbol: String,
    pub exchange: String,
    pub trigger_prices: Vec<f64>,
    pub last_price: f64,
    pub legs: Vec<GttLeg>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row;

    fn symbols() -> SymbolResolver {
        let r = SymbolResolver::new();
        r.load(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]);
        r
    }

    fn order() -> OrderRequest {
        OrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 10,
            price: 0.0,
            order_type: "MARKET".into(),
            product: "MIS".into(),
            validity: String::new(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        }
    }

    #[test]
    fn auth_token_redacts_and_splits() {
        let t = AuthToken::new("key123:access456").with_feed(Some("feed"));
        let dbg = format!("{:?}", t);
        assert!(!dbg.contains("access456"));
        assert!(!dbg.contains("feed\""));
        assert_eq!(t.pair(), Some(("key123", "access456")));
        assert_eq!(AuthToken::new("nocolon").pair(), None);
        assert_eq!(AuthToken::new(":x").pair(), None);
    }

    #[test]
    fn resolve_order_validates_and_resolves() {
        let r = symbols();
        let o = ResolvedOrder::resolve(&order(), &r).unwrap();
        assert_eq!(o.brsymbol(), "SBIN-EQ");
        assert_eq!(o.token(), "3045");
        assert_eq!(o.action, Action::Buy);
        assert_eq!(o.validity, Validity::Day);

        let mut bad = order();
        bad.order_type = "STOP".into();
        let e = ResolvedOrder::resolve(&bad, &r).unwrap_err();
        assert_eq!(e.client_message(), "Invalid pricetype 'STOP'");

        let mut missing = order();
        missing.symbol = "NOPE".into();
        let e = ResolvedOrder::resolve(&missing, &r).unwrap_err();
        assert!(e.client_message().contains("NOPE was not found on NSE"));
    }

    #[test]
    fn close_all_messages_match_web() {
        assert_eq!(
            CloseAllResult::default().message(),
            "No Open Positions Found"
        );
        let ok = CloseAllResult {
            placed: vec!["1".into()],
            failed: vec![],
        };
        assert_eq!(ok.message(), "All Open Positions SquaredOff");
        let bad = CloseAllResult {
            placed: vec![],
            failed: vec!["SBIN (NSE): margin".into()],
        };
        assert!(bad
            .message()
            .starts_with("1 position(s) could not be squared off"));
    }

    #[test]
    fn crypto_quantity_is_exact_and_crypto_only() {
        use serde_json::json;
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!(0.0005)).unwrap();
        assert_eq!(q.to_string(), "0.0005");
        assert!(!q.is_whole());
        assert_eq!(q.truncated(), 0);
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!("0.123456789")).unwrap();
        assert_eq!(q.to_string(), "0.123456789");
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!(1.5)).unwrap();
        assert_eq!((q.truncated(), q.as_whole()), (1, None));
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!(3)).unwrap();
        assert_eq!(q.as_whole(), Some(3));
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!(2.0)).unwrap();
        assert_eq!((q.as_whole(), q.to_string().as_str()), (Some(2), "2"));
        let q = CryptoQuantity::parse(Exchange::Crypto, &json!("1e-4")).unwrap();
        assert_eq!(q.to_string(), "0.0001");
        for bad in [json!(0), json!(-1.5), json!("abc"), json!(null), json!("")] {
            assert!(
                CryptoQuantity::parse(Exchange::Crypto, &bad).is_err(),
                "{}",
                bad
            );
        }
        // Every other exchange stays whole-unit only.
        for ex in Exchange::ALL.iter().filter(|e| **e != Exchange::Crypto) {
            assert!(CryptoQuantity::parse(*ex, &json!(1)).is_err(), "{}", ex);
            assert!(CryptoQuantity::parse(*ex, &json!(0.5)).is_err(), "{}", ex);
        }
        assert_eq!(CryptoQuantity::whole(7).as_whole(), Some(7));
        assert!(CryptoQuantity::from_decimal(Decimal::ZERO).is_none());
    }

    #[test]
    fn portfolio_stats() {
        let h = Holding {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            product: "CNC".into(),
            isin: None,
            quantity: 10,
            t1_quantity: 0,
            average_price: 100.0,
            ltp: 110.0,
            close_price: 0.0,
            pnl: 100.0,
            pnl_percentage: 10.0,
            current_value: 1100.0,
        };
        let s = PortfolioStats::from_holdings(&[h]);
        assert_eq!(s.totalholdingvalue, 1100.0);
        assert_eq!(s.totalinvvalue, 1000.0);
        assert_eq!(s.totalpnlpercentage, 10.0);
        assert_eq!(PortfolioStats::from_holdings(&[]).totalpnlpercentage, 0.0);
    }
}
