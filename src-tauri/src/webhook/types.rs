//! Webhook and REST API types
//!
//! This module provides types compatible with OpenAlgo SDK for:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - REST API endpoints (/api/v1/*)
//!
//! Note: The OpenAlgo Python SDK sends all numeric values as strings,
//! so we use custom deserializers to accept both strings and numbers.

use serde::{Deserialize, Deserializer, Serialize};

// ============================================================================
// Custom Deserializers for SDK Compatibility
// ============================================================================

/// Deserialize a value that can be either a number or a string representation of a number
fn deserialize_flexible_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FlexibleInt {
        Int(i32),
        Float(f64),
        Str(String),
    }

    match FlexibleInt::deserialize(deserializer)? {
        FlexibleInt::Int(i) => Ok(i),
        FlexibleInt::Float(f) => Ok(f as i32),
        FlexibleInt::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

fn deserialize_flexible_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FlexibleFloat {
        Float(f64),
        Int(i64),
        Str(String),
    }

    match FlexibleFloat::deserialize(deserializer)? {
        FlexibleFloat::Float(f) => Ok(f),
        FlexibleFloat::Int(i) => Ok(i as f64),
        FlexibleFloat::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

#[allow(dead_code)]
fn deserialize_optional_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FlexibleOptInt {
        None,
        Int(i32),
        Float(f64),
        Str(String),
    }

    match Option::<FlexibleOptInt>::deserialize(deserializer)? {
        None => Ok(None),
        Some(FlexibleOptInt::None) => Ok(None),
        Some(FlexibleOptInt::Int(i)) => Ok(Some(i)),
        Some(FlexibleOptInt::Float(f)) => Ok(Some(f as i32)),
        Some(FlexibleOptInt::Str(s)) if s.is_empty() => Ok(None),
        Some(FlexibleOptInt::Str(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[allow(dead_code)]
fn deserialize_optional_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum FlexibleOptFloat {
        None,
        Float(f64),
        Int(i64),
        Str(String),
    }

    match Option::<FlexibleOptFloat>::deserialize(deserializer)? {
        None => Ok(None),
        Some(FlexibleOptFloat::None) => Ok(None),
        Some(FlexibleOptFloat::Float(f)) => Ok(Some(f)),
        Some(FlexibleOptFloat::Int(i)) => Ok(Some(i as f64)),
        Some(FlexibleOptFloat::Str(s)) if s.is_empty() => Ok(None),
        Some(FlexibleOptFloat::Str(s)) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Default value for i32 fields
fn default_i32() -> i32 {
    0
}

/// Default value for f64 fields
fn default_f64() -> f64 {
    0.0
}

// ============================================================================
// Common Types
// ============================================================================

/// Standard API response format (OpenAlgo SDK compatible)
#[derive(Debug, Clone, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orderid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success() -> Self {
        Self {
            status: "success".to_string(),
            message: None,
            data: None,
            orderid: None,
            mode: None,
        }
    }

    pub fn success_with_message(message: &str) -> Self {
        Self {
            status: "success".to_string(),
            message: Some(message.to_string()),
            data: None,
            orderid: None,
            mode: None,
        }
    }

    pub fn success_with_data(data: T) -> Self {
        Self {
            status: "success".to_string(),
            message: None,
            data: Some(data),
            orderid: None,
            mode: None,
        }
    }

    pub fn success_with_orderid(orderid: &str) -> Self {
        Self {
            status: "success".to_string(),
            message: None,
            data: None,
            orderid: Some(orderid.to_string()),
            mode: None,
        }
    }

    pub fn error(message: &str) -> Self {
        Self {
            status: "error".to_string(),
            message: Some(message.to_string()),
            data: None,
            orderid: None,
            mode: None,
        }
    }

    pub fn with_mode(mut self, mode: &str) -> Self {
        self.mode = Some(mode.to_string());
        self
    }
}

/// Empty data type for responses without data
#[derive(Debug, Clone, Serialize)]
pub struct Empty {}

// ============================================================================
// REST API Request Types (OpenAlgo SDK Compatible)
// ============================================================================

/// Place order request - POST /api/v1/placeorder
/// Compatible with OpenAlgo SDK which sends numeric values as strings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
    #[serde(default = "default_i32", deserialize_with = "deserialize_flexible_i32")]
    pub disclosed_quantity: i32,
}

/// Place smart order request - POST /api/v1/placesmartorder
/// Compatible with OpenAlgo SDK which sends numeric values as strings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceSmartOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub position_size: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
    #[serde(default = "default_i32", deserialize_with = "deserialize_flexible_i32")]
    pub disclosed_quantity: i32,
}

/// Modify order request - POST /api/v1/modifyorder
/// Compatible with OpenAlgo SDK which sends numeric values as strings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub orderid: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    pub pricetype: String,
    pub product: String,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
    #[serde(default = "default_i32", deserialize_with = "deserialize_flexible_i32")]
    pub disclosed_quantity: i32,
}

/// Cancel order request - POST /api/v1/cancelorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub orderid: String,
}

/// Cancel all orders request - POST /api/v1/cancelallorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAllOrdersRequest {
    pub apikey: String,
    pub strategy: String,
}

/// Close position request - POST /api/v1/closeposition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosePositionRequest {
    pub apikey: String,
    pub strategy: String,
}

/// API key only request (for orderbook, tradebook, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRequest {
    pub apikey: String,
}

/// Quote request - POST /api/v1/quotes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
}

/// Basket order request - POST /api/v1/basketorder
/// Places multiple orders in a single request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub orders: Vec<BasketOrderItem>,
}

/// Individual order in a basket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketOrderItem {
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
}

/// Split order request - POST /api/v1/splitorder
/// Splits a large order into smaller chunks to avoid rejection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default = "default_split_size", deserialize_with = "deserialize_flexible_i32")]
    pub splitsize: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
}

/// Order status request - POST /api/v1/orderstatus
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusRequest {
    pub apikey: String,
    pub strategy: String,
    pub orderid: String,
}

/// Open position request - POST /api/v1/openposition
/// Get position for a specific symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPositionRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    #[serde(default = "default_product")]
    pub product: String,
}

/// Market depth request - POST /api/v1/depth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
}

/// Symbol info request - POST /api/v1/symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
}

/// Historical data request - POST /api/v1/history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
}

/// Intervals request - POST /api/v1/intervals
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntervalsRequest {
    pub apikey: String,
}

/// Analyzer status request - POST /api/v1/analyzer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerRequest {
    pub apikey: String,
}

/// Analyzer toggle request - POST /api/v1/analyzer/toggle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerToggleRequest {
    pub apikey: String,
    pub mode: bool,
}

/// Margin request - POST /api/v1/margin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginRequest {
    pub apikey: String,
    pub positions: Vec<MarginPosition>,
}

/// Position for margin calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginPosition {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub product: String,
    pub pricetype: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub price: f64,
    #[serde(default = "default_f64", deserialize_with = "deserialize_flexible_f64")]
    pub trigger_price: f64,
}

/// Multi-quotes request - POST /api/v1/multiquotes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiQuotesRequest {
    pub apikey: String,
    pub symbols: Vec<SymbolExchangePair>,
}

/// Symbol-exchange pair for multi-quotes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolExchangePair {
    pub symbol: String,
    pub exchange: String,
}

/// Search request - POST /api/v1/search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub apikey: String,
    pub query: String,
    #[serde(default)]
    pub exchange: Option<String>,
}

/// Expiry request - POST /api/v1/expiry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
    pub instrumenttype: String,
}

/// Instruments request - GET /api/v1/instruments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentsRequest {
    pub apikey: String,
    #[serde(default)]
    pub exchange: Option<String>,
}

/// Synthetic future request - POST /api/v1/syntheticfuture
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticFutureRequest {
    pub apikey: String,
    pub underlying: String,
    pub exchange: String,
    pub expiry_date: String,
}

/// Option chain request - POST /api/v1/optionchain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionChainRequest {
    pub apikey: String,
    pub underlying: String,
    pub exchange: String,
    #[serde(default)]
    pub expiry_date: Option<String>,
    #[serde(default)]
    pub strike_count: Option<i32>,
}

/// Option Greeks request - POST /api/v1/optiongreeks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionGreeksRequest {
    pub apikey: String,
    pub symbol: String,
    pub exchange: String,
    #[serde(default)]
    pub interest_rate: Option<f64>,
    #[serde(default)]
    pub forward_price: Option<f64>,
    #[serde(default)]
    pub underlying_symbol: Option<String>,
    #[serde(default)]
    pub underlying_exchange: Option<String>,
    #[serde(default)]
    pub expiry_time: Option<String>,
}

/// Options order request - POST /api/v1/optionsorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionsOrderRequest {
    pub apikey: String,
    #[serde(default = "default_strategy")]
    pub strategy: String,
    pub underlying: String,
    pub exchange: String,
    #[serde(default)]
    pub strike_int: Option<i32>,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub offset: i32,
    pub option_type: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default)]
    pub expiry_date: Option<String>,
    #[serde(default = "default_pricetype")]
    pub price_type: String,
    #[serde(default = "default_product")]
    pub product: String,
}

/// Options symbol request - POST /api/v1/optionsymbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionSymbolRequest {
    pub apikey: String,
    #[serde(default)]
    pub strategy: Option<String>,
    pub underlying: String,
    pub exchange: String,
    #[serde(default)]
    pub strike_int: Option<i32>,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub offset: i32,
    pub option_type: String,
    #[serde(default)]
    pub expiry_date: Option<String>,
}

/// Options multi-order request - POST /api/v1/optionsmultiorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionsMultiOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub underlying: String,
    pub exchange: String,
    pub legs: Vec<OptionLeg>,
    #[serde(default)]
    pub expiry_date: Option<String>,
    #[serde(default)]
    pub strike_int: Option<i32>,
}

/// Option leg for multi-order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptionLeg {
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub offset: i32,
    pub option_type: String,
    pub action: String,
    #[serde(deserialize_with = "deserialize_flexible_i32")]
    pub quantity: i32,
    #[serde(default = "default_pricetype")]
    pub price_type: String,
    #[serde(default = "default_product")]
    pub product: String,
}

// ============================================================================
// REST API Response Data Types
// ============================================================================

/// Order in orderbook
/// SDK expects: symbol, exchange, action, quantity, price, trigger_price, pricetype, product, orderid, order_status, timestamp
#[derive(Debug, Clone, Serialize)]
pub struct OrderData {
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i32,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: String,
    pub product: String,
    pub orderid: String,
    pub order_status: String,
    /// Time of order (HH:MM:SS format)
    pub timestamp: String,
}

/// Trade in tradebook
/// SDK expects: symbol, exchange, product, action, quantity, average_price, trade_value, orderid, timestamp
#[derive(Debug, Clone, Serialize)]
pub struct TradeData {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub action: String,
    pub quantity: i32,
    pub average_price: f64,
    pub trade_value: f64,
    pub orderid: String,
    /// Time of trade (HH:MM:SS format)
    pub timestamp: String,
}

/// Position in positionbook
/// SDK expects: symbol, exchange, product, quantity, average_price, ltp, pnl
#[derive(Debug, Clone, Serialize)]
pub struct PositionData {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
}

/// Holding
/// SDK expects: symbol, exchange, quantity, product, pnl, pnlpercent
#[derive(Debug, Clone, Serialize)]
pub struct HoldingData {
    pub symbol: String,
    pub exchange: String,
    pub quantity: i32,
    pub product: String,
    pub pnl: f64,
    /// P&L percentage (no underscore to match SDK)
    pub pnlpercent: f64,
}

/// Funds data
#[derive(Debug, Clone, Serialize)]
pub struct FundsData {
    pub availablecash: f64,
    pub collateral: f64,
    pub m2munrealized: f64,
    pub m2mrealized: f64,
    pub utiliseddebits: f64,
}

/// Quote data
/// SDK expects: bid, ask, open, high, low, ltp, prev_close, volume, oi
#[derive(Debug, Clone, Serialize)]
pub struct QuoteData {
    pub bid: f64,
    pub ask: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub ltp: f64,
    pub prev_close: f64,
    pub volume: i64,
    pub oi: i64,
}

/// Basket order response - individual order result
#[derive(Debug, Clone, Serialize)]
pub struct BasketOrderResult {
    pub symbol: String,
    pub exchange: String,
    pub orderid: Option<String>,
    pub status: String,
    pub message: Option<String>,
}

/// Split order response
#[derive(Debug, Clone, Serialize)]
pub struct SplitOrderResult {
    pub total_quantity: i32,
    pub split_size: i32,
    pub num_orders: i32,
    pub orderids: Vec<String>,
}

/// Order status response
#[derive(Debug, Clone, Serialize)]
pub struct OrderStatusData {
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i32,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: String,
    pub product: String,
    pub order_status: String,
    pub filled_quantity: i32,
    pub pending_quantity: i32,
    pub average_price: f64,
    pub timestamp: String,
}

/// Open position response (single position for symbol)
#[derive(Debug, Clone, Serialize)]
pub struct OpenPositionData {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
}

/// Market depth level
#[derive(Debug, Clone, Serialize)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i32,
    pub orders: i32,
}

/// Market depth data
#[derive(Debug, Clone, Serialize)]
pub struct DepthData {
    pub symbol: String,
    pub exchange: String,
    pub buy: Vec<DepthLevel>,
    pub sell: Vec<DepthLevel>,
    pub ltp: f64,
    pub ltq: i32,
    pub volume: i64,
    pub oi: i64,
    pub totalbuyqty: i64,
    pub totalsellqty: i64,
}

/// Symbol info data
#[derive(Debug, Clone, Serialize)]
pub struct SymbolData {
    pub symbol: String,
    pub exchange: String,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option_type: Option<String>,
    pub lot_size: i32,
    pub tick_size: f64,
    pub instrument_type: String,
}

/// Historical data candle (OHLCV)
#[derive(Debug, Clone, Serialize)]
pub struct Candle {
    pub timestamp: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oi: Option<i64>,
}

/// Historical data response
#[derive(Debug, Clone, Serialize)]
pub struct HistoryData {
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub candles: Vec<Candle>,
}

/// Supported intervals
#[derive(Debug, Clone, Serialize)]
pub struct IntervalsData {
    pub intervals: Vec<String>,
}

/// Analyzer status data
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzerData {
    pub analyze_mode: bool,
    pub mode: String,
    pub total_logs: i64,
}

/// Margin data
#[derive(Debug, Clone, Serialize)]
pub struct MarginData {
    pub total_margin_required: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_margin: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure_margin: Option<f64>,
}

/// Multi-quotes data (array of quotes)
pub type MultiQuotesData = Vec<QuoteData>;

/// Search result item
#[derive(Debug, Clone, Serialize)]
pub struct SearchResultItem {
    pub symbol: String,
    pub name: String,
    pub exchange: String,
    pub token: String,
    pub instrumenttype: String,
    pub lotsize: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
}

/// Expiry data
#[derive(Debug, Clone, Serialize)]
pub struct ExpiryData {
    pub expiry_dates: Vec<String>,
}

/// Instruments data (list of instruments)
pub type InstrumentsData = Vec<InstrumentItem>;

/// Instrument item
#[derive(Debug, Clone, Serialize)]
pub struct InstrumentItem {
    pub symbol: String,
    pub brsymbol: String,
    pub name: String,
    pub exchange: String,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike: Option<f64>,
    pub lotsize: i32,
    pub instrumenttype: String,
    pub tick_size: f64,
}

/// Synthetic future data
#[derive(Debug, Clone, Serialize)]
pub struct SyntheticFutureData {
    pub underlying: String,
    pub underlying_ltp: f64,
    pub expiry: String,
    pub atm_strike: f64,
    pub synthetic_future_price: f64,
}

/// Option chain data
#[derive(Debug, Clone, Serialize)]
pub struct OptionChainData {
    pub underlying: String,
    pub underlying_ltp: f64,
    pub expiry: String,
    pub atm_strike: f64,
    pub strikes: Vec<OptionStrike>,
}

/// Option strike in chain
#[derive(Debug, Clone, Serialize)]
pub struct OptionStrike {
    pub strike: f64,
    pub ce_symbol: String,
    pub ce_ltp: f64,
    pub ce_oi: i64,
    pub ce_volume: i64,
    pub ce_iv: f64,
    pub pe_symbol: String,
    pub pe_ltp: f64,
    pub pe_oi: i64,
    pub pe_volume: i64,
    pub pe_iv: f64,
}

/// Option Greeks data
#[derive(Debug, Clone, Serialize)]
pub struct OptionGreeksData {
    pub symbol: String,
    pub ltp: f64,
    pub iv: f64,
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}

/// Options order result
#[derive(Debug, Clone, Serialize)]
pub struct OptionsOrderResult {
    pub symbol: String,
    pub orderid: String,
}

/// Option symbol result
#[derive(Debug, Clone, Serialize)]
pub struct OptionSymbolResult {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub strike: f64,
    pub option_type: String,
    pub expiry: String,
}

/// Options multi-order result
#[derive(Debug, Clone, Serialize)]
pub struct OptionsMultiOrderResult {
    pub results: Vec<OptionsOrderLegResult>,
}

/// Options multi-order leg result
#[derive(Debug, Clone, Serialize)]
pub struct OptionsOrderLegResult {
    pub leg: i32,
    pub symbol: String,
    pub orderid: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ============================================================================
// Dynamic Webhook Types (Strategy-based)
// ============================================================================

/// Webhook payload - flexible format supporting multiple platforms
/// The webhook_id in the URL determines the strategy, not the payload
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookPayload {
    // Symbol identification (multiple field names supported)
    pub symbol: Option<String>,
    pub ticker: Option<String>,

    // Action (multiple field names supported)
    pub action: Option<String>,
    pub order: Option<String>,
    pub side: Option<String>,

    // Position size for smart orders (TradingView strategy alerts)
    pub position_size: Option<i32>,

    // Optional overrides (usually from strategy config)
    pub exchange: Option<String>,
    pub quantity: Option<i32>,
    #[serde(alias = "qty")]
    pub qty_alias: Option<i32>,
    pub pricetype: Option<String>,
    #[serde(alias = "orderType")]
    pub order_type: Option<String>,
    pub product: Option<String>,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    #[serde(alias = "triggerPrice")]
    pub trigger_price_alias: Option<f64>,

    // Chartink specific - comma-separated stock list
    pub stocks: Option<String>,
    pub scan_name: Option<String>,
}

impl WebhookPayload {
    /// Get the symbol from various possible field names
    pub fn get_symbol(&self) -> Option<String> {
        self.symbol.clone().or(self.ticker.clone())
    }

    /// Get the action from various possible field names
    pub fn get_action(&self) -> Option<String> {
        self.action.clone()
            .or(self.order.clone())
            .or(self.side.clone())
            .or_else(|| {
                // Chartink: derive action from scan_name
                self.scan_name.as_ref().and_then(|name| {
                    let name_upper = name.to_uppercase();
                    if name_upper.contains("BUY") || name_upper.contains("COVER") {
                        Some("BUY".to_string())
                    } else if name_upper.contains("SELL") || name_upper.contains("SHORT") {
                        Some("SELL".to_string())
                    } else {
                        None
                    }
                })
            })
    }

    /// Get quantity from various possible field names
    pub fn get_quantity(&self) -> Option<i32> {
        self.quantity.or(self.qty_alias)
    }

    /// Get trigger price from various possible field names
    pub fn get_trigger_price(&self) -> Option<f64> {
        self.trigger_price.or(self.trigger_price_alias)
    }

    /// Get price type from various possible field names
    pub fn get_pricetype(&self) -> String {
        self.pricetype.clone()
            .or(self.order_type.clone())
            .unwrap_or_else(|| "MARKET".to_string())
            .to_uppercase()
    }

    /// Check if this is a Chartink multi-stock payload
    pub fn is_chartink_multi_stock(&self) -> bool {
        self.stocks.is_some()
    }

    /// Get list of symbols (for Chartink multi-stock)
    pub fn get_symbols(&self) -> Vec<String> {
        if let Some(stocks) = &self.stocks {
            stocks.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else if let Some(symbol) = self.get_symbol() {
            vec![symbol]
        } else {
            vec![]
        }
    }
}

/// Processed webhook alert (after strategy lookup and validation)
#[derive(Debug, Clone, Serialize)]
pub struct ProcessedAlert {
    pub strategy_id: i64,
    pub strategy_name: String,
    pub webhook_id: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i32,
    pub product: String,
    pub pricetype: String,
    pub price: f64,
    pub trigger_price: f64,
    pub position_size: Option<i32>,
    pub is_smart_order: bool,
    pub timestamp: String,
}

/// Webhook processing result
#[derive(Debug, Clone, Serialize)]
pub struct WebhookResult {
    pub alerts_processed: usize,
    pub orders_queued: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Default Functions
// ============================================================================

fn default_pricetype() -> String {
    "MARKET".to_string()
}

fn default_product() -> String {
    "MIS".to_string()
}

fn default_split_size() -> i32 {
    100
}

fn default_strategy() -> String {
    "Python".to_string()
}

#[allow(dead_code)]
fn default_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ============================================================================
// Legacy Types (for backward compatibility during migration)
// ============================================================================

/// Legacy webhook response (deprecated, use ApiResponse)
#[derive(Debug, Clone, Serialize)]
pub struct WebhookResponse {
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alerts_processed: Option<usize>,
}

impl WebhookResponse {
    pub fn success(message: &str) -> Self {
        Self {
            status: "success".to_string(),
            message: message.to_string(),
            order_id: None,
            alerts_processed: None,
        }
    }

    pub fn error(message: &str) -> Self {
        Self {
            status: "error".to_string(),
            message: message.to_string(),
            order_id: None,
            alerts_processed: None,
        }
    }

    pub fn with_order_id(mut self, order_id: &str) -> Self {
        self.order_id = Some(order_id.to_string());
        self
    }

    pub fn with_count(mut self, count: usize) -> Self {
        self.alerts_processed = Some(count);
        self
    }
}
