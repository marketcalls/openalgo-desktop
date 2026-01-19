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

    // Run each migration
    run_migration(conn, "001_users", CREATE_USERS_TABLE)?;
    run_migration(conn, "002_auth", CREATE_AUTH_TABLE)?;
    run_migration(conn, "003_api_keys", CREATE_API_KEYS_TABLE)?;
    run_migration(conn, "004_symtoken", CREATE_SYMTOKEN_TABLE)?;
    run_migration(conn, "005_strategies", CREATE_STRATEGIES_TABLE)?;
    run_migration(conn, "006_strategy_mappings", CREATE_STRATEGY_MAPPINGS_TABLE)?;
    run_migration(conn, "007_chartink_strategies", CREATE_CHARTINK_STRATEGIES_TABLE)?;
    run_migration(conn, "008_chartink_mappings", CREATE_CHARTINK_MAPPINGS_TABLE)?;
    run_migration(conn, "009_settings", CREATE_SETTINGS_TABLE)?;
    run_migration(conn, "010_chart_preferences", CREATE_CHART_PREFERENCES_TABLE)?;
    run_migration(conn, "011_qty_freeze", CREATE_QTY_FREEZE_TABLE)?;
    run_migration(conn, "012_pending_orders", CREATE_PENDING_ORDERS_TABLE)?;
    run_migration(conn, "013_market_holidays", CREATE_MARKET_HOLIDAYS_TABLE)?;
    run_migration(conn, "014_market_timings", CREATE_MARKET_TIMINGS_TABLE)?;
    run_migration(conn, "015_order_logs", CREATE_ORDER_LOGS_TABLE)?;
    run_migration(conn, "016_sandbox_orders", CREATE_SANDBOX_ORDERS_TABLE)?;
    run_migration(conn, "017_sandbox_positions", CREATE_SANDBOX_POSITIONS_TABLE)?;
    run_migration(conn, "018_sandbox_trades", CREATE_SANDBOX_TRADES_TABLE)?;
    run_migration(conn, "019_sandbox_holdings", CREATE_SANDBOX_HOLDINGS_TABLE)?;
    run_migration(conn, "020_sandbox_funds", CREATE_SANDBOX_FUNDS_TABLE)?;
    run_migration(conn, "021_sandbox_daily_pnl", CREATE_SANDBOX_DAILY_PNL_TABLE)?;
    run_migration(conn, "022_auth_separate_nonces", ALTER_AUTH_SEPARATE_NONCES)?;
    run_migration(conn, "023_auto_logout_settings", ADD_AUTO_LOGOUT_SETTINGS)?;
    run_migration(conn, "024_webhook_settings", ADD_WEBHOOK_SETTINGS)?;
    run_migration(conn, "025_analyzer_logs", CREATE_ANALYZER_LOGS_TABLE)?;
    run_migration(conn, "026_latency_logs", CREATE_LATENCY_LOGS_TABLE)?;
    run_migration(conn, "027_traffic_logs", CREATE_TRAFFIC_LOGS_TABLE)?;
    run_migration(conn, "028_ip_bans", CREATE_IP_BANS_TABLE)?;
    run_migration(conn, "029_error_trackers", CREATE_ERROR_TRACKERS_TABLES)?;
    run_migration(conn, "030_sandbox_config", CREATE_SANDBOX_CONFIG_TABLE)?;
    run_migration(conn, "031_symtoken_broker_fields", ADD_SYMTOKEN_BROKER_FIELDS)?;
    run_migration(conn, "032_analyze_mode", ADD_ANALYZE_MODE)?;

    tracing::info!("Database migrations completed");
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
        conn.execute(
            "INSERT INTO migrations (name) VALUES (?)",
            [name],
        )?;
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
