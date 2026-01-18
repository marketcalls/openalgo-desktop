//! Broker connection commands

use crate::brokers::BrokerCredentials;
use crate::error::{AppError, Result};
use crate::state::{AppState, BrokerSession};
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct BrokerLoginRequest {
    pub broker_id: String,
    pub credentials: BrokerCredentials,
}

#[derive(Debug, Serialize)]
pub struct BrokerLoginResponse {
    pub success: bool,
    pub broker_id: String,
    pub user_id: String,
    pub user_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrokerStatus {
    pub connected: bool,
    pub broker_id: Option<String>,
    pub user_id: Option<String>,
    pub authenticated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BrokerInfo {
    pub id: String,
    pub name: String,
    pub logo: String,
    pub requires_totp: bool,
}

/// Login to a broker
#[tauri::command]
pub async fn broker_login(
    state: State<'_, AppState>,
    request: BrokerLoginRequest,
) -> Result<BrokerLoginResponse> {
    tracing::info!("Broker login attempt: {}", request.broker_id);

    // Ensure user is authenticated first
    if !state.is_authenticated() {
        return Err(AppError::Auth("User not authenticated".to_string()));
    }

    // Get broker from registry
    let broker = state
        .brokers
        .get(&request.broker_id)
        .ok_or_else(|| AppError::Broker(format!("Unknown broker: {}", request.broker_id)))?;

    // Authenticate with broker
    let auth_response = broker.authenticate(request.credentials).await?;

    // Create broker session
    let session = BrokerSession {
        broker_id: request.broker_id.clone(),
        auth_token: auth_response.auth_token,
        feed_token: auth_response.feed_token,
        user_id: auth_response.user_id.clone(),
        authenticated_at: chrono::Utc::now(),
    };

    // Store encrypted auth token in database
    state.sqlite.store_auth_token(
        &request.broker_id,
        &session.auth_token,
        session.feed_token.as_deref(),
        &state.security,
    )?;

    state.set_broker_session(Some(session));

    tracing::info!("Broker {} login successful", request.broker_id);

    Ok(BrokerLoginResponse {
        success: true,
        broker_id: request.broker_id,
        user_id: auth_response.user_id,
        user_name: auth_response.user_name,
    })
}

/// Logout from current broker
#[tauri::command]
pub async fn broker_logout(state: State<'_, AppState>) -> Result<()> {
    tracing::info!("Broker logout");

    if let Some(session) = state.get_broker_session() {
        // Clear stored auth token
        state.sqlite.delete_auth_token(&session.broker_id)?;
    }

    state.set_broker_session(None);

    Ok(())
}

/// Get current broker connection status
#[tauri::command]
pub async fn get_broker_status(state: State<'_, AppState>) -> Result<BrokerStatus> {
    Ok(match state.get_broker_session() {
        Some(session) => BrokerStatus {
            connected: true,
            broker_id: Some(session.broker_id),
            user_id: Some(session.user_id),
            authenticated_at: Some(session.authenticated_at.to_rfc3339()),
        },
        None => BrokerStatus {
            connected: false,
            broker_id: None,
            user_id: None,
            authenticated_at: None,
        },
    })
}

/// Set active broker (for switching between brokers)
#[tauri::command]
pub async fn set_active_broker(
    state: State<'_, AppState>,
    broker_id: String,
) -> Result<()> {
    tracing::info!("Setting active broker: {}", broker_id);

    // Verify broker exists
    if state.brokers.get(&broker_id).is_none() {
        return Err(AppError::Broker(format!("Unknown broker: {}", broker_id)));
    }

    // Try to restore session from stored auth token
    if let Some((auth_token, feed_token)) = state.sqlite.get_auth_token(&broker_id, &state.security)? {
        let session = BrokerSession {
            broker_id: broker_id.clone(),
            auth_token,
            feed_token,
            user_id: String::new(), // Will be populated on first API call
            authenticated_at: chrono::Utc::now(),
        };
        state.set_broker_session(Some(session));
    }

    Ok(())
}

/// Get list of available brokers
#[tauri::command]
pub async fn get_available_brokers(state: State<'_, AppState>) -> Result<Vec<BrokerInfo>> {
    Ok(state
        .brokers
        .list()
        .iter()
        .map(|b| BrokerInfo {
            id: b.id().to_string(),
            name: b.name().to_string(),
            logo: b.logo().to_string(),
            requires_totp: b.requires_totp(),
        })
        .collect())
}
