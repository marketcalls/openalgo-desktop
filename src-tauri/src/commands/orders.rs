//! Order management commands

use crate::brokers::types::{Order, OrderRequest, ModifyOrderRequest};
use crate::error::{AppError, Result};
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
#[tauri::command]
pub async fn place_order(
    state: State<'_, AppState>,
    order: OrderRequest,
) -> Result<OrderResponse> {
    tracing::info!("Placing order: {:?}", order);

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    let response = broker.place_order(&session.auth_token, order).await?;

    Ok(OrderResponse {
        success: true,
        order_id: response.order_id,
        message: response.message,
    })
}

/// Modify an existing order
#[tauri::command]
pub async fn modify_order(
    state: State<'_, AppState>,
    order_id: String,
    order: ModifyOrderRequest,
) -> Result<OrderResponse> {
    tracing::info!("Modifying order {}: {:?}", order_id, order);

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    let response = broker.modify_order(&session.auth_token, &order_id, order).await?;

    Ok(OrderResponse {
        success: true,
        order_id: response.order_id,
        message: response.message,
    })
}

/// Cancel an order
#[tauri::command]
pub async fn cancel_order(
    state: State<'_, AppState>,
    order_id: String,
    variety: Option<String>,
) -> Result<OrderResponse> {
    tracing::info!("Cancelling order: {}", order_id);

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.cancel_order(&session.auth_token, &order_id, variety.as_deref()).await?;

    Ok(OrderResponse {
        success: true,
        order_id,
        message: Some("Order cancelled successfully".to_string()),
    })
}

/// Get order book
#[tauri::command]
pub async fn get_order_book(state: State<'_, AppState>) -> Result<Vec<Order>> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_order_book(&session.auth_token).await
}

/// Get trade book
#[tauri::command]
pub async fn get_trade_book(state: State<'_, AppState>) -> Result<Vec<Order>> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_trade_book(&session.auth_token).await
}
