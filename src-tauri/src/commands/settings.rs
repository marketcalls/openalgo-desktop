//! Settings management commands

use crate::db::sqlite::models::Settings;
use crate::db::sqlite::{AutoLogoutConfig, WebhookConfig};
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
pub struct UpdateAutoLogoutRequest {
    pub enabled: Option<bool>,
    pub hour: Option<u32>,
    pub minute: Option<u32>,
    pub warnings: Option<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWebhookConfigRequest {
    pub enabled: Option<bool>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub ngrok_url: Option<String>,
    pub webhook_secret: Option<String>,
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

    // Store in keychain
    state.security.store_broker_credentials(
        &request.broker_id,
        &request.api_key,
        request.api_secret.as_deref(),
        request.client_id.as_deref(),
    )?;

    // Track in SQLite (for efficient has_credentials lookups)
    state.sqlite.mark_broker_configured(&request.broker_id)?;

    Ok(())
}

/// Delete broker credentials from OS keychain
#[tauri::command]
pub async fn delete_broker_credentials(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<()> {
    tracing::info!("Deleting credentials for broker: {}", broker_id);

    // Delete from keychain
    state.security.delete_broker_credentials(&broker_id)?;

    // Remove from SQLite tracking
    state.sqlite.unmark_broker_configured(&broker_id)?;

    Ok(())
}

/// Get auto-logout configuration
#[tauri::command]
pub async fn get_auto_logout_config(state: State<'_, AppState>) -> Result<AutoLogoutConfig> {
    state.sqlite.get_auto_logout_config()
}

/// Update auto-logout configuration
#[tauri::command]
pub async fn update_auto_logout_config(
    state: State<'_, AppState>,
    request: UpdateAutoLogoutRequest,
) -> Result<AutoLogoutConfig> {
    tracing::info!("Updating auto-logout config: {:?}", request);

    state.sqlite.update_auto_logout_config(
        request.enabled,
        request.hour,
        request.minute,
        request.warnings,
    )
}

/// Get webhook configuration
#[tauri::command]
pub async fn get_webhook_config(state: State<'_, AppState>) -> Result<WebhookConfig> {
    state.sqlite.get_webhook_config()
}

/// Update webhook configuration
#[tauri::command]
pub async fn update_webhook_config(
    state: State<'_, AppState>,
    request: UpdateWebhookConfigRequest,
) -> Result<WebhookConfig> {
    tracing::info!("Updating webhook config: {:?}", request);

    state.sqlite.update_webhook_config(
        request.enabled,
        request.port,
        request.host,
        request.ngrok_url,
        request.webhook_secret,
    )
}

// ============================================================================
// Broker Configuration Types and Commands
// ============================================================================

/// Broker info for frontend
#[derive(Debug, Clone, Serialize)]
pub struct BrokerInfo {
    pub id: String,
    pub name: String,
    pub auth_type: String,  // "totp" or "oauth"
    pub has_credentials: bool,
}

/// Broker config response (similar to Flask /auth/broker-config)
#[derive(Debug, Serialize)]
pub struct BrokerConfigResponse {
    pub status: String,
    pub broker_name: Option<String>,
    pub broker_api_key: Option<String>,  // Masked for security
    pub redirect_url: String,
    pub available_brokers: Vec<BrokerInfo>,
}

/// Broker credentials response (masked)
#[derive(Debug, Serialize)]
pub struct BrokerCredentialsResponse {
    pub broker_id: String,
    pub api_key_masked: String,
    pub has_api_secret: bool,
    pub client_id: Option<String>,
}

/// All supported brokers (hardcoded list)
fn get_all_brokers() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("angel", "Angel One", "totp"),
        ("zerodha", "Zerodha", "oauth"),
        ("fyers", "Fyers", "oauth"),
        ("fivepaisa", "5 Paisa", "totp"),
        ("fivepaisaxts", "5 Paisa (XTS)", "totp"),
        ("aliceblue", "Alice Blue", "totp"),
        ("compositedge", "CompositEdge", "oauth"),
        ("dhan", "Dhan", "oauth"),
        ("dhan_sandbox", "Dhan (Sandbox)", "totp"),
        ("definedge", "Definedge", "totp"),
        ("firstock", "Firstock", "totp"),
        ("flattrade", "Flattrade", "oauth"),
        ("groww", "Groww", "totp"),
        ("ibulls", "Ibulls", "totp"),
        ("iifl", "IIFL", "totp"),
        ("indmoney", "IndMoney", "totp"),
        ("jainamxts", "JainamXts", "totp"),
        ("kotak", "Kotak Securities", "totp"),
        ("motilal", "Motilal Oswal", "totp"),
        ("mstock", "mStock by Mirae Asset", "totp"),
        ("nubra", "Nubra", "totp"),
        ("paytm", "Paytm Money", "oauth"),
        ("pocketful", "Pocketful", "oauth"),
        ("samco", "Samco", "totp"),
        ("shoonya", "Shoonya", "totp"),
        ("tradejini", "Tradejini", "totp"),
        ("upstox", "Upstox", "oauth"),
        ("wisdom", "Wisdom Capital", "totp"),
        ("zebu", "Zebu", "totp"),
    ]
}

/// Mask API key for display (show first 4 and last 4 chars)
fn mask_api_key(key: &str) -> String {
    if key.len() <= 8 {
        "*".repeat(key.len())
    } else {
        format!("{}...{}", &key[..4], &key[key.len()-4..])
    }
}

/// Get broker configuration - available brokers and configured credentials
#[tauri::command]
pub async fn get_broker_config(state: State<'_, AppState>) -> Result<BrokerConfigResponse> {
    tracing::info!("Getting broker configuration");

    // Get settings to find default/configured broker
    let settings = state.sqlite.get_settings()?;
    let default_broker = settings.default_broker;

    // Get list of configured brokers from SQLite (avoids keychain prompts)
    let configured_broker_ids = state.sqlite.get_configured_brokers()?;
    let configured_set: std::collections::HashSet<_> = configured_broker_ids.into_iter().collect();

    // Build list of available brokers with credential status
    let mut available_brokers = Vec::new();
    let mut configured_broker: Option<String> = None;
    let mut configured_api_key: Option<String> = None;

    for (id, name, auth_type) in get_all_brokers() {
        let has_credentials = configured_set.contains(id);

        // If this broker has credentials and matches default, get the masked key
        if has_credentials {
            if configured_broker.is_none() || Some(id.to_string()) == default_broker {
                // Only access keychain for the specific configured broker we need to display
                if let Ok(Some((api_key, _, _))) = state.security.get_broker_credentials(id) {
                    configured_broker = Some(id.to_string());
                    configured_api_key = Some(mask_api_key(&api_key));
                }
            }
        }

        available_brokers.push(BrokerInfo {
            id: id.to_string(),
            name: name.to_string(),
            auth_type: auth_type.to_string(),
            has_credentials,
        });
    }

    // Default redirect URL for desktop app (webhook server)
    let webhook_config = state.sqlite.get_webhook_config()?;
    let redirect_url = webhook_config.ngrok_url.unwrap_or_else(|| {
        format!("http://{}:{}", webhook_config.host, webhook_config.port)
    });

    Ok(BrokerConfigResponse {
        status: "success".to_string(),
        broker_name: configured_broker,
        broker_api_key: configured_api_key,
        redirect_url: format!("{}/{{broker}}/callback", redirect_url),
        available_brokers,
    })
}

/// Get credentials for a specific broker (masked)
#[tauri::command]
pub async fn get_broker_credentials(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<Option<BrokerCredentialsResponse>> {
    tracing::info!("Getting credentials for broker: {}", broker_id);

    match state.security.get_broker_credentials(&broker_id) {
        Ok(Some((api_key, api_secret, client_id))) => {
            Ok(Some(BrokerCredentialsResponse {
                broker_id,
                api_key_masked: mask_api_key(&api_key),
                has_api_secret: api_secret.is_some(),
                client_id,
            }))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!("Error getting credentials: {}", e);
            Ok(None)
        }
    }
}

/// Raw broker credentials for internal use (login)
#[derive(Debug, Serialize)]
pub struct RawBrokerCredentials {
    pub api_key: String,
    pub api_secret: Option<String>,
    pub client_id: Option<String>,
}

/// Get raw credentials for broker login (internal use only)
#[tauri::command]
pub async fn get_raw_broker_credentials(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<Option<RawBrokerCredentials>> {
    tracing::debug!("Getting raw credentials for broker login: {}", broker_id);

    match state.security.get_broker_credentials(&broker_id) {
        Ok(Some((api_key, api_secret, client_id))) => {
            Ok(Some(RawBrokerCredentials {
                api_key,
                api_secret,
                client_id,
            }))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!("Error getting raw credentials: {}", e);
            Ok(None)
        }
    }
}

/// Check if broker has credentials configured (without retrieving them)
#[tauri::command]
pub async fn has_broker_credentials(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<bool> {
    match state.security.get_broker_credentials(&broker_id) {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(_) => Ok(false),
    }
}
