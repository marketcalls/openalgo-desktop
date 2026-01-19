//! SQLite database models

use serde::{Deserialize, Serialize};

/// User model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub created_at: String,
}

/// Strategy model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub id: i64,
    pub name: String,
    pub webhook_id: String,
    pub exchange: String,
    pub symbol: String,
    pub product: String,
    pub quantity: i32,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Settings model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub id: i64,
    pub theme: String,
    pub default_broker: Option<String>,
    pub default_exchange: String,
    pub default_product: String,
    pub order_confirm: bool,
    pub sound_enabled: bool,
    // Auto-logout configuration
    pub auto_logout_enabled: bool,
    pub auto_logout_hour: u32,
    pub auto_logout_minute: u32,
    pub auto_logout_warnings: Vec<u32>,
    // Analyze mode (sandbox/paper trading)
    pub analyze_mode: Option<bool>,
}

/// Auto-logout configuration (subset of Settings)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoLogoutConfig {
    pub enabled: bool,
    pub hour: u32,
    pub minute: u32,
    pub warnings: Vec<u32>,
}

/// Webhook server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub enabled: bool,
    pub port: u16,
    pub host: String,
    pub ngrok_url: Option<String>,
    pub webhook_secret: Option<String>,
}

/// Sandbox order model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxOrder {
    pub id: i64,
    pub order_id: String,
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub quantity: i32,
    pub price: f64,
    pub order_type: String,
    pub product: String,
    pub status: String,
    pub filled_quantity: Option<i32>,
    pub average_price: Option<f64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Sandbox position model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPosition {
    pub id: i64,
    pub symbol: String,
    pub exchange: String,
    pub product: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub created_at: String,
    pub updated_at: String,
}

/// Sandbox holding model (CNC holdings)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxHolding {
    pub id: i64,
    pub symbol: String,
    pub exchange: String,
    pub quantity: i32,
    pub average_price: f64,
    pub ltp: f64,
    pub pnl: f64,
    pub created_at: String,
    pub updated_at: String,
}

/// Sandbox funds model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxFunds {
    pub available_cash: f64,
    pub used_margin: f64,
    pub total_value: f64,
    pub updated_at: String,
}

/// API key model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: i64,
    pub name: String,
    pub key_hash: String,
    pub encrypted_key: String,
    pub nonce: String,
    pub permissions: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

/// API key response (masked for security)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub id: i64,
    pub name: String,
    pub key_masked: String,
    pub permissions: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}
