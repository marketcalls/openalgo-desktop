//! Application state management

use crate::brokers::BrokerRegistry;
use crate::db::sqlite::SqliteDb;
use crate::db::duckdb::DuckDb;
use crate::error::{AppError, Result};
use crate::security::SecurityManager;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// User session information
#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: i64,
    pub username: String,
    pub authenticated_at: chrono::DateTime<chrono::Utc>,
}

/// Broker session information
#[derive(Debug, Clone)]
pub struct BrokerSession {
    pub broker_id: String,
    pub auth_token: String,
    pub feed_token: Option<String>,
    pub user_id: String,
    pub authenticated_at: chrono::DateTime<chrono::Utc>,
}

/// Symbol cache entry
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub symbol: String,
    pub token: String,
    pub exchange: String,
    pub name: String,
    pub lot_size: i32,
    pub tick_size: f64,
    pub instrument_type: String,
}

/// Application state shared across all commands
pub struct AppState {
    /// SQLite database connection
    pub sqlite: Arc<SqliteDb>,

    /// DuckDB connection for historical data
    pub duckdb: Arc<DuckDb>,

    /// Security manager for encryption/keychain
    pub security: Arc<SecurityManager>,

    /// Broker registry
    pub brokers: Arc<BrokerRegistry>,

    /// Current user session
    pub user_session: RwLock<Option<UserSession>>,

    /// Current broker session
    pub broker_session: RwLock<Option<BrokerSession>>,

    /// Symbol cache (token -> symbol info)
    pub symbol_cache: DashMap<String, SymbolInfo>,

    /// Reverse symbol cache (symbol -> token)
    pub symbol_reverse_cache: DashMap<String, String>,

    /// Application data directory
    pub data_dir: PathBuf,
}

impl AppState {
    /// Create new application state
    pub fn new(app_handle: &AppHandle) -> Result<Self> {
        // Get app data directory
        let data_dir = app_handle
            .path()
            .app_data_dir()
            .map_err(|e| AppError::Config(format!("Failed to get app data directory: {}", e)))?;

        // Create data directory if it doesn't exist
        std::fs::create_dir_all(&data_dir)?;

        tracing::info!("Data directory: {:?}", data_dir);

        // Initialize SQLite database
        let sqlite_path = data_dir.join("openalgo.db");
        let sqlite = Arc::new(SqliteDb::new(&sqlite_path)?);

        // Initialize DuckDB for historical data
        let duckdb_path = data_dir.join("historify.duckdb");
        let duckdb = Arc::new(DuckDb::new(&duckdb_path)?);

        // Initialize security manager
        let security = Arc::new(SecurityManager::new()?);

        // Initialize broker registry
        let brokers = Arc::new(BrokerRegistry::new());

        Ok(Self {
            sqlite,
            duckdb,
            security,
            brokers,
            user_session: RwLock::new(None),
            broker_session: RwLock::new(None),
            symbol_cache: DashMap::new(),
            symbol_reverse_cache: DashMap::new(),
            data_dir,
        })
    }

    /// Check if user is authenticated
    pub fn is_authenticated(&self) -> bool {
        self.user_session.read().is_some()
    }

    /// Check if broker is connected
    pub fn is_broker_connected(&self) -> bool {
        self.broker_session.read().is_some()
    }

    /// Get current user session
    pub fn get_user_session(&self) -> Option<UserSession> {
        self.user_session.read().clone()
    }

    /// Set user session
    pub fn set_user_session(&self, session: Option<UserSession>) {
        *self.user_session.write() = session;
    }

    /// Get current broker session
    pub fn get_broker_session(&self) -> Option<BrokerSession> {
        self.broker_session.read().clone()
    }

    /// Set broker session
    pub fn set_broker_session(&self, session: Option<BrokerSession>) {
        *self.broker_session.write() = session;
    }

    /// Get symbol info by token
    pub fn get_symbol_by_token(&self, token: &str) -> Option<SymbolInfo> {
        self.symbol_cache.get(token).map(|r| r.clone())
    }

    /// Get token by symbol
    pub fn get_token_by_symbol(&self, symbol: &str) -> Option<String> {
        self.symbol_reverse_cache.get(symbol).map(|r| r.clone())
    }

    /// Load symbols into cache
    pub fn load_symbol_cache(&self, symbols: Vec<SymbolInfo>) {
        self.symbol_cache.clear();
        self.symbol_reverse_cache.clear();

        for symbol in symbols {
            let key = format!("{}:{}", symbol.exchange, symbol.token);
            self.symbol_reverse_cache.insert(
                format!("{}:{}", symbol.exchange, symbol.symbol.clone()),
                symbol.token.clone(),
            );
            self.symbol_cache.insert(key, symbol);
        }

        tracing::info!("Loaded {} symbols into cache", self.symbol_cache.len());
    }
}
