//! Position management commands

use crate::brokers::types::Position;
use crate::error::Result;
use crate::services::PositionService;
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
///
/// Routes to sandbox positions when in analyze mode.
#[tauri::command]
pub async fn get_positions(state: State<'_, AppState>) -> Result<Vec<Position>> {
    let result = PositionService::get_positions(&state, None).await?;
    tracing::info!("Positions retrieved in {} mode", result.mode);
    Ok(result.positions)
}

/// Close a specific position
///
/// Routes to sandbox in analyze mode.
#[tauri::command]
pub async fn close_position(
    state: State<'_, AppState>,
    request: ClosePositionRequest,
) -> Result<ClosePositionResponse> {
    tracing::info!("Closing position: {:?}", request);

    let result = PositionService::close_position(
        &state,
        &request.exchange,
        &request.symbol,
        &request.product,
        None,
    ).await?;

    Ok(ClosePositionResponse {
        success: result.success,
        order_id: result.order_id,
        message: result.message,
    })
}

/// Close all open positions
///
/// Routes to sandbox in analyze mode.
#[tauri::command]
pub async fn close_all_positions(state: State<'_, AppState>) -> Result<Vec<ClosePositionResponse>> {
    tracing::info!("Closing all positions");

    let results = PositionService::close_all_positions(&state, None).await?;

    Ok(results.into_iter().map(|r| ClosePositionResponse {
        success: r.success,
        order_id: r.order_id,
        message: r.message,
    }).collect())
}
