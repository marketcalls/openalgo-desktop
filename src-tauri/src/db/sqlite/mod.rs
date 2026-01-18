//! SQLite database module

pub mod models;
mod connection;
mod migrations;
mod auth;
mod user;
mod api_keys;
mod symbol;
mod strategy;
mod settings;
mod sandbox;
mod order_logs;
mod market;

use crate::error::Result;
use crate::security::SecurityManager;
use crate::state::SymbolInfo;
pub use models::{AutoLogoutConfig, WebhookConfig, ApiKey, ApiKeyInfo, SandboxFunds, SandboxHolding};
pub use order_logs::{OrderLog, LogStats};
pub use market::{MarketHoliday, MarketTiming, CreateHolidayRequest, UpdateTimingRequest};
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

    /// Create a new API key
    ///
    /// Returns (id, plaintext_key) - the plaintext key is only shown once
    pub fn create_api_key(
        &self,
        name: &str,
        permissions: &str,
        security: &SecurityManager,
    ) -> Result<(i64, String)> {
        let conn = self.conn.lock();
        api_keys::create_api_key(&conn, name, permissions, security)
    }

    /// Validate API key and return the ApiKey if valid
    pub fn validate_api_key(
        &self,
        apikey: &str,
        security: &SecurityManager,
    ) -> Result<ApiKey> {
        let conn = self.conn.lock();
        api_keys::validate_api_key(&conn, apikey, security)
    }

    /// List all API keys (with masked key values)
    pub fn list_api_keys(&self, security: &SecurityManager) -> Result<Vec<ApiKeyInfo>> {
        let conn = self.conn.lock();
        api_keys::list_api_keys(&conn, security)
    }

    /// Get API key by name
    pub fn get_api_key_by_name(&self, name: &str) -> Result<Option<ApiKey>> {
        let conn = self.conn.lock();
        api_keys::get_api_key_by_name(&conn, name)
    }

    /// Delete API key by name
    pub fn delete_api_key(&self, name: &str) -> Result<bool> {
        let conn = self.conn.lock();
        api_keys::delete_api_key(&conn, name)
    }

    /// Delete API key by ID
    pub fn delete_api_key_by_id(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock();
        api_keys::delete_api_key_by_id(&conn, id)
    }

    /// Count total API keys
    pub fn count_api_keys(&self) -> Result<i64> {
        let conn = self.conn.lock();
        api_keys::count_api_keys(&conn)
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

    /// Get sandbox holdings
    pub fn get_sandbox_holdings(&self) -> Result<Vec<SandboxHolding>> {
        let conn = self.conn.lock();
        sandbox::get_holdings(&conn)
    }

    /// Get sandbox funds
    pub fn get_sandbox_funds(&self) -> Result<SandboxFunds> {
        let conn = self.conn.lock();
        sandbox::get_funds(&conn)
    }

    /// Update sandbox LTP and recalculate P&L
    pub fn update_sandbox_ltp(&self, exchange: &str, symbol: &str, ltp: f64) -> Result<()> {
        let conn = self.conn.lock();
        sandbox::update_position_ltp(&conn, exchange, symbol, ltp)
    }

    /// Cancel sandbox order
    pub fn cancel_sandbox_order(&self, order_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        sandbox::cancel_order(&conn, order_id)
    }

    // ========== Order Logs Methods ==========

    /// Create an order log entry
    #[allow(clippy::too_many_arguments)]
    pub fn create_order_log(
        &self,
        order_id: Option<&str>,
        broker: &str,
        symbol: &str,
        exchange: &str,
        side: &str,
        quantity: i32,
        price: Option<f64>,
        order_type: &str,
        product: &str,
        status: &str,
        message: Option<&str>,
        source: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock();
        order_logs::create_log(
            &conn, order_id, broker, symbol, exchange, side, quantity, price, order_type, product, status, message, source,
        )
    }

    /// Get order logs with pagination and filters
    pub fn get_order_logs(
        &self,
        limit: usize,
        offset: usize,
        broker: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<OrderLog>> {
        let conn = self.conn.lock();
        order_logs::get_logs(&conn, limit, offset, broker, status)
    }

    /// Get logs for a specific order
    pub fn get_order_logs_by_order_id(&self, order_id: &str) -> Result<Vec<OrderLog>> {
        let conn = self.conn.lock();
        order_logs::get_logs_by_order_id(&conn, order_id)
    }

    /// Get recent order logs
    pub fn get_recent_order_logs(&self, limit: usize) -> Result<Vec<OrderLog>> {
        let conn = self.conn.lock();
        order_logs::get_recent_logs(&conn, limit)
    }

    /// Count order logs
    pub fn count_order_logs(&self, broker: Option<&str>, status: Option<&str>) -> Result<i64> {
        let conn = self.conn.lock();
        order_logs::count_logs(&conn, broker, status)
    }

    /// Clear old order logs
    pub fn clear_old_order_logs(&self, days: i32) -> Result<usize> {
        let conn = self.conn.lock();
        order_logs::clear_old_logs(&conn, days)
    }

    /// Get order log statistics
    pub fn get_order_log_stats(&self) -> Result<LogStats> {
        let conn = self.conn.lock();
        order_logs::get_stats(&conn)
    }

    // ========== Market Holiday Methods ==========

    /// Create a market holiday
    pub fn create_market_holiday(&self, req: &CreateHolidayRequest) -> Result<MarketHoliday> {
        let conn = self.conn.lock();
        market::create_holiday(&conn, req)
    }

    /// Get holidays by year
    pub fn get_market_holidays_by_year(&self, year: i32) -> Result<Vec<MarketHoliday>> {
        let conn = self.conn.lock();
        market::get_holidays_by_year(&conn, year)
    }

    /// Get holidays by exchange
    pub fn get_market_holidays_by_exchange(&self, exchange: &str, year: Option<i32>) -> Result<Vec<MarketHoliday>> {
        let conn = self.conn.lock();
        market::get_holidays_by_exchange(&conn, exchange, year)
    }

    /// Check if a date is a holiday
    pub fn is_market_holiday(&self, exchange: &str, date: &str) -> Result<bool> {
        let conn = self.conn.lock();
        market::is_holiday(&conn, exchange, date)
    }

    /// Delete a market holiday
    pub fn delete_market_holiday(&self, id: i64) -> Result<bool> {
        let conn = self.conn.lock();
        market::delete_holiday(&conn, id)
    }

    // ========== Market Timing Methods ==========

    /// Get all market timings
    pub fn get_all_market_timings(&self) -> Result<Vec<MarketTiming>> {
        let conn = self.conn.lock();
        market::get_all_timings(&conn)
    }

    /// Get timing for an exchange
    pub fn get_market_timing(&self, exchange: &str) -> Result<Option<MarketTiming>> {
        let conn = self.conn.lock();
        market::get_timing_by_exchange(&conn, exchange)
    }

    /// Update market timing
    pub fn update_market_timing(&self, exchange: &str, req: &UpdateTimingRequest) -> Result<MarketTiming> {
        let conn = self.conn.lock();
        market::update_timing(&conn, exchange, req)
    }

    /// Create market timing
    pub fn create_market_timing(&self, timing: &MarketTiming) -> Result<MarketTiming> {
        let conn = self.conn.lock();
        market::create_timing(&conn, timing)
    }

    /// Check if market is open
    pub fn is_market_open(&self, exchange: &str) -> Result<bool> {
        let conn = self.conn.lock();
        market::is_market_open(&conn, exchange)
    }
}
