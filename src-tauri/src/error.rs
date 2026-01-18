//! Application error types

use serde::Serialize;
use thiserror::Error;

/// Application-wide error type
#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("DuckDB error: {0}")]
    DuckDb(#[from] duckdb::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("Keychain error: {0}")]
    Keychain(#[from] keyring::Error),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Broker error: {0}")]
    Broker(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

/// Serializable error response for frontend
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

impl From<AppError> for ErrorResponse {
    fn from(err: AppError) -> Self {
        let (code, message) = match &err {
            AppError::Database(_) => ("DATABASE_ERROR", err.to_string()),
            AppError::DuckDb(_) => ("DUCKDB_ERROR", err.to_string()),
            AppError::Serialization(_) => ("SERIALIZATION_ERROR", err.to_string()),
            AppError::Http(_) => ("HTTP_ERROR", err.to_string()),
            AppError::WebSocket(_) => ("WEBSOCKET_ERROR", err.to_string()),
            AppError::Keychain(_) => ("KEYCHAIN_ERROR", err.to_string()),
            AppError::Encryption(_) => ("ENCRYPTION_ERROR", err.to_string()),
            AppError::Auth(_) => ("AUTH_ERROR", err.to_string()),
            AppError::Broker(_) => ("BROKER_ERROR", err.to_string()),
            AppError::Validation(_) => ("VALIDATION_ERROR", err.to_string()),
            AppError::NotFound(_) => ("NOT_FOUND", err.to_string()),
            AppError::Config(_) => ("CONFIG_ERROR", err.to_string()),
            AppError::Io(_) => ("IO_ERROR", err.to_string()),
            AppError::Internal(_) => ("INTERNAL_ERROR", err.to_string()),
        };

        ErrorResponse {
            code: code.to_string(),
            message,
        }
    }
}

// Allow AppError to be returned from Tauri commands
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let response = ErrorResponse::from(AppError::Internal(self.to_string()));
        response.serialize(serializer)
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
