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

    /// The first sign-in of an install from an earlier build could not read
    /// its keys from the OS keychain to move them into openalgo.db (locked,
    /// or access refused). Nothing changed; the trader retries or starts
    /// fresh (KEY-01).
    #[error("The keychain holding the account's keys could not be read")]
    KeychainUnavailable,

    /// The keys for the account are not where an earlier build kept them
    /// (no keychain entries, or entries that open none of the account's
    /// data: a folder from another computer, or from before an account
    /// reset). The trader restores the folder or starts fresh.
    #[error("The keys for the account are missing or belong to other data")]
    KeysDoNotMatch,

    /// The key vault in openalgo.db (or an earlier build's vault.json)
    /// cannot be read. The trader starts fresh.
    #[error("The key vault cannot be read")]
    VaultUnreadable,

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Broker error: {0}")]
    Broker(String),

    /// The broker answered, readably, that it did not place the order (or
    /// the adapter refused it before sending). Inside an order placement
    /// this is the only broker error that reads as "not placed"; any other
    /// broker error leaves the outcome uncertain (LOG-08). Built with
    /// `brokers::common::outcome::refusal`.
    #[error("Order refused: {0}")]
    Refused(String),

    /// Refused before sending because the request's turn in the broker's
    /// pacing queue was too far away (web `BrokerBusyError`): nothing
    /// reached the broker. `/api/v1` answers it with 429 and this sentence,
    /// as the web does. Built with `brokers::common::outcome::busy`.
    #[error("Broker busy: {0}")]
    Busy(String),

    /// An order request reached the broker, or may have, and no definite
    /// answer came back (a timeout or a dropped connection after sending, a
    /// server error, an unreadable reply, a success without an order id).
    /// The order may exist: it is looked up, never placed again blindly.
    #[error("Order outcome unknown: {}", .0.message)]
    Uncertain(Box<UncertainOrder>),

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

/// What is known about an order whose placement had no definite answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncertainOrder {
    /// Trader-facing text.
    pub message: String,
    /// The client tag the order was sent with, when the adapter tags its
    /// orders, so it can be found in the broker's order book.
    pub client_tag: Option<String>,
    /// It came from a transport or parsing failure, which the order
    /// service answers on the wire as before (500, its internal-error
    /// text); otherwise the wire carries `message` with 400, as a broker
    /// error always has. The uncertainty itself never reaches the wire.
    pub internal: bool,
    /// The broker said the session had ended, but only after a server
    /// error in the same placement (review P2-4): the order service
    /// answers it as the session error it reads as (403, `message`).
    pub session: bool,
}

impl AppError {
    /// An order whose outcome is unknown, with the trader-facing text.
    pub fn uncertain(message: impl Into<String>, client_tag: Option<String>) -> Self {
        AppError::Uncertain(Box::new(UncertainOrder {
            message: message.into(),
            client_tag,
            internal: false,
            session: false,
        }))
    }
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
            AppError::KeychainUnavailable => "KEYCHAIN_UNAVAILABLE",
            AppError::KeysDoNotMatch => "KEYS_DO_NOT_MATCH",
            AppError::VaultUnreadable => "VAULT_UNREADABLE",
            AppError::Auth(_) => "AUTH_ERROR",
            AppError::Broker(_) | AppError::Refused(_) => "BROKER_ERROR",
            AppError::Busy(_) => "BROKER_BUSY",
            AppError::Uncertain(_) => "ORDER_UNCERTAIN",
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
            | AppError::Refused(m)
            | AppError::Busy(m)
            | AppError::Validation(m)
            | AppError::NotFound(m) => m.clone(),
            AppError::Uncertain(u) => u.message.clone(),
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
            AppError::KeychainUnavailable => KEYCHAIN_UNAVAILABLE_MESSAGE.to_string(),
            AppError::KeysDoNotMatch => KEYS_DO_NOT_MATCH_MESSAGE.to_string(),
            AppError::VaultUnreadable => VAULT_UNREADABLE_MESSAGE.to_string(),
            _ => "An unexpected error occurred".to_string(),
        }
    }
}

/// What a sign-in answers when the keys of an earlier build could not be
/// read from the system keychain to move them into openalgo.db (KEY-01).
pub const KEYCHAIN_UNAVAILABLE_MESSAGE: &str = "OpenAlgo needs to read your keys from the system \
keychain one last time, to move them into its own database, and the keychain is locked or refused \
access. Choose Allow the keychain once and retry, and allow access when your computer asks: after \
this OpenAlgo never uses the keychain again. Or choose Start fresh: your account, settings and \
history stay, and you enter your broker keys again, get a new API key and set up two-factor login \
again.";

/// What a sign-in answers when the account's keys are not in this
/// computer's keychain, or open none of its data.
pub const KEYS_DO_NOT_MATCH_MESSAGE: &str = "The keys for this account are not in this computer's \
keychain: the data folder may come from another computer, or from before an account reset. Restore \
the folder on the computer it came from, or choose Start fresh: your account, settings and history \
stay, and you enter your broker keys again, get a new API key and set up two-factor login again.";

/// What a sign-in answers when the saved keys cannot be read.
pub const VAULT_UNREADABLE_MESSAGE: &str = "OpenAlgo could not read the keys saved with your \
account. Choose Start fresh: your account, settings and history stay, and you enter your broker \
keys again, get a new API key and set up two-factor login again.";

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

    /// KEY-01 security review: OS and key errors carry file paths, the
    /// keychain service and item names; HTTP and window answers carry the
    /// trader's sentence only (the detail goes to the log).
    #[test]
    fn key_and_os_errors_never_reach_an_answer() {
        let detail = "/Users/t/Library/Keychains/login.keychain-db com.openalgo.desktop data-key";
        for e in [
            AppError::Keychain(detail.into()),
            AppError::Encryption(detail.into()),
            AppError::Io(std::io::Error::other(detail)),
            AppError::Config(detail.into()),
            AppError::KeychainUnavailable,
            AppError::KeysDoNotMatch,
            AppError::VaultUnreadable,
        ] {
            let answer = serde_json::to_string(&e).unwrap();
            for word in ["/Users/", "keychain-db", "com.openalgo.desktop", "data-key"] {
                assert!(!answer.contains(word), "{}: {}", word, answer);
            }
        }
    }
}
