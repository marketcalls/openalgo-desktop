//! Application context: the one object every service, HTTP handler and Tauri
//! command receives. It owns the databases, key material, event bus, broker
//! session, web sessions, rate limiter, shared HTTP client, background tasks
//! and the shutdown token.
//!
//! `AppState` is the historical name; `AppContext` is an alias.

use crate::brokers::BrokerRegistry;
use crate::clock::{Clock, SystemClock};
use crate::config::ServerConfig;
use crate::db::duckdb::DuckDb;
use crate::db::sqlite::logs::LogsDb;
use crate::db::sqlite::SqliteDb;
use crate::error::Result;
use crate::events::subscribers::{SocketEmitter, UiEmitter};
use crate::events::EventBus;
use crate::security::keystore::KeyStore;
use crate::security::{Secret, SecurityManager};
use crate::server::ratelimit::RateLimiter;
use crate::services::apikey_service::ApiKeyCache;
use crate::session::web::WebSessionStore;
use crate::websocket::WebSocketManager;
use chrono::{DateTime, Utc};
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub type AppContext = AppState;

/// Live broker session held in memory while the trader is connected.
#[derive(Debug, Clone)]
pub struct BrokerSession {
    pub broker_id: String,
    pub auth_token: Secret,
    pub feed_token: Option<Secret>,
    pub user_id: String,
    pub user_name: Option<String>,
    pub authenticated_at: DateTime<Utc>,
}

/// Symbol master row (every web `SymToken` column).
pub use crate::brokers::common::symbols::SymToken as SymbolInfo;
use crate::brokers::common::symbols::SymbolResolver;

/// State of the HTTP listener, shown to the trader when it is not running.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerStatus {
    Starting,
    Running { host: String, port: u16 },
    PortInUse { port: u16, message: String },
    Failed { message: String },
}

pub struct AppState {
    pub sqlite: Arc<SqliteDb>,
    pub logs: Arc<LogsDb>,
    pub duckdb: Arc<DuckDb>,
    pub security: Arc<SecurityManager>,
    pub brokers: Arc<BrokerRegistry>,
    pub websocket: Arc<WebSocketManager>,
    pub bus: Arc<EventBus>,
    pub ui: Arc<SocketEmitter>,
    pub clock: Arc<dyn Clock>,
    pub config: RwLock<ServerConfig>,
    pub sessions: WebSessionStore,
    pub limiter: RateLimiter,
    pub api_keys: ApiKeyCache,
    pub broker_session: RwLock<Option<BrokerSession>>,
    pub server_status: RwLock<ServerStatus>,
    /// Shared outbound HTTP client (explicit timeouts) for non-broker calls.
    pub http: reqwest::Client,
    pub shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
    /// The symbol master, shared with every broker adapter in `brokers`.
    pub symbols: SymbolResolver,
    pub data_dir: PathBuf,
    /// Traffic, latency and security monitoring (bounded queue + writer).
    pub monitor: crate::services::monitor::Monitor,
    /// The sandbox (analyzer mode) engine, in its own `sandbox.db`.
    pub sandbox: crate::sandbox::Sandbox,
    /// Telegram and WhatsApp bots and their alerts.
    pub messaging: crate::messaging::Messaging,
    /// The strategy module and RMS (`/strategy`).
    pub strategy: Arc<crate::strategy::StrategyModule>,
}

pub struct OpenOptions {
    pub keystore: Arc<dyn KeyStore>,
    pub clock: Arc<dyn Clock>,
    pub brokers: Arc<BrokerRegistry>,
}

impl AppState {
    /// Open everything under `data_dir`. Must run inside a Tokio runtime
    /// (subscribers are spawned here).
    pub fn open(data_dir: &Path, opts: OpenOptions) -> Result<Arc<Self>> {
        crate::security::fsperm::ensure_private_dir(data_dir)?;
        let security = Arc::new(SecurityManager::open(data_dir, opts.keystore)?);
        let sqlite = Arc::new(SqliteDb::new(&data_dir.join("openalgo.db"))?);
        let logs = Arc::new(LogsDb::new(&data_dir.join("logs.db"))?);
        {
            let main = sqlite.conn()?;
            logs.import_from_main(&main)?;
        }
        let duck_path = data_dir.join("historify.duckdb");
        let duckdb = Arc::new(DuckDb::new(&duck_path)?);
        crate::security::fsperm::restrict_db_files(&duck_path)?;
        crate::db::sqlite::data_migrations::run(&sqlite, &security)?;
        let config = {
            let conn = sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        let ui = Arc::new(SocketEmitter::default());
        let bus = Arc::new(EventBus::new());
        crate::events::subscribers::register_all(
            &bus,
            logs.clone(),
            ui.clone() as Arc<dyn UiEmitter>,
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        let sandbox_db = Arc::new(crate::sandbox::SandboxDb::open(
            &data_dir.join("sandbox.db"),
        )?);
        crate::security::fsperm::restrict_db_files(&data_dir.join("sandbox.db"))?;
        let symbols = opts.brokers.symbols();
        let sandbox_bus = bus.clone();
        let sandbox_clock = opts.clock.clone();
        let websocket = Arc::new(WebSocketManager::new());
        let strategy_db = sqlite.clone();
        let strategy_ui = ui.clone();
        let strategy_symbols = symbols.clone();
        let strategy_clock = opts.clock.clone();
        let strategy_feed = websocket.clone();
        let strategy_session = (config.session_expiry_hour, config.session_expiry_minute);
        let ctx = Arc::new_cyclic(|me: &std::sync::Weak<Self>| Self {
            strategy: crate::strategy::StrategyModule::new(crate::strategy::Deps {
                db: strategy_db,
                gateway: Arc::new(crate::strategy::dispatch::AppGateway::new(me.clone())),
                rooms: Arc::new(crate::strategy::broadcast::SocketRooms::new(strategy_ui)),
                clock: strategy_clock,
                symbols: strategy_symbols,
                session_hour: strategy_session.0,
                session_minute: strategy_session.1,
                prices: Some(Arc::new(crate::strategy::tick_feed::FeedPrices::new(
                    me.clone(),
                    strategy_feed,
                ))),
            }),
            sandbox: crate::sandbox::Sandbox::with_db(
                sandbox_db,
                crate::sandbox::SandboxDeps {
                    symbols: Arc::new(crate::services::sandbox_feed::MasterSymbols(
                        symbols.clone(),
                    )),
                    quotes: Arc::new(crate::services::sandbox_feed::LiveQuotes::new(me.clone())),
                    clock: sandbox_clock,
                    bus: Some(sandbox_bus),
                },
                crate::sandbox::SandboxOptions::default(),
            ),
            sqlite,
            logs,
            duckdb,
            security,
            symbols,
            brokers: opts.brokers,
            websocket,
            bus,
            ui,
            clock: opts.clock,
            config: RwLock::new(config),
            sessions: WebSessionStore::new(),
            limiter: RateLimiter::new(),
            api_keys: ApiKeyCache::new(),
            broker_session: RwLock::new(None),
            server_status: RwLock::new(ServerStatus::Starting),
            http,
            shutdown: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
            data_dir: data_dir.to_path_buf(),
            monitor: crate::services::monitor::Monitor::new(),
            messaging: crate::messaging::Messaging::new(),
        });
        crate::messaging::register(&ctx);
        crate::strategy::register(&ctx);
        // Analyzer mode survives restarts: resume the sandbox engine.
        if ctx.sqlite.get_analyze_mode().unwrap_or(false) {
            crate::services::analyzer_service::AnalyzerService::spawn_engine_transition(&ctx, true);
        }
        Ok(ctx)
    }

    /// Production defaults: OS keychain, system clock, every broker adapter.
    pub fn open_default(data_dir: &Path) -> Result<Arc<Self>> {
        Self::open(
            data_dir,
            OpenOptions {
                keystore: Arc::new(crate::security::keystore::KeyringStore::new()),
                clock: Arc::new(SystemClock),
                brokers: Arc::new(BrokerRegistry::new()),
            },
        )
    }

    /// Spawn a background task owned by the context; aborted on shutdown.
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.tasks.lock();
        // Reap finished tasks so the set does not grow with one-shot jobs.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    pub fn task_count(&self) -> usize {
        self.tasks.lock().len()
    }

    /// Stop background work: cancel, drain the bus, abort owned tasks,
    /// close the market feed.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.messaging.shutdown().await;
        self.strategy.shutdown().await;
        self.sandbox.shutdown().await;
        self.bus.shutdown(Duration::from_secs(2)).await;
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = self.websocket.disconnect().await;
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn server_config(&self) -> ServerConfig {
        self.config.read().clone()
    }

    pub fn reload_config(&self) -> Result<ServerConfig> {
        let c = {
            let conn = self.sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        *self.config.write() = c.clone();
        Ok(c)
    }

    /// Port the listener is actually bound to (falls back to the setting).
    pub fn listening_port(&self) -> u16 {
        match &*self.server_status.read() {
            ServerStatus::Running { port, .. } => *port,
            _ => self.config.read().http_port,
        }
    }

    /// Broker connected and its token still inside today's session.
    pub fn is_broker_connected(&self) -> bool {
        self.get_broker_session().is_some()
    }

    /// The live broker session, or `None` once it has crossed the daily
    /// boundary (it is then dropped from memory; the scheduler revokes the
    /// stored row).
    pub fn get_broker_session(&self) -> Option<BrokerSession> {
        let s = self.broker_session.read().clone()?;
        let cfg = self.config.read();
        if crate::session::boundary::is_fresh(
            s.authenticated_at,
            self.clock.now(),
            cfg.session_expiry_hour,
            cfg.session_expiry_minute,
        ) {
            Some(s)
        } else {
            drop(cfg);
            *self.broker_session.write() = None;
            None
        }
    }

    pub fn set_broker_session(&self, session: Option<BrokerSession>) {
        *self.broker_session.write() = session;
    }

    /// The signed-in OpenAlgo user, if any (single-user app).
    pub fn signed_in_user(&self) -> Option<String> {
        self.sessions.signed_in_user()
    }

    /// Master row by exchange and broker token.
    pub fn get_symbol_by_token(&self, exchange: &str, token: &str) -> Option<SymbolInfo> {
        self.symbols.by_token(exchange, token)
    }

    /// Master row by exchange and OpenAlgo symbol.
    pub fn get_symbol_by_name(&self, exchange: &str, symbol: &str) -> Option<SymbolInfo> {
        self.symbols.by_symbol(exchange, symbol)
    }

    /// Broker token by exchange and OpenAlgo symbol.
    pub fn get_token_by_symbol(&self, exchange: &str, symbol: &str) -> Option<String> {
        self.symbols.token(symbol, exchange)
    }

    pub fn symbol_exists(&self, exchange: &str, symbol: &str) -> bool {
        self.symbols.by_symbol(exchange, symbol).is_some()
    }

    /// Instruments in the loaded master.
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Replace the master with a new generation (bounded by its size).
    pub fn load_symbol_cache(&self, symbols: Vec<SymbolInfo>) {
        self.symbols.load(symbols);
    }

    /// Drop the master (logout).
    pub fn clear_symbol_cache(&self) {
        self.symbols.clear();
    }

    /// Every instrument on one exchange.
    pub fn get_symbols_by_exchange(&self, exchange: &str) -> Vec<SymbolInfo> {
        self.symbols
            .snapshot()
            .rows()
            .iter()
            .filter(|s| s.exchange.eq_ignore_ascii_case(exchange))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
pub mod testing {
    //! Context factory for unit and HTTP tests: temp dir, memory keystore,
    //! manual clock, chosen broker adapters.
    use super::*;
    use crate::clock::ManualClock;
    use crate::security::keystore::MemoryKeyStore;

    pub struct TestCtx {
        pub ctx: Arc<AppState>,
        pub clock: Arc<ManualClock>,
        pub dir: tempfile::TempDir,
    }

    pub fn build(brokers: BrokerRegistry, now: DateTime<Utc>) -> TestCtx {
        let dir = tempfile::tempdir().expect("tempdir");
        let clock = ManualClock::new(now);
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: clock.clone(),
                brokers: Arc::new(brokers),
            },
        )
        .expect("open context");
        TestCtx { ctx, clock, dir }
    }
}
