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
//! **A deployment keeps its id on both sides, so every read of its orders
//! names the side.** Started in analyzer mode and again live, its sandbox
//! and live rows share the id; mixed, a sandbox position left by a Pause was
//! once sold at the broker by a later live Stop. And a Stop's exit is capped
//! by what the destination itself holds in the contract (its position book,
//! plus holdings for delivery): never more than that, never the other way.
//! The web closes from its in-memory ledger alone; the cap is a deliberate
//! desktop addition.
//!
//! **A Stop is a lifecycle, shared with the page.** It halts the page, then
//! cancels the run's working orders and waits until the destination says
//! each is finished (a cancel is a request, not proof), and only then
//! measures the position from its own fills and sends the exit, watched to
//! a fill. A close that did not happen and left nothing of its own working
//! resumes the page, so the script manages its position again (as the web
//! keeps cycling a script whose flatten failed). One that left an order
//! working is close pending: halted, reported as such, until Stop is pressed
//! again or the run is paused. A ledger that cannot be read is never taken
//! as flat.
//!
//! **Every task is owned.** One pump per run (order frames, new bars, the
//! page's heartbeat) kept in the run's entry and aborted when the run ends,
//! and one scheduler task kept here and aborted at shutdown. A page that
//! stops answering is noticed by the pump and its run is ended, so a run is
//! never reported running with nothing behind it.
//!
//! The registry's methods are split by what they do: `launch` (start),
//! `ending` (pause, Stop, removal), `page` (what the run's page asks); the
//! rest (reading, orders, the pump, schedules, logs) is here.

pub mod books;
mod deployments;
mod ending;
pub mod host;
pub mod inbox;
mod launch;
pub mod orders;
mod page;
pub mod reconcile;
pub mod schedule;
mod sending;
pub mod services;
pub mod store;
#[cfg(feature = "runner-window")]
pub mod window;

use crate::clock::Clock;
use crate::db::sqlite::SqliteDb;
use crate::strategy::dispatch::{OrderStatusResult, RunMode};
use crate::trading::names::is_script_name;
use crate::trading::scripts::ScriptStore;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use chrono_tz::Asia::Kolkata;
use host::RunnerHost;
use inbox::{Inbox, Message};
use orders::Target;
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use services::RunnerServices;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
/// The end of every answer that leaves a close pending: what the run is
/// doing and what the trader does next. It never says the run manages the
/// position, because a close-pending run sends nothing.
pub const CLOSE_PENDING_TAIL: &str = "This strategy has stopped sending orders and its close is pending. Check your orders and positions, then press Stop again, or Pause it to keep the position as it is.";
/// A Stop whose exit did not fill in time.
pub const CLOSE_TIMEOUT_MESSAGE: &str = "The order closing this strategy's position has not filled yet, so the position may still be open. This strategy has stopped sending orders and its close is pending. Check your orders and positions, then press Stop again, or Pause it to keep the position as it is.";
/// A Stop that could not read the run's own orders.
pub const ORDERS_UNREADABLE: &str = "This strategy's own orders could not be read just now.";
/// The end of an answer for a Stop that sent nothing and resumed the run.
pub const NOTHING_CLOSED: &str = "Nothing was closed. It is still running and still holding its position. Try Stop again in a moment.";
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
    /// A Stop left an order of this run working: it sends nothing until
    /// Stop is pressed again or it is paused.
    pub close_pending: bool,
    /// Placement answers the run could not save yet (it sends nothing new
    /// until they are saved).
    pub unsaved_orders: usize,
    /// Open orders of this run with no number from the destination, which
    /// it cannot follow (a Stop does not go ahead while there are any).
    pub unresolved_orders: usize,
}

impl RunInfo {
    /// One run as the routes report it (web `_run_answer`). `state` is
    /// `running`, or (desktop) `close_pending`: a run that has ended is not
    /// in the registry at all. There is no process, so `pid` is null.
    pub fn answer(&self) -> Value {
        json!({
            "id": self.run_id,
            "deployment": self.run_id,
            "file": self.script,
            "state": if self.close_pending { "close_pending" } else { "running" },
            "unsaved_orders": self.unsaved_orders,
            "unresolved_orders": self.unresolved_orders,
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
    /// A Stop left an order of this run working (see [`RunInfo::close_pending`]).
    close_pending: AtomicBool,
    /// Placement answers not yet written to their rows.
    unsaved: Mutex<Vec<Unsaved>>,
    /// Open orders with no number, as of the last refresh.
    unresolved: AtomicUsize,
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

impl RunEntry {
    /// The run as it stands now, its close-pending state and unsaved
    /// answers included.
    fn snapshot(&self) -> RunInfo {
        let mut info = self.info.clone();
        info.close_pending = self.close_pending.load(Ordering::SeqCst);
        info.unsaved_orders = self.unsaved.lock().len();
        info.unresolved_orders = self.unresolved.load(Ordering::SeqCst);
        info
    }
}

/// How many times a placement's answer is written before it is kept on the
/// run to be written later.
const ACK_ATTEMPTS: u32 = 3;

/// A destination's answer to a placement that could not be written to the
/// order's row yet (LOG-04). Kept on the run and written again on every
/// refresh; until it is, the run sends nothing new and a Stop does not
/// report closed.
#[derive(Debug, Clone)]
struct Unsaved {
    row_id: i64,
    orderid: Option<String>,
    /// `working`, `rejected` or [`store::UNCONFIRMED`].
    status: &'static str,
    message: Option<String>,
    client_tag: Option<String>,
}

/// Why a Stop did not close, and whether anything of it may still fill.
struct CloseFailure {
    message: String,
    /// An order of this run may still be working: the run stays halted.
    /// Otherwise its page is resumed and its script trades on.
    pending: bool,
}

impl CloseFailure {
    fn resume(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            pending: false,
        }
    }

    fn pending(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            pending: true,
        }
    }
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
    /// Book reads that missed each unconfirmed row, and the Live rows held
    /// for the trader (the order reconciler; LOG-08).
    absence: crate::services::order_reconciler::AbsenceTracker,
    /// Unconfirmed rows matching several book orders the trader was told of.
    ambiguous: Mutex<HashSet<String>>,
    cancel: CancellationToken,
}

fn random_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Where a run's orders go, as a trader reads it.
fn side_words(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Live => "at your broker",
        RunMode::Sandbox => "in sandbox mode",
    }
}

fn side_name(quantity: i64) -> &'static str {
    if quantity < 0 {
        "short"
    } else {
        "long"
    }
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
            absence: crate::services::order_reconciler::AbsenceTracker::new(),
            ambiguous: Mutex::new(HashSet::new()),
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
        self.reg.lock().runs.get(&id).map(|e| e.snapshot())
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
            .map(|e| e.snapshot())
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
        // Never started: it has no rows on either side, so this only picks
        // which empty book to show.
        recorded.unwrap_or_else(|| match self.services().analyzer_on() {
            Ok(true) => RunMode::Sandbox,
            Ok(false) | Err(_) => RunMode::Live,
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

    // ------------------------------------------------------------- orders

    /// A deployment's own orders on one side, or the sentence saying they
    /// could not be read. Never an empty list on a failure: a ledger that
    /// cannot be read is not a flat one (LOG-04).
    fn own_orders(&self, run_id: &str, mode: RunMode) -> Result<Vec<store::OrderRow>, String> {
        self.db
            .conn()
            .and_then(|c| store::orders_of(&c, run_id, mode.as_str()))
            .map_err(|e| {
                tracing::error!("Could not read the orders of {}: {}", run_id, e);
                ORDERS_UNREADABLE.to_string()
            })
    }

    fn read_own(&self, info: &RunInfo) -> Result<Vec<store::OrderRow>, String> {
        self.own_orders(&info.run_id, info.mode)
    }

    /// The orders a deployment placed on one side (for its books).
    pub fn orders_of(&self, given: &str, mode: RunMode) -> Result<Vec<store::OrderRow>, String> {
        self.own_orders(&self.as_run_id(given), mode)
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
        let row_id = self.record_sent(info, target, p, internal)?;
        let result = self
            .services()
            .place(info.mode, &target.request(p), internal)
            .await;
        let status = sending::row_status(&result);
        let ack = Unsaved {
            row_id,
            orderid: result.broker_order_id.clone(),
            status,
            message: result.error.clone(),
            client_tag: result.client_tag.clone(),
        };
        self.keep_answer(info, entry, ack).await;
        self.write_log(info, &[sending::sent_line(info, target, p, &result)]);
        if result.uncertain {
            self.services().request_reconcile();
        }
        if let Some(e) = entry.filter(|_| p.intent_id >= 0) {
            self.tell_page(e, p, status, &result);
        }
        Ok((row_id, result))
    }

    fn try_save(&self, ack: &Unsaved) -> crate::error::Result<()> {
        let conn = self.db.conn()?;
        if ack.status == store::UNCONFIRMED {
            return store::unconfirmed(
                &conn,
                ack.row_id,
                ack.client_tag.as_deref(),
                ack.message.as_deref(),
                self.clock.now(),
            );
        }
        store::placed(
            &conn,
            ack.row_id,
            ack.orderid.as_deref(),
            ack.status,
            ack.message.as_deref(),
            self.clock.now(),
        )
    }

    /// Save a placement's answer, trying a few times. False when it could
    /// not be saved.
    async fn save_ack(&self, ack: &Unsaved) -> bool {
        for attempt in 1..=ACK_ATTEMPTS {
            match self.try_save(ack) {
                Ok(()) => return true,
                Err(e) => {
                    tracing::error!(
                        "Could not save the answer for OpenScript order row {} (attempt {}): {}",
                        ack.row_id,
                        attempt,
                        e
                    );
                    if attempt < ACK_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
        false
    }

    /// Save the answers a run could not save before. What still cannot be
    /// saved stays on the run.
    fn flush_unsaved(&self, entry: &RunEntry) {
        let pending = std::mem::take(&mut *entry.unsaved.lock());
        if pending.is_empty() {
            return;
        }
        let mut left = Vec::new();
        for ack in pending {
            match self.try_save(&ack) {
                Ok(()) => self.write_log(
                    &entry.info,
                    &[format!(
                        "The answer for order {} is saved now.",
                        ack.orderid.as_deref().unwrap_or("without a number")
                    )],
                ),
                Err(e) => {
                    tracing::warn!(
                        "Still cannot save OpenScript order row {}: {}",
                        ack.row_id,
                        e
                    );
                    left.push(ack);
                }
            }
        }
        entry.unsaved.lock().extend(left);
    }

    /// Read back every open order of a run and tell the page what changed.
    async fn refresh_orders(&self, entry: &RunEntry) {
        self.refresh_orders_until(entry, None).await;
    }

    /// One order's status, or `None` when it could not be read before
    /// `deadline`.
    async fn status_until(
        &self,
        mode: RunMode,
        oid: &str,
        deadline: Option<Instant>,
    ) -> Option<OrderStatusResult> {
        let services = self.services();
        let read = services.order_status(mode, oid);
        let Some(d) = deadline else {
            return Some(read.await);
        };
        tokio::time::timeout(d.saturating_duration_since(Instant::now()), read)
            .await
            .ok()
    }

    /// [`Self::refresh_orders`], making no read past `deadline` (TIME-03): a
    /// Stop's close budget bounds the whole close, not one pass of serial
    /// status reads. The rows left unread stay as they were, which the close
    /// reads as still working.
    async fn refresh_orders_until(&self, entry: &RunEntry, deadline: Option<Instant>) {
        self.flush_unsaved(entry);
        let read = self
            .db
            .conn()
            .and_then(|c| store::open_orders(&c, &entry.info.run_id, entry.info.mode.as_str()));
        let open = match read {
            Ok(rows) => rows,
            Err(e) => {
                // Whoever needs the rows reads them again and says so.
                tracing::warn!(
                    "Could not read the open orders of {}: {}",
                    entry.info.run_id,
                    e
                );
                return;
            }
        };
        entry.unresolved.store(
            open.iter().filter(|o| o.orderid.is_none()).count(),
            Ordering::SeqCst,
        );
        for o in open {
            let Some(oid) = o.orderid.clone() else {
                continue;
            };
            let Some(read) = self.status_until(entry.info.mode, &oid, deadline).await else {
                tracing::info!(
                    "The close of {} ran out of time while reading its orders",
                    entry.info.run_id
                );
                return;
            };
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
