//! Funds/margin commands

use crate::brokers::types::Funds;
use crate::error::Result;
use crate::services::FundsService;
use crate::state::AppState;
use tauri::State;

/// Get available funds/margin
///
/// Routes to sandbox funds when in analyze mode.
#[tauri::command]
pub async fn get_funds(state: State<'_, AppState>) -> Result<Funds> {
    let result = FundsService::get_funds(&state, None).await?;
    tracing::info!("Funds retrieved in {} mode", result.mode);
    Ok(result.funds)
}
