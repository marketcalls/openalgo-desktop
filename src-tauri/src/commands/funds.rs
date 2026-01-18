//! Funds/margin commands

use crate::brokers::types::Funds;
use crate::error::{AppError, Result};
use crate::state::AppState;
use tauri::State;

/// Get available funds/margin
#[tauri::command]
pub async fn get_funds(state: State<'_, AppState>) -> Result<Funds> {
    let session = state
        .get_broker_session()
        .ok_or_else(|| AppError::Auth("Broker not connected".to_string()))?;

    let broker = state
        .brokers
        .get(&session.broker_id)
        .ok_or_else(|| AppError::Broker("Broker not found".to_string()))?;

    broker.get_funds(&session.auth_token).await
}
