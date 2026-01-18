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

    /// Validate API key and return the key name if valid
    fn validate_api_key(&self, apikey: &str) -> Result<String, String> {
        match self.get_app_state() {
            Some(state) => {
                state.sqlite.validate_api_key(apikey, &state.security)
                    .map(|key| key.name)
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
        bid: 0.0,
        ask: 0.0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        ltp: 0.0,
        prev_close: 0.0,
        volume: 0,
        oi: 0,
    };
    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(quote))
    )
}

/// Place basket order - POST /api/v1/basketorder
/// Places multiple orders in a single request
pub async fn place_basket_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<BasketOrderRequest>,
) -> impl IntoResponse {
    info!("Basket order request: {} orders", req.orders.len());

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<BasketOrderResult>>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<Vec<BasketOrderResult>>::error("Broker not connected"))
        );
    }

    // Process each order in basket
    let mut results: Vec<BasketOrderResult> = Vec::new();
    for order in &req.orders {
        // TODO: Execute via broker adapter
        let order_id = format!("ORD{}", chrono::Utc::now().timestamp_millis());
        results.push(BasketOrderResult {
            symbol: order.symbol.clone(),
            exchange: order.exchange.clone(),
            orderid: Some(order_id),
            status: "success".to_string(),
            message: None,
        });
    }

    state.emit("api_basket_order", &req);

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(results))
    )
}

/// Place split order - POST /api/v1/splitorder
/// Splits a large order into smaller chunks
pub async fn place_split_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<SplitOrderRequest>,
) -> impl IntoResponse {
    info!("Split order request: {} qty, {} split size", req.quantity, req.splitsize);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<SplitOrderResult>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<SplitOrderResult>::error("Broker not connected"))
        );
    }

    let split_size = if req.splitsize > 0 { req.splitsize } else { 100 };
    let num_orders = (req.quantity + split_size - 1) / split_size;

    // TODO: Execute via broker adapter
    let mut orderids = Vec::new();
    for _ in 0..num_orders {
        orderids.push(format!("ORD{}", chrono::Utc::now().timestamp_millis()));
    }

    state.emit("api_split_order", &req);

    let result = SplitOrderResult {
        total_quantity: req.quantity,
        split_size,
        num_orders,
        orderids,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(result))
    )
}

/// Get order status - POST /api/v1/orderstatus
pub async fn get_order_status(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OrderStatusRequest>,
) -> impl IntoResponse {
    info!("Order status request: {}", req.orderid);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OrderStatusData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OrderStatusData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let status = OrderStatusData {
        orderid: req.orderid,
        symbol: String::new(),
        exchange: String::new(),
        action: String::new(),
        quantity: 0,
        price: 0.0,
        trigger_price: 0.0,
        pricetype: String::new(),
        product: String::new(),
        order_status: "PENDING".to_string(),
        filled_quantity: 0,
        pending_quantity: 0,
        average_price: 0.0,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(status))
    )
}

/// Get open position - POST /api/v1/openposition
/// Get position for a specific symbol
pub async fn get_open_position(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OpenPositionRequest>,
) -> impl IntoResponse {
    info!("Open position request: {} {}", req.exchange, req.symbol);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OpenPositionData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OpenPositionData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let position = OpenPositionData {
        symbol: req.symbol,
        exchange: req.exchange,
        product: req.product,
        quantity: 0,
        average_price: 0.0,
        ltp: 0.0,
        pnl: 0.0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(position))
    )
}

/// Get market depth - POST /api/v1/depth
pub async fn get_depth(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<DepthRequest>,
) -> impl IntoResponse {
    info!("Depth request: {} {}", req.exchange, req.symbol);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<DepthData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<DepthData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let depth = DepthData {
        symbol: req.symbol,
        exchange: req.exchange,
        buy: vec![],
        sell: vec![],
        ltp: 0.0,
        ltq: 0,
        volume: 0,
        oi: 0,
        totalbuyqty: 0,
        totalsellqty: 0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(depth))
    )
}

/// Get symbol info - POST /api/v1/symbol
pub async fn get_symbol(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<SymbolRequest>,
) -> impl IntoResponse {
    info!("Symbol request: {} {}", req.exchange, req.symbol);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<SymbolData>::error(&e)));
    }

    // Get from symbol cache (doesn't require broker connection)
    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SymbolData>::error("Internal error"))
            );
        }
    };

    match app_state.get_symbol_by_name(&req.exchange, &req.symbol) {
        Some(symbol_info) => {
            let data = SymbolData {
                symbol: symbol_info.symbol,
                exchange: symbol_info.exchange,
                token: symbol_info.token,
                name: Some(symbol_info.name),
                expiry: None,      // Not available in current SymbolInfo
                strike: None,      // Not available in current SymbolInfo
                option_type: None, // Not available in current SymbolInfo
                lot_size: symbol_info.lot_size,
                tick_size: symbol_info.tick_size,
                instrument_type: symbol_info.instrument_type,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        None => {
            (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<SymbolData>::error("Symbol not found"))
            )
        }
    }
}

/// Get historical data - POST /api/v1/history
pub async fn get_history(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<HistoryRequest>,
) -> impl IntoResponse {
    info!("History request: {} {} {}", req.exchange, req.symbol, req.interval);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<HistoryData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<HistoryData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter or DuckDB cache
    let history = HistoryData {
        symbol: req.symbol,
        exchange: req.exchange,
        interval: req.interval,
        candles: vec![],
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(history))
    )
}

/// Get supported intervals - POST /api/v1/intervals
pub async fn get_intervals(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<IntervalsRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<IntervalsData>::error(&e)));
    }

    // Standard intervals supported by most brokers
    let intervals = IntervalsData {
        intervals: vec![
            "1m".to_string(),
            "3m".to_string(),
            "5m".to_string(),
            "10m".to_string(),
            "15m".to_string(),
            "30m".to_string(),
            "1h".to_string(),
            "2h".to_string(),
            "4h".to_string(),
            "1d".to_string(),
            "1w".to_string(),
            "1M".to_string(),
        ],
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(intervals))
    )
}

/// Get analyzer status - POST /api/v1/analyzer
pub async fn get_analyzer_status(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<AnalyzerRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<AnalyzerData>::error(&e)));
    }

    // TODO: Get actual analyzer status from state
    let data = AnalyzerData {
        analyze_mode: false,
        mode: "live".to_string(),
        total_logs: 0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Toggle analyzer mode - POST /api/v1/analyzer/toggle
pub async fn toggle_analyzer(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<AnalyzerToggleRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<AnalyzerData>::error(&e)));
    }

    // TODO: Toggle actual analyzer mode in state
    let mode = if req.mode { "analyze" } else { "live" };
    let data = AnalyzerData {
        analyze_mode: req.mode,
        mode: mode.to_string(),
        total_logs: 0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Calculate margin - POST /api/v1/margin
pub async fn get_margin(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<MarginRequest>,
) -> impl IntoResponse {
    info!("Margin request: {} positions", req.positions.len());

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<MarginData>::error(&e)));
    }

    if req.positions.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<MarginData>::error("Positions array cannot be empty"))
        );
    }

    if req.positions.len() > 50 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<MarginData>::error("Maximum 50 positions allowed"))
        );
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<MarginData>::error("Broker not connected"))
        );
    }

    // TODO: Calculate from broker adapter
    let data = MarginData {
        total_margin_required: 0.0,
        span_margin: Some(0.0),
        exposure_margin: Some(0.0),
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Get multi-quotes - POST /api/v1/multiquotes
pub async fn get_multiquotes(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<MultiQuotesRequest>,
) -> impl IntoResponse {
    info!("Multi-quotes request: {} symbols", req.symbols.len());

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<MultiQuotesData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<MultiQuotesData>::error("Broker not connected"))
        );
    }

    // TODO: Fetch from broker adapter
    let quotes: MultiQuotesData = vec![];

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(quotes))
    )
}

/// Search symbols - POST /api/v1/search
pub async fn search_symbols(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<SearchRequest>,
) -> impl IntoResponse {
    info!("Search request: {}", req.query);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<SearchResultItem>>::error(&e)));
    }

    // TODO: Search from symbol cache
    let results: Vec<SearchResultItem> = vec![];

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(results))
    )
}

/// Get expiry dates - POST /api/v1/expiry
pub async fn get_expiry(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ExpiryRequest>,
) -> impl IntoResponse {
    info!("Expiry request: {} {} {}", req.symbol, req.exchange, req.instrumenttype);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<ExpiryData>::error(&e)));
    }

    // TODO: Get from symbol cache
    let data = ExpiryData {
        expiry_dates: vec![],
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Get instruments - GET /api/v1/instruments
pub async fn get_instruments(
    AxumState(state): AxumState<Arc<WebhookState>>,
    axum::extract::Query(req): axum::extract::Query<InstrumentsRequest>,
) -> impl IntoResponse {
    info!("Instruments request: {:?}", req.exchange);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<InstrumentsData>::error(&e)));
    }

    // TODO: Get from symbol cache
    let data: InstrumentsData = vec![];

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Calculate synthetic future - POST /api/v1/syntheticfuture
pub async fn get_synthetic_future(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<SyntheticFutureRequest>,
) -> impl IntoResponse {
    info!("Synthetic future request: {} {} {}", req.underlying, req.exchange, req.expiry_date);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<SyntheticFutureData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<SyntheticFutureData>::error("Broker not connected"))
        );
    }

    // TODO: Calculate from broker quotes
    let data = SyntheticFutureData {
        underlying: req.underlying,
        underlying_ltp: 0.0,
        expiry: req.expiry_date,
        atm_strike: 0.0,
        synthetic_future_price: 0.0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Get option chain - POST /api/v1/optionchain
pub async fn get_option_chain(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OptionChainRequest>,
) -> impl IntoResponse {
    info!("Option chain request: {} {}", req.underlying, req.exchange);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OptionChainData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OptionChainData>::error("Broker not connected"))
        );
    }

    // TODO: Build option chain from broker quotes
    let data = OptionChainData {
        underlying: req.underlying,
        underlying_ltp: 0.0,
        expiry: req.expiry_date.unwrap_or_default(),
        atm_strike: 0.0,
        strikes: vec![],
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Get option Greeks - POST /api/v1/optiongreeks
pub async fn get_option_greeks(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OptionGreeksRequest>,
) -> impl IntoResponse {
    info!("Option Greeks request: {} {}", req.symbol, req.exchange);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OptionGreeksData>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OptionGreeksData>::error("Broker not connected"))
        );
    }

    // TODO: Calculate Greeks using Black-Scholes
    let data = OptionGreeksData {
        symbol: req.symbol,
        ltp: 0.0,
        iv: 0.0,
        delta: 0.0,
        gamma: 0.0,
        theta: 0.0,
        vega: 0.0,
        rho: 0.0,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Place options order - POST /api/v1/optionsorder
pub async fn place_options_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OptionsOrderRequest>,
) -> impl IntoResponse {
    info!("Options order request: {} {} {} {}", req.underlying, req.exchange, req.option_type, req.action);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OptionsOrderResult>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OptionsOrderResult>::error("Broker not connected"))
        );
    }

    // TODO: Resolve option symbol and place order via broker adapter
    let order_id = format!("ORD{}", chrono::Utc::now().timestamp_millis());
    let data = OptionsOrderResult {
        symbol: format!("{}_{}", req.underlying, req.option_type),
        orderid: order_id,
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Get options symbol - POST /api/v1/optionsymbol
pub async fn get_option_symbol(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OptionSymbolRequest>,
) -> impl IntoResponse {
    info!("Option symbol request: {} {} {}", req.underlying, req.exchange, req.option_type);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OptionSymbolResult>::error(&e)));
    }

    // TODO: Resolve from symbol cache
    let data = OptionSymbolResult {
        symbol: format!("{}_{}", req.underlying, req.option_type),
        token: "0".to_string(),
        exchange: req.exchange,
        strike: 0.0,
        option_type: req.option_type,
        expiry: req.expiry_date.unwrap_or_default(),
    };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
    )
}

/// Place options multi-order - POST /api/v1/optionsmultiorder
pub async fn place_options_multi_order(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<OptionsMultiOrderRequest>,
) -> impl IntoResponse {
    info!("Options multi-order request: {} {} legs", req.underlying, req.legs.len());

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<OptionsMultiOrderResult>::error(&e)));
    }

    if !state.is_broker_connected() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<OptionsMultiOrderResult>::error("Broker not connected"))
        );
    }

    // TODO: Process each leg via broker adapter
    let mut results = Vec::new();
    for (i, leg) in req.legs.iter().enumerate() {
        let order_id = format!("ORD{}", chrono::Utc::now().timestamp_millis());
        results.push(OptionsOrderLegResult {
            leg: (i + 1) as i32,
            symbol: format!("{}_{}", req.underlying, leg.option_type),
            orderid: Some(order_id),
            status: "success".to_string(),
            message: None,
        });
    }

    let data = OptionsMultiOrderResult { results };

    (
        StatusCode::OK,
        Json(ApiResponse::success_with_data(data))
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
