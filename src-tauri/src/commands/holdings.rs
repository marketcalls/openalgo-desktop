//! Holdings commands

use crate::brokers::types::Holding;
use crate::error::{AppError, Result};
use crate::state::AppState;
use tauri::State;

/// Get all holdings
#[tauri::command]
pub async fn get_holdings(state: State<'_, AppState>) -> Result<Vec<Holding>> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_holdings(&session.auth_token).await
}
