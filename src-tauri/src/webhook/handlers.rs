//! Webhook and REST API endpoint handlers
//!
//! Provides handlers for:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - OpenAlgo SDK compatible REST API (/api/v1/*)

use crate::brokers::types::{ModifyOrderRequest as BrokerModifyOrder, OrderRequest as BrokerOrderRequest};
use crate::services::{
    AnalyzerService, FundsService, HoldingsService, HistoryService, OptionsService,
    OrderService, OrderbookService, PositionService, QuotesService, SmartOrderService,
    SymbolService,
};
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

    // Get AppState
    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error: AppState not available"))
            );
        }
    };

    // Build broker order request
    let order = BrokerOrderRequest {
        symbol: req.symbol.clone(),
        exchange: req.exchange.clone(),
        side: req.action.clone(),
        quantity: req.quantity,
        price: req.price,
        order_type: req.pricetype.clone(),
        product: req.product.clone(),
        trigger_price: if req.trigger_price > 0.0 { Some(req.trigger_price) } else { None },
        disclosed_quantity: if req.disclosed_quantity > 0 { Some(req.disclosed_quantity) } else { None },
        validity: "DAY".to_string(),
        amo: false,
    };

    // Execute order via service
    match OrderService::place_order(&app_state, order, Some(&req.apikey)).await {
        Ok(result) => {
            state.emit("api_order", &req);
            if result.success {
                (
                    StatusCode::OK,
                    Json(ApiResponse::<Empty> {
                        status: "success".to_string(),
                        message: Some(result.message),
                        data: None,
                        orderid: result.order_id,
                        mode: Some(result.mode),
                    })
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<Empty>::error(&result.message))
                )
            }
        }
        Err(e) => {
            error!("Place order failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error: AppState not available"))
            );
        }
    };

    // Build smart order request
    let smart_order_req = crate::services::smart_order_service::SmartOrderRequest {
        symbol: req.symbol.clone(),
        exchange: req.exchange.clone(),
        action: req.action.clone(),
        position_size: req.position_size,
        product: req.product.clone(),
        pricetype: Some(req.pricetype.clone()),
        price: if req.price > 0.0 { Some(req.price) } else { None },
    };

    match SmartOrderService::place_smart_order(&app_state, smart_order_req, Some(&req.apikey)).await {
        Ok(result) => {
            state.emit("api_smart_order", &req);
            if result.success {
                (
                    StatusCode::OK,
                    Json(ApiResponse::<Empty> {
                        status: "success".to_string(),
                        message: Some(result.message),
                        data: None,
                        orderid: result.order_id,
                        mode: None,
                    })
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<Empty>::error(&result.message))
                )
            }
        }
        Err(e) => {
            error!("Place smart order failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error"))
            );
        }
    };

    // Build broker modify order request
    let modify_req = BrokerModifyOrder {
        quantity: if req.quantity > 0 { Some(req.quantity) } else { None },
        price: if req.price > 0.0 { Some(req.price) } else { None },
        order_type: if !req.pricetype.is_empty() { Some(req.pricetype.clone()) } else { None },
        trigger_price: if req.trigger_price > 0.0 { Some(req.trigger_price) } else { None },
        validity: None,
    };

    match OrderService::modify_order(&app_state, &req.orderid, modify_req, Some(&req.apikey)).await {
        Ok(result) => {
            state.emit("api_modify_order", &req);
            if result.success {
                (
                    StatusCode::OK,
                    Json(ApiResponse::<Empty>::success_with_orderid(&result.order_id))
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<Empty>::error(&result.message))
                )
            }
        }
        Err(e) => {
            error!("Modify order failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error"))
            );
        }
    };

    match OrderService::cancel_order(&app_state, &req.orderid, None, Some(&req.apikey)).await {
        Ok(result) => {
            state.emit("api_cancel_order", &req);
            if result.success {
                (
                    StatusCode::OK,
                    Json(ApiResponse::<Empty>::success_with_orderid(&result.order_id))
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<Empty>::error(&result.message))
                )
            }
        }
        Err(e) => {
            error!("Cancel order failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error"))
            );
        }
    };

    match OrderService::cancel_all_orders(&app_state, Some(&req.apikey)).await {
        Ok(results) => {
            state.emit("api_cancel_all_orders", &req);
            let cancelled_count = results.iter().filter(|r| r.success).count();
            let failed_count = results.len() - cancelled_count;
            let message = if failed_count == 0 {
                format!("{} orders cancelled successfully", cancelled_count)
            } else {
                format!("{} cancelled, {} failed", cancelled_count, failed_count)
            };
            (
                StatusCode::OK,
                Json(ApiResponse::<Empty>::success_with_message(&message))
            )
        }
        Err(e) => {
            error!("Cancel all orders failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
}

/// Close position - POST /api/v1/closeposition
/// Note: This endpoint closes ALL positions (ClosePositionRequest only has apikey and strategy)
pub async fn close_position(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ClosePositionRequest>,
) -> impl IntoResponse {
    info!("Close position request: {:?}", req);

    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Empty>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error("Internal error"))
            );
        }
    };

    // Close all positions (ClosePositionRequest only has apikey and strategy)
    match PositionService::close_all_positions(&app_state, Some(&req.apikey)).await {
        Ok(results) => {
            state.emit("api_close_position", &req);
            let closed_count = results.iter().filter(|r| r.success).count();
            (
                StatusCode::OK,
                Json(ApiResponse::<Empty>::success_with_message(&format!("{} positions closed", closed_count)))
            )
        }
        Err(e) => {
            error!("Close all positions failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Empty>::error(&e.to_string()))
            )
        }
    }
}

/// Get order book - POST /api/v1/orderbook
pub async fn get_orderbook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<OrderData>>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<OrderData>>::error("Internal error"))
            );
        }
    };

    match OrderbookService::get_orderbook(&app_state, Some(&req.apikey)).await {
        Ok(result) => {
            let orders: Vec<OrderData> = result.orders.into_iter().map(|o| OrderData {
                orderid: o.order_id,
                symbol: o.symbol,
                exchange: o.exchange,
                action: o.side,
                quantity: o.quantity,
                price: o.price,
                trigger_price: o.trigger_price,
                pricetype: o.order_type,
                product: o.product,
                order_status: o.status,
                timestamp: o.order_timestamp,
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(orders))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<OrderData>>::error(&e.to_string()))
            )
        }
    }
}

/// Get trade book - POST /api/v1/tradebook
pub async fn get_tradebook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<TradeData>>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<TradeData>>::error("Internal error"))
            );
        }
    };

    match OrderbookService::get_tradebook(&app_state, Some(&req.apikey)).await {
        Ok(result) => {
            let trades: Vec<TradeData> = result.trades.into_iter().map(|t| {
                let trade_value = t.filled_quantity as f64 * t.average_price;
                TradeData {
                    orderid: t.order_id,
                    symbol: t.symbol,
                    exchange: t.exchange,
                    product: t.product,
                    action: t.side,
                    quantity: t.filled_quantity,
                    average_price: t.average_price,
                    trade_value,
                    timestamp: t.order_timestamp,
                }
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(trades))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<TradeData>>::error(&e.to_string()))
            )
        }
    }
}

/// Get position book - POST /api/v1/positionbook
pub async fn get_positionbook(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<PositionData>>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<PositionData>>::error("Internal error"))
            );
        }
    };

    match PositionService::get_positions(&app_state, Some(&req.apikey)).await {
        Ok(result) => {
            let positions: Vec<PositionData> = result.positions.into_iter().map(|p| PositionData {
                symbol: p.symbol,
                exchange: p.exchange,
                product: p.product,
                quantity: p.quantity,
                average_price: p.average_price,
                ltp: p.ltp,
                pnl: p.pnl,
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(positions))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<PositionData>>::error(&e.to_string()))
            )
        }
    }
}

/// Get holdings - POST /api/v1/holdings
pub async fn get_holdings(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<Vec<HoldingData>>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<HoldingData>>::error("Internal error"))
            );
        }
    };

    match HoldingsService::get_holdings(&app_state, Some(&req.apikey)).await {
        Ok(result) => {
            let holdings: Vec<HoldingData> = result.holdings.into_iter().map(|h| HoldingData {
                symbol: h.symbol,
                exchange: h.exchange,
                quantity: h.quantity,
                product: "CNC".to_string(), // Holdings are typically CNC
                pnl: h.pnl,
                pnlpercent: h.pnl_percentage,
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(holdings))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<HoldingData>>::error(&e.to_string()))
            )
        }
    }
}

/// Get funds - POST /api/v1/funds
pub async fn get_funds(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<ApiKeyRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<FundsData>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<FundsData>::error("Internal error"))
            );
        }
    };

    match FundsService::get_funds(&app_state, Some(&req.apikey)).await {
        Ok(result) => {
            let funds = FundsData {
                availablecash: result.funds.available_cash,
                collateral: result.funds.collateral,
                m2munrealized: 0.0, // Not directly available in Funds struct
                m2mrealized: 0.0,   // Not directly available in Funds struct
                utiliseddebits: result.funds.used_margin,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(funds))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<FundsData>::error(&e.to_string()))
            )
        }
    }
}

/// Get quotes - POST /api/v1/quotes
pub async fn get_quotes(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<QuoteRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<QuoteData>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<QuoteData>::error("Internal error"))
            );
        }
    };

    match QuotesService::get_quote(&app_state, &req.exchange, &req.symbol, Some(&req.apikey)).await {
        Ok(q) => {
            let quote = QuoteData {
                bid: q.bid,
                ask: q.ask,
                open: q.open,
                high: q.high,
                low: q.low,
                ltp: q.ltp,
                prev_close: q.close,
                volume: q.volume,
                oi: q.oi,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(quote))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<QuoteData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<BasketOrderResult>>::error("Internal error"))
            );
        }
    };

    // Convert to broker order requests
    let orders: Vec<BrokerOrderRequest> = req.orders.iter().map(|o| BrokerOrderRequest {
        symbol: o.symbol.clone(),
        exchange: o.exchange.clone(),
        side: o.action.clone(),
        quantity: o.quantity,
        price: o.price,
        order_type: o.pricetype.clone(),
        product: o.product.clone(),
        trigger_price: if o.trigger_price > 0.0 { Some(o.trigger_price) } else { None },
        disclosed_quantity: None,
        validity: "DAY".to_string(),
        amo: false,
    }).collect();

    match SmartOrderService::place_basket_order(&app_state, orders, Some(&req.apikey)).await {
        Ok(order_results) => {
            let results: Vec<BasketOrderResult> = order_results.into_iter()
                .enumerate()
                .map(|(i, r)| BasketOrderResult {
                    symbol: req.orders.get(i).map(|o| o.symbol.clone()).unwrap_or_default(),
                    exchange: req.orders.get(i).map(|o| o.exchange.clone()).unwrap_or_default(),
                    orderid: r.order_id,
                    status: if r.success { "success".to_string() } else { "error".to_string() },
                    message: if r.success { None } else { Some(r.message) },
                })
                .collect();
            state.emit("api_basket_order", &req);
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(results))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<BasketOrderResult>>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SplitOrderResult>::error("Internal error"))
            );
        }
    };

    let split_req = crate::services::smart_order_service::SplitOrderRequest {
        symbol: req.symbol.clone(),
        exchange: req.exchange.clone(),
        action: req.action.clone(),
        quantity: req.quantity,
        split_size: req.splitsize,
        product: req.product.clone(),
        pricetype: Some(req.pricetype.clone()),
        price: if req.price > 0.0 { Some(req.price) } else { None },
    };

    match SmartOrderService::place_split_order(&app_state, split_req, Some(&req.apikey)).await {
        Ok(result) => {
            state.emit("api_split_order", &req);
            let api_result = SplitOrderResult {
                total_quantity: result.total_quantity,
                split_size: result.split_size,
                num_orders: result.num_orders,
                orderids: result.order_ids,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(api_result))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SplitOrderResult>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OrderStatusData>::error("Internal error"))
            );
        }
    };

    match OrderbookService::get_order_status(&app_state, &req.orderid, Some(&req.apikey)).await {
        Ok(result) => {
            match result.order {
                Some(o) => {
                    let status = OrderStatusData {
                        orderid: o.order_id,
                        symbol: o.symbol,
                        exchange: o.exchange,
                        action: o.side,
                        quantity: o.quantity,
                        price: o.price,
                        trigger_price: o.trigger_price,
                        pricetype: o.order_type,
                        product: o.product,
                        order_status: o.status,
                        filled_quantity: o.filled_quantity,
                        pending_quantity: o.pending_quantity,
                        average_price: o.average_price,
                        timestamp: o.order_timestamp,
                    };
                    (
                        StatusCode::OK,
                        Json(ApiResponse::success_with_data(status))
                    )
                }
                None => {
                    (
                        StatusCode::NOT_FOUND,
                        Json(ApiResponse::<OrderStatusData>::error("Order not found"))
                    )
                }
            }
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OrderStatusData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OpenPositionData>::error("Internal error"))
            );
        }
    };

    match PositionService::get_open_position(&app_state, &req.exchange, &req.symbol, &req.product, Some(&req.apikey)).await {
        Ok(Some(p)) => {
            let position = OpenPositionData {
                symbol: p.symbol,
                exchange: p.exchange,
                product: p.product,
                quantity: p.quantity,
                average_price: p.average_price,
                ltp: p.ltp,
                pnl: p.pnl,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(position))
            )
        }
        Ok(None) => {
            // No position found - return zero quantity
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
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OpenPositionData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<DepthData>::error("Internal error"))
            );
        }
    };

    match QuotesService::get_market_depth(&app_state, &req.exchange, &req.symbol, Some(&req.apikey)).await {
        Ok(result) => {
            let depth_data = result.depth;
            let buy: Vec<DepthLevel> = depth_data.bids.into_iter().map(|d| DepthLevel {
                price: d.price,
                quantity: d.quantity,
                orders: d.orders,
            }).collect();
            let sell: Vec<DepthLevel> = depth_data.asks.into_iter().map(|d| DepthLevel {
                price: d.price,
                quantity: d.quantity,
                orders: d.orders,
            }).collect();
            let totalbuyqty = buy.iter().map(|d| d.quantity as i64).sum();
            let totalsellqty = sell.iter().map(|d| d.quantity as i64).sum();

            let depth = DepthData {
                symbol: depth_data.symbol,
                exchange: depth_data.exchange,
                buy,
                sell,
                ltp: 0.0, // Would need to fetch quote for this
                ltq: 0,
                volume: 0,
                oi: 0,
                totalbuyqty,
                totalsellqty,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(depth))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<DepthData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<HistoryData>::error("Internal error"))
            );
        }
    };

    let start_date = req.start_date.as_deref().unwrap_or("");
    let end_date = req.end_date.as_deref().unwrap_or("");

    match HistoryService::get_history(
        &app_state,
        &req.symbol,
        &req.exchange,
        &req.interval,
        start_date,
        end_date,
        Some(&req.apikey),
    ).await {
        Ok(result) => {
            let candles: Vec<Candle> = result.candles.into_iter().map(|c| Candle {
                timestamp: c.timestamp,
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                volume: c.volume,
                oi: None,
            }).collect();
            let history = HistoryData {
                symbol: result.symbol,
                exchange: result.exchange,
                interval: result.interval,
                candles,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(history))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<HistoryData>::error(&e.to_string()))
            )
        }
    }
}

/// Get supported intervals - POST /api/v1/intervals
pub async fn get_intervals(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<IntervalsRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<IntervalsData>::error(&e)));
    }

    let result = HistoryService::get_intervals();
    let intervals = IntervalsData {
        intervals: result.intervals,
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<AnalyzerData>::error("Internal error"))
            );
        }
    };

    match AnalyzerService::get_status(&app_state) {
        Ok(status) => {
            let data = AnalyzerData {
                analyze_mode: status.analyze_mode,
                mode: status.mode,
                total_logs: status.total_logs,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<AnalyzerData>::error(&e.to_string()))
            )
        }
    }
}

/// Toggle analyzer mode - POST /api/v1/analyzer/toggle
pub async fn toggle_analyzer(
    AxumState(state): AxumState<Arc<WebhookState>>,
    Json(req): Json<AnalyzerToggleRequest>,
) -> impl IntoResponse {
    if let Err(e) = state.validate_api_key(&req.apikey) {
        return (StatusCode::FORBIDDEN, Json(ApiResponse::<AnalyzerData>::error(&e)));
    }

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<AnalyzerData>::error("Internal error"))
            );
        }
    };

    match AnalyzerService::toggle_mode(&app_state, req.mode) {
        Ok(status) => {
            let data = AnalyzerData {
                analyze_mode: status.analyze_mode,
                mode: status.mode,
                total_logs: status.total_logs,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<AnalyzerData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<MultiQuotesData>::error("Internal error"))
            );
        }
    };

    // Convert symbols to (exchange, symbol) pairs
    let symbols: Vec<(String, String)> = req.symbols.iter()
        .map(|s| (s.exchange.clone(), s.symbol.clone()))
        .collect();

    match QuotesService::get_multi_quotes(&app_state, symbols, Some(&req.apikey)).await {
        Ok(result) => {
            // MultiQuotesData is Vec<QuoteData>
            let quotes: MultiQuotesData = result.quotes.into_iter().map(|q| QuoteData {
                bid: q.bid,
                ask: q.ask,
                open: q.open,
                high: q.high,
                low: q.low,
                ltp: q.ltp,
                prev_close: q.close,
                volume: q.volume,
                oi: q.oi,
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(quotes))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<MultiQuotesData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<SearchResultItem>>::error("Internal error"))
            );
        }
    };

    match SymbolService::search_symbols(&app_state, &req.query, req.exchange.as_deref(), Some(50)) {
        Ok(symbols) => {
            let results: Vec<SearchResultItem> = symbols.into_iter().map(|s| SearchResultItem {
                symbol: s.symbol,
                name: s.name,
                exchange: s.exchange,
                token: s.token,
                instrumenttype: s.instrument_type,
                lotsize: s.lot_size,
                strike: s.strike,
                expiry: s.expiry,
            }).collect();
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(results))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Vec<SearchResultItem>>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<ExpiryData>::error("Internal error"))
            );
        }
    };

    match SymbolService::get_expiry_dates(&app_state, &req.symbol, &req.exchange, &req.instrumenttype) {
        Ok(result) => {
            let data = ExpiryData {
                expiry_dates: result.expiry_dates,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<ExpiryData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<InstrumentsData>::error("Internal error"))
            );
        }
    };

    let symbols = SymbolService::get_instruments(&app_state, req.exchange.as_deref());
    let data: InstrumentsData = symbols.into_iter().map(|s| InstrumentItem {
        symbol: s.symbol.clone(),
        brsymbol: s.symbol, // broker symbol same as symbol
        name: s.name,
        exchange: s.exchange,
        token: s.token,
        expiry: s.expiry,
        strike: s.strike,
        lotsize: s.lot_size,
        instrumenttype: s.instrument_type,
        tick_size: s.tick_size,
    }).collect();

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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SyntheticFutureData>::error("Internal error"))
            );
        }
    };

    match OptionsService::get_synthetic_future(&app_state, &req.underlying, &req.exchange, &req.expiry_date, Some(&req.apikey)).await {
        Ok(result) => {
            let data = SyntheticFutureData {
                underlying: result.underlying,
                underlying_ltp: result.underlying_ltp,
                expiry: result.expiry,
                atm_strike: result.atm_strike,
                synthetic_future_price: result.synthetic_future_price,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<SyntheticFutureData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionChainData>::error("Internal error"))
            );
        }
    };

    match OptionsService::get_option_chain(&app_state, &req.underlying, &req.exchange, req.expiry_date.as_deref(), Some(&req.apikey)).await {
        Ok(result) => {
            let strikes: Vec<OptionStrike> = result.strikes.into_iter().map(|s| OptionStrike {
                strike: s.strike,
                ce_symbol: s.call_symbol.unwrap_or_default(),
                ce_ltp: s.call_ltp.unwrap_or(0.0),
                ce_oi: s.call_oi.unwrap_or(0),
                ce_volume: s.call_volume.unwrap_or(0),
                ce_iv: s.call_iv.unwrap_or(0.0),
                pe_symbol: s.put_symbol.unwrap_or_default(),
                pe_ltp: s.put_ltp.unwrap_or(0.0),
                pe_oi: s.put_oi.unwrap_or(0),
                pe_volume: s.put_volume.unwrap_or(0),
                pe_iv: s.put_iv.unwrap_or(0.0),
            }).collect();
            let data = OptionChainData {
                underlying: result.underlying,
                underlying_ltp: result.underlying_ltp,
                expiry: result.expiry,
                atm_strike: result.atm_strike,
                strikes,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionChainData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionGreeksData>::error("Internal error"))
            );
        }
    };

    match OptionsService::get_option_greeks(&app_state, &req.symbol, &req.exchange, Some(&req.apikey)).await {
        Ok(result) => {
            let data = OptionGreeksData {
                symbol: result.symbol,
                ltp: result.ltp,
                iv: result.iv,
                delta: result.delta,
                gamma: result.gamma,
                theta: result.theta,
                vega: result.vega,
                rho: result.rho,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionGreeksData>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionsOrderResult>::error("Internal error"))
            );
        }
    };

    // Convert offset to strike_selection string
    let strike_selection = offset_to_strike_selection(req.offset);

    let options_req = crate::services::options_service::OptionsOrderRequest {
        underlying: req.underlying.clone(),
        exchange: req.exchange.clone(),
        option_type: req.option_type.clone(),
        strike_selection,
        expiry_date: req.expiry_date.clone(),
        action: req.action.clone(),
        quantity: req.quantity,
        product: req.product.clone(),
        pricetype: Some(req.price_type.clone()),
    };

    match OptionsService::place_options_order(&app_state, options_req, Some(&req.apikey)).await {
        Ok(result) => {
            if result.success {
                let data = OptionsOrderResult {
                    symbol: result.order_id.clone().unwrap_or_default(),
                    orderid: result.order_id.unwrap_or_default(),
                };
                (
                    StatusCode::OK,
                    Json(ApiResponse::success_with_data(data))
                )
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<OptionsOrderResult>::error(&result.message))
                )
            }
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionsOrderResult>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionSymbolResult>::error("Internal error"))
            );
        }
    };

    // Get underlying LTP for strike calculation (default to 0 if unavailable)
    let underlying_ltp = match QuotesService::get_quote(&app_state, &req.exchange, &req.underlying, Some(&req.apikey)).await {
        Ok(q) => q.ltp,
        Err(_) => 0.0,
    };

    // Convert offset to strike_selection string
    let strike_selection = offset_to_strike_selection(req.offset);

    match OptionsService::get_option_symbol(&app_state, &req.underlying, &req.exchange, &req.option_type, &strike_selection, req.expiry_date.as_deref(), underlying_ltp) {
        Ok(result) => {
            let data = OptionSymbolResult {
                symbol: result.symbol,
                token: result.token,
                exchange: result.exchange,
                strike: result.strike,
                option_type: result.option_type,
                expiry: result.expiry,
            };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionSymbolResult>::error(&e.to_string()))
            )
        }
    }
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

    let app_state = match state.get_app_state() {
        Some(s) => s,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionsMultiOrderResult>::error("Internal error"))
            );
        }
    };

    // Get product from first leg (or default to MIS)
    let product = req.legs.first().map(|l| l.product.clone()).unwrap_or_else(|| "MIS".to_string());

    let legs: Vec<crate::services::options_service::OptionsLeg> = req.legs.iter().map(|leg| {
        crate::services::options_service::OptionsLeg {
            option_type: leg.option_type.clone(),
            strike_selection: offset_to_strike_selection(leg.offset),
            action: leg.action.clone(),
            quantity: leg.quantity,
        }
    }).collect();

    match OptionsService::place_options_multi_order(&app_state, &req.underlying, &req.exchange, req.expiry_date.as_deref(), &product, legs, Some(&req.apikey)).await {
        Ok(order_results) => {
            let results: Vec<OptionsOrderLegResult> = order_results.into_iter().enumerate().map(|(i, r)| {
                OptionsOrderLegResult {
                    leg: (i + 1) as i32,
                    symbol: r.order_id.clone().unwrap_or_default(),
                    orderid: r.order_id,
                    status: if r.success { "success".to_string() } else { "error".to_string() },
                    message: if r.success { None } else { Some(r.message) },
                }
            }).collect();
            let data = OptionsMultiOrderResult { results };
            (
                StatusCode::OK,
                Json(ApiResponse::success_with_data(data))
            )
        }
        Err(e) => {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<OptionsMultiOrderResult>::error(&e.to_string()))
            )
        }
    }
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

/// Convert offset integer to strike_selection string
/// Offset 0 = ATM, positive = OTM, negative = ITM
fn offset_to_strike_selection(offset: i32) -> String {
    match offset {
        0 => "ATM".to_string(),
        n if n > 0 => format!("OTM{}", n),
        n => format!("ITM{}", n.abs()),
    }
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
