//! Response bodies of the public API, shaped exactly as the web's
//! analyze-mode responses (`tests/fixtures/web/rest/**`): key names, status
//! strings, and floats vs integers per field. The API layer serialises these
//! with `serde_json` and the status code it gets alongside (200 for every
//! `Ok`, [`super::SandboxError::http_status`] for every `Err`).

use serde::Serialize;

pub const SUCCESS: &str = "success";
pub const ANALYZE: &str = "analyze";

/// `{"status":"success","orderid","mode":"analyze"}` (placeorder, smart
/// order that placed).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderPlaced {
    pub status: &'static str,
    pub orderid: String,
    pub mode: &'static str,
}

impl OrderPlaced {
    pub fn new(orderid: String) -> Self {
        Self {
            status: SUCCESS,
            orderid,
            mode: ANALYZE,
        }
    }
}

/// `{"status":"success","orderid","message","mode"}` (modify, cancel,
/// close one position).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderMessage {
    pub status: &'static str,
    pub orderid: String,
    pub message: String,
    pub mode: &'static str,
}

impl OrderMessage {
    pub fn new(orderid: String, message: impl Into<String>) -> Self {
        Self {
            status: SUCCESS,
            orderid,
            message: message.into(),
            mode: ANALYZE,
        }
    }
}

/// `{"status":"success","message","mode"}`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Message {
    pub status: &'static str,
    pub message: String,
    pub mode: &'static str,
}

impl Message {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            status: SUCCESS,
            message: message.into(),
            mode: ANALYZE,
        }
    }
}

/// Smart order outcome: an order, or the web's no-action message.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum SmartOrderReply {
    Placed(OrderPlaced),
    NoAction(Message),
}

/// One failed cancellation in `cancelallorder`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FailedCancellation {
    pub orderid: String,
    pub message: String,
}

/// `cancelallorder`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CancelAllReply {
    pub status: &'static str,
    pub message: String,
    pub canceled_orders: Vec<String>,
    pub failed_cancellations: Vec<FailedCancellation>,
    pub mode: &'static str,
}

/// `closeposition` without a symbol.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum CloseAllReply {
    /// `{"status","message":"No open positions to close","mode"}`.
    Nothing(Message),
    Closed {
        status: &'static str,
        message: String,
        closed_positions: i64,
        failed_closures: i64,
        mode: &'static str,
    },
}

/// One order book row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderbookRow {
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub pricetype: String,
    pub product: String,
    pub order_status: String,
    pub average_price: f64,
    pub filled_quantity: i64,
    pub pending_quantity: i64,
    pub rejection_reason: String,
    pub timestamp: String,
    pub strategy: String,
}

/// Order book counts.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct OrderStatistics {
    pub total_buy_orders: i64,
    pub total_sell_orders: i64,
    pub total_completed_orders: i64,
    pub total_open_orders: i64,
    pub total_rejected_orders: i64,
    pub total_trigger_pending_orders: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderbookData {
    pub orders: Vec<OrderbookRow>,
    pub statistics: OrderStatistics,
}

/// `orderbook`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderbookReply {
    pub status: &'static str,
    pub data: OrderbookData,
    pub mode: &'static str,
}

/// `orderstatus` data (note `price_type`, not `pricetype`, and no
/// `rejection_reason`, as the web returns it).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderStatusData {
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub trigger_price: f64,
    pub price_type: String,
    pub product: String,
    pub order_status: String,
    pub average_price: f64,
    pub filled_quantity: i64,
    pub pending_quantity: i64,
    pub timestamp: String,
    pub strategy: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OrderStatusReply {
    pub status: &'static str,
    pub data: OrderStatusData,
    pub mode: &'static str,
}

/// One trade book row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TradebookRow {
    pub tradeid: String,
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub average_price: f64,
    pub price: f64,
    pub trade_value: f64,
    pub product: String,
    pub strategy: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TradebookReply {
    pub status: &'static str,
    pub data: Vec<TradebookRow>,
    pub mode: &'static str,
}

/// One position book row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PositionbookRow {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i64,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub pnlpercent: f64,
    pub unrealized_pnl: f64,
    pub today_realized_pnl: f64,
    pub total_pnl_today: f64,
    /// Contract value multiplier, as a float (the web's field name).
    pub lot_size: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PositionbookReply {
    pub status: &'static str,
    pub data: Vec<PositionbookRow>,
    pub total_pnl: f64,
    pub total_unrealized_pnl: f64,
    pub total_today_realized_pnl: f64,
    pub total_pnl_today: f64,
    pub mode: &'static str,
}

/// `openposition`: flat `{"status","quantity","mode"}`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenPositionReply {
    pub status: &'static str,
    pub quantity: i64,
    pub mode: &'static str,
}

/// One `pnl/symbols` row.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PnlSymbolRow {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i64,
    pub pnl: f64,
    pub unrealized_pnl: f64,
    pub today_realized_pnl: f64,
    pub total_pnl_today: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PnlSymbolsReply {
    pub status: &'static str,
    pub data: Vec<PnlSymbolRow>,
    pub total_pnl: f64,
    pub total_unrealized_pnl: f64,
    pub total_today_realized_pnl: f64,
    pub total_pnl_today: f64,
    pub mode: &'static str,
}

/// One holding.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HoldingRowOut {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i64,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub pnlpercent: f64,
    pub current_value: f64,
    pub settlement_date: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct HoldingsStatistics {
    pub totalholdingvalue: f64,
    pub totalinvvalue: f64,
    pub totalprofitandloss: f64,
    pub totalpnlpercentage: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HoldingsData {
    pub holdings: Vec<HoldingRowOut>,
    pub statistics: HoldingsStatistics,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HoldingsReply {
    pub status: &'static str,
    pub data: HoldingsData,
    pub mode: &'static str,
}

/// `funds` data.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FundsData {
    pub availablecash: f64,
    pub collateral: f64,
    pub m2munrealized: f64,
    pub m2mrealized: f64,
    pub total_realized_pnl: f64,
    pub today_realized_pnl: f64,
    /// Brokerage and statutory charges (web openalgo#2496). `availablecash`
    /// is already net of them; the P&L figures are gross, as a broker's are.
    pub today_charges: f64,
    pub total_charges: f64,
    pub utiliseddebits: f64,
    pub grossexposure: f64,
    pub totalpnl: f64,
    pub last_reset: String,
    pub reset_count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FundsReply {
    pub status: &'static str,
    pub data: FundsData,
    pub mode: &'static str,
}

/// GTT place / modify / cancel success: `{"status","mode","trigger_id"}`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GttReply {
    pub status: &'static str,
    pub mode: &'static str,
    pub trigger_id: String,
}

/// One GTT leg in the GTT order book.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GttLegOut {
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub pricetype: String,
    pub product: String,
    pub triggered_order_id: Option<String>,
}

/// One GTT order book entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GttEntry {
    pub trigger_id: String,
    pub trigger_type: String,
    pub status: String,
    pub symbol: String,
    pub exchange: String,
    pub trigger_prices: Vec<f64>,
    pub last_price: f64,
    pub legs: Vec<GttLegOut>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
    pub strategy: Option<String>,
    pub margin_blocked: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GttOrderbookReply {
    pub status: &'static str,
    pub mode: &'static str,
    pub data: Vec<GttEntry>,
}

/// One scheduler job in the square-off status.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JobStatus {
    pub id: String,
    pub name: String,
    pub next_run: String,
}

/// `GET /sandbox/squareoff-status` data.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SquareOffStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub jobs: Vec<JobStatus>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SquareOffStatusReply {
    pub status: &'static str,
    pub data: SquareOffStatus,
    pub mode: &'static str,
}

/// `/sandbox/mypnl/api/data` pieces.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlSummary {
    pub today_realized_pnl: f64,
    pub all_time_realized_pnl: f64,
    pub positions_unrealized_pnl: f64,
    pub holdings_unrealized_pnl: f64,
    pub total_unrealized_pnl: f64,
    pub today_total_mtm: f64,
    /// Brokerage and statutory charges (web openalgo#2496); every P&L
    /// figure here is gross, as a broker's position book is.
    pub today_charges: f64,
    pub total_charges: f64,
    pub today_net_mtm: f64,
    pub all_time_net_realized_pnl: f64,
    pub total_pnl: f64,
    pub available_balance: f64,
    pub total_capital: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlDaily {
    pub date: String,
    pub realized_pnl: f64,
    pub positions_unrealized: f64,
    pub holdings_unrealized: f64,
    pub total_unrealized: f64,
    pub total_mtm: f64,
    pub charges: f64,
    pub net_mtm: f64,
    pub portfolio_value: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlPosition {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i64,
    pub average_price: f64,
    pub ltp: f64,
    pub unrealized_pnl: f64,
    pub today_realized_pnl: f64,
    pub all_time_realized_pnl: f64,
    pub status: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlHolding {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i64,
    pub average_price: f64,
    pub ltp: f64,
    pub unrealized_pnl: f64,
    pub pnl_percent: f64,
    pub settlement_date: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlTrade {
    pub tradeid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i64,
    pub price: f64,
    pub product: String,
    pub charges: f64,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlData {
    pub summary: MyPnlSummary,
    pub daily_pnl: Vec<MyPnlDaily>,
    pub positions: Vec<MyPnlPosition>,
    pub holdings: Vec<MyPnlHolding>,
    pub trades: Vec<MyPnlTrade>,
}

/// `{"status":"success","data":{summary, daily_pnl, positions, holdings, trades}}`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MyPnlReply {
    pub status: &'static str,
    pub data: MyPnlData,
}

/// `/sandbox/update` and `/sandbox/reset`: `{"status","message"}` (no mode,
/// as the web's blueprint returns it).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SettingsMessage {
    pub status: &'static str,
    pub message: String,
}
