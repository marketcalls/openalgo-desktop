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

#[derive(Debug, Serialize)]
pub struct SetupCheckResponse {
    pub status: String,
    pub needs_setup: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct SetupResponse {
    pub status: String,
    pub message: String,
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

/// Check if initial setup is required (no users exist)
#[tauri::command]
pub async fn check_setup(state: State<'_, AppState>) -> Result<SetupCheckResponse> {
    let needs_setup = !state.sqlite.has_user()?;

    tracing::info!("Setup check: needs_setup = {}", needs_setup);

    Ok(SetupCheckResponse {
        status: "success".to_string(),
        needs_setup,
    })
}

/// Validate password strength
/// Requirements: 8+ chars, 1 uppercase, 1 lowercase, 1 number, 1 special char (!@#$%^&*)
fn validate_password_strength(password: &str) -> std::result::Result<(), String> {
    if password.len() < 8 {
        return Err("Password must be at least 8 characters long".to_string());
    }

    if !password.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("Password must contain at least 1 uppercase letter (A-Z)".to_string());
    }

    if !password.chars().any(|c| c.is_ascii_lowercase()) {
        return Err("Password must contain at least 1 lowercase letter (a-z)".to_string());
    }

    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err("Password must contain at least 1 number (0-9)".to_string());
    }

    let special_chars = ['!', '@', '#', '$', '%', '^', '&', '*'];
    if !password.chars().any(|c| special_chars.contains(&c)) {
        return Err("Password must contain at least 1 special character (!@#$%^&*)".to_string());
    }

    Ok(())
}

/// Reset user data (for password recovery when pepper changes)
#[tauri::command]
pub async fn reset_user_data(state: State<'_, AppState>) -> Result<SetupResponse> {
    tracing::info!("Resetting user data for password recovery");

    // Clear any existing sessions
    state.set_user_session(None);
    state.set_broker_session(None);

    // Delete all users
    state.sqlite.delete_all_users()?;

    tracing::info!("User data reset successfully");

    Ok(SetupResponse {
        status: "success".to_string(),
        message: "User data reset. Please set up a new account.".to_string(),
    })
}

/// Initial setup - create first admin user and auto-generate API key
#[tauri::command]
pub async fn setup(state: State<'_, AppState>, request: SetupRequest) -> Result<SetupResponse> {
    tracing::info!("Setup request for user: {}", request.username);

    // Check if setup is already complete
    if state.sqlite.has_user()? {
        return Err(AppError::Auth(
            "Setup already completed. Please login instead.".to_string(),
        ));
    }

    // Validate username
    if request.username.trim().is_empty() {
        return Err(AppError::Validation("Username is required".to_string()));
    }

    // Validate email
    if request.email.trim().is_empty() || !request.email.contains('@') {
        return Err(AppError::Validation("Valid email is required".to_string()));
    }

    // Validate password strength
    if let Err(msg) = validate_password_strength(&request.password) {
        return Err(AppError::Validation(msg));
    }

    // Create the user
    let user = state
        .sqlite
        .create_user(&request.username, &request.password, &state.security)?;

    tracing::info!("Admin user '{}' created successfully (id: {})", user.username, user.id);

    // Auto-generate API key for the user
    match state.sqlite.create_api_key("default", "read,write", &state.security) {
        Ok((id, _api_key)) => {
            tracing::info!("Auto-generated API key (id: {}) for user '{}'", id, user.username);
        }
        Err(e) => {
            tracing::warn!("Failed to auto-generate API key: {}", e);
            // Don't fail setup if API key generation fails
        }
    }

    Ok(SetupResponse {
        status: "success".to_string(),
        message: "Account created successfully! Please login to continue.".to_string(),
    })
}
