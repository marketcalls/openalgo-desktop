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
    /// Listener ports fixed for this context's lifetime over the stored
    /// settings and the development override (tests on ephemeral ports).
    port_pin: RwLock<Option<(u16, u16)>>,
    pub sessions: WebSessionStore,
    pub limiter: RateLimiter,
    pub api_keys: ApiKeyCache,
    pub broker_session: RwLock<Option<BrokerSession>>,
    /// The account the connected session trades through, worked out once
    /// for it (`services::session_account`, NEW-TR-01).
    pub session_account: crate::services::session_account::AccountCache,
    /// A broker session ended in memory whose stored row is still to be
    /// revoked (the database write failed), and which rows (every one, or
    /// the daily boundary's). While set, no stored session is resumed; the
    /// session poll and the next sign-in finish the revoke
    /// (`BrokerAuthService::retry_pending_revoke`).
    pub revoke_pending: Mutex<Option<crate::services::broker_auth_service::PendingRevoke>>,
    /// One broker is logged in at a time. Completing a sign-in (which ends
    /// any other broker's session first), resuming one, ending one and
    /// forgetting the logged-in broker run under this lock, so two of them
    /// never interleave (`BrokerAuthService::session_lock`).
    pub session_switch: tokio::sync::Mutex<()>,
    /// What still depends on the logged-in broker's Live session, asked
    /// before any switch ends it (`services::broker_switch`).
    pub switch_guard: RwLock<Arc<dyn crate::services::broker_switch::SwitchGuard>>,
    pub server_status: RwLock<ServerStatus>,
    /// State of the 8765 market data listener (written by `FeedService`),
    /// shown with the fix when its port is taken.
    pub feed_status: RwLock<ServerStatus>,
    /// The feed server's source over the broker managers.
    pub bridge: Arc<crate::feed::bridge::BrokerBridge>,
    /// Broker session streaming: master contract, feeds, order updates.
    pub runtime: crate::services::broker_runtime::BrokerRuntime,
    me: std::sync::Weak<AppState>,
    /// Shared outbound HTTP client (explicit timeouts) for non-broker calls.
    pub http: reqwest::Client,
    pub shutdown: CancellationToken,
    tasks: Mutex<JoinSet<()>>,
    /// The symbol master, shared with every broker adapter in `brokers`.
    pub symbols: SymbolResolver,
    /// Freeze limits: the trader's entries over NSE's built-in table.
    pub freeze: crate::services::freeze_qty_service::FreezeLimits,
    pub data_dir: PathBuf,
    /// Traffic, latency and security monitoring (bounded queue + writer).
    pub monitor: crate::services::monitor::Monitor,
    /// The Latency page's report, one computation for concurrent requests.
    pub latency_reports: crate::services::report_flight::ReportFlight,
    /// The Traffic page's statistics, one computation for concurrent requests.
    pub traffic_reports: crate::services::report_flight::ReportFlight,
    /// The log reports built at once (`AppState::report`).
    pub reports: crate::services::report_gate::ReportGate,
    /// The Action Center's answer per user and status, one read for the
    /// reloads a burst of order events asks for at once (each answer read
    /// after it was asked).
    pub action_center_reads:
        crate::services::report_flight::KeyedFlight<(String, Option<String>), axum::body::Bytes>,
    /// The sandbox (analyzer mode) engine, in its own `sandbox.db`.
    pub sandbox: crate::sandbox::Sandbox,
    /// Telegram and WhatsApp bots and their alerts.
    pub messaging: crate::messaging::Messaging,
    /// Historify: the DuckDB history store, download jobs and schedules.
    pub historify: Arc<crate::historify::Historify>,
    /// The strategy module and RMS (`/strategy`).
    pub strategy: Arc<crate::strategy::StrategyModule>,
    /// The /trading terminal: custom indicators, OpenScript files, the runner.
    pub trading: crate::trading::Trading,
    /// The scalping terminal backend and its risk monitor (`/scalping`).
    pub scalping: Arc<crate::scalping::Scalping>,
    /// Settles open and unconfirmed orders against the broker's order book
    /// (LOG-08, ARCH-01).
    pub reconciler: Arc<crate::services::order_reconciler::OrderReconciler>,
    /// Chartink strategies and their webhook (`/chartink`).
    pub chartink: Arc<crate::chartink::Chartink>,
    /// MCP transport state: per-token rate windows and event-stream slots.
    pub mcp: crate::mcp::McpRuntime,
    /// In-app updates: the check, the offer, the download and the restart
    /// decision (`services::update`).
    pub updates: Arc<crate::services::update::UpdateService>,
    /// The built-in ngrok tunnel (`services::tunnel`).
    pub tunnel: crate::services::tunnel::Tunnel,
}

/// Health alert type raised when Historify had to drop an unreplayable WAL.
pub const HISTORIFY_RECOVERY_ALERT: &str = "historify_wal_recovered";

/// Tell the trader, on the health page, that unsaved Historify changes from
/// the last session were lost. The detail is already in the log.
pub fn report_historify_recovery(logs: &LogsDb, now: DateTime<Utc>) {
    let raised = logs.conn().and_then(|c| {
        crate::db::sqlite::monitor::raise_alert(
            &c,
            &crate::db::sqlite::monitor::AlertRow {
                alert_type: HISTORIFY_RECOVERY_ALERT.into(),
                severity: "warn".into(),
                metric_name: "historify_recovery".into(),
                message: crate::db::duckdb::WAL_RECOVERED.into(),
                ..Default::default()
            },
            now,
        )
    });
    if let Err(e) = raised {
        tracing::warn!("Could not record the Historify recovery alert: {}", e);
    }
}

pub struct OpenOptions {
    pub keystore: Arc<dyn KeyStore>,
    pub clock: Arc<dyn Clock>,
    pub brokers: Arc<BrokerRegistry>,
}

impl OpenOptions {
    /// The installed app's: OS keychain, system clock, every broker adapter.
    pub fn production() -> Self {
        Self {
            keystore: Arc::new(crate::security::keystore::KeyringStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(BrokerRegistry::new()),
        }
    }
}

/// What [`AppState::open`] opens first, in this order: the data folder's
/// keys and databases, migrated, and the server settings.
struct Stores {
    security: Arc<SecurityManager>,
    sqlite: Arc<SqliteDb>,
    logs: Arc<LogsDb>,
    duckdb: Arc<DuckDb>,
    config: ServerConfig,
}

impl Stores {
    fn open(data_dir: &Path, opts: &OpenOptions) -> Result<Self> {
        crate::security::fsperm::ensure_private_dir(data_dir)?;
        let security = Arc::new(SecurityManager::open(data_dir, opts.keystore.clone())?);
        let sqlite = Arc::new(SqliteDb::new(&data_dir.join("openalgo.db"))?);
        let logs = Arc::new(LogsDb::new(&data_dir.join("logs.db"))?);
        {
            let main = sqlite.conn()?;
            logs.import_from_main(&main)?;
        }
        let duck_path = data_dir.join("historify.duckdb");
        let duckdb = Arc::new(DuckDb::new(&duck_path)?);
        if duckdb.recovery().is_some() {
            report_historify_recovery(&logs, opts.clock.now());
        }
        crate::security::fsperm::restrict_db_files(&duck_path)?;
        crate::db::sqlite::data_migrations::run(&sqlite, &security)?;
        let config = {
            let conn = sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        Ok(Self {
            security,
            sqlite,
            logs,
            duckdb,
            config,
        })
    }
}

/// Everything [`AppState::assemble`] builds the context from.
struct Parts<'a> {
    data_dir: &'a Path,
    opts: OpenOptions,
    stores: Stores,
    ui: Arc<SocketEmitter>,
    bus: Arc<EventBus>,
    http: reqwest::Client,
    sandbox_db: Arc<crate::sandbox::SandboxDb>,
    symbols: SymbolResolver,
    freeze: crate::services::freeze_qty_service::FreezeLimits,
    websocket: Arc<WebSocketManager>,
    runtime: crate::services::broker_runtime::BrokerRuntime,
    bridge: Arc<crate::feed::bridge::BrokerBridge>,
}

impl Parts<'_> {
    fn historify(&self, me: &std::sync::Weak<AppState>) -> Arc<crate::historify::Historify> {
        Arc::new(crate::historify::Historify::new(
            (*self.stores.duckdb).clone(),
            Arc::new(crate::historify::source::BrokerSource::new(me.clone())),
            self.bus.clone(),
            self.opts.clock.clone(),
            &self.data_dir.join("historify_work"),
            crate::historify::jobs::EngineConfig::default(),
        ))
    }

    fn strategy(&self, me: &std::sync::Weak<AppState>) -> Arc<crate::strategy::StrategyModule> {
        crate::strategy::StrategyModule::new(crate::strategy::Deps {
            db: self.stores.sqlite.clone(),
            gateway: Arc::new(crate::strategy::dispatch::AppGateway::new(me.clone())),
            rooms: Arc::new(crate::strategy::broadcast::SocketRooms::new(
                self.ui.clone(),
            )),
            clock: self.opts.clock.clone(),
            symbols: self.symbols.clone(),
            session_hour: self.stores.config.session_expiry_hour,
            session_minute: self.stores.config.session_expiry_minute,
            prices: Some(Arc::new(crate::strategy::tick_feed::FeedPrices::new(
                me.clone(),
                self.websocket.clone(),
                crate::websocket::Reader::Strategy,
            ))),
        })
    }

    fn scalping(&self, me: &std::sync::Weak<AppState>) -> Arc<crate::scalping::Scalping> {
        crate::scalping::Scalping::new(crate::scalping::MonitorDeps {
            store: crate::scalping::Store::new(self.stores.sqlite.clone()),
            gateway: Arc::new(crate::strategy::dispatch::AppGateway::new(me.clone())),
            prices: Some(Arc::new(crate::strategy::tick_feed::FeedPrices::new(
                me.clone(),
                self.websocket.clone(),
                crate::websocket::Reader::Scalping,
            ))),
            ui: self.ui.clone(),
            clock: self.opts.clock.clone(),
            symbols: self.symbols.clone(),
            freeze: self.freeze.clone(),
        })
    }

    fn sandbox(&self, me: &std::sync::Weak<AppState>) -> crate::sandbox::Sandbox {
        crate::sandbox::Sandbox::with_db(
            self.sandbox_db.clone(),
            crate::sandbox::SandboxDeps {
                symbols: Arc::new(crate::services::sandbox_feed::MasterSymbols(
                    self.symbols.clone(),
                )),
                quotes: Arc::new(crate::services::sandbox_feed::LiveQuotes::new(me.clone())),
                clock: self.opts.clock.clone(),
                bus: Some(self.bus.clone()),
            },
            crate::sandbox::SandboxOptions {
                trading_day: crate::services::market_calendar_service::sandbox_trading_day(
                    me.clone(),
                ),
                ..crate::sandbox::SandboxOptions::default()
            },
        )
    }
}

impl AppState {
    /// Open everything under `data_dir`. Must run inside a Tokio runtime
    /// (subscribers are spawned here).
    pub fn open(data_dir: &Path, opts: OpenOptions) -> Result<Arc<Self>> {
        let stores = Stores::open(data_dir, &opts)?;
        let ui = Arc::new(SocketEmitter::default());
        let bus = Arc::new(EventBus::new());
        crate::events::subscribers::register_all(
            &bus,
            stores.logs.clone(),
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
        let freeze = crate::services::freeze_qty_service::FreezeLimits::loaded(&stores.sqlite);
        let websocket = Arc::new(WebSocketManager::new());
        let runtime = crate::services::broker_runtime::BrokerRuntime::new();
        let bridge = crate::feed::bridge::BrokerBridge::with_depth_manager(
            websocket.clone(),
            runtime.depth_ws.clone(),
            symbols.clone(),
        );
        let parts = Parts {
            data_dir,
            opts,
            stores,
            ui,
            bus,
            http,
            sandbox_db,
            symbols,
            freeze,
            websocket,
            runtime,
            bridge,
        };
        let ctx = Arc::new_cyclic(|me: &std::sync::Weak<Self>| Self::assemble(me, parts));
        ctx.start_services();
        Ok(ctx)
    }

    /// The context from its opened parts. `me` is the context's own weak
    /// reference, which the modules that call back into it hold.
    fn assemble(me: &std::sync::Weak<Self>, p: Parts<'_>) -> Self {
        let historify = p.historify(me);
        let strategy = p.strategy(me);
        let trading = crate::trading::Trading::new(
            p.data_dir,
            p.stores.sqlite.clone(),
            p.opts.clock.clone(),
            Arc::new(crate::trading::runner::services::AppServices::new(
                me.clone(),
            )),
        );
        let scalping = p.scalping(me);
        let reconciler = crate::services::order_reconciler::OrderReconciler::new(
            Arc::new(crate::strategy::dispatch::AppGateway::new(me.clone())),
            p.opts.clock.clone(),
            Some(p.bus.clone()),
        );
        let chartink = crate::chartink::Chartink::new(
            me.clone(),
            p.stores.sqlite.clone(),
            p.opts.clock.clone(),
        );
        let sandbox = p.sandbox(me);
        let Parts {
            data_dir,
            opts,
            stores,
            ui,
            bus,
            http,
            symbols,
            freeze,
            websocket,
            runtime,
            bridge,
            ..
        } = p;
        Self {
            historify,
            strategy,
            trading,
            scalping,
            reconciler,
            chartink,
            sandbox,
            sqlite: stores.sqlite,
            logs: stores.logs,
            duckdb: stores.duckdb,
            security: stores.security,
            symbols,
            freeze,
            brokers: opts.brokers,
            websocket,
            bridge,
            runtime,
            me: me.clone(),
            bus,
            ui,
            clock: opts.clock,
            config: RwLock::new(stores.config),
            port_pin: RwLock::new(None),
            sessions: WebSessionStore::new(),
            limiter: RateLimiter::new(),
            api_keys: ApiKeyCache::new(),
            broker_session: RwLock::new(None),
            session_account: Default::default(),
            revoke_pending: Mutex::new(None),
            session_switch: tokio::sync::Mutex::new(()),
            switch_guard: RwLock::new(Arc::new(crate::services::obligations::OutcomesGuard)),
            server_status: RwLock::new(ServerStatus::Starting),
            feed_status: RwLock::new(ServerStatus::Starting),
            http,
            shutdown: CancellationToken::new(),
            tasks: Mutex::new(JoinSet::new()),
            data_dir: data_dir.to_path_buf(),
            monitor: crate::services::monitor::Monitor::new(),
            latency_reports: Default::default(),
            traffic_reports: Default::default(),
            reports: Default::default(),
            action_center_reads: crate::services::report_flight::KeyedFlight::fresh(
                "the Action Center",
            ),
            messaging: crate::messaging::Messaging::new(),
            mcp: crate::mcp::McpRuntime::new(),
            updates: crate::services::update::UpdateService::new(env!("CARGO_PKG_VERSION")),
            tunnel: Default::default(),
        }
    }

    /// Register the subscribers and the reconciler's order owners, start
    /// the background services, and resume the sandbox engine when
    /// analyzer mode was on.
    fn start_services(self: &Arc<Self>) {
        let ctx = self;
        crate::messaging::register(ctx);
        ctx.historify.start();
        crate::strategy::register(ctx);
        crate::trading::register(ctx);
        crate::scalping::register(&ctx.bus, &ctx.scalping);
        // One reconciler over every owner of orders (LOG-08, ARCH-01). A gap
        // in order facts pauses new strategy entries until it is repaired.
        ctx.reconciler
            .register(Arc::new(crate::strategy::reconcile::StrategyOrders(
                Arc::downgrade(&ctx.strategy),
            )));
        ctx.reconciler
            .register(Arc::new(crate::scalping::monitor::ScalpingOrders(
                Arc::downgrade(&ctx.scalping.monitor),
            )));
        ctx.reconciler
            .register(Arc::new(crate::trading::runner::reconcile::RunnerOrders(
                Arc::downgrade(&ctx.trading.runner),
            )));
        ctx.reconciler
            .register(Arc::new(crate::scalping::unconfirmed::ScalpingOrders::new(
                Arc::downgrade(ctx),
            )));
        ctx.strategy.set_entry_gate(ctx.reconciler.clone());
        let alerts = Arc::downgrade(ctx);
        ctx.reconciler.set_alert_sink(Arc::new(move |a| {
            if let Some(ctx) = alerts.upgrade() {
                crate::services::health_service::order_facts_alert(&ctx, a);
            }
        }));
        // A scalping stop that cannot be judged safely is left in place and
        // the trader told on the Health page.
        let alerts = Arc::downgrade(ctx);
        ctx.scalping
            .monitor
            .set_alert_sink(Arc::new(move |message: String| {
                if let Some(ctx) = alerts.upgrade() {
                    crate::services::health_service::scalping_alert(&ctx, message);
                }
            }));
        ctx.reconciler
            .watch_feed(ctx.runtime.order_ws.watch_status());
        ctx.reconciler.watch_feed(ctx.websocket.watch_status());
        crate::services::order_reconciler::register(&ctx.bus, &ctx.reconciler);
        ctx.reconciler.start();
        crate::services::broker_config_service::BrokerConfigService::apply_rate_plans(ctx);
        crate::services::request_budget_service::restore(ctx);
        crate::services::request_budget_service::start(ctx);
        // Analyzer mode survives restarts: resume the sandbox engine.
        if ctx.sqlite.get_analyze_mode().unwrap_or(false) {
            crate::services::analyzer_service::AnalyzerService::spawn_resume(ctx);
        }
    }

    /// Open the data folder as version `version` of the app (REL-02):
    /// refuse a folder written by a newer version, back up one from an
    /// older version before the migrations run, put the backup back if
    /// opening then fails, and record `version` once it opened
    /// (`db::upgrade`).
    pub fn open_upgrading(
        data_dir: &Path,
        opts: OpenOptions,
        version: &str,
    ) -> std::result::Result<Arc<Self>, crate::db::upgrade::OpenError> {
        let now = opts.clock.now();
        crate::db::upgrade::open_data(data_dir, version, now, || Self::open(data_dir, opts))
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

    /// Run synchronous database work on the blocking pool, so a slow query
    /// or a database busy with a long write never holds an async worker
    /// (DB-01; four of them on a Raspberry Pi).
    pub async fn blocking<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&AppState) -> Result<T> + Send + 'static,
    {
        let ctx = self.clone();
        tokio::task::spawn_blocking(move || f(&ctx))
            .await
            .map_err(|e| crate::error::AppError::Internal(format!("Blocking task failed: {}", e)))?
    }

    /// The context as an `Arc` (for work that outlives a call).
    pub fn arc(&self) -> Option<Arc<Self>> {
        self.me.upgrade()
    }

    /// Stop background work: cancel, stop the broker session's streaming
    /// (feeds, order updates, adapter pollers), drain the bus, abort owned
    /// tasks.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tunnel
            .stop(crate::services::tunnel::Off::SignedOut)
            .await;
        self.reconciler.stop();
        self.messaging.shutdown().await;
        self.trading.shutdown().await;
        self.strategy.shutdown().await;
        self.scalping.shutdown().await;
        self.chartink.shutdown().await;
        self.historify.shutdown().await;
        self.runtime.teardown(self).await;
        self.sandbox.shutdown().await;
        self.bus.shutdown(Duration::from_secs(2)).await;
        // The monitor's writer saves what was queued when shutdown began.
        self.monitor.stop(Duration::from_secs(3)).await;
        if let Err(e) = crate::services::request_budget_service::save(self) {
            tracing::warn!("Broker request counts could not be saved at quit: {}", e);
        }
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        // Quitting wipes the keys like a logout (KEY-01).
        self.security.lock();
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub fn server_config(&self) -> ServerConfig {
        self.config.read().clone()
    }

    pub fn reload_config(&self) -> Result<ServerConfig> {
        let mut c = {
            let conn = self.sqlite.conn()?;
            ServerConfig::load(&conn)?
        };
        if let Some((http_port, ws_port)) = *self.port_pin.read() {
            c.http_port = http_port;
            c.ws_port = ws_port;
        }
        *self.config.write() = c.clone();
        Ok(c)
    }

    /// Fix the HTTP and WebSocket listener ports for this context, over
    /// the stored settings and the development override, through every
    /// settings reload (a broker sign-in reloads them, and the feed server's
    /// watcher follows the result). For tests on ephemeral ports, which must
    /// never take 5000/8765 or the development ports; the app never calls
    /// it.
    #[cfg(feature = "test-support")]
    pub fn pin_listener_ports(&self, http_port: u16, ws_port: u16) {
        *self.port_pin.write() = Some((http_port, ws_port));
        let mut c = self.config.write();
        c.http_port = http_port;
        c.ws_port = ws_port;
        c.bind_host = "127.0.0.1".into();
    }

    /// Port the listener is actually bound to (falls back to the setting).
    pub fn listening_port(&self) -> u16 {
        match &*self.server_status.read() {
            ServerStatus::Running { port, .. } => *port,
            _ => self.config.read().http_port,
        }
    }

    /// The port the HTTP listener is bound to right now, from the bound
    /// socket itself; 0 before the first bind, after a failed bind and once
    /// the listener stops. What the app window's trust checks read, so they
    /// fail closed whenever no listener of this app holds the port.
    pub fn live_port(&self) -> u16 {
        match &*self.server_status.read() {
            ServerStatus::Running { port, .. } => *port,
            _ => 0,
        }
    }

    /// Broker connected and its token still inside today's session.
    pub fn is_broker_connected(&self) -> bool {
        self.get_broker_session().is_some()
    }

    /// The live broker session, or `None` once it has crossed the daily
    /// boundary (it is then dropped from memory; the scheduler revokes the
    /// stored row). A continuous (crypto) session has no daily boundary
    /// (SES-01).
    pub fn get_broker_session(&self) -> Option<BrokerSession> {
        let s = self.broker_session.read().clone()?;
        let continuous = !crate::brokers::catalog::has_daily_boundary(&s.broker_id);
        let fresh = {
            let cfg = self.config.read();
            crate::session::boundary::is_fresh(
                s.authenticated_at,
                self.clock.now(),
                cfg.session_expiry_hour,
                cfg.session_expiry_minute,
            )
        };
        if continuous || fresh {
            return Some(s);
        }
        // Clear only the session this read found stale: a sign-in that set a
        // new one in between keeps it (KEY-01 cache rules: a writer never
        // undoes a newer value).
        let mut live = self.broker_session.write();
        if live
            .as_ref()
            .is_some_and(|l| l.broker_id == s.broker_id && l.authenticated_at == s.authenticated_at)
        {
            *live = None;
        }
        None
    }

    pub fn set_broker_session(&self, session: Option<BrokerSession>) {
        *self.broker_session.write() = session;
    }

    /// The signed-in OpenAlgo user, if any (single-user app).
    pub fn signed_in_user(&self) -> Option<String> {
        self.sessions.signed_in_user()
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

#[cfg(test)]
mod recovery_alert_tests {
    use super::*;

    #[test]
    fn historify_recovery_is_raised_as_a_health_alert_in_trader_words() {
        let dir = tempfile::tempdir().unwrap();
        let logs = LogsDb::new(&dir.path().join("logs.db")).unwrap();
        report_historify_recovery(&logs, Utc::now());
        report_historify_recovery(&logs, Utc::now());
        let c = logs.conn().unwrap();
        let alerts = crate::db::sqlite::monitor::active_alerts(&c).unwrap();
        let ours: Vec<_> = alerts
            .iter()
            .filter(|a| a.alert_type == HISTORIFY_RECOVERY_ALERT)
            .collect();
        assert_eq!(ours.len(), 1, "a repeat refreshes the open alert");
        assert_eq!(ours[0].message, crate::db::duckdb::WAL_RECOVERED);
        for word in ["WAL", "DuckDB", "replay", "rror"] {
            assert!(!ours[0].message.contains(word), "{}", word);
        }
    }
}
