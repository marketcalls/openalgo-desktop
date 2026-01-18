//! Strategy management commands

use crate::db::sqlite::models::Strategy;
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct CreateStrategyRequest {
    pub name: String,
    pub webhook_id: String,
    pub exchange: String,
    pub symbol: String,
    pub product: String,
    pub quantity: i32,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStrategyRequest {
    pub id: i64,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub symbol: Option<String>,
    pub product: Option<String>,
    pub quantity: Option<i32>,
    pub enabled: Option<bool>,
}

/// Get all strategies
#[tauri::command]
pub async fn get_strategies(state: State<'_, AppState>) -> Result<Vec<Strategy>> {
    state.sqlite.get_strategies()
}

/// Create a new strategy
#[tauri::command]
pub async fn create_strategy(
    state: State<'_, AppState>,
    request: CreateStrategyRequest,
) -> Result<Strategy> {
    tracing::info!("Creating strategy: {}", request.name);

    let strategy = Strategy {
        id: 0, // Will be set by database
        name: request.name,
        webhook_id: request.webhook_id,
        exchange: request.exchange,
        symbol: request.symbol,
        product: request.product,
        quantity: request.quantity,
        enabled: request.enabled,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    state.sqlite.create_strategy(&strategy)
}

/// Update an existing strategy
#[tauri::command]
pub async fn update_strategy(
    state: State<'_, AppState>,
    request: UpdateStrategyRequest,
) -> Result<Strategy> {
    tracing::info!("Updating strategy: {}", request.id);

    state.sqlite.update_strategy(
        request.id,
        request.name,
        request.exchange,
        request.symbol,
        request.product,
        request.quantity,
        request.enabled,
    )
}

/// Delete a strategy
#[tauri::command]
pub async fn delete_strategy(state: State<'_, AppState>, id: i64) -> Result<()> {
    tracing::info!("Deleting strategy: {}", id);
    state.sqlite.delete_strategy(id)
}

/// Toggle strategy enabled/disabled
#[tauri::command]
pub async fn toggle_strategy(state: State<'_, AppState>, id: i64, enabled: bool) -> Result<Strategy> {
    tracing::info!("Toggling strategy {} to enabled={}", id, enabled);
    state.sqlite.update_strategy(id, None, None, None, None, None, Some(enabled))
}
