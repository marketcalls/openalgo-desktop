//! Authentication commands

use crate::error::{AppError, Result};
use crate::state::{AppState, UserSession};
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub user_id: i64,
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct UserInfo {
    pub user_id: i64,
    pub username: String,
    pub authenticated_at: String,
}

/// Login with username and password
#[tauri::command]
pub async fn login(
    state: State<'_, AppState>,
    request: LoginRequest,
) -> Result<LoginResponse> {
    tracing::info!("Login attempt for user: {}", request.username);

    // Verify credentials against database
    let user = state
        .sqlite
        .verify_user(&request.username, &request.password, &state.security)?
        .ok_or_else(|| AppError::Auth("Invalid username or password".to_string()))?;

    // Create session
    let session = UserSession {
        user_id: user.id,
        username: user.username.clone(),
        authenticated_at: chrono::Utc::now(),
    };

    state.set_user_session(Some(session));

    tracing::info!("User {} logged in successfully", user.username);

    Ok(LoginResponse {
        success: true,
        user_id: user.id,
        username: user.username,
    })
}

/// Logout current user
#[tauri::command]
pub async fn logout(state: State<'_, AppState>) -> Result<()> {
    tracing::info!("User logout");

    // Clear user session
    state.set_user_session(None);

    // Clear broker session
    state.set_broker_session(None);

    Ok(())
}

/// Check if user is authenticated
#[tauri::command]
pub async fn check_session(state: State<'_, AppState>) -> Result<bool> {
    Ok(state.is_authenticated())
}

/// Get current user info
#[tauri::command]
pub async fn get_current_user(state: State<'_, AppState>) -> Result<Option<UserInfo>> {
    Ok(state.get_user_session().map(|s| UserInfo {
        user_id: s.user_id,
        username: s.username,
        authenticated_at: s.authenticated_at.to_rfc3339(),
    }))
}
