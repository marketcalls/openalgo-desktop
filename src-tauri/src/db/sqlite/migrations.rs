//! SQLite database migrations

use crate::error::Result;
use rusqlite::Connection;

/// Run all database migrations
pub fn run_migrations(conn: &Connection) -> Result<()> {
    // Create migrations table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS migrations (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;

    run_legacy_schema(conn)?;
    run_rust_migration(conn, "037_users_email_totp", m037_users_email_totp)?;
    run_rust_migration(conn, "038_auth_session_columns", m038_auth_session_columns)?;
    run_rust_migration(conn, "039_api_keys_lookup", m039_api_keys_lookup)?;
    run_rust_migration(conn, "040_pending_oauth", m040_pending_oauth)?;
    run_rust_migration(conn, "041_server_settings", m041_server_settings)?;
    run_rust_migration(
        conn,
        "042_broker_credentials_market",
        m042_broker_credentials_market,
    )?;
    run_rust_migration(conn, "043_symtoken_master", super::symbol::migrate_symtoken)?;
    run_rust_migration(conn, "050_market_calendar", super::market_calendar::migrate)?;
    run_rust_migration(
        conn,
        "051_security_settings",
        super::webui::migrate_security_settings,
    )?;
    run_rust_migration(conn, "052_leverage_config", super::webui::migrate_leverage)?;
    run_rust_migration(
        conn,
        "060_action_center_pending_orders",
        super::action_center::migrate,
    )?;
    run_rust_migration(conn, "061_watchlists", super::watchlist::migrate)?;
    run_rust_migration(conn, "062_alert_log", super::alert_log::migrate)?;
    run_rust_migration(
        conn,
        "063_strategy_portfolio",
        super::strategy_portfolio::migrate,
    )?;
    run_rust_migration(
        conn,
        "064_pending_oauth_redirect",
        m064_pending_oauth_redirect,
    )?;
    run_rust_migration(
        conn,
        "065_symtoken_contract_value",
        super::symbol::migrate_contract_value,
    )?;
    run_rust_migration(
        conn,
        "066_master_contract_status",
        super::master_contract_status::migrate,
    )?;
    run_rust_migration(
        conn,
        "070_telegram",
        crate::messaging::telegram::db::migrate,
    )?;
    run_rust_migration(
        conn,
        "071_whatsapp",
        crate::messaging::whatsapp::db::migrate,
    )?;
    run_rust_migration(conn, "072_strategy_module", crate::strategy::store::migrate)?;
    run_rust_migration(conn, "073_strategy_book", crate::strategy::book::migrate)?;
    run_rust_migration(
        conn,
        "074_openscript_runner",
        crate::trading::runner::store::migrate,
    )?;
    run_rust_migration(conn, "075_scalping", crate::scalping::store::migrate)?;
    run_rust_migration(conn, "076_chartink", crate::chartink::store::migrate)?;

    tracing::info!("Database migrations completed");
    Ok(())
}

/// Migrations 001-036, the schema shipped before this version. Separate so
/// migration tests can build a database exactly as an older build left it.
pub fn run_legacy_schema(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS migrations (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;
    run_migration(conn, "001_users", CREATE_USERS_TABLE)?;
    run_migration(conn, "002_auth", CREATE_AUTH_TABLE)?;
    run_migration(conn, "003_api_keys", CREATE_API_KEYS_TABLE)?;
    run_migration(conn, "004_symtoken", CREATE_SYMTOKEN_TABLE)?;
    run_migration(conn, "005_strategies", CREATE_STRATEGIES_TABLE)?;
    run_migration(
        conn,
        "006_strategy_mappings",
        CREATE_STRATEGY_MAPPINGS_TABLE,
    )?;
    run_migration(
        conn,
        "007_chartink_strategies",
        CREATE_CHARTINK_STRATEGIES_TABLE,
    )?;
    run_migration(
        conn,
        "008_chartink_mappings",
        CREATE_CHARTINK_MAPPINGS_TABLE,
    )?;
    run_migration(conn, "009_settings", CREATE_SETTINGS_TABLE)?;
    run_migration(
        conn,
        "010_chart_preferences",
        CREATE_CHART_PREFERENCES_TABLE,
    )?;
    run_migration(conn, "011_qty_freeze", CREATE_QTY_FREEZE_TABLE)?;
    run_migration(conn, "012_pending_orders", CREATE_PENDING_ORDERS_TABLE)?;
    run_migration(conn, "013_market_holidays", CREATE_MARKET_HOLIDAYS_TABLE)?;
    run_migration(conn, "014_market_timings", CREATE_MARKET_TIMINGS_TABLE)?;
    run_migration(conn, "015_order_logs", CREATE_ORDER_LOGS_TABLE)?;
    run_migration(conn, "016_sandbox_orders", CREATE_SANDBOX_ORDERS_TABLE)?;
    run_migration(
        conn,
        "017_sandbox_positions",
        CREATE_SANDBOX_POSITIONS_TABLE,
    )?;
    run_migration(conn, "018_sandbox_trades", CREATE_SANDBOX_TRADES_TABLE)?;
    run_migration(conn, "019_sandbox_holdings", CREATE_SANDBOX_HOLDINGS_TABLE)?;
    run_migration(conn, "020_sandbox_funds", CREATE_SANDBOX_FUNDS_TABLE)?;
    run_migration(
        conn,
        "021_sandbox_daily_pnl",
        CREATE_SANDBOX_DAILY_PNL_TABLE,
    )?;
    run_migration(conn, "022_auth_separate_nonces", ALTER_AUTH_SEPARATE_NONCES)?;
    run_migration(conn, "023_auto_logout_settings", ADD_AUTO_LOGOUT_SETTINGS)?;
    run_migration(conn, "024_webhook_settings", ADD_WEBHOOK_SETTINGS)?;
    run_migration(conn, "025_analyzer_logs", CREATE_ANALYZER_LOGS_TABLE)?;
    run_migration(conn, "026_latency_logs", CREATE_LATENCY_LOGS_TABLE)?;
    run_migration(conn, "027_traffic_logs", CREATE_TRAFFIC_LOGS_TABLE)?;
    run_migration(conn, "028_ip_bans", CREATE_IP_BANS_TABLE)?;
    run_migration(conn, "029_error_trackers", CREATE_ERROR_TRACKERS_TABLES)?;
    run_migration(conn, "030_sandbox_config", CREATE_SANDBOX_CONFIG_TABLE)?;
    run_migration(
        conn,
        "031_symtoken_broker_fields",
        ADD_SYMTOKEN_BROKER_FIELDS,
    )?;
    run_migration(conn, "032_analyze_mode", ADD_ANALYZE_MODE)?;
    run_migration(
        conn,
        "033_configured_brokers",
        CREATE_CONFIGURED_BROKERS_TABLE,
    )?;
    run_migration(
        conn,
        "034_broker_credentials",
        CREATE_BROKER_CREDENTIALS_TABLE,
    )?;
    run_migration(conn, "035_rate_limit_settings", ADD_RATE_LIMIT_SETTINGS)?;
    run_migration(
        conn,
        "036_enable_webhook_default",
        ENABLE_WEBHOOK_BY_DEFAULT,
    )?;
    Ok(())
}

/// Whether a migration has been recorded.
pub fn is_applied(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name = ?)",
        [name],
        |row| row.get(0),
    )?)
}

/// Record a migration as applied (used by data migrations run from Rust).
pub fn mark_applied(conn: &Connection, name: &str) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO migrations (name) VALUES (?)", [name])?;
    Ok(())
}

/// Run a migration written in Rust, inside one transaction, once.
fn run_rust_migration(
    conn: &Connection,
    name: &str,
    f: fn(&Connection) -> Result<()>,
) -> Result<()> {
    if is_applied(conn, name)? {
        return Ok(());
    }
    tracing::info!("Running migration: {}", name);
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match f(conn).and_then(|_| mark_applied(conn, name)) {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|n| n == column))
}

/// `ALTER TABLE ADD COLUMN` only when the column is missing, so a migration
/// re-run on a partially migrated database never fails.
fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            table, column, decl
        ))?;
    }
    Ok(())
}

fn m037_users_email_totp(conn: &Connection) -> Result<()> {
    add_column(conn, "users", "email", "TEXT")?;
    add_column(conn, "users", "totp_secret_encrypted", "TEXT")?;
    add_column(conn, "users", "totp_nonce", "TEXT")?;
    add_column(conn, "users", "totp_enabled", "INTEGER NOT NULL DEFAULT 0")?;
    add_column(
        conn,
        "users",
        "totp_required_for_login",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(
        conn,
        "users",
        "totp_required_for_password_reset",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(
        conn,
        "users",
        "totp_required_for_mcp",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn m038_auth_session_columns(conn: &Connection) -> Result<()> {
    add_column(conn, "auth", "user_id", "TEXT")?;
    add_column(conn, "auth", "user_name", "TEXT")?;
    add_column(conn, "auth", "authenticated_at", "TEXT")?;
    add_column(conn, "auth", "is_revoked", "INTEGER NOT NULL DEFAULT 0")?;
    // Backfill from the row's own history rather than "now": a token issued
    // before the last 03:00 IST boundary must stay expired after the upgrade.
    conn.execute_batch(
        "UPDATE auth SET authenticated_at = COALESCE(authenticated_at,
            strftime('%Y-%m-%dT%H:%M:%SZ', updated_at),
            strftime('%Y-%m-%dT%H:%M:%SZ', created_at))",
    )?;
    Ok(())
}

fn m039_api_keys_lookup(conn: &Connection) -> Result<()> {
    add_column(conn, "api_keys", "lookup_hmac", "TEXT")?;
    add_column(
        conn,
        "api_keys",
        "order_mode",
        "TEXT NOT NULL DEFAULT 'auto'",
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_lookup ON api_keys(lookup_hmac)
         WHERE lookup_hmac IS NOT NULL",
    )?;
    Ok(())
}

fn m040_pending_oauth(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_oauth (
            state_hash TEXT PRIMARY KEY,
            broker TEXT NOT NULL,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

/// The callback address a pending sign-in's authorize URL was built with,
/// for the code exchange (Upstox checks it byte for byte), and the hash of
/// the browser session that started it, for redirects that drop `state`.
/// the hash of a broker-issued login id the callback may repeat (Dhan's
/// `consentAppId`). Rows from before the columns have none: they fall back
/// to the web redirect convention and can only be completed with `state`.
fn m064_pending_oauth_redirect(conn: &Connection) -> Result<()> {
    add_column(conn, "pending_oauth", "redirect_uri", "TEXT")?;
    add_column(conn, "pending_oauth", "session_hash", "TEXT")?;
    add_column(conn, "pending_oauth", "binding_hash", "TEXT")?;
    Ok(())
}

fn m041_server_settings(conn: &Connection) -> Result<()> {
    add_column(conn, "settings", "http_port", "INTEGER")?;
    add_column(conn, "settings", "ws_port", "INTEGER")?;
    add_column(conn, "settings", "bind_host", "TEXT")?;
    add_column(conn, "settings", "active_broker", "TEXT")?;
    add_column(conn, "settings", "redirect_url", "TEXT")?;
    add_column(conn, "settings", "host_server", "TEXT")?;
    add_column(conn, "settings", "websocket_url", "TEXT")?;
    add_column(
        conn,
        "settings",
        "ngrok_allow",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Backfill from what the user already chose for the old webhook server;
    // only fill what is still empty.
    conn.execute_batch(
        "UPDATE settings SET
            http_port = COALESCE(http_port, webhook_port, 5000),
            ws_port = COALESCE(ws_port, 8765),
            bind_host = COALESCE(bind_host, webhook_host, '127.0.0.1'),
            host_server = COALESCE(host_server, ngrok_url),
            active_broker = COALESCE(active_broker, default_broker)
         WHERE id = 1",
    )?;
    Ok(())
}

fn m042_broker_credentials_market(conn: &Connection) -> Result<()> {
    add_column(
        conn,
        "broker_credentials",
        "api_key_market_encrypted",
        "TEXT",
    )?;
    add_column(conn, "broker_credentials", "api_key_market_nonce", "TEXT")?;
    add_column(
        conn,
        "broker_credentials",
        "api_secret_market_encrypted",
        "TEXT",
    )?;
    add_column(
        conn,
        "broker_credentials",
        "api_secret_market_nonce",
        "TEXT",
    )?;
    Ok(())
}

fn run_migration(conn: &Connection, name: &str, sql: &str) -> Result<()> {
    // Check if migration already applied
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name = ?)",
        [name],
        |row| row.get(0),
    )?;

    if !exists {
        tracing::info!("Running migration: {}", name);
        conn.execute_batch(sql)?;
        conn.execute("INSERT INTO migrations (name) VALUES (?)", [name])?;
    }

    Ok(())
}

const CREATE_USERS_TABLE: &str = r#"
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_AUTH_TABLE: &str = r#"
CREATE TABLE auth (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    broker_id TEXT NOT NULL UNIQUE,
    auth_token_encrypted TEXT NOT NULL,
    feed_token_encrypted TEXT,
    nonce TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_API_KEYS_TABLE: &str = r#"
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,
    encrypted_key TEXT NOT NULL,
    nonce TEXT NOT NULL,
    permissions TEXT NOT NULL DEFAULT 'read',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_used_at TEXT
);
"#;

const CREATE_SYMTOKEN_TABLE: &str = r#"
CREATE TABLE symtoken (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    token TEXT NOT NULL,
    exchange TEXT NOT NULL,
    name TEXT NOT NULL,
    lot_size INTEGER NOT NULL DEFAULT 1,
    tick_size REAL NOT NULL DEFAULT 0.05,
    instrument_type TEXT NOT NULL DEFAULT 'EQ',
    expiry TEXT,
    strike REAL,
    option_type TEXT,
    UNIQUE(exchange, symbol)
);
CREATE INDEX IF NOT EXISTS idx_symtoken_exchange ON symtoken(exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_token ON symtoken(token);
CREATE INDEX IF NOT EXISTS idx_symtoken_symbol ON symtoken(symbol);
"#;

const CREATE_STRATEGIES_TABLE: &str = r#"
CREATE TABLE strategies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    webhook_id TEXT NOT NULL UNIQUE,
    exchange TEXT NOT NULL,
    symbol TEXT NOT NULL,
    product TEXT NOT NULL DEFAULT 'MIS',
    quantity INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_STRATEGY_MAPPINGS_TABLE: &str = r#"
CREATE TABLE strategy_symbol_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id INTEGER NOT NULL REFERENCES strategies(id) ON DELETE CASCADE,
    exchange TEXT NOT NULL,
    symbol TEXT NOT NULL,
    quantity INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_CHARTINK_STRATEGIES_TABLE: &str = r#"
CREATE TABLE chartink_strategies (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    webhook_id TEXT NOT NULL UNIQUE,
    scan_url TEXT,
    product TEXT NOT NULL DEFAULT 'MIS',
    quantity INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_CHARTINK_MAPPINGS_TABLE: &str = r#"
CREATE TABLE chartink_symbol_mappings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id INTEGER NOT NULL REFERENCES chartink_strategies(id) ON DELETE CASCADE,
    exchange TEXT NOT NULL,
    symbol TEXT NOT NULL,
    quantity INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_SETTINGS_TABLE: &str = r#"
CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    theme TEXT NOT NULL DEFAULT 'system',
    default_broker TEXT,
    default_exchange TEXT NOT NULL DEFAULT 'NSE',
    default_product TEXT NOT NULL DEFAULT 'MIS',
    order_confirm INTEGER NOT NULL DEFAULT 1,
    sound_enabled INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO settings (id) VALUES (1);
"#;

const CREATE_CHART_PREFERENCES_TABLE: &str = r#"
CREATE TABLE chart_preferences (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    timeframe TEXT NOT NULL DEFAULT '1D',
    chart_type TEXT NOT NULL DEFAULT 'candle',
    indicators TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(exchange, symbol)
);
"#;

const CREATE_QTY_FREEZE_TABLE: &str = r#"
CREATE TABLE qty_freeze (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    exchange TEXT NOT NULL,
    symbol TEXT NOT NULL,
    freeze_qty INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(exchange, symbol)
);
"#;

const CREATE_PENDING_ORDERS_TABLE: &str = r#"
CREATE TABLE pending_orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    strategy_id INTEGER,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    side TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    price REAL NOT NULL DEFAULT 0,
    order_type TEXT NOT NULL DEFAULT 'MARKET',
    product TEXT NOT NULL DEFAULT 'MIS',
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    processed_at TEXT
);
"#;

const CREATE_MARKET_HOLIDAYS_TABLE: &str = r#"
CREATE TABLE market_holidays (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    date TEXT NOT NULL,
    description TEXT,
    year INTEGER NOT NULL,
    UNIQUE(date)
);

CREATE TABLE market_holiday_exchanges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    holiday_id INTEGER NOT NULL REFERENCES market_holidays(id) ON DELETE CASCADE,
    exchange TEXT NOT NULL
);
"#;

const CREATE_MARKET_TIMINGS_TABLE: &str = r#"
CREATE TABLE market_timings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    exchange TEXT NOT NULL UNIQUE,
    pre_open_start TEXT,
    pre_open_end TEXT,
    market_open TEXT NOT NULL,
    market_close TEXT NOT NULL,
    post_close_end TEXT
);
INSERT OR IGNORE INTO market_timings (exchange, market_open, market_close)
VALUES ('NSE', '09:15', '15:30'), ('BSE', '09:15', '15:30'), ('NFO', '09:15', '15:30'),
       ('MCX', '09:00', '23:30'), ('CDS', '09:00', '17:00');
"#;

const CREATE_ORDER_LOGS_TABLE: &str = r#"
CREATE TABLE order_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id TEXT,
    broker TEXT NOT NULL,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    side TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    price REAL,
    order_type TEXT NOT NULL,
    product TEXT NOT NULL,
    status TEXT NOT NULL,
    message TEXT,
    source TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_order_logs_created ON order_logs(created_at);
"#;

const CREATE_SANDBOX_ORDERS_TABLE: &str = r#"
CREATE TABLE sandbox_orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id TEXT NOT NULL UNIQUE,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    side TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    price REAL NOT NULL,
    order_type TEXT NOT NULL,
    product TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    filled_quantity INTEGER NOT NULL DEFAULT 0,
    average_price REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_SANDBOX_POSITIONS_TABLE: &str = r#"
CREATE TABLE sandbox_positions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    product TEXT NOT NULL,
    quantity INTEGER NOT NULL DEFAULT 0,
    average_price REAL NOT NULL DEFAULT 0,
    ltp REAL NOT NULL DEFAULT 0,
    pnl REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(exchange, symbol, product)
);
"#;

const CREATE_SANDBOX_TRADES_TABLE: &str = r#"
CREATE TABLE sandbox_trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id TEXT NOT NULL,
    trade_id TEXT NOT NULL UNIQUE,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    side TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    price REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

const CREATE_SANDBOX_HOLDINGS_TABLE: &str = r#"
CREATE TABLE sandbox_holdings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    exchange TEXT NOT NULL,
    quantity INTEGER NOT NULL,
    average_price REAL NOT NULL,
    ltp REAL NOT NULL DEFAULT 0,
    pnl REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(exchange, symbol)
);
"#;

const CREATE_SANDBOX_FUNDS_TABLE: &str = r#"
CREATE TABLE sandbox_funds (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    available_cash REAL NOT NULL DEFAULT 1000000,
    used_margin REAL NOT NULL DEFAULT 0,
    total_value REAL NOT NULL DEFAULT 1000000,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO sandbox_funds (id) VALUES (1);
"#;

const CREATE_SANDBOX_DAILY_PNL_TABLE: &str = r#"
CREATE TABLE sandbox_daily_pnl (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    date TEXT NOT NULL UNIQUE,
    realized_pnl REAL NOT NULL DEFAULT 0,
    unrealized_pnl REAL NOT NULL DEFAULT 0,
    total_pnl REAL NOT NULL DEFAULT 0,
    portfolio_value REAL NOT NULL DEFAULT 1000000,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Migration to add separate nonce column for feed_token
/// This fixes the critical bug where auth_token and feed_token were encrypted
/// with different nonces but only one was stored
const ALTER_AUTH_SEPARATE_NONCES: &str = r#"
-- Rename existing nonce column to auth_token_nonce
ALTER TABLE auth RENAME COLUMN nonce TO auth_token_nonce;

-- Add separate nonce column for feed_token
ALTER TABLE auth ADD COLUMN feed_token_nonce TEXT;
"#;

/// Migration to add auto-logout configuration to settings
/// Allows users to configure the daily auto-logout time (default: 3:00 AM IST)
const ADD_AUTO_LOGOUT_SETTINGS: &str = r#"
-- Add auto-logout time configuration (hour and minute in IST)
ALTER TABLE settings ADD COLUMN auto_logout_hour INTEGER NOT NULL DEFAULT 3;
ALTER TABLE settings ADD COLUMN auto_logout_minute INTEGER NOT NULL DEFAULT 0;

-- Add warning intervals as JSON array (minutes before logout)
ALTER TABLE settings ADD COLUMN auto_logout_warnings TEXT NOT NULL DEFAULT '[30, 15, 5, 1]';

-- Add flag to enable/disable auto-logout
ALTER TABLE settings ADD COLUMN auto_logout_enabled INTEGER NOT NULL DEFAULT 1;
"#;

/// Migration to add webhook server configuration
/// For receiving TradingView, GoCharting, and Chartink alerts
const ADD_WEBHOOK_SETTINGS: &str = r#"
-- Webhook server configuration
ALTER TABLE settings ADD COLUMN webhook_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE settings ADD COLUMN webhook_port INTEGER NOT NULL DEFAULT 5000;
ALTER TABLE settings ADD COLUMN webhook_host TEXT NOT NULL DEFAULT '127.0.0.1';

-- Ngrok/external URL for strategies to use
ALTER TABLE settings ADD COLUMN ngrok_url TEXT;

-- Optional webhook authentication secret
ALTER TABLE settings ADD COLUMN webhook_secret TEXT;
"#;

/// Migration to create analyzer_logs table for paper trading logs
const CREATE_ANALYZER_LOGS_TABLE: &str = r#"
CREATE TABLE analyzer_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_type TEXT NOT NULL,
    request_data TEXT NOT NULL,
    response_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_analyzer_logs_api_type ON analyzer_logs(api_type);
CREATE INDEX idx_analyzer_logs_created_at ON analyzer_logs(created_at);
"#;

/// Migration to create latency_logs table for performance monitoring
const CREATE_LATENCY_LOGS_TABLE: &str = r#"
CREATE TABLE latency_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    order_id TEXT NOT NULL,
    broker TEXT,
    symbol TEXT,
    order_type TEXT,
    rtt_ms REAL DEFAULT 0,
    validation_ms REAL DEFAULT 0,
    broker_response_ms REAL DEFAULT 0,
    overhead_ms REAL DEFAULT 0,
    total_ms REAL NOT NULL,
    status TEXT NOT NULL,
    error TEXT
);
CREATE INDEX idx_latency_logs_timestamp ON latency_logs(timestamp);
CREATE INDEX idx_latency_logs_broker ON latency_logs(broker);
CREATE INDEX idx_latency_logs_status ON latency_logs(status);
"#;

/// Migration to create traffic_logs table for HTTP request tracking
const CREATE_TRAFFIC_LOGS_TABLE: &str = r#"
CREATE TABLE traffic_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    client_ip TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    duration_ms REAL NOT NULL,
    host TEXT,
    error TEXT
);
CREATE INDEX idx_traffic_logs_timestamp ON traffic_logs(timestamp);
CREATE INDEX idx_traffic_logs_client_ip ON traffic_logs(client_ip);
CREATE INDEX idx_traffic_logs_status_code ON traffic_logs(status_code);
"#;

/// Migration to create ip_bans table for security
const CREATE_IP_BANS_TABLE: &str = r#"
CREATE TABLE ip_bans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL UNIQUE,
    ban_reason TEXT,
    ban_count INTEGER DEFAULT 1,
    banned_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT,
    is_permanent INTEGER DEFAULT 0,
    created_by TEXT DEFAULT 'system'
);
CREATE INDEX idx_ip_bans_ip_address ON ip_bans(ip_address);
"#;

/// Migration to create error tracking tables
const CREATE_ERROR_TRACKERS_TABLES: &str = r#"
-- 404 error tracker
CREATE TABLE error_404_tracker (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL,
    error_count INTEGER DEFAULT 1,
    first_error_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_error_at TEXT NOT NULL DEFAULT (datetime('now')),
    paths_attempted TEXT
);
CREATE INDEX idx_404_tracker_ip ON error_404_tracker(ip_address);
CREATE INDEX idx_404_tracker_count ON error_404_tracker(error_count);

-- Invalid API key tracker
CREATE TABLE invalid_api_key_tracker (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ip_address TEXT NOT NULL,
    attempt_count INTEGER DEFAULT 1,
    first_attempt_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_attempt_at TEXT NOT NULL DEFAULT (datetime('now')),
    api_keys_tried TEXT
);
CREATE INDEX idx_api_tracker_ip ON invalid_api_key_tracker(ip_address);
CREATE INDEX idx_api_tracker_count ON invalid_api_key_tracker(attempt_count);
"#;

/// Migration to create sandbox_config table for paper trading settings
const CREATE_SANDBOX_CONFIG_TABLE: &str = r#"
CREATE TABLE sandbox_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    starting_capital REAL NOT NULL DEFAULT 10000000,
    reset_day TEXT NOT NULL DEFAULT 'Never',
    reset_time TEXT NOT NULL DEFAULT '00:00',
    order_check_interval INTEGER NOT NULL DEFAULT 5,
    mtm_update_interval INTEGER NOT NULL DEFAULT 1,
    nse_mis_leverage REAL NOT NULL DEFAULT 5.0,
    nfo_mis_leverage REAL NOT NULL DEFAULT 2.0,
    cds_mis_leverage REAL NOT NULL DEFAULT 2.0,
    mcx_mis_leverage REAL NOT NULL DEFAULT 2.0,
    nse_cnc_leverage REAL NOT NULL DEFAULT 1.0,
    nfo_nrml_leverage REAL NOT NULL DEFAULT 1.0,
    cds_nrml_leverage REAL NOT NULL DEFAULT 1.0,
    mcx_nrml_leverage REAL NOT NULL DEFAULT 1.0,
    nse_square_off_time TEXT NOT NULL DEFAULT '15:15',
    nfo_square_off_time TEXT NOT NULL DEFAULT '15:25',
    cds_square_off_time TEXT NOT NULL DEFAULT '16:55',
    mcx_square_off_time TEXT NOT NULL DEFAULT '23:25',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO sandbox_config (id) VALUES (1);
"#;

/// Migration to add brsymbol and brexchange columns to symtoken table
const ADD_SYMTOKEN_BROKER_FIELDS: &str = r#"
ALTER TABLE symtoken ADD COLUMN brsymbol TEXT;
ALTER TABLE symtoken ADD COLUMN brexchange TEXT;
CREATE INDEX IF NOT EXISTS idx_symtoken_brsymbol ON symtoken(brsymbol);
"#;

/// Migration to add analyze_mode column to settings table
const ADD_ANALYZE_MODE: &str = r#"
ALTER TABLE settings ADD COLUMN analyze_mode INTEGER NOT NULL DEFAULT 0;
"#;

/// Migration to track configured brokers (to avoid unnecessary keychain access)
const CREATE_CONFIGURED_BROKERS_TABLE: &str = r#"
CREATE TABLE configured_brokers (
    broker_id TEXT PRIMARY KEY,
    configured_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Migration to store encrypted broker credentials in SQLite (replaces OS keychain)
const CREATE_BROKER_CREDENTIALS_TABLE: &str = r#"
CREATE TABLE broker_credentials (
    broker_id TEXT PRIMARY KEY,
    api_key_encrypted TEXT NOT NULL,
    api_key_nonce TEXT NOT NULL,
    api_secret_encrypted TEXT,
    api_secret_nonce TEXT,
    client_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

/// Migration to add rate limit settings for broker API calls
const ADD_RATE_LIMIT_SETTINGS: &str = r#"
-- Rate limits to prevent hitting broker API limits
-- api_rate_limit: general API calls per second (default 100)
-- order_rate_limit: order placement calls per second (default 10)
-- smart_order_rate_limit: smart order calls per second (default 2)
-- smart_order_delay: delay in seconds between multi-legged orders (default 0.5)
ALTER TABLE settings ADD COLUMN api_rate_limit INTEGER NOT NULL DEFAULT 100;
ALTER TABLE settings ADD COLUMN order_rate_limit INTEGER NOT NULL DEFAULT 10;
ALTER TABLE settings ADD COLUMN smart_order_rate_limit INTEGER NOT NULL DEFAULT 2;
ALTER TABLE settings ADD COLUMN smart_order_delay REAL NOT NULL DEFAULT 0.5;
"#;

/// Migration to enable webhook server by default (required for OAuth)
const ENABLE_WEBHOOK_BY_DEFAULT: &str = r#"
-- Enable webhook server by default so OAuth callbacks work
-- This is required for Fyers, Zerodha, and other OAuth-based brokers
UPDATE settings SET webhook_enabled = 1 WHERE id = 1;
"#;
