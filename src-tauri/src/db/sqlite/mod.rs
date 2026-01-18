//! SQLite database module

pub mod models;
mod connection;
mod migrations;
mod auth;
mod user;
mod symbol;
mod strategy;
mod settings;
mod sandbox;

use crate::error::Result;
use crate::security::SecurityManager;
use crate::state::SymbolInfo;
pub use models::{AutoLogoutConfig, WebhookConfig};
use models::*;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;

/// SQLite database wrapper
pub struct SqliteDb {
    conn: Mutex<Connection>,
}

impl SqliteDb {
    /// Create new SQLite database connection
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrent access
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let db = Self {
            conn: Mutex::new(conn),
        };

        // Run migrations
        db.run_migrations()?;

        Ok(db)
    }

    /// Run database migrations
    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn.lock();
        migrations::run_migrations(&conn)
    }

    // ========== User Methods ==========

    /// Verify user credentials
    pub fn verify_user(
        &self,
        username: &str,
        password: &str,
        security: &SecurityManager,
    ) -> Result<Option<User>> {
        let conn = self.conn.lock();
        user::verify_user(&conn, username, password, security)
    }

    /// Create a new user
    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        security: &SecurityManager,
    ) -> Result<User> {
        let conn = self.conn.lock();
        user::create_user(&conn, username, password, security)
    }

    /// Check if any user exists
    pub fn has_user(&self) -> Result<bool> {
        let conn = self.conn.lock();
        user::has_user(&conn)
    }

    // ========== Auth Token Methods ==========

    /// Store encrypted auth token
    pub fn store_auth_token(
        &self,
        broker_id: &str,
        auth_token: &str,
        feed_token: Option<&str>,
        security: &SecurityManager,
    ) -> Result<()> {
        let conn = self.conn.lock();
        auth::store_auth_token(&conn, broker_id, auth_token, feed_token, security)
    }

    /// Get decrypted auth token
    pub fn get_auth_token(
        &self,
        broker_id: &str,
        security: &SecurityManager,
    ) -> Result<Option<(String, Option<String>)>> {
        let conn = self.conn.lock();
        auth::get_auth_token(&conn, broker_id, security)
    }

    /// Delete auth token
    pub fn delete_auth_token(&self, broker_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        auth::delete_auth_token(&conn, broker_id)
    }

    /// Clear all auth tokens (used by auto-logout)
    pub fn clear_all_auth_tokens(&self) -> Result<()> {
        let conn = self.conn.lock();
        auth::clear_all_auth_tokens(&conn)
    }

    // ========== Symbol Methods ==========

    /// Store symbols in database
    pub fn store_symbols(&self, symbols: &[SymbolInfo]) -> Result<()> {
        let mut conn = self.conn.lock();
        symbol::store_symbols(&mut conn, symbols)
    }

    /// Load all symbols from database
    pub fn load_symbols(&self) -> Result<Vec<SymbolInfo>> {
        let conn = self.conn.lock();
        symbol::load_symbols(&conn)
    }

    // ========== Strategy Methods ==========

    /// Get all strategies
    pub fn get_strategies(&self) -> Result<Vec<Strategy>> {
        let conn = self.conn.lock();
        strategy::get_strategies(&conn)
    }

    /// Create a new strategy
    pub fn create_strategy(&self, strategy: &Strategy) -> Result<Strategy> {
        let conn = self.conn.lock();
        strategy::create_strategy(&conn, strategy)
    }

    /// Update a strategy
    pub fn update_strategy(
        &self,
        id: i64,
        name: Option<String>,
        exchange: Option<String>,
        symbol: Option<String>,
        product: Option<String>,
        quantity: Option<i32>,
        enabled: Option<bool>,
    ) -> Result<Strategy> {
        let conn = self.conn.lock();
        strategy::update_strategy(&conn, id, name, exchange, symbol, product, quantity, enabled)
    }

    /// Delete a strategy
    pub fn delete_strategy(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock();
        strategy::delete_strategy(&conn, id)
    }

    /// Get strategy by webhook_id (for webhook handler)
    pub fn get_strategy_by_webhook_id(
        &self,
        webhook_id: &str,
    ) -> Result<Option<crate::webhook::handlers::Strategy>> {
        let conn = self.conn.lock();
        strategy::get_strategy_by_webhook_id(&conn, webhook_id)
    }

    /// Get symbol mapping for a strategy (for webhook handler)
    pub fn get_symbol_mapping(
        &self,
        strategy_id: &i64,
        symbol: &str,
    ) -> Result<Option<crate::webhook::handlers::SymbolMapping>> {
        let conn = self.conn.lock();
        strategy::get_symbol_mapping(&conn, *strategy_id, symbol)
    }

    // ========== API Key Methods ==========

    /// Validate API key and return user_id if valid
    pub fn validate_api_key(&self, apikey: &str) -> Result<String> {
        let _conn = self.conn.lock();
        // For now, use a simple validation
        // TODO: Implement proper API key validation with hashing
        // In production, this should query the api_keys table
        if apikey.len() >= 32 {
            // Return a placeholder user_id - in production this should lookup the key
            Ok("user_1".to_string())
        } else {
            Err(crate::error::AppError::Auth("Invalid API key".to_string()))
        }
    }

    // ========== Settings Methods ==========

    /// Get settings
    pub fn get_settings(&self) -> Result<Settings> {
        let conn = self.conn.lock();
        settings::get_settings(&conn)
    }

    /// Update settings
    pub fn update_settings(
        &self,
        theme: Option<String>,
        default_broker: Option<String>,
        default_exchange: Option<String>,
        default_product: Option<String>,
        order_confirm: Option<bool>,
        sound_enabled: Option<bool>,
    ) -> Result<Settings> {
        let conn = self.conn.lock();
        settings::update_settings(
            &conn,
            theme,
            default_broker,
            default_exchange,
            default_product,
            order_confirm,
            sound_enabled,
        )
    }

    /// Get auto-logout configuration
    pub fn get_auto_logout_config(&self) -> Result<AutoLogoutConfig> {
        let conn = self.conn.lock();
        settings::get_auto_logout_config(&conn)
    }

    /// Update auto-logout configuration
    pub fn update_auto_logout_config(
        &self,
        enabled: Option<bool>,
        hour: Option<u32>,
        minute: Option<u32>,
        warnings: Option<Vec<u32>>,
    ) -> Result<AutoLogoutConfig> {
        let conn = self.conn.lock();
        settings::update_auto_logout_config(&conn, enabled, hour, minute, warnings)
    }

    /// Get webhook configuration
    pub fn get_webhook_config(&self) -> Result<WebhookConfig> {
        let conn = self.conn.lock();
        settings::get_webhook_config(&conn)
    }

    /// Update webhook configuration
    pub fn update_webhook_config(
        &self,
        enabled: Option<bool>,
        port: Option<u16>,
        host: Option<String>,
        ngrok_url: Option<String>,
        webhook_secret: Option<String>,
    ) -> Result<WebhookConfig> {
        let conn = self.conn.lock();
        settings::update_webhook_config(&conn, enabled, port, host, ngrok_url, webhook_secret)
    }

    // ========== Sandbox Methods ==========

    /// Get sandbox positions
    pub fn get_sandbox_positions(&self) -> Result<Vec<SandboxPosition>> {
        let conn = self.conn.lock();
        sandbox::get_positions(&conn)
    }

    /// Get sandbox orders
    pub fn get_sandbox_orders(&self) -> Result<Vec<SandboxOrder>> {
        let conn = self.conn.lock();
        sandbox::get_orders(&conn)
    }

    /// Place sandbox order
    pub fn place_sandbox_order(
        &self,
        symbol: &str,
        exchange: &str,
        side: &str,
        quantity: i32,
        price: f64,
        order_type: &str,
        product: &str,
    ) -> Result<SandboxOrder> {
        let conn = self.conn.lock();
        sandbox::place_order(&conn, symbol, exchange, side, quantity, price, order_type, product)
    }

    /// Reset sandbox
    pub fn reset_sandbox(&self) -> Result<()> {
        let conn = self.conn.lock();
        sandbox::reset(&conn)
    }
}
