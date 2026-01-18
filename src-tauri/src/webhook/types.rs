//! Webhook and REST API types
//!
//! This module provides types compatible with OpenAlgo SDK for:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - REST API endpoints (/api/v1/*)

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    pub quantity: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub trigger_price: f64,
    #[serde(default)]
    pub disclosed_quantity: i32,
}

/// Place smart order request - POST /api/v1/placesmartorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceSmartOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub action: String,
    pub quantity: i32,
    pub position_size: i32,
    #[serde(default = "default_pricetype")]
    pub pricetype: String,
    #[serde(default = "default_product")]
    pub product: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub trigger_price: f64,
    #[serde(default)]
    pub disclosed_quantity: i32,
}

/// Modify order request - POST /api/v1/modifyorder
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyOrderRequest {
    pub apikey: String,
    pub strategy: String,
    pub exchange: String,
    pub symbol: String,
    pub orderid: String,
    pub action: String,
    pub quantity: i32,
    pub pricetype: String,
    pub product: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub trigger_price: f64,
    #[serde(default)]
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

// ============================================================================
// REST API Response Data Types
// ============================================================================

/// Order in orderbook
#[derive(Debug, Clone, Serialize)]
pub struct OrderData {
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
    pub average_price: f64,
    pub filled_quantity: i32,
    pub pending_quantity: i32,
    pub order_timestamp: String,
}

/// Trade in tradebook
#[derive(Debug, Clone, Serialize)]
pub struct TradeData {
    pub tradeid: String,
    pub orderid: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub quantity: i32,
    pub average_price: f64,
    pub price: f64,
    pub trade_value: f64,
    pub product: String,
    pub strategy: String,
    pub timestamp: String,
}

/// Position in positionbook
#[derive(Debug, Clone, Serialize)]
pub struct PositionData {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub pnl_percent: f64,
}

/// Holding
#[derive(Debug, Clone, Serialize)]
pub struct HoldingData {
    pub symbol: String,
    pub exchange: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub pnl_percent: f64,
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
#[derive(Debug, Clone, Serialize)]
pub struct QuoteData {
    pub symbol: String,
    pub exchange: String,
    pub ltp: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: i64,
    pub oi: i64,
    pub bid: f64,
    pub ask: f64,
    pub bid_size: i32,
    pub ask_size: i32,
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
