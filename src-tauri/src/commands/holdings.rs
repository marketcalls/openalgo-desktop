//! Holdings commands

use crate::brokers::types::Holding;
use crate::error::Result;
use crate::services::HoldingsService;
use crate::state::AppState;
use tauri::State;

/// Get all holdings
///
/// Routes to sandbox holdings when in analyze mode.
#[tauri::command]
pub async fn get_holdings(state: State<'_, AppState>) -> Result<Vec<Holding>> {
    let result = HoldingsService::get_holdings(&state, None).await?;
    tracing::info!("Holdings retrieved in {} mode", result.mode);
    Ok(result.holdings)
}
