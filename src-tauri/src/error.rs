//! Application error types.
//!
//! Large foreign errors are boxed so `Result<T, AppError>` stays small on the
//! hot path. `client_message` is the only text that ever reaches a trader or an
//! HTTP client: it names the cause in plain words and never carries SQL,
//! URLs, file paths or library error text. The technical detail is logged once
//! at the boundary that handles the error.

use serde::Serialize;
use thiserror::Error;

/// Application-wide error type
#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Database pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("DuckDB error: {0}")]
    DuckDb(Box<duckdb::Error>),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    // Displayed without any URL credentials (tokens on a query string).
    #[error("HTTP request error: {}", crate::brokers::common::redact::url_safe_error(&**.0))]
    Http(Box<reqwest::Error>),

    #[error("WebSocket error: {}", crate::brokers::common::redact::url_safe_error(&**.0))]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    #[error("Keychain error: {0}")]
    Keychain(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    /// The data key is not in memory yet (password-derived key mode before
    /// the trader has signed in).
    #[error("Secure storage is locked")]
    Locked,

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Broker error: {0}")]
    Broker(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Not found: {0}")]
    NotFound(String),

    /// The connected broker does not offer this capability (GTT, margin,
    /// streaming, ...). The web answers these with 501.
    #[error("Unsupported by broker: {0}")]
    Unsupported(&'static str),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<duckdb::Error> for AppError {
    fn from(e: duckdb::Error) -> Self {
        AppError::DuckDb(Box::new(e))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        // The URL can carry credentials (tokens on the query); errors
        // are logged, so it is dropped here once for every caller.
        AppError::Http(Box::new(e.without_url()))
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for AppError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        AppError::WebSocket(Box::new(e))
    }
}

impl From<keyring::Error> for AppError {
    fn from(e: keyring::Error) -> Self {
        AppError::Keychain(e.to_string())
    }
}

impl AppError {
    /// Stable machine-readable code for the frontend.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Database(_) | AppError::Pool(_) => "DATABASE_ERROR",
            AppError::DuckDb(_) => "DUCKDB_ERROR",
            AppError::Serialization(_) => "SERIALIZATION_ERROR",
            AppError::Http(_) => "HTTP_ERROR",
            AppError::WebSocket(_) => "WEBSOCKET_ERROR",
            AppError::Keychain(_) => "KEYCHAIN_ERROR",
            AppError::Encryption(_) => "ENCRYPTION_ERROR",
            AppError::Locked => "LOCKED",
            AppError::Auth(_) => "AUTH_ERROR",
            AppError::Broker(_) => "BROKER_ERROR",
            AppError::Validation(_) => "VALIDATION_ERROR",
            AppError::NotFound(_) => "NOT_FOUND",
            AppError::Unsupported(_) => "UNSUPPORTED",
            AppError::Config(_) => "CONFIG_ERROR",
            AppError::Io(_) => "IO_ERROR",
            AppError::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// Message safe to show a trader or return to an HTTP client.
    ///
    /// Business errors (broker rejections, validation, not found) carry their
    /// own text, which adapters write for traders. Everything technical is
    /// replaced by a plain sentence.
    pub fn client_message(&self) -> String {
        match self {
            AppError::Auth(m)
            | AppError::Broker(m)
            | AppError::Validation(m)
            | AppError::NotFound(m) => m.clone(),
            AppError::Unsupported(what) => {
                format!(
                    "{} is not available for your broker.",
                    unsupported_label(what)
                )
            }
            AppError::Http(_) | AppError::WebSocket(_) => {
                "Could not reach the broker. Check your internet connection and try again."
                    .to_string()
            }
            AppError::Locked => {
                "Sign in to OpenAlgo to unlock your saved broker credentials.".to_string()
            }
            AppError::Keychain(_) => {
                "OpenAlgo could not use the system keychain. Restart the app and try again."
                    .to_string()
            }
            _ => "An unexpected error occurred".to_string(),
        }
    }
}

/// Trader-facing name of an optional broker capability.
fn unsupported_label(what: &str) -> String {
    match what {
        "gtt" => "GTT orders".to_string(),
        "margin" => "Margin calculation".to_string(),
        "history" => "Historical data".to_string(),
        "streaming" => "Live market data".to_string(),
        "depth" => "Market depth".to_string(),
        "close_all" => "Closing all positions".to_string(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => "This feature".to_string(),
            }
        }
    }
}

/// Serializable error response for the frontend
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub code: String,
    pub message: String,
}

impl From<&AppError> for ErrorResponse {
    fn from(err: &AppError) -> Self {
        ErrorResponse {
            code: err.code().to_string(),
            message: err.client_message(),
        }
    }
}

// Allow AppError to be returned from Tauri commands without leaking detail.
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        ErrorResponse::from(self).serialize(serializer)
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_stays_small() {
        assert!(std::mem::size_of::<AppError>() <= 64);
    }

    #[test]
    fn technical_errors_do_not_leak() {
        let e = AppError::Internal("SELECT * FROM users".into());
        assert_eq!(e.client_message(), "An unexpected error occurred");
        let e = AppError::Broker("Insufficient funds".into());
        assert_eq!(e.client_message(), "Insufficient funds");
        let e = AppError::Unsupported("gtt");
        assert_eq!(
            e.client_message(),
            "GTT orders is not available for your broker."
        );
        assert_eq!(e.code(), "UNSUPPORTED");
    }
}
