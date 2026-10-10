//! SQLite database module (main database, `openalgo.db`).
//!
//! All access goes through an r2d2 pool. A connection is checked out for the
//! duration of one synchronous call and returned when it drops; no method
//! holds a connection across an `.await`.

pub mod action_center;
pub mod alert_log;
mod analyzer_logs;
pub mod api_keys;
pub mod auth;
pub mod credentials;
pub mod data_migrations;
mod latency_logs;
pub mod logs;
mod market;
pub mod market_calendar;
pub mod master_contract_status;
pub(crate) mod migrations;
pub mod models;
pub mod monitor;
pub mod oauth_state;
mod order_logs;
pub mod sandbox;
mod settings;
mod strategy;
pub mod strategy_portfolio;
pub mod symbol;
mod traffic_logs;
pub mod user;
pub mod watchlist;
pub mod webui;

use crate::error::Result;
use crate::state::SymbolInfo;
pub use analyzer_logs::{AnalyzerLog, AnalyzerLogStats};
pub use latency_logs::{BrokerLatencyStats, LatencyLog, LatencyStats};
pub use market::{CreateHolidayRequest, MarketHoliday, MarketTiming, UpdateTimingRequest};
use models::*;
pub use models::{AutoLogoutConfig, SandboxFunds, SandboxHolding, WebhookConfig};
pub use order_logs::{LogStats, OrderLog};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use std::path::Path;
use std::time::Duration;
pub use traffic_logs::{IPBan, TrafficLog, TrafficStats};

pub type DbConn = PooledConnection<SqliteConnectionManager>;

/// How far a store's commits are pushed to disk before they return
/// (`PRAGMA synchronous`). Every SQLite store runs in WAL mode, where an app
/// crash never loses a committed transaction at either level; the levels
/// differ only on power loss or an operating-system crash.
///
/// The policy is chosen per store, explicitly (ARCH-02):
///
/// | Store | Level | Why |
/// | --- | --- | --- |
/// | `openalgo.db` ([`Durability::MAIN`]) | `FULL` | Holds the order and obligation journal: strategy runs, orders and the position book, OpenScript and scalping rows, credentials. A committed order intent or fill must survive a power cut, or recovery acts on a past that did not happen. |
/// | `logs.db` ([`Durability::LOGS`]) | `NORMAL` | Request logs, latency and traffic samples, health history: losing the last commits before a power cut loses only diagnostics. |
/// | `sandbox.db` ([`Durability::SANDBOX`]) | `NORMAL` | Simulated orders and funds; the sandbox engine replays its catch-up on start. |
///
/// `FULL` costs one log sync per commit, small at one trader's write rate;
/// master contract replacement is one transaction, so it pays one sync. The
/// web runs every database at `NORMAL`: `openalgo.db` at `FULL` is a
/// deliberate deviation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// `synchronous=FULL`: the log is synced on every commit.
    Full,
    /// `synchronous=NORMAL`: the log is synced at checkpoints.
    Normal,
}

impl Durability {
    /// `openalgo.db`, the order and obligation journal.
    pub const MAIN: Durability = Durability::Full;
    /// `logs.db`.
    pub const LOGS: Durability = Durability::Normal;
    /// `sandbox.db`.
    pub const SANDBOX: Durability = Durability::Normal;

    /// The statement that applies this level to one connection.
    pub const fn pragma(self) -> &'static str {
        match self {
            Durability::Full => "PRAGMA synchronous=FULL;",
            Durability::Normal => "PRAGMA synchronous=NORMAL;",
        }
    }
}

/// Build a pool with the pragmas every connection needs, at the store's
/// [`Durability`] (applied to each connection the pool opens).
pub fn open_pool(
    path: &Path,
    max_size: u32,
    durability: Durability,
) -> Result<Pool<SqliteConnectionManager>> {
    let manager = SqliteConnectionManager::file(path).with_init(move |c| {
        c.execute_batch("PRAGMA journal_mode=WAL;")?;
        c.execute_batch(durability.pragma())?;
        c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
    });
    let pool = Pool::builder()
        .max_size(max_size)
        .min_idle(Some(1))
        .connection_timeout(Duration::from_secs(10))
        .idle_timeout(Some(Duration::from_secs(300)))
        .build(manager)?;
    crate::security::fsperm::restrict_db_files(path)?;
    Ok(pool)
}

/// SQLite database wrapper
pub struct SqliteDb {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteDb {
    /// Open (creating if needed) the main database and run schema migrations.
    pub fn new(path: &Path) -> Result<Self> {
        let db = Self {
            pool: open_pool(path, 8, Durability::MAIN)?,
        };
        db.run_migrations()?;
        crate::security::fsperm::restrict_db_files(path)?;
        Ok(db)
    }

    /// Check out a pooled connection. Drop it before any network await.
    pub fn conn(&self) -> Result<DbConn> {
        Ok(self.pool.get()?)
    }

    /// Open and idle pooled connections (health monitor).
    pub fn pool_state(&self) -> (u32, u32) {
        let st = self.pool.state();
        (st.connections, st.idle_connections)
    }

    /// Run database migrations
    fn run_migrations(&self) -> Result<()> {
        let conn = self.conn()?;
        migrations::run_migrations(&conn)
    }

    /// Whether a named migration (schema or data) has been recorded.
    pub fn migration_applied(&self, name: &str) -> Result<bool> {
        let conn = self.conn()?;
        migrations::is_applied(&conn, name)
    }
    // ========== Symbol Methods ==========

    /// Store symbols in database
    pub fn store_symbols(&self, symbols: &[SymbolInfo]) -> Result<()> {
        let mut conn = self.conn()?;
        symbol::store_symbols(&mut conn, symbols)
    }

    /// Load all symbols from database
    pub fn load_symbols(&self) -> Result<Vec<SymbolInfo>> {
        let conn = self.conn()?;
        symbol::load_symbols(&conn)
    }

    /// Store a master download with its contract multipliers (crypto).
    pub fn store_master(&self, master: &crate::brokers::types::MasterContract) -> Result<()> {
        let mut conn = self.conn()?;
        symbol::store_master(&mut conn, master)
    }

    /// Load the master with its contract multipliers.
    pub fn load_master(&self) -> Result<crate::brokers::types::MasterContract> {
        let conn = self.conn()?;
        symbol::load_master(&conn)
    }

    // ========== Strategy Methods ==========

    /// Get all strategies
    pub fn get_strategies(&self) -> Result<Vec<Strategy>> {
        let conn = self.conn()?;
        strategy::get_strategies(&conn)
    }

    /// Create a new strategy
    pub fn create_strategy(&self, strategy: &Strategy) -> Result<Strategy> {
        let conn = self.conn()?;
        strategy::create_strategy(&conn, strategy)
    }

    /// Update a strategy
    #[allow(clippy::too_many_arguments)]
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
        let conn = self.conn()?;
        strategy::update_strategy(
            &conn, id, name, exchange, symbol, product, quantity, enabled,
        )
    }

    /// Delete a strategy
    pub fn delete_strategy(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        strategy::delete_strategy(&conn, id)
    }

    /// Get strategy by webhook_id (for webhook handler)
    pub fn get_strategy_by_webhook_id(
        &self,
        webhook_id: &str,
    ) -> Result<Option<crate::webhook::handlers::Strategy>> {
        let conn = self.conn()?;
        strategy::get_strategy_by_webhook_id(&conn, webhook_id)
    }

    /// Get symbol mapping for a strategy (for webhook handler)
    pub fn get_symbol_mapping(
        &self,
        strategy_id: &i64,
        symbol: &str,
    ) -> Result<Option<crate::webhook::handlers::SymbolMapping>> {
        let conn = self.conn()?;
        strategy::get_symbol_mapping(&conn, *strategy_id, symbol)
    }

    // ========== Settings Methods ==========

    /// Get settings
    pub fn get_settings(&self) -> Result<Settings> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
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
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        settings::update_auto_logout_config(&conn, enabled, hour, minute, warnings)
    }

    /// Get webhook configuration
    pub fn get_webhook_config(&self) -> Result<WebhookConfig> {
        let conn = self.conn()?;
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
        let conn = self.conn()?;
        settings::update_webhook_config(&conn, enabled, port, host, ngrok_url, webhook_secret)
    }

    /// Get rate limit configuration
    pub fn get_rate_limit_config(&self) -> Result<models::RateLimitConfig> {
        let conn = self.conn()?;
        settings::get_rate_limit_config(&conn)
    }

    /// Update rate limit configuration
    pub fn update_rate_limit_config(
        &self,
        api_rate_limit: Option<u32>,
        order_rate_limit: Option<u32>,
        smart_order_rate_limit: Option<u32>,
        smart_order_delay: Option<f64>,
    ) -> Result<models::RateLimitConfig> {
        let conn = self.conn()?;
        settings::update_rate_limit_config(
            &conn,
            api_rate_limit,
            order_rate_limit,
            smart_order_rate_limit,
            smart_order_delay,
        )
    }

    // ========== Sandbox Methods ==========

    /// Get sandbox positions
    pub fn get_sandbox_positions(&self) -> Result<Vec<SandboxPosition>> {
        let conn = self.conn()?;
        sandbox::get_positions(&conn)
    }

    /// Get sandbox orders
    pub fn get_sandbox_orders(&self) -> Result<Vec<SandboxOrder>> {
        let conn = self.conn()?;
        sandbox::get_orders(&conn)
    }

    /// Place sandbox order
    #[allow(clippy::too_many_arguments)]
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
        let conn = self.conn()?;
        sandbox::place_order(
            &conn, symbol, exchange, side, quantity, price, order_type, product,
        )
    }

    /// Reset sandbox
    pub fn reset_sandbox(&self) -> Result<()> {
        let conn = self.conn()?;
        sandbox::reset(&conn)
    }

    /// Get sandbox holdings
    pub fn get_sandbox_holdings(&self) -> Result<Vec<SandboxHolding>> {
        let conn = self.conn()?;
        sandbox::get_holdings(&conn)
    }

    /// Get sandbox funds
    pub fn get_sandbox_funds(&self) -> Result<SandboxFunds> {
        let conn = self.conn()?;
        sandbox::get_funds(&conn)
    }

    /// Update sandbox LTP and recalculate P&L
    pub fn update_sandbox_ltp(&self, exchange: &str, symbol: &str, ltp: f64) -> Result<()> {
        let conn = self.conn()?;
        sandbox::update_position_ltp(&conn, exchange, symbol, ltp)
    }

    /// Cancel sandbox order
    pub fn cancel_sandbox_order(&self, order_id: &str) -> Result<bool> {
        let conn = self.conn()?;
        sandbox::cancel_order(&conn, order_id)
    }

    /// Get sandbox configuration
    pub fn get_sandbox_config(&self) -> Result<sandbox::SandboxConfig> {
        let conn = self.conn()?;
        sandbox::get_config(&conn)
    }

    /// Update sandbox configuration
    pub fn update_sandbox_config(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn()?;
        sandbox::update_config(&conn, key, value)
    }

    /// Get sandbox trades
    pub fn get_sandbox_trades(&self) -> Result<Vec<sandbox::SandboxTrade>> {
        let conn = self.conn()?;
        sandbox::get_trades(&conn)
    }

    /// Get sandbox daily P&L history
    pub fn get_sandbox_daily_pnl(&self) -> Result<Vec<sandbox::SandboxDailyPnl>> {
        let conn = self.conn()?;
        sandbox::get_daily_pnl(&conn)
    }

    /// Get consolidated sandbox P&L data
    pub fn get_sandbox_pnl(&self) -> Result<sandbox::SandboxPnlData> {
        let conn = self.conn()?;
        sandbox::get_pnl_data(&conn)
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
        let conn = self.conn()?;
        order_logs::create_log(
            &conn, order_id, broker, symbol, exchange, side, quantity, price, order_type, product,
            status, message, source,
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
        let conn = self.conn()?;
        order_logs::get_logs(&conn, limit, offset, broker, status)
    }

    /// Get logs for a specific order
    pub fn get_order_logs_by_order_id(&self, order_id: &str) -> Result<Vec<OrderLog>> {
        let conn = self.conn()?;
        order_logs::get_logs_by_order_id(&conn, order_id)
    }

    /// Get recent order logs
    pub fn get_recent_order_logs(&self, limit: usize) -> Result<Vec<OrderLog>> {
        let conn = self.conn()?;
        order_logs::get_recent_logs(&conn, limit)
    }

    /// Count order logs
    pub fn count_order_logs(&self, broker: Option<&str>, status: Option<&str>) -> Result<i64> {
        let conn = self.conn()?;
        order_logs::count_logs(&conn, broker, status)
    }

    /// Clear old order logs
    pub fn clear_old_order_logs(&self, days: i32) -> Result<usize> {
        let conn = self.conn()?;
        order_logs::clear_old_logs(&conn, days)
    }

    /// Get order log statistics
    pub fn get_order_log_stats(&self) -> Result<LogStats> {
        let conn = self.conn()?;
        order_logs::get_stats(&conn)
    }

    // ========== Market Holiday Methods ==========

    /// Create a market holiday
    pub fn create_market_holiday(&self, req: &CreateHolidayRequest) -> Result<MarketHoliday> {
        let conn = self.conn()?;
        market::create_holiday(&conn, req)
    }

    /// Get holidays by year
    pub fn get_market_holidays_by_year(&self, year: i32) -> Result<Vec<MarketHoliday>> {
        let conn = self.conn()?;
        market::get_holidays_by_year(&conn, year)
    }

    /// Get holidays by exchange
    pub fn get_market_holidays_by_exchange(
        &self,
        exchange: &str,
        year: Option<i32>,
    ) -> Result<Vec<MarketHoliday>> {
        let conn = self.conn()?;
        market::get_holidays_by_exchange(&conn, exchange, year)
    }

    /// Check if a date is a holiday
    pub fn is_market_holiday(&self, exchange: &str, date: &str) -> Result<bool> {
        let conn = self.conn()?;
        market::is_holiday(&conn, exchange, date)
    }

    /// Delete a market holiday
    pub fn delete_market_holiday(&self, id: i64) -> Result<bool> {
        let conn = self.conn()?;
        market::delete_holiday(&conn, id)
    }

    // ========== Market Timing Methods ==========

    /// Get all market timings
    pub fn get_all_market_timings(&self) -> Result<Vec<MarketTiming>> {
        let conn = self.conn()?;
        market::get_all_timings(&conn)
    }

    /// Get timing for an exchange
    pub fn get_market_timing(&self, exchange: &str) -> Result<Option<MarketTiming>> {
        let conn = self.conn()?;
        market::get_timing_by_exchange(&conn, exchange)
    }

    /// Update market timing
    pub fn update_market_timing(
        &self,
        exchange: &str,
        req: &UpdateTimingRequest,
    ) -> Result<MarketTiming> {
        let conn = self.conn()?;
        market::update_timing(&conn, exchange, req)
    }

    /// Create market timing
    pub fn create_market_timing(&self, timing: &MarketTiming) -> Result<MarketTiming> {
        let conn = self.conn()?;
        market::create_timing(&conn, timing)
    }

    /// Check if market is open
    pub fn is_market_open(&self, exchange: &str) -> Result<bool> {
        let conn = self.conn()?;
        market::is_market_open(&conn, exchange)
    }

    // ========== Analyze Mode Methods ==========

    /// Get analyze mode (sandbox/paper trading mode)
    pub fn get_analyze_mode(&self) -> Result<bool> {
        let settings = self.get_settings()?;
        Ok(settings.analyze_mode.unwrap_or(false))
    }

    /// Set analyze mode
    pub fn set_analyze_mode(&self, enabled: bool) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("UPDATE settings SET analyze_mode = ?1", [enabled])?;
        Ok(())
    }

    // ========== Order Logging Helper ==========

    /// Log an order (convenience wrapper for order_logs::create_log)
    #[allow(clippy::too_many_arguments)]
    pub fn log_order(
        &self,
        order_id: &str,
        action: &str,
        symbol: &str,
        exchange: &str,
        side: &str,
        quantity: i32,
        price: Option<f64>,
        order_type: &str,
        product: &str,
        status: &str,
        message: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<i64> {
        // Get broker from current session (we'll use "api" as placeholder if from API)
        let broker = api_key.map(|_| "api").unwrap_or("ui");

        self.create_order_log(
            Some(order_id),
            broker,
            symbol,
            exchange,
            side,
            quantity,
            price,
            order_type,
            product,
            status,
            message,
            Some(action),
        )
    }

    // ========== Analyzer Logs Methods (Paper Trading) ==========

    /// Create analyzer log entry
    pub fn create_analyzer_log(
        &self,
        api_type: &str,
        request_data: &str,
        response_data: &str,
    ) -> Result<i64> {
        let conn = self.conn()?;
        Ok(analyzer_logs::create_log(
            &conn,
            api_type,
            request_data,
            response_data,
        )?)
    }

    /// Get analyzer logs with pagination
    pub fn get_analyzer_logs(
        &self,
        api_type: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AnalyzerLog>> {
        let conn = self.conn()?;
        Ok(analyzer_logs::get_logs(&conn, api_type, limit, offset)?)
    }

    /// Get recent analyzer logs
    pub fn get_recent_analyzer_logs(&self, limit: i64) -> Result<Vec<AnalyzerLog>> {
        let conn = self.conn()?;
        Ok(analyzer_logs::get_recent_logs(&conn, limit)?)
    }

    /// Count analyzer logs
    pub fn count_analyzer_logs(&self, api_type: Option<&str>) -> Result<i64> {
        let conn = self.conn()?;
        Ok(analyzer_logs::count_logs(&conn, api_type)?)
    }

    /// Clear old analyzer logs
    pub fn clear_old_analyzer_logs(&self, days: i64) -> Result<usize> {
        let conn = self.conn()?;
        Ok(analyzer_logs::clear_old_logs(&conn, days)?)
    }

    /// Clear all analyzer logs
    pub fn clear_all_analyzer_logs(&self) -> Result<usize> {
        let conn = self.conn()?;
        Ok(analyzer_logs::clear_all_logs(&conn)?)
    }

    /// Get analyzer log statistics
    pub fn get_analyzer_log_stats(&self) -> Result<AnalyzerLogStats> {
        let conn = self.conn()?;
        Ok(analyzer_logs::get_stats(&conn)?)
    }

    // ========== Latency Logs Methods (Performance Monitoring) ==========

    /// Log latency for an order/request
    #[allow(clippy::too_many_arguments)]
    pub fn log_latency(
        &self,
        order_id: &str,
        broker: &str,
        symbol: &str,
        order_type: &str,
        rtt_ms: f64,
        validation_ms: f64,
        broker_response_ms: f64,
        overhead_ms: f64,
        total_ms: f64,
        status: &str,
        error: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        Ok(latency_logs::log_latency(
            &conn,
            order_id,
            broker,
            symbol,
            order_type,
            rtt_ms,
            validation_ms,
            broker_response_ms,
            overhead_ms,
            total_ms,
            status,
            error,
        )?)
    }

    /// Get recent latency logs
    pub fn get_recent_latency_logs(&self, limit: i64) -> Result<Vec<LatencyLog>> {
        let conn = self.conn()?;
        Ok(latency_logs::get_recent_logs(&conn, limit)?)
    }

    /// Get latency statistics
    pub fn get_latency_stats(&self) -> Result<LatencyStats> {
        let conn = self.conn()?;
        Ok(latency_logs::get_stats(&conn)?)
    }

    /// Purge old non-order latency logs
    pub fn purge_old_latency_logs(&self, days: i64) -> Result<usize> {
        let conn = self.conn()?;
        Ok(latency_logs::purge_old_data_logs(&conn, days)?)
    }

    /// Clear all latency logs
    pub fn clear_all_latency_logs(&self) -> Result<usize> {
        let conn = self.conn()?;
        Ok(latency_logs::clear_all_logs(&conn)?)
    }

    // ========== Traffic Logs Methods (HTTP Monitoring) ==========

    /// Log HTTP request
    #[allow(clippy::too_many_arguments)]
    pub fn log_traffic(
        &self,
        client_ip: &str,
        method: &str,
        path: &str,
        status_code: i32,
        duration_ms: f64,
        host: Option<&str>,
        error: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn()?;
        Ok(traffic_logs::log_request(
            &conn,
            client_ip,
            method,
            path,
            status_code,
            duration_ms,
            host,
            error,
        )?)
    }

    /// Get recent traffic logs
    pub fn get_recent_traffic_logs(&self, limit: i64) -> Result<Vec<TrafficLog>> {
        let conn = self.conn()?;
        Ok(traffic_logs::get_recent_logs(&conn, limit)?)
    }

    /// Get traffic statistics
    pub fn get_traffic_stats(&self) -> Result<TrafficStats> {
        let conn = self.conn()?;
        Ok(traffic_logs::get_stats(&conn)?)
    }

    /// Clear old traffic logs
    pub fn clear_old_traffic_logs(&self, days: i64) -> Result<usize> {
        let conn = self.conn()?;
        Ok(traffic_logs::clear_old_logs(&conn, days)?)
    }

    // ========== IP Ban Methods (Security) ==========

    /// Check if IP is banned
    pub fn is_ip_banned(&self, ip_address: &str) -> Result<bool> {
        let conn = self.conn()?;
        Ok(traffic_logs::is_ip_banned(&conn, ip_address)?)
    }

    /// Ban an IP address
    pub fn ban_ip(
        &self,
        ip_address: &str,
        reason: &str,
        duration_hours: Option<i64>,
        permanent: bool,
        created_by: &str,
    ) -> Result<bool> {
        let conn = self.conn()?;
        Ok(traffic_logs::ban_ip(
            &conn,
            ip_address,
            reason,
            duration_hours,
            permanent,
            created_by,
        )?)
    }

    /// Unban an IP address
    pub fn unban_ip(&self, ip_address: &str) -> Result<bool> {
        let conn = self.conn()?;
        Ok(traffic_logs::unban_ip(&conn, ip_address)?)
    }

    /// Get all IP bans
    pub fn get_all_ip_bans(&self) -> Result<Vec<IPBan>> {
        let conn = self.conn()?;
        Ok(traffic_logs::get_all_bans(&conn)?)
    }

    // ========== Error Tracking Methods (Security) ==========

    /// Track 404 error
    pub fn track_404(&self, ip_address: &str, path: &str) -> Result<()> {
        let conn = self.conn()?;
        Ok(traffic_logs::track_404(&conn, ip_address, path)?)
    }

    /// Get suspicious IPs with high 404 counts
    pub fn get_suspicious_404_ips(&self, min_errors: i32) -> Result<Vec<(String, i32, String)>> {
        let conn = self.conn()?;
        Ok(traffic_logs::get_suspicious_404_ips(&conn, min_errors)?)
    }

    /// Track invalid API key attempt
    pub fn track_invalid_api_key(
        &self,
        ip_address: &str,
        api_key_hash: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        Ok(traffic_logs::track_invalid_api_key(
            &conn,
            ip_address,
            api_key_hash,
        )?)
    }

    /// Get suspicious API users
    pub fn get_suspicious_api_users(&self, min_attempts: i32) -> Result<Vec<(String, i32)>> {
        let conn = self.conn()?;
        Ok(traffic_logs::get_suspicious_api_users(&conn, min_attempts)?)
    }

    // ========== Configured Brokers Methods (Keychain Optimization) ==========

    /// Mark a broker as configured (called when credentials are saved)
    pub fn mark_broker_configured(&self, broker_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO configured_brokers (broker_id) VALUES (?1)",
            [broker_id],
        )?;
        Ok(())
    }

    /// Remove broker from configured list (called when credentials are deleted)
    pub fn unmark_broker_configured(&self, broker_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM configured_brokers WHERE broker_id = ?1",
            [broker_id],
        )?;
        Ok(())
    }

    /// Get list of configured broker IDs
    pub fn get_configured_brokers(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT broker_id FROM configured_brokers")?;
        let brokers = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(brokers)
    }

    /// Check if a broker is configured
    pub fn is_broker_configured(&self, broker_id: &str) -> Result<bool> {
        let conn = self.conn()?;
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM configured_brokers WHERE broker_id = ?1)",
            [broker_id],
            |row| row.get(0),
        )?;
        Ok(exists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PRAGMA synchronous` as SQLite reports it: 1 is NORMAL, 2 is FULL.
    fn synchronous(c: &rusqlite::Connection) -> rusqlite::Result<i64> {
        c.query_row("PRAGMA synchronous", [], |r| r.get(0))
    }

    /// ARCH-02: the order and obligation journal syncs every commit; logs
    /// and the sandbox stay at NORMAL. Read back from live connections.
    #[test]
    fn each_store_runs_at_its_own_durability() {
        let dir = tempfile::tempdir().unwrap();
        let main = SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
        // Every pooled connection, not only the first one opened.
        let (a, b) = (main.conn().unwrap(), main.conn().unwrap());
        assert_eq!(synchronous(&a).unwrap(), 2, "openalgo.db runs at FULL");
        assert_eq!(synchronous(&b).unwrap(), 2, "openalgo.db runs at FULL");

        let logs = logs::LogsDb::new(&dir.path().join("logs.db")).unwrap();
        assert_eq!(
            synchronous(&logs.conn().unwrap()).unwrap(),
            1,
            "logs.db stays at NORMAL"
        );

        let sandbox = crate::sandbox::SandboxDb::open(&dir.path().join("sandbox.db")).unwrap();
        assert_eq!(
            sandbox.with_conn(synchronous).unwrap(),
            1,
            "sandbox.db stays at NORMAL"
        );
    }
}
