//! Position management commands

use crate::brokers::types::Position;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct ClosePositionRequest {
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub position_type: String, // "long" or "short"
}

#[derive(Debug, Serialize)]
pub struct ClosePositionResponse {
    pub success: bool,
    pub order_id: Option<String>,
    pub message: String,
}

/// Get all positions
#[tauri::command]
pub async fn get_positions(state: State<'_, AppState>) -> Result<Vec<Position>> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_positions(&session.auth_token).await
}

/// Close a specific position
#[tauri::command]
pub async fn close_position(
    state: State<'_, AppState>,
    request: ClosePositionRequest,
) -> Result<ClosePositionResponse> {
    tracing::info!("Closing position: {:?}", request);

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    // Determine order side based on position type
    let side = if request.position_type == "long" {
        "SELL"
    } else {
        "BUY"
    };

    let order = crate::brokers::types::OrderRequest {
        symbol: request.symbol,
        exchange: request.exchange,
        side: side.to_string(),
        quantity: request.quantity,
        price: 0.0,
        order_type: "MARKET".to_string(),
        product: request.product,
        validity: "DAY".to_string(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    };

    let response = broker.place_order(&session.auth_token, order).await?;

    Ok(ClosePositionResponse {
        success: true,
        order_id: Some(response.order_id),
        message: "Position close order placed".to_string(),
    })
}

/// Close all open positions
#[tauri::command]
pub async fn close_all_positions(state: State<'_, AppState>) -> Result<Vec<ClosePositionResponse>> {
    tracing::info!("Closing all positions");

    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    let positions = broker.get_positions(&session.auth_token).await?;

    let mut results = Vec::new();

    for position in positions {
        if position.quantity == 0 {
            continue;
        }

        let side = if position.quantity > 0 { "SELL" } else { "BUY" };
        let qty = position.quantity.abs();

        let order = crate::brokers::types::OrderRequest {
            symbol: position.symbol.clone(),
            exchange: position.exchange.clone(),
            side: side.to_string(),
            quantity: qty,
            price: 0.0,
            order_type: "MARKET".to_string(),
            product: position.product.clone(),
            validity: "DAY".to_string(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };

        match broker.place_order(&session.auth_token, order).await {
            Ok(response) => {
                results.push(ClosePositionResponse {
                    success: true,
                    order_id: Some(response.order_id),
                    message: format!("Closed position: {}", position.symbol),
                });
            }
            Err(e) => {
                results.push(ClosePositionResponse {
                    success: false,
                    order_id: None,
                    message: format!("Failed to close {}: {}", position.symbol, e),
                });
            }
        }
    }

    Ok(results)
}
