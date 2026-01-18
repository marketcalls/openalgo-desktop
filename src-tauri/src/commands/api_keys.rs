//! API key management commands

use crate::db::sqlite::{ApiKeyInfo};
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub permissions: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    pub status: String,
    pub id: i64,
    pub name: String,
    pub api_key: String,  // Only returned once on creation
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ApiKeyListResponse {
    pub status: String,
    pub api_keys: Vec<ApiKeyInfo>,
}

#[derive(Debug, Serialize)]
pub struct DeleteApiKeyResponse {
    pub status: String,
    pub deleted: bool,
    pub message: String,
}

/// Create a new API key
#[tauri::command]
pub async fn create_api_key(
    state: State<'_, AppState>,
    request: CreateApiKeyRequest,
) -> Result<CreateApiKeyResponse> {
    tracing::info!("Creating API key: {}", request.name);

    let permissions = request.permissions.unwrap_or_else(|| "read,write".to_string());

    let (id, api_key) = state.sqlite.create_api_key(
        &request.name,
        &permissions,
        &state.security,
    )?;

    Ok(CreateApiKeyResponse {
        status: "success".to_string(),
        id,
        name: request.name,
        api_key,  // Only shown once
        message: "API key created successfully. Save this key - it won't be shown again!".to_string(),
    })
}

/// List all API keys (masked)
#[tauri::command]
pub async fn list_api_keys(
    state: State<'_, AppState>,
) -> Result<ApiKeyListResponse> {
    tracing::info!("Listing API keys");

    let api_keys = state.sqlite.list_api_keys(&state.security)?;

    Ok(ApiKeyListResponse {
        status: "success".to_string(),
        api_keys,
    })
}

/// Delete an API key by name
#[tauri::command]
pub async fn delete_api_key(
    state: State<'_, AppState>,
    name: String,
) -> Result<DeleteApiKeyResponse> {
    tracing::info!("Deleting API key: {}", name);

    let deleted = state.sqlite.delete_api_key(&name)?;

    Ok(DeleteApiKeyResponse {
        status: "success".to_string(),
        deleted,
        message: if deleted {
            format!("API key '{}' deleted successfully", name)
        } else {
            format!("API key '{}' not found", name)
        },
    })
}

/// Delete an API key by ID
#[tauri::command]
pub async fn delete_api_key_by_id(
    state: State<'_, AppState>,
    id: i64,
) -> Result<DeleteApiKeyResponse> {
    tracing::info!("Deleting API key by id: {}", id);

    let deleted = state.sqlite.delete_api_key_by_id(id)?;

    Ok(DeleteApiKeyResponse {
        status: "success".to_string(),
        deleted,
        message: if deleted {
            format!("API key with id {} deleted successfully", id)
        } else {
            format!("API key with id {} not found", id)
        },
    })
}
