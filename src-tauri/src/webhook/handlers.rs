//! Webhook and REST API endpoint handlers
//!
//! Provides handlers for:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - OpenAlgo SDK compatible REST API (/api/v1/*)

use crate::state::AppState;
use crate::webhook::types::*;
use axum::{
    extract::{Json, Path, State as AxumState},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tracing::{error, info, warn};

/// Shared state for webhook/API handlers
pub struct WebhookState {
    pub app_handle: AppHandle,
}

impl WebhookState {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }

    /// Get AppState from Tauri
    fn get_app_state(&self) -> Option<tauri::State<'_, AppState>> {
        self.app_handle.try_state::<AppState>()
    }

    /// Validate API key and return user_id if valid
    fn validate_api_key(&self, apikey: &str) -> Result<String, String> {
        match self.get_app_state() {
            Some(state) => {
                state.sqlite.validate_api_key(apikey)
                    .map_err(|e| format!("Invalid openalgo apikey: {}", e))
            }
            None => Err("Internal error: AppState not available".to_string())
        }
    }

    /// Emit event to frontend
    fn emit<T: serde::Serialize + Clone>(&self, event: &str, payload: &T) {
        if let Err(e) = self.app_handle.emit(event, payload) {
            warn!("Failed to emit {}: {}", event, e);
        }
    }

    /// Check if broker is connected
    fn is_broker_connected(&self) -> bool {
        self.get_app_state()
            .map(|s| s.is_broker_connected())
            .unwrap_or(false)
    }
}

// ============================================================================
// Health Check
// ============================================================================

/// Health check endpoint - GET /health or GET /
pub async fn health_check() -> impl IntoResponse {
    Json(ApiResponse::<Empty>::success_with_message("OpenAlgo Desktop API is running"))
}

// ============================================================================
// Dynamic Webhook Handler
// ============================================================================

/// Dynamic webhook endpoint - POST /webhook/{webhook_id}
///
/// This is the main webhook entry point. The webhook_id determines which
/// strategy to use. Supports TradingView, GoCharting, Chartink payloads.
pub async fn webhook_handler(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Path(webhook_id): Path<String>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    info!("Received webhook for strategy: {}", webhook_id);

    // Helper to create error response
    fn error_response(msg: &str) -> (StatusCode, Json<ApiResponse<WebhookResult>>) {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                status: "error".to_string(),
                message: Some(msg.to_string()),
                data: None,
                orderid: None,
                mode: None,
            })
        )
    }

    // Get AppState
    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            error!("AppState not available");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    status: "error".to_string(),
                    message: Some("Internal error: AppState not available".to_string()),
                    data: None,
                    orderid: None,
                    mode: None,
                })
            );
        }
    };

    // Look up strategy by webhook_id
    let strategy = match app_state.sqlite.get_strategy_by_webhook_id(&webhook_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!("Strategy not found for webhook_id: {}", webhook_id);
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse {
                    status: "error".to_string(),
                    message: Some("Strategy not found".to_string()),
                    data: None,
                    orderid: None,
                    mode: None,
                })
            );
        }
        Err(e) => {
            error!("Failed to lookup strategy: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse {
                    status: "error".to_string(),
                    message: Some(format!("Failed to lookup strategy: {}", e)),
                    data: None,
                    orderid: None,
                    mode: None,
                })
            );
        }
    };

    // Check if strategy is active
    if !strategy.is_active {
        warn!("Strategy {} is not active", strategy.name);
        return error_response("Strategy is not active");
    }

    // Check trading hours for intraday strategies
    if strategy.is_intraday {
        if let Err(e) = validate_trading_hours(&strategy, &payload) {
            warn!("Trading hours validation failed: {}", e);
            return error_response(&e);
        }
    }

    // Get action from payload
    let action = match payload.get_action() {
        Some(a) => a.to_uppercase(),
        None => {
            return error_response("Missing action in webhook payload");
        }
    };

    // Validate action against trading mode
    if let Err(e) = validate_trading_mode(&strategy.trading_mode, &action) {
        warn!("Trading mode validation failed: {}", e);
        return error_response(&e);
    }

    // Get symbols to process (Chartink can have multiple)
    let symbols = payload.get_symbols();
    if symbols.is_empty() {
        return error_response("No symbol found in webhook payload");
    }

    // Process each symbol
    let mut alerts_processed = 0;
    let mut orders_queued = 0;
    let mut errors = Vec::new();

    for symbol in symbols {
        // Look up symbol mapping for this strategy
        let mapping = match app_state.sqlite.get_symbol_mapping(&strategy.id, &symbol) {
            Ok(Some(m)) => m,
            Ok(None) => {
                errors.push(format!("Symbol {} not mapped in strategy", symbol));
                continue;
            }
            Err(e) => {
                errors.push(format!("Failed to lookup symbol mapping: {}", e));
                continue;
            }
        };

        // Build processed alert
        let processed_alert = ProcessedAlert {
            strategy_id: strategy.id,
            strategy_name: strategy.name.clone(),
            webhook_id: webhook_id.clone(),
            symbol: mapping.symbol.clone(),
            exchange: mapping.exchange.clone(),
            action: action.clone(),
            quantity: payload.get_quantity().unwrap_or(mapping.quantity),
            product: mapping.product_type.clone(),
            pricetype: payload.get_pricetype(),
            price: payload.price.unwrap_or(0.0),
            trigger_price: payload.get_trigger_price().unwrap_or(0.0),
            position_size: payload.position_size,
            is_smart_order: payload.position_size.is_some(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        // Emit alert to frontend
        state.emit("webhook_alert", &processed_alert);
        alerts_processed += 1;

        // Check broker connection before placing order
        if !state.is_broker_connected() {
            warn!("Broker not connected, alert emitted but order not placed");
            // TODO: Queue to pending_orders table for later execution
            continue;
        }

        // TODO: Execute order via broker adapter
        // For now, just queue the order for later implementation
        orders_queued += 1;
        info!("Order queued: {:?}", processed_alert);
    }

    // Return result
    let result = WebhookResult {
        alerts_processed,
        orders_queued,
        errors: errors.clone(),
    };

    if errors.is_empty() {
        (
            StatusCode::OK,
            Json(ApiResponse {
                status: "success".to_string(),
                message: Some(format!("{} alerts processed, {} orders queued", alerts_processed, orders_queued)),
                data: Some(result),
                orderid: None,
                mode: None,
            })
        )
    } else if alerts_processed > 0 {
        (
            StatusCode::PARTIAL_CONTENT,
            Json(ApiResponse {
                status: "success".to_string(),
                message: Some(format!("Partial success: {}", errors.join(", "))),
                data: Some(result),
                orderid: None,
                mode: None,
            })
        )
    } else {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse {
                status: "error".to_string(),
                message: Some(errors.join(", ")),
                data: Some(result),
                orderid: None,
                mode: None,
            })
        )
    }
}

// ============================================================================
// REST API Handlers (OpenAlgo SDK Compatible)
// ============================================================================

/// Place order - POST /api/v1/placeorder
pub async fn place_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<PlaceOrderRequest>,
) -> impl IntoResponse {
    info!("Place order request: {:?}", req);

    // Validate API key
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    // Check broker connection
    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute order via broker adapter
    // For now, emit event and return placeholder
    state.emit("api_order", &req);

    // Placeholder response
    let order_id = format!("ORD{}", chrono::Utc::now().timestamp_millis());
    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_orderid(&order_id))
    )
}

/// Place smart order - POST /api/v1/placesmartorder
pub async fn place_smart_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<PlaceSmartOrderRequest>,
) -> impl IntoResponse {
    info!("Place smart order request: {:?}", req);

    // Validate API key
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    // Check broker connection
    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute smart order via broker adapter
    state.emit("api_smart_order", &req);

    let order_id = format!("ORD{}", chrono::Utc::now().timestamp_millis());
    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_orderid(&order_id))
    )
}

/// Modify order - POST /api/v1/modifyorder
pub async fn modify_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ModifyOrderRequest>,
) -> impl IntoResponse {
    info!("Modify order request: {:?}", req);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute modify via broker adapter
    state.emit("api_modify_order", &req);

    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_orderid(&req.orderid))
    )
}

/// Cancel order - POST /api/v1/cancelorder
pub async fn cancel_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<CancelOrderRequest>,
) -> impl IntoResponse {
    info!("Cancel order request: {:?}", req);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute cancel via broker adapter
    state.emit("api_cancel_order", &req);

    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_orderid(&req.orderid))
    )
}

/// Cancel all orders - POST /api/v1/cancelallorder
pub async fn cancel_all_orders(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<CancelAllOrdersRequest>,
) -> impl IntoResponse {
    info!("Cancel all orders request: {:?}", req);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute cancel all via broker adapter
    state.emit("api_cancel_all_orders", &req);

    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_message("All open orders cancelled"))
    )
}

/// Close position - POST /api/v1/closeposition
pub async fn close_position(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ClosePositionRequest>,
) -> impl IntoResponse {
    info!("Close position request: {:?}", req);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Empty>::error("Broker not connected"))
        );
    }

    // TODO: Execute close position via broker adapter
    state.emit("api_close_position", &req);

    (
        StatusCode::OK,
        Json(ApiResponse::<Empty>::success_with_message("Position close order placed"))
    )
}

/// Get order book - POST /api/v1/orderbook
pub async fn get_orderbook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<OrderData>>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Vec<OrderData>>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let orders: Vec<OrderData> = vec![];
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(orders))
    )
}

/// Get trade book - POST /api/v1/tradebook
pub async fn get_tradebook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<TradeData>>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Vec<TradeData>>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let trades: Vec<TradeData> = vec![];
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(trades))
    )
}

/// Get position book - POST /api/v1/positionbook
pub async fn get_positionbook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<PositionData>>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Vec<PositionData>>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let positions: Vec<PositionData> = vec![];
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(positions))
    )
}

/// Get holdings - POST /api/v1/holdings
pub async fn get_holdings(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<HoldingData>>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Vec<HoldingData>>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let holdings: Vec<HoldingData> = vec![];
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(holdings))
    )
}

/// Get funds - POST /api/v1/funds
pub async fn get_funds(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<FundsData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<FundsData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let funds = FundsData {
        availablecash: 0.0,
        collateral: 0.0,
        m2munrealized: 0.0,
        m2mrealized: 0.0,
        utiliseddebits: 0.0,
    };
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(funds))
    )
}

/// Get quotes - POST /api/v1/quotes
pub async fn get_quotes(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<QuoteRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<QuoteData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<QuoteData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let quote = QuoteData {
        symbol: req.symbol,
        exchange: req.exchange,
        ltp: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0,
        oi: 0,
        bid: 0.0,
        ask: 0.0,
        bid_size: 0,
        ask_size: 0,
    };
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(quote))
    )
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Strategy model (simplified for handler use)
#[derive(Debug, Clone)]
pub struct Strategy {
    pub id: i64,
    pub name: String,
    pub webhook_id: String,
    pub is_active: bool,
    pub is_intraday: bool,
    pub trading_mode: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub squareoff_time: Option<String>,
}

/// Symbol mapping model
#[derive(Debug, Clone)]
pub struct SymbolMapping {
    pub symbol: String,
    pub exchange: String,
    pub quantity: i32,
    pub product_type: String,
}

/// Validate trading hours for intraday strategies
fn validate_trading_hours(strategy: &Strategy, payload: &WebhookPayload) -> Result<(), String> {
    use chrono::{Timelike, Utc};
    use chrono_tz::Asia::Kolkata;

    let now = Utc::now().with_timezone(&Kolkata);
    let current_time = format!("{:02}:{:02}", now.hour(), now.minute());

    let action = payload.get_action().unwrap_or_default().to_uppercase();
    let is_entry = action == "BUY" || action == "SELL";

    // Get time boundaries
    let start_time = strategy.start_time.as_deref().unwrap_or("09:15");
    let end_time = strategy.end_time.as_deref().unwrap_or("15:15");
    let squareoff_time = strategy.squareoff_time.as_deref().unwrap_or("15:25");

    // Before start time - reject all
    if current_time < start_time.to_string() {
        return Err(format!("Trading not started. Starts at {}", start_time));
    }

    // After squareoff time - reject all
    if current_time > squareoff_time.to_string() {
        return Err(format!("Trading ended. Squareoff was at {}", squareoff_time));
    }

    // Between end_time and squareoff_time - only exit orders allowed
    if current_time > end_time.to_string() && is_entry {
        return Err(format!("Entry orders not allowed after {}. Only exit orders until {}", end_time, squareoff_time));
    }

    Ok(())
}

/// Validate action against strategy trading mode
fn validate_trading_mode(trading_mode: &str, action: &str) -> Result<(), String> {
    match trading_mode.to_uppercase().as_str() {
        "LONG" => {
            if action == "SELL" {
                // SELL in LONG mode is an exit, which is allowed
            }
            Ok(())
        }
        "SHORT" => {
            if action == "BUY" {
                // BUY in SHORT mode is a cover, which is allowed
            }
            Ok(())
        }
        "BOTH" => Ok(()),
        _ => Err(format!("Invalid trading mode: {}", trading_mode))
    }
}
