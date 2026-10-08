//! The OpenScript live runner (web `services/openscript_runner_service.py`
//! and `blueprints/openscript_runner.py`).
//!
//! **Where the engine runs.** A live run executes the compiled program on the
//! same TypeScript engine (`openalgo-script`) the chart and the backtest use,
//! in a runner page the shell opens for the run (a hidden Tauri window in the
//! app, see `host.rs`). Backtest and live therefore run one engine and cannot
//! drift. The page holds no API key and no session: it is handed a per-run
//! secret in its URL fragment and talks only to the runner's host routes,
//! which serve it the program, its bars and its order frames, and take its
//! order intents.
//!
//! **What Rust owns.** Everything that decides: the registry of runs and its
//! claims (start, pause, close, remove, one at a time per deployment, claimed
//! under the lock that checks), the side a run trades on (fixed at start; a
//! live run's orders carry `force_live`), turning intents into orders on the
//! platform's own order path tagged with the deployment's id, the
//! per-deployment books, closing a position on Stop (from the deployment's
//! own fills, and a close that did not happen leaves the run open and
//! managed), the IST schedules, and teardown on logout and shutdown.
//!
//! **Every task is owned.** One pump per run (order frames, new bars, the
//! page's heartbeat) kept in the run's entry and aborted when the run ends,
//! and one scheduler task kept here and aborted at shutdown. A page that
//! stops answering is noticed by the pump and its run is ended, so a run is
//! never reported running with nothing behind it.

pub mod books;
pub mod host;
pub mod inbox;
pub mod orders;
pub mod schedule;
pub mod services;
pub mod store;
#[cfg(feature = "runner-window")]
pub mod window;

use crate::clock::Clock;
use crate::db::sqlite::SqliteDb;
use crate::strategy::dispatch::RunMode;
use crate::trading::names::{is_script_name, names_something};
use crate::trading::scripts::ScriptStore;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use chrono_tz::Asia::Kolkata;
use host::{Launch, RunnerHost};
use inbox::{Inbox, Message};
use orders::{Planned, Target};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use services::RunnerServices;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// What a start is told once shutdown has begun.
pub const SHUTTING_DOWN_MESSAGE: &str =
    "The server is shutting down, so this strategy was not started";
/// What a Pause is told while a Stop is closing the position.
pub const CLOSING_MESSAGE: &str = "This strategy is closing its position. Wait for it to finish.";
/// What removing a running deployment is told.
pub const RUNNING_DELETE_REFUSAL: &str = "This strategy is running, so it has not been removed. Pause it to keep its position, or Stop it to close the position first, then remove it.";
/// What removing a deployment is told while it is busy.
pub const BUSY_DELETE_REFUSAL: &str =
    "This strategy is still being stopped or removed. Try again in a moment.";
/// What a start is told when this copy of the app cannot open runner pages.
pub const NO_HOST_MESSAGE: &str = "Strategies cannot be run in this copy of OpenAlgo because it has no window to run them in. Start OpenAlgo Desktop normally and try again.";
/// A Stop whose exit did not complete in time.
pub const CLOSE_TIMEOUT_MESSAGE: &str = "This strategy did not close its position in time, so it is still running and still holding it. Its own log says what happened. Deal with the position and stop it again, or use Pause to end the strategy and keep the position.";
/// The most log lines one write takes, and the largest a run's log grows.
const MAX_LOG_LINES: usize = 200;
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// The most logs one status answer names.
pub const MAX_LOGS_REPORTED: usize = 20;

/// Timings, injectable so tests do not wait.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    /// A page silent this long has stopped and its run is ended.
    pub heartbeat_timeout: Duration,
    /// How often open orders are read back.
    pub order_poll: Duration,
    /// How often history is read for new bars.
    pub bar_poll: Duration,
    /// How long a Stop waits for its exit to fill, and how often it looks.
    pub close_wait: Duration,
    pub close_look: Duration,
    /// The longest a page's read of its inbox is held open.
    pub inbox_wait: Duration,
    /// How often schedules are checked.
    pub schedule_tick: Duration,
    /// Days of history a run is replayed over before it sends anything.
    pub history_days: i64,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            heartbeat_timeout: Duration::from_secs(90),
            order_poll: Duration::from_secs(2),
            bar_poll: Duration::from_secs(10),
            close_wait: Duration::from_secs(25),
            close_look: Duration::from_millis(500),
            inbox_wait: Duration::from_secs(20),
            schedule_tick: Duration::from_secs(15),
            history_days: 5,
        }
    }
}

/// One run, as fixed when it started.
#[derive(Debug, Clone)]
pub struct RunInfo {
    pub run_id: String,
    pub script: String,
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub product: String,
    pub mode: RunMode,
    pub started_at: DateTime<Utc>,
    pub log_file: PathBuf,
}

impl RunInfo {
    /// One run as the routes report it (web `_run_answer`). `state` is always
    /// running: a run that has ended is not in the registry at all. There is
    /// no process, so `pid` is null.
    pub fn answer(&self) -> Value {
        json!({
            "id": self.run_id,
            "deployment": self.run_id,
            "file": self.script,
            "state": "running",
            "symbol": self.symbol,
            "exchange": self.exchange,
            "interval": self.interval,
            "product": self.product,
            "mode": self.mode.as_str(),
            "pid": Value::Null,
            "started_at": self.started_at.with_timezone(&Kolkata).to_rfc3339(),
            "log": self.log_file.to_string_lossy(),
        })
    }
}

struct RunEntry {
    info: RunInfo,
    token: String,
    inbox: Inbox,
    halted: AtomicBool,
    last_seen: Mutex<Instant>,
    /// Bars at or after this open time are pushed (set by the page's first
    /// history read), and the last bar pushed.
    anchor: Mutex<Option<i64>>,
    last_bar: Mutex<Option<Value>>,
    pump: Mutex<Option<JoinHandle<()>>>,
    cancel: CancellationToken,
    /// Intents and a close never interleave for one run.
    order_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Registry {
    runs: HashMap<String, Arc<RunEntry>>,
    starting: HashSet<String>,
    stopping: HashSet<String>,
    closing: HashSet<String>,
    deleting: HashSet<String>,
}

/// A refusal with the status the routes answer it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub message: String,
}

impl Refusal {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

pub struct Runner {
    me: Weak<Runner>,
    db: Arc<SqliteDb>,
    scripts: Arc<ScriptStore>,
    logs_dir: PathBuf,
    clock: Arc<dyn Clock>,
    services: RwLock<Arc<dyn RunnerServices>>,
    host: RwLock<Option<Arc<dyn RunnerHost>>>,
    options: RwLock<RunnerOptions>,
    reg: Mutex<Registry>,
    shutting_down: AtomicBool,
    scheduler: Mutex<Option<JoinHandle<()>>>,
    last_tick: Mutex<Option<DateTime<Utc>>>,
    cancel: CancellationToken,
}

fn random_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

impl Runner {
    pub fn new(
        db: Arc<SqliteDb>,
        scripts: Arc<ScriptStore>,
        logs_dir: PathBuf,
        clock: Arc<dyn Clock>,
        services: Arc<dyn RunnerServices>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            db,
            scripts,
            logs_dir,
            clock,
            services: RwLock::new(services),
            host: RwLock::new(None),
            options: RwLock::new(RunnerOptions::default()),
            reg: Mutex::new(Registry::default()),
            shutting_down: AtomicBool::new(false),
            scheduler: Mutex::new(None),
            last_tick: Mutex::new(None),
            cancel: CancellationToken::new(),
        })
    }

    /// The shell's host for runner pages (the hidden window in the app).
    pub fn set_host(&self, host: Arc<dyn RunnerHost>) {
        *self.host.write() = Some(host);
    }

    /// Replace the services (tests inject a fake).
    pub fn set_services(&self, services: Arc<dyn RunnerServices>) {
        *self.services.write() = services;
    }

    pub fn set_options(&self, options: RunnerOptions) {
        *self.options.write() = options;
    }

    pub fn options(&self) -> RunnerOptions {
        self.options.read().clone()
    }

    fn services(&self) -> Arc<dyn RunnerServices> {
        self.services.read().clone()
    }

    /// The services in force (the book routes read through them).
    pub fn services_handle(&self) -> Arc<dyn RunnerServices> {
        self.services()
    }

    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    // ------------------------------------------------------------- reading

    /// A deployment id for whatever a caller named: the script's single
    /// deployment, or the script's own id when it has none.
    pub fn as_run_id(&self, given: &str) -> String {
        if is_script_name(given) {
            if let Ok(conn) = self.db.conn() {
                if let Ok(Some(d)) = store::read(&conn, given) {
                    return d.deployment;
                }
            }
            return store::deployment_id(given, "", "", "", "");
        }
        given.to_string()
    }

    /// The run of a deployment (or of a script deployed once), if running.
    pub fn status_of(&self, given: &str) -> Option<RunInfo> {
        let id = self.as_run_id(given);
        self.reg.lock().runs.get(&id).map(|e| e.info.clone())
    }

    pub fn is_running(&self, given: &str) -> bool {
        self.status_of(given).is_some()
    }

    pub fn is_starting(&self, given: &str) -> bool {
        let id = self.as_run_id(given);
        self.reg.lock().starting.contains(&id)
    }

    /// Every run, ordered by script.
    pub fn running(&self) -> Vec<RunInfo> {
        let mut all: Vec<RunInfo> = self
            .reg
            .lock()
            .runs
            .values()
            .map(|e| e.info.clone())
            .collect();
        all.sort_by(|a, b| (&a.script, &a.run_id).cmp(&(&b.script, &b.run_id)));
        all
    }

    /// How many pump tasks are alive (hygiene).
    pub fn task_count(&self) -> usize {
        let pumps = self
            .reg
            .lock()
            .runs
            .values()
            .filter(|e| e.pump.lock().as_ref().is_some_and(|h| !h.is_finished()))
            .count();
        let sched = self
            .scheduler
            .lock()
            .as_ref()
            .is_some_and(|h| !h.is_finished());
        pumps + usize::from(sched)
    }

    /// How many runner pages the host has open.
    pub fn open_pages(&self) -> usize {
        self.host.read().as_ref().map_or(0, |h| h.open_count())
    }

    /// The names of a run's recent logs, newest first.
    pub fn logs_for(&self, run_id: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.logs_dir) else {
            return Vec::new();
        };
        let prefix = format!("{}_", run_id);
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(&prefix) && n.ends_with(".log"))
            .collect();
        names.sort_by(|a, b| b.cmp(a));
        names.truncate(MAX_LOGS_REPORTED);
        names
    }

    /// The side a deployment's book is read from: the run's own while it
    /// runs, the side it last started on once stopped, else the platform's.
    pub fn mode_for(&self, given: &str) -> RunMode {
        if let Some(info) = self.status_of(given) {
            return info.mode;
        }
        let id = self.as_run_id(given);
        let recorded = self
            .db
            .conn()
            .ok()
            .and_then(|c| store::read(&c, &id).ok().flatten())
            .and_then(|d| d.mode)
            .and_then(|m| RunMode::parse(&m));
        recorded.unwrap_or_else(|| {
            if self.services().analyzer_on() {
                RunMode::Sandbox
            } else {
                RunMode::Live
            }
        })
    }

    /// Why this cannot be started, as `(status, sentence)` (web
    /// `_why_not_runnable`).
    pub fn why_not_runnable(&self, name: &str) -> Option<Refusal> {
        let file = if is_script_name(name) {
            name.to_string()
        } else {
            self.db
                .conn()
                .ok()
                .and_then(|c| store::read(&c, name).ok().flatten())
                .map(|d| d.script)
                .unwrap_or_default()
        };
        if file.is_empty() || !self.scripts.has_source(&file) {
            return Some(Refusal::new(
                404,
                format!("There is no strategy saved for {}.", name),
            ));
        }
        if !self.scripts.has_program(&file) {
            return Some(Refusal::new(
                409,
                format!(
                    "{} has no compiled program yet. Open it in the chart and save it once the console shows no errors.",
                    file
                ),
            ));
        }
        None
    }

    // ------------------------------------------------------------- start

    /// Start one deployment and answer with its run. Never waits for the
    /// strategy to do anything: the page loads, replays history and begins.
    pub fn start(&self, name: &str) -> Result<RunInfo, Refusal> {
        if !names_something(name) {
            return Err(Refusal::new(
                400,
                crate::trading::names::script_name_refusal(name),
            ));
        }
        let saved = {
            let conn = self
                .db
                .conn()
                .map_err(|_| Refusal::new(409, "The run settings could not be read just now."))?;
            match store::require(&conn, name) {
                Ok(Ok(d)) => d,
                Ok(Err(why)) => return Err(Refusal::new(409, why)),
                Err(e) => {
                    tracing::error!("Could not read OpenScript run settings: {}", e);
                    return Err(Refusal::new(
                        409,
                        "The run settings could not be read just now.",
                    ));
                }
            }
        };
        let run_id = saved.deployment.clone();
        let where_ = format!(
            "{} on {} {} at {}",
            saved.script, saved.symbol, saved.exchange, saved.interval
        );
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(Refusal::new(409, SHUTTING_DOWN_MESSAGE));
        }
        let Some(host) = self.host.read().clone() else {
            return Err(Refusal::new(409, NO_HOST_MESSAGE));
        };
        if self.scripts.program(&saved.script).is_none() {
            return Err(Refusal::new(
                409,
                format!(
                    "{} has no compiled program yet. Open it in the chart and save it once the console shows no errors.",
                    saved.script
                ),
            ));
        }
        {
            let mut reg = self.reg.lock();
            if self.shutting_down.load(Ordering::SeqCst) {
                return Err(Refusal::new(409, SHUTTING_DOWN_MESSAGE));
            }
            if reg.runs.contains_key(&run_id) {
                return Err(Refusal::new(409, format!("{} is already running", where_)));
            }
            if reg.stopping.contains(&run_id) || reg.closing.contains(&run_id) {
                return Err(Refusal::new(
                    409,
                    format!("{} is still stopping, try again in a moment", where_),
                ));
            }
            if reg.starting.contains(&run_id) {
                return Err(Refusal::new(409, format!("{} is already starting", where_)));
            }
            if reg.deleting.contains(&run_id) {
                return Err(Refusal::new(
                    409,
                    format!("{} is being removed, so it was not started", where_),
                ));
            }
            // Claimed in the hold that checked; released on every path below.
            reg.starting.insert(run_id.clone());
        }
        let result = self.launch(&saved, &host, &where_);
        self.reg.lock().starting.remove(&run_id);
        result
    }

    fn launch(
        &self,
        saved: &store::Deployment,
        host: &Arc<dyn RunnerHost>,
        where_: &str,
    ) -> Result<RunInfo, Refusal> {
        let services = self.services();
        let run_id = saved.deployment.clone();
        // The settings were read before the claim: a removal may have landed.
        let still = self
            .db
            .conn()
            .ok()
            .and_then(|c| store::read(&c, &run_id).ok().flatten());
        if still.is_none() {
            return Err(Refusal::new(
                409,
                format!(
                    "{} was removed while it was being started, so it was not started",
                    where_
                ),
            ));
        }
        // The side is decided here, once, and kept for the life of the run.
        let mode = if services.analyzer_on() {
            RunMode::Sandbox
        } else {
            RunMode::Live
        };
        if let Err(why) = services.authorised(mode) {
            return Err(Refusal::new(
                409,
                format!(
                    "{} trades live and cannot start until your broker is connected. {}",
                    saved.script, why
                ),
            ));
        }
        let started = self.clock.now();
        let log_file = self.logs_dir.join(format!(
            "{}_{}_IST.log",
            run_id,
            started.with_timezone(&Kolkata).format("%Y%m%d_%H%M%S")
        ));
        if let Err(e) = std::fs::create_dir_all(&self.logs_dir) {
            tracing::error!("Could not create the OpenScript log folder: {}", e);
            return Err(Refusal::new(
                409,
                "The folder strategy logs are written to could not be created. Check that the app's data folder can be written to.",
            ));
        }
        let info = RunInfo {
            run_id: run_id.clone(),
            script: saved.script.clone(),
            symbol: saved.symbol.clone(),
            exchange: saved.exchange.clone(),
            interval: saved.interval.clone(),
            product: saved.product.clone(),
            mode,
            started_at: started,
            log_file,
        };
        let entry = Arc::new(RunEntry {
            info: info.clone(),
            token: random_token(),
            inbox: Inbox::new(),
            halted: AtomicBool::new(false),
            last_seen: Mutex::new(Instant::now()),
            anchor: Mutex::new(None),
            last_bar: Mutex::new(None),
            pump: Mutex::new(None),
            cancel: self.cancel.child_token(),
            order_lock: tokio::sync::Mutex::new(()),
        });
        self.write_log(
            &info,
            &[format!(
                "=== Run started at {} on the {} side ===",
                started
                    .with_timezone(&Kolkata)
                    .format("%Y-%m-%d %H:%M:%S IST"),
                if mode == RunMode::Live {
                    "live"
                } else {
                    "sandbox"
                }
            )],
        );
        self.reg.lock().runs.insert(run_id.clone(), entry.clone());
        let launch = Launch {
            run_id: run_id.clone(),
            page: format!(
                "/openscript-runner.html#run={}&token={}",
                urlencoding::encode(&run_id),
                entry.token
            ),
            title: format!("OpenScript {}", saved.script),
        };
        if let Err(why) = host.open(&launch) {
            self.reg.lock().runs.remove(&run_id);
            entry.inbox.close();
            self.write_log(
                &info,
                &[format!("The strategy could not be started: {}", why)],
            );
            return Err(Refusal::new(409, why));
        }
        if let Ok(conn) = self.db.conn() {
            if let Err(e) = store::record_mode(&conn, &run_id, mode.as_str()) {
                tracing::error!("Could not record the side of {}: {}", run_id, e);
            }
        }
        self.spawn_pump(&entry);
        tracing::info!("Started the OpenScript run {}", run_id);
        Ok(info)
    }

    // ------------------------------------------------------------- ending

    /// Close the page, stop the pump, and say so in the run's log.
    fn teardown(&self, entry: &RunEntry, why: &str) {
        entry.halted.store(true, Ordering::SeqCst);
        entry.inbox.push(Message::Halt);
        entry.inbox.close();
        entry.cancel.cancel();
        if let Some(h) = entry.pump.lock().take() {
            h.abort();
        }
        if let Some(host) = self.host.read().clone() {
            host.close(&entry.info.run_id);
        }
        self.write_log(&entry.info, &[format!("=== {} ===", why)]);
    }

    /// End a run and leave its position exactly where it is.
    pub fn pause(&self, given: &str) -> Result<String, Refusal> {
        self.pause_for(given, true, "Paused")
    }

    fn pause_for(
        &self,
        given: &str,
        refuse_while_closing: bool,
        why: &str,
    ) -> Result<String, Refusal> {
        let run_id = self.as_run_id(given);
        let entry = {
            let mut reg = self.reg.lock();
            if reg.stopping.contains(&run_id) {
                return Err(Refusal::new(409, "That run is already stopping"));
            }
            if refuse_while_closing && reg.closing.contains(&run_id) {
                return Err(Refusal::new(409, CLOSING_MESSAGE));
            }
            let Some(entry) = reg.runs.remove(&run_id) else {
                return Err(Refusal::new(409, "That run is not running"));
            };
            reg.stopping.insert(run_id.clone());
            entry
        };
        self.teardown(&entry, why);
        self.reg.lock().stopping.remove(&run_id);
        tracing::info!("The OpenScript run {} was paused", run_id);
        Ok(format!("{} paused", entry.info.script))
    }

    /// Close what the run holds, then end it. A close that did not happen
    /// leaves the run running and managed.
    pub async fn close(&self, given: &str) -> Result<String, Refusal> {
        let run_id = self.as_run_id(given);
        let entry = {
            let mut reg = self.reg.lock();
            if reg.stopping.contains(&run_id) {
                return Err(Refusal::new(409, "That run is already stopping"));
            }
            if reg.closing.contains(&run_id) {
                return Err(Refusal::new(409, CLOSING_MESSAGE));
            }
            let Some(entry) = reg.runs.get(&run_id).cloned() else {
                return Err(Refusal::new(409, "That run is not running"));
            };
            reg.closing.insert(run_id.clone());
            entry
        };
        let outcome = self.close_claimed(&entry).await;
        let mut reg = self.reg.lock();
        reg.closing.remove(&run_id);
        match outcome {
            Ok(()) => {
                if let Some(e) = reg.runs.remove(&run_id) {
                    drop(reg);
                    self.teardown(&e, "Closed and stopped");
                }
                tracing::info!("Closed and stopped the OpenScript run {}", run_id);
                Ok(format!("{} closed and stopped", entry.info.script))
            }
            Err(why) => {
                drop(reg);
                entry.halted.store(false, Ordering::SeqCst);
                self.write_log(
                    &entry.info,
                    &[format!("The close did not complete: {}", why)],
                );
                Err(Refusal::new(409, why))
            }
        }
    }

    async fn close_claimed(&self, entry: &Arc<RunEntry>) -> Result<(), String> {
        // Nothing new is sent from here on.
        entry.halted.store(true, Ordering::SeqCst);
        entry.inbox.push(Message::Halt);
        let _held = entry.order_lock.lock().await;
        let services = self.services();
        let info = &entry.info;
        self.refresh_orders(entry).await;

        // Withdraw what is still working, then square what was filled.
        let own = self.own_orders(&info.run_id);
        for o in own
            .iter()
            .filter(|o| !store::TERMINAL.contains(&o.status.as_str()))
        {
            if let Some(oid) = o.orderid.as_deref() {
                let r = services.cancel(info.mode, oid).await;
                if !r.ok {
                    self.write_log(
                        info,
                        &[format!(
                            "Order {} could not be cancelled: {}",
                            oid,
                            r.error.unwrap_or_default()
                        )],
                    );
                }
            }
        }
        self.refresh_orders(entry).await;
        let held = store::holdings(&self.own_orders(&info.run_id));
        let mut exits = Vec::new();
        for h in held.iter().filter(|h| h.quantity != 0) {
            let placement = orders::Placement {
                intent_id: -1,
                tag: "close".into(),
                action: if h.quantity > 0 { "SELL" } else { "BUY" },
                quantity: h.quantity.abs(),
                pricetype: "MARKET",
                price: None,
                trigger_price: None,
            };
            let target = Target {
                strategy: &info.run_id,
                symbol: &h.symbol,
                exchange: &h.exchange,
                product: &h.product,
            };
            let (id, result) = self
                .send(info, &target, &placement, None, true)
                .await
                .map_err(|e| format!("This strategy's position could not be closed: {}", e))?;
            if !result.ok {
                return Err(format!(
                    "This strategy's position could not be closed: {}. It is still running and still holding it. Close the position yourself, or try again.",
                    result.error.unwrap_or_else(|| "the order was refused".into())
                ));
            }
            exits.push(id);
        }
        if exits.is_empty() {
            return Ok(());
        }
        let (wait, look) = {
            let o = self.options.read();
            (o.close_wait, o.close_look)
        };
        let deadline = Instant::now() + wait;
        loop {
            self.refresh_orders(entry).await;
            let rows = self.own_orders(&info.run_id);
            let mine: Vec<&store::OrderRow> =
                rows.iter().filter(|r| exits.contains(&r.id)).collect();
            if mine.iter().all(|r| r.status == "filled") {
                return Ok(());
            }
            if let Some(r) = mine
                .iter()
                .find(|r| matches!(r.status.as_str(), "rejected" | "cancelled" | "expired"))
            {
                return Err(format!(
                    "The order closing this strategy's position was {}. It is still running and still holding it. Close the position yourself, or try again.",
                    r.status
                ));
            }
            if Instant::now() >= deadline {
                return Err(CLOSE_TIMEOUT_MESSAGE.into());
            }
            tokio::time::sleep(look).await;
        }
    }

    /// The page ended by itself (a diagnostic stopped the script) or stopped
    /// answering: drop the run and leave the position where it is.
    pub fn end_run(&self, run_id: &str, why: &str) {
        let entry = {
            let mut reg = self.reg.lock();
            if reg.closing.contains(run_id) || reg.stopping.contains(run_id) {
                return;
            }
            reg.runs.remove(run_id)
        };
        if let Some(e) = entry {
            self.teardown(&e, why);
            tracing::warn!("The OpenScript run {} ended: {}", run_id, why);
        }
    }

    /// Pause every run (logout, shutdown). Positions are left as they are.
    pub fn pause_all(&self, why: &str) -> Vec<String> {
        let ids: Vec<String> = self.reg.lock().runs.keys().cloned().collect();
        let mut done = Vec::new();
        for id in ids {
            if self.pause_for(&id, false, why).is_ok() {
                done.push(id);
            }
        }
        done
    }

    /// Refuse new starts, pause every run, stop the scheduler.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.cancel.cancel();
        let paused = self.pause_all("Paused because OpenAlgo is closing");
        if !paused.is_empty() {
            tracing::info!("Paused {} OpenScript run(s) on the way out", paused.len());
        }
        let handle = self.scheduler.lock().take();
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
    }

    // ------------------------------------------------------------- removal

    /// Remove one deployment's settings and schedule, refusing while it runs.
    pub fn remove_deployment(&self, given: &str) -> Result<String, Refusal> {
        let conn = self
            .db
            .conn()
            .map_err(|_| Refusal::new(500, "The run settings could not be read just now."))?;
        let found = store::read(&conn, given).ok().flatten();
        drop(conn);
        let Some(found) = found else {
            return Err(Refusal::new(
                404,
                format!("{} has no run settings saved.", given),
            ));
        };
        let run_id = found.deployment.clone();
        {
            let mut reg = self.reg.lock();
            if reg.runs.contains_key(&run_id) {
                return Err(Refusal::new(409, RUNNING_DELETE_REFUSAL));
            }
            if reg.starting.contains(&run_id)
                || reg.stopping.contains(&run_id)
                || reg.closing.contains(&run_id)
                || reg.deleting.contains(&run_id)
            {
                return Err(Refusal::new(409, BUSY_DELETE_REFUSAL));
            }
            reg.deleting.insert(run_id.clone());
        }
        let out = (|| {
            let mut conn = self
                .db
                .conn()
                .map_err(|_| Refusal::new(500, "These run settings could not be removed."))?;
            let msg = match store::delete(&mut conn, &run_id) {
                Ok(Ok((_, msg))) => msg,
                Ok(Err(why)) => return Err(Refusal::new(500, why)),
                Err(e) => {
                    tracing::error!("Could not remove {}: {}", run_id, e);
                    return Err(Refusal::new(
                        500,
                        "These run settings could not be removed.",
                    ));
                }
            };
            // The schedule goes with it, under either name it was saved by.
            for name in [run_id.as_str(), given] {
                if let Err(e) = store::delete_schedule(&conn, name) {
                    tracing::error!("Could not remove the schedule for {}: {}", name, e);
                }
            }
            Ok(msg)
        })();
        self.reg.lock().deleting.remove(&run_id);
        out
    }

    // ------------------------------------------------------------- orders

    fn own_orders(&self, run_id: &str) -> Vec<store::OrderRow> {
        self.db
            .conn()
            .ok()
            .and_then(|c| store::orders_of(&c, run_id).ok())
            .unwrap_or_default()
    }

    /// The orders a deployment placed (for its books).
    pub fn orders_of(&self, given: &str) -> Vec<store::OrderRow> {
        self.own_orders(&self.as_run_id(given))
    }

    /// Place one order for a run and record it. Answers the row id and the
    /// destination's answer. The frame for an intent goes to the page.
    async fn send(
        &self,
        info: &RunInfo,
        target: &Target<'_>,
        p: &orders::Placement,
        entry: Option<&RunEntry>,
        internal: bool,
    ) -> Result<(i64, crate::strategy::dispatch::DispatchResult), String> {
        let services = self.services();
        let row_id = {
            let conn = self.db.conn().map_err(|e| e.to_string())?;
            store::insert_order(
                &conn,
                &store::NewOrder {
                    deployment: info.run_id.clone(),
                    mode: info.mode.as_str().into(),
                    intent_id: (p.intent_id >= 0).then_some(p.intent_id),
                    tag: p.tag.clone(),
                    symbol: target.symbol.into(),
                    exchange: target.exchange.into(),
                    action: p.action.into(),
                    quantity: p.quantity,
                    pricetype: p.pricetype.into(),
                    product: target.product.into(),
                    is_exit: internal,
                },
                self.clock.now(),
            )
            .map_err(|e| e.to_string())?
        };
        let result = services
            .place(info.mode, &target.request(p), internal)
            .await;
        let status = if result.ok { "working" } else { "rejected" };
        if let Ok(conn) = self.db.conn() {
            let _ = store::placed(
                &conn,
                row_id,
                result.broker_order_id.as_deref(),
                status,
                result.error.as_deref(),
                self.clock.now(),
            );
        }
        let line = if result.ok {
            format!(
                "Sent {} {} {} as {} {} on the {} side. Order {}.",
                p.action,
                p.quantity,
                target.symbol,
                p.pricetype,
                target.product,
                info.mode.as_str(),
                result.broker_order_id.as_deref().unwrap_or("not numbered")
            )
        } else {
            format!(
                "An order was not sent: {}",
                result.error.as_deref().unwrap_or("it was refused")
            )
        };
        self.write_log(info, &[line]);
        if let Some(e) = entry {
            if p.intent_id >= 0 {
                e.inbox.push(Message::Frame {
                    intent_id: p.intent_id,
                    frame: orders::frame(
                        p.intent_id,
                        status,
                        0,
                        None,
                        result.broker_order_id.as_deref(),
                        result.error.as_deref(),
                        self.clock.now().timestamp_millis(),
                    ),
                });
            }
        }
        Ok((row_id, result))
    }

    /// Read back every open order of a run and tell the page what changed.
    async fn refresh_orders(&self, entry: &RunEntry) {
        let services = self.services();
        let open = self
            .db
            .conn()
            .ok()
            .and_then(|c| store::open_orders(&c, &entry.info.run_id).ok())
            .unwrap_or_default();
        for o in open {
            let Some(oid) = o.orderid.clone() else {
                continue;
            };
            let read = services.order_status(entry.info.mode, &oid).await;
            if !read.ok {
                continue;
            }
            let Some(now) = orders::progress(&read.order, o.quantity) else {
                continue;
            };
            if now.status == o.status
                && now.filled == o.filled_quantity
                && (now.average_price.is_none() || now.average_price == o.average_price)
            {
                continue;
            }
            if let Ok(conn) = self.db.conn() {
                let _ = store::progressed(
                    &conn,
                    o.id,
                    now.status,
                    now.filled,
                    now.average_price,
                    self.clock.now(),
                );
            }
            if now.status == "filled" {
                self.write_log(
                    &entry.info,
                    &[format!(
                        "Order {} filled: {} {} at {}.",
                        oid,
                        o.action,
                        now.filled,
                        now.average_price
                            .map(|p| format!("{:.2}", p))
                            .unwrap_or_else(|| "an unreported price".into())
                    )],
                );
            }
            if let Some(intent) = o.intent_id {
                entry.inbox.push(Message::Frame {
                    intent_id: intent,
                    frame: orders::frame(
                        intent,
                        now.status,
                        now.filled,
                        now.average_price.or(o.average_price),
                        Some(&oid),
                        None,
                        self.clock.now().timestamp_millis(),
                    ),
                });
            }
        }
    }

    // ------------------------------------------------------------- the page

    fn entry_for(&self, run_id: &str, token: &str) -> Option<Arc<RunEntry>> {
        let entry = self.reg.lock().runs.get(run_id).cloned()?;
        if !crate::server::form::tokens_match(token, &entry.token) {
            return None;
        }
        *entry.last_seen.lock() = Instant::now();
        Some(entry)
    }

    /// Whether this token belongs to this running run.
    pub fn authenticate(&self, run_id: &str, token: &str) -> bool {
        self.entry_for(run_id, token).is_some()
    }

    /// What the page needs to load the program.
    pub fn page_spec(&self, run_id: &str, token: &str) -> Option<Result<Value, Refusal>> {
        let entry = self.entry_for(run_id, token)?;
        let info = &entry.info;
        let Some(program) = self.scripts.program(&info.script) else {
            return Some(Err(Refusal::new(
                409,
                format!("{} has no compiled program any more.", info.script),
            )));
        };
        let program = String::from_utf8_lossy(&program).into_owned();
        let inputs = self
            .db
            .conn()
            .ok()
            .and_then(|c| store::read(&c, run_id).ok().flatten())
            .map(|d| Value::Object(d.inputs))
            .unwrap_or_else(|| json!({}));
        let today = self.clock.now().with_timezone(&Kolkata).date_naive();
        let facts = self
            .services()
            .instrument(&info.symbol, &info.exchange, today);
        Some(Ok(json!({
            "status": "success",
            "run": info.answer(),
            "program": program,
            "inputs": inputs,
            "facts": facts,
            "product": orders::product_for(&info.product),
            "lotSize": self.services().lot_size(&info.symbol, &info.exchange),
            "barPollMs": self.options.read().bar_poll.as_millis() as u64,
        })))
    }

    /// The history a run is replayed over; sets the anchor new bars follow.
    pub async fn page_bars(&self, run_id: &str, token: &str) -> Option<Result<Value, Refusal>> {
        let entry = self.entry_for(run_id, token)?;
        let info = &entry.info;
        let today = self.clock.now().with_timezone(&Kolkata).date_naive();
        let days = self.options.read().history_days.max(1);
        let start = today - ChronoDuration::days(days);
        let got = self
            .services()
            .history(&info.symbol, &info.exchange, &info.interval, start, today)
            .await;
        Some(match got {
            Ok(bars) => {
                if let Some(last) = bars.last() {
                    *entry.anchor.lock() = Some(last.time);
                    *entry.last_bar.lock() = Some(last.json());
                }
                Ok(json!({
                    "status": "success",
                    "data": bars.iter().map(|b| b.json()).collect::<Vec<_>>(),
                }))
            }
            Err(why) => Err(Refusal::new(
                502,
                if why.is_empty() {
                    "History could not be read just now.".to_string()
                } else {
                    why
                },
            )),
        })
    }

    /// The page's long poll.
    pub async fn page_inbox(&self, run_id: &str, token: &str, after: u64) -> Option<Value> {
        let entry = self.entry_for(run_id, token)?;
        let wait = self.options.read().inbox_wait;
        let (messages, closed) = entry.inbox.next(after, wait).await;
        *entry.last_seen.lock() = Instant::now();
        Some(json!({"status": "success", "messages": messages, "stop": closed}))
    }

    /// Lines the page writes into its run's log.
    pub fn page_log(&self, run_id: &str, token: &str, lines: &[String]) -> bool {
        let Some(entry) = self.entry_for(run_id, token) else {
            return false;
        };
        let kept: Vec<String> = lines
            .iter()
            .take(MAX_LOG_LINES)
            .map(|l| l.chars().take(2000).collect())
            .collect();
        self.write_log(&entry.info, &kept);
        true
    }

    /// The page says its script stopped.
    pub fn page_ended(&self, run_id: &str, token: &str, message: &str) -> bool {
        if self.entry_for(run_id, token).is_none() {
            return false;
        }
        let why: String = message.chars().take(500).collect();
        self.end_run(
            run_id,
            &format!(
                "Stopped: {}",
                if why.is_empty() {
                    "the script ended"
                } else {
                    &why
                }
            ),
        );
        true
    }

    /// Intents from a confirmed bar. Each answers with a frame in the inbox.
    pub async fn page_intents(
        &self,
        run_id: &str,
        token: &str,
        intents: &[Value],
    ) -> Option<Value> {
        let entry = self.entry_for(run_id, token)?;
        let info = entry.info.clone();
        let now_ms = || self.clock.now().timestamp_millis();
        let reject = |id: i64, text: &str| {
            entry.inbox.push(Message::Frame {
                intent_id: id,
                frame: orders::frame(id, "rejected", 0, None, None, Some(text), now_ms()),
            });
        };
        let _held = entry.order_lock.lock().await;
        if entry.halted.load(Ordering::SeqCst) {
            for i in intents {
                reject(
                    i.get("intentId").and_then(Value::as_i64).unwrap_or(-1),
                    "this run is stopping, so nothing further is sent",
                );
            }
            return Some(json!({"status": "success", "sent": 0, "halted": true}));
        }
        let services = self.services();
        let lot = services.lot_size(&info.symbol, &info.exchange);
        let planned: Vec<Planned> = intents.iter().map(|i| orders::plan(i, lot)).collect();
        if let Some(Planned::Unroutable { kind, .. }) = planned
            .iter()
            .find(|p| matches!(p, Planned::Unroutable { .. }))
        {
            for i in intents {
                reject(
                    i.get("intentId").and_then(Value::as_i64).unwrap_or(-1),
                    "this runner cannot send that order as one piece",
                );
            }
            let why = format!(
                "{} asked for an order this runner cannot send as one piece ({}). Nothing was sent for this bar and the run has stopped.",
                info.script, kind
            );
            drop(_held);
            self.end_run(run_id, &why);
            return Some(json!({"status": "success", "sent": 0, "stopped": true}));
        }
        let product = orders::product_for(&info.product);
        let target = Target {
            strategy: &info.run_id,
            symbol: &info.symbol,
            exchange: &info.exchange,
            product: &product,
        };
        let mut sent = 0;
        for p in planned {
            match p {
                Planned::Place(placement) => match self
                    .send(&info, &target, &placement, Some(&entry), false)
                    .await
                {
                    Ok((_, r)) => sent += usize::from(r.ok),
                    Err(e) => {
                        tracing::error!("Could not record an OpenScript order: {}", e);
                        reject(placement.intent_id, "the order could not be recorded");
                    }
                },
                Planned::Cancel { tag, .. } => {
                    for o in self
                        .own_orders(run_id)
                        .into_iter()
                        .filter(|o| o.tag == tag && !store::TERMINAL.contains(&o.status.as_str()))
                    {
                        if let Some(oid) = o.orderid.as_deref() {
                            let r = services.cancel(info.mode, oid).await;
                            self.write_log(
                                &info,
                                &[if r.ok {
                                    format!("Asked for order {} to be cancelled.", oid)
                                } else {
                                    format!(
                                        "Order {} could not be cancelled: {}",
                                        oid,
                                        r.error.unwrap_or_default()
                                    )
                                }],
                            );
                        }
                    }
                }
                Planned::Refuse { intent_id, reason } => {
                    self.write_log(&info, &[format!("An order was not sent: {}", reason)]);
                    reject(intent_id, &reason);
                }
                Planned::Unroutable { .. } => {}
            }
        }
        Some(json!({"status": "success", "sent": sent}))
    }

    // ------------------------------------------------------------- pump

    fn spawn_pump(&self, entry: &Arc<RunEntry>) {
        let me = self.me.clone();
        let weak = Arc::downgrade(entry);
        let cancel = entry.cancel.clone();
        let handle = tokio::spawn(async move {
            let mut last_bars = Instant::now();
            loop {
                let (order_poll, bar_poll, heartbeat) = match me.upgrade() {
                    Some(r) => {
                        let o = r.options.read();
                        (o.order_poll, o.bar_poll, o.heartbeat_timeout)
                    }
                    None => return,
                };
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(order_poll) => {}
                }
                let (Some(runner), Some(entry)) = (me.upgrade(), weak.upgrade()) else {
                    return;
                };
                if entry.last_seen.lock().elapsed() > heartbeat {
                    runner.end_run(
                        &entry.info.run_id,
                        "Stopped: the strategy page stopped answering. Its position was left as it is; start it again to resume",
                    );
                    return;
                }
                runner.refresh_orders(&entry).await;
                if last_bars.elapsed() >= bar_poll {
                    last_bars = Instant::now();
                    runner.pump_bars(&entry).await;
                }
            }
        });
        *entry.pump.lock() = Some(handle);
    }

    async fn pump_bars(&self, entry: &RunEntry) {
        let Some(anchor) = *entry.anchor.lock() else {
            return;
        };
        let info = &entry.info;
        let today = self.clock.now().with_timezone(&Kolkata).date_naive();
        let from = DateTime::<Utc>::from_timestamp_millis(anchor)
            .map(|d| d.with_timezone(&Kolkata).date_naive())
            .unwrap_or(today)
            .min(today);
        let bars = match self
            .services()
            .history(&info.symbol, &info.exchange, &info.interval, from, today)
            .await
        {
            Ok(b) => b,
            Err(why) => {
                tracing::debug!(
                    "History for {} was not read this time: {}",
                    info.run_id,
                    why
                );
                return;
            }
        };
        let last = entry.last_bar.lock().clone();
        let last_time = last
            .as_ref()
            .and_then(|b| b["time"].as_i64())
            .unwrap_or(anchor);
        let fresh: Vec<Value> = bars
            .iter()
            .filter(|b| b.time >= last_time)
            .map(|b| b.json())
            .filter(|b| Some(b) != last.as_ref())
            .collect();
        if let Some(newest) = fresh.last() {
            *entry.last_bar.lock() = Some(newest.clone());
            entry.inbox.push(Message::Bars(fresh));
        }
    }

    /// Push bars to a run's page as if history had answered them (tests and
    /// a future tick source share this door).
    pub fn push_bars(&self, run_id: &str, bars: Vec<Value>) -> bool {
        let Some(entry) = self.reg.lock().runs.get(run_id).cloned() else {
            return false;
        };
        if let Some(newest) = bars.last() {
            *entry.last_bar.lock() = Some(newest.clone());
        }
        entry.inbox.push(Message::Bars(bars));
        true
    }

    /// One pass of the pump for one run, for tests that do not wait on it.
    pub async fn pump_once(&self, run_id: &str) {
        let Some(entry) = self.reg.lock().runs.get(run_id).cloned() else {
            return;
        };
        self.refresh_orders(&entry).await;
        self.pump_bars(&entry).await;
    }

    // ------------------------------------------------------------- schedule

    /// Start the schedule task (owned; aborted at shutdown).
    pub fn start_scheduler(&self) {
        let mut slot = self.scheduler.lock();
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let me = self.me.clone();
        let cancel = self.cancel.clone();
        *slot = Some(tokio::spawn(async move {
            loop {
                let every = match me.upgrade() {
                    Some(r) => r.options.read().schedule_tick,
                    None => return,
                };
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(every) => {}
                }
                let Some(r) = me.upgrade() else { return };
                r.tick().await;
            }
        }));
    }

    /// Act on every start and stop that fell since the last tick.
    pub async fn tick(&self) {
        let now = self.clock.now();
        let every = ChronoDuration::from_std(self.options.read().schedule_tick)
            .unwrap_or_else(|_| ChronoDuration::seconds(15));
        let from = self.last_tick.lock().replace(now).unwrap_or(now - every);
        let schedules = match self.db.conn().map(|c| store::all_schedules(&c)) {
            Ok(Ok(s)) => s,
            _ => return,
        };
        for (name, action, date) in schedule::due(&schedules, from, now) {
            match action {
                schedule::Action::Stop => {
                    // Always attempted: a skipped stop is a position nobody
                    // is watching, and stopping a stopped run costs nothing.
                    match self.pause(&name) {
                        Ok(m) => tracing::info!("Stopped {} on schedule: {}", name, m),
                        Err(r) => {
                            tracing::info!("{} was not stopped on schedule: {}", name, r.message)
                        }
                    }
                }
                schedule::Action::Start => self.scheduled_start(&name, date),
            }
        }
    }

    fn scheduled_start(&self, name: &str, date: chrono::NaiveDate) {
        let services = self.services();
        if !services.signed_in() {
            tracing::info!("{} was not started on schedule: nobody is signed in", name);
            return;
        }
        let exchange = self
            .db
            .conn()
            .ok()
            .and_then(|c| store::read(&c, name).ok().flatten())
            .map(|d| d.exchange)
            .unwrap_or_default();
        if !exchange.is_empty() && !services.is_trading_day(&exchange, date) {
            tracing::info!("{} was not started: the market is closed today", name);
            return;
        }
        if let Some(r) = self.why_not_runnable(name) {
            tracing::warn!("{} was not started on schedule: {}", name, r.message);
            return;
        }
        match self.start(name) {
            Ok(info) => tracing::info!("Started {} on schedule as {}", name, info.run_id),
            Err(r) => tracing::warn!("{} did not start on schedule: {}", name, r.message),
        }
    }

    // ------------------------------------------------------------- logs

    fn write_log(&self, info: &RunInfo, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let path = &info.log_file;
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_LOG_BYTES {
            return;
        }
        let stamp = self
            .clock
            .now()
            .with_timezone(&Kolkata)
            .format("%H:%M:%S")
            .to_string();
        let mut text = String::new();
        for l in lines {
            text.push('[');
            text.push_str(&stamp);
            text.push_str(" IST] ");
            text.push_str(&l.replace(['\r', '\n'], " "));
            text.push('\n');
        }
        // Opened and closed per write: no descriptor is held for a run.
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            tracing::warn!("Could not write the log of {}: {}", info.run_id, e);
        }
    }
}
