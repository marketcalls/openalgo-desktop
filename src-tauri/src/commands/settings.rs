//! Settings management commands

use crate::db::sqlite::models::Settings;
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub theme: Option<String>,
    pub default_broker: Option<String>,
    pub default_exchange: Option<String>,
    pub default_product: Option<String>,
    pub order_confirm: Option<bool>,
    pub sound_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SaveBrokerCredentialsRequest {
    pub broker_id: String,
    pub api_key: String,
    pub api_secret: Option<String>,
    pub client_id: Option<String>,
}

/// Get current settings
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings> {
    state.sqlite.get_settings()
}

/// Update settings
#[tauri::command]
pub async fn update_settings(
    state: State<'_, AppState>,
    request: UpdateSettingsRequest,
) -> Result<Settings> {
    tracing::info!("Updating settings");

    state.sqlite.update_settings(
        request.theme,
        request.default_broker,
        request.default_exchange,
        request.default_product,
        request.order_confirm,
        request.sound_enabled,
    )
}

/// Save broker credentials to OS keychain
#[tauri::command]
pub async fn save_broker_credentials(
    state: State<'_, AppState>,
    request: SaveBrokerCredentialsRequest,
) -> Result<()> {
    tracing::info!("Saving credentials for broker: {}", request.broker_id);

    state.security.store_broker_credentials(
        &request.broker_id,
        &request.api_key,
        request.api_secret.as_deref(),
        request.client_id.as_deref(),
    )?;

    Ok(())
}

/// Delete broker credentials from OS keychain
#[tauri::command]
pub async fn delete_broker_credentials(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<()> {
    tracing::info!("Deleting credentials for broker: {}", broker_id);

    state.security.delete_broker_credentials(&broker_id)?;

    Ok(())
}
