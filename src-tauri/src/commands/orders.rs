//! Order management commands

use crate::brokers::types::{Order, OrderRequest, ModifyOrderRequest};
use crate::error::Result;
use crate::services::{OrderService, OrderbookService};
use crate::state::AppState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct OrderResponse {
    pub success: bool,
    pub order_id: String,
    pub message: Option<String>,
}

/// Place a new order
///
/// Routes to sandbox in analyze mode.
#[tauri::command]
pub async fn place_order(
    state: State<'_, AppState>,
    order: OrderRequest,
) -> Result<OrderResponse> {
    tracing::info!("Placing order: {:?}", order);

    let result = OrderService::place_order(&state, order, None).await?;
    tracing::info!("Order placed in {} mode", result.mode);

    Ok(OrderResponse {
        success: result.success,
        order_id: result.order_id.unwrap_or_default(),
        message: Some(result.message),
    })
}

/// Modify an existing order
///
/// Routes to sandbox in analyze mode.
#[tauri::command]
pub async fn modify_order(
    state: State<'_, AppState>,
    order_id: String,
    order: ModifyOrderRequest,
) -> Result<OrderResponse> {
    tracing::info!("Modifying order {}: {:?}", order_id, order);

    let result = OrderService::modify_order(&state, &order_id, order, None).await?;

    Ok(OrderResponse {
        success: result.success,
        order_id: result.order_id,
        message: Some(result.message),
    })
}

/// Cancel an order
///
/// Routes to sandbox in analyze mode.
#[tauri::command]
pub async fn cancel_order(
    state: State<'_, AppState>,
    order_id: String,
    variety: Option<String>,
) -> Result<OrderResponse> {
    tracing::info!("Cancelling order: {}", order_id);

    let result = OrderService::cancel_order(&state, &order_id, variety.as_deref(), None).await?;

    Ok(OrderResponse {
        success: result.success,
        order_id: result.order_id,
        message: Some(result.message),
    })
}

/// Get order book
///
/// Routes to sandbox orders in analyze mode.
#[tauri::command]
pub async fn get_order_book(state: State<'_, AppState>) -> Result<Vec<Order>> {
    let result = OrderbookService::get_orderbook(&state, None).await?;
    tracing::info!("Order book retrieved in {} mode", result.mode);
    Ok(result.orders)
}

/// Get trade book
///
/// Routes to sandbox trades in analyze mode.
#[tauri::command]
pub async fn get_trade_book(state: State<'_, AppState>) -> Result<Vec<Order>> {
    let result = OrderbookService::get_tradebook(&state, None).await?;
    tracing::info!("Trade book retrieved in {} mode", result.mode);
    Ok(result.trades)
}
