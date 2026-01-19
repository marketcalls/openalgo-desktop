//! Common broker types

use serde::{Deserialize, Serialize};

/// Order request for placing new orders
#[derive(Debug, Clone, Deserialize)]
pub struct OrderRequest {
    pub symbol: String,
    pub exchange: String,
    pub side: String,         // BUY or SELL
    pub quantity: i32,
    pub price: f64,
    pub order_type: String,   // MARKET, LIMIT, SL, SL-M
    pub product: String,      // CNC, MIS, NRML
    pub validity: String,     // DAY, IOC
    pub trigger_price: Option<f64>,
    pub disclosed_quantity: Option<i32>,
    pub amo: bool,
}

/// Modify order request
#[derive(Debug, Clone, Deserialize)]
pub struct ModifyOrderRequest {
    pub quantity: Option<i32>,
    pub price: Option<f64>,
    pub order_type: Option<String>,
    pub trigger_price: Option<f64>,
    pub validity: Option<String>,
}

/// Order response from broker
#[derive(Debug, Clone, Serialize)]
pub struct OrderResponse {
    pub order_id: String,
    pub message: Option<String>,
}

/// Order from order book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub exchange_order_id: Option<String>,
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub quantity: i32,
    pub filled_quantity: i32,
    pub pending_quantity: i32,
    pub price: f64,
    pub trigger_price: f64,
    pub average_price: f64,
    pub order_type: String,
    pub product: String,
    pub status: String,
    pub validity: String,
    pub order_timestamp: String,
    pub exchange_timestamp: Option<String>,
    pub rejection_reason: Option<String>,
}

/// Position
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Holding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub symbol: String,
    pub exchange: String,
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

/// Funds/Margin
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

/// Quote
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub bid_qty: i32,
    pub ask_qty: i32,
    pub oi: i64,
    pub change: f64,
    pub change_percent: f64,
    pub timestamp: String,
}

/// Market depth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDepth {
    pub symbol: String,
    pub exchange: String,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

/// Depth level (bid/ask)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepthLevel {
    pub price: f64,
    pub quantity: i32,
    pub orders: i32,
}

/// Symbol data from master contract
#[derive(Debug, Clone)]
pub struct SymbolData {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub name: String,
    pub lot_size: i32,
    pub tick_size: f64,
    pub instrument_type: String,
    pub expiry: Option<String>,
    pub strike: Option<f64>,
    pub option_type: Option<String>,
}
