//! WebSocket Tauri Commands
//!
//! Provides IPC commands for controlling the WebSocket connection
//! from the frontend.

use crate::error::Result;
use crate::state::AppState;
use crate::websocket::{SubscriptionMode, SubscriptionRequest};
use serde::{Deserialize, Serialize};
use tauri::State;

/// WebSocket connection status
#[derive(Debug, Clone, Serialize)]
pub struct WebSocketStatus {
    pub connected: bool,
    pub broker: Option<String>,
    pub subscriptions: usize,
}

/// Subscription request from frontend
#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeRequest {
    pub exchange: String,
    pub token: String,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_mode() -> String {
    "quote".to_string()
}

/// Connect to WebSocket
#[tauri::command]
pub async fn websocket_connect(state: State<'_, AppState>) -> Result<bool> {
    // Get broker credentials from session
    let broker_session = state.get_broker_session()
        .ok_or_else(|| crate::error::AppError::Auth("Not logged in to broker".to_string()))?;

    let broker_id = broker_session.broker_id.clone();
    let client_id = broker_session.user_id.clone();

    // Get auth tokens (auth_token, feed_token)
    let tokens = state
        .sqlite
        .get_auth_token(&broker_id, &state.security)?
        .ok_or_else(|| crate::error::AppError::Auth("No auth tokens found".to_string()))?;

    let _auth_token = tokens.0;
    let feed_token = tokens.1
        .ok_or_else(|| crate::error::AppError::Auth("No feed token found".to_string()))?;

    // Get API key from keychain for WebSocket authentication
    let creds = state.security.get_broker_credentials(&broker_id)?
        .ok_or_else(|| crate::error::AppError::Auth("No broker credentials found".to_string()))?;
    let api_key = creds.0;

    // Connect
    state
        .websocket
        .connect(&broker_id, &client_id, &api_key, &feed_token)
        .await?;

    Ok(true)
}

/// Disconnect from WebSocket
#[tauri::command]
pub async fn websocket_disconnect(state: State<'_, AppState>) -> Result<bool> {
    state.websocket.disconnect().await?;
    Ok(true)
}

/// Get WebSocket status
#[tauri::command]
pub fn websocket_status(state: State<'_, AppState>) -> WebSocketStatus {
    WebSocketStatus {
        connected: state.websocket.is_connected(),
        broker: state.websocket.get_broker(),
        subscriptions: 0, // Could track this in manager if needed
    }
}

/// Subscribe to symbols
#[tauri::command]
pub async fn websocket_subscribe(
    state: State<'_, AppState>,
    symbols: Vec<SubscribeRequest>,
) -> Result<bool> {
    let requests: Vec<SubscriptionRequest> = symbols
        .into_iter()
        .map(|s| {
            let mode = match s.mode.to_lowercase().as_str() {
                "ltp" => SubscriptionMode::Ltp,
                "quote" => SubscriptionMode::Quote,
                "snapquote" | "snap" => SubscriptionMode::SnapQuote,
                "full" | "depth" => SubscriptionMode::Full,
                _ => SubscriptionMode::Quote,
            };

            // Register symbol mapping if provided
            if let Some(symbol) = &s.symbol {
                state.websocket.register_symbol(&s.token, symbol, &s.exchange);
            }

            SubscriptionRequest {
                exchange: s.exchange,
                token: s.token,
                mode,
            }
        })
        .collect();

    state.websocket.subscribe(requests).await?;
    Ok(true)
}

/// Unsubscribe from symbols
#[tauri::command]
pub async fn websocket_unsubscribe(
    state: State<'_, AppState>,
    symbols: Vec<(String, String)>, // [(exchange, token)]
) -> Result<bool> {
    state.websocket.unsubscribe(symbols).await?;
    Ok(true)
}

/// Register symbol mapping (token -> symbol name)
#[tauri::command]
pub fn websocket_register_symbol(
    state: State<'_, AppState>,
    token: String,
    symbol: String,
    exchange: String,
) {
    state.websocket.register_symbol(&token, &symbol, &exchange);
}
