//! The download job engine (web `historify_service` job functions).
//!
//! One owned tokio task per active job, kept in a `JoinSet` and aborted on
//! shutdown. A semaphore bounds how many jobs download at once (the web's
//! five-worker pool); every broker call across all jobs goes through one
//! pacer (the web's ~3 history requests per second), and each job waits a
//! random 1-3 s between symbols with a longer cooldown every ten.
//!
//! Control (pause, resume, cancel) is a `watch` value per job. Every job
//! status write that can race with the processor's own terminal write is
//! taken under that job's status lock, and the processor claims its terminal
//! state under the same lock, so a late pause can never overwrite
//! `completed` (or the reverse). A retry claims the job's slot under the
//! slot-table lock that checks it, so two retries cannot start two
//! processors.
//!
//! Items are processed from `pending` (and `downloading`, an item that was
//! interrupted), so a job is resumable: after a restart, jobs that were
//! running come back `paused` and Resume continues where they stopped.

use super::db::{self, Item, NewJob};
use super::interval;
use super::source::{HistorySource, Notifier};
use super::time::{ist_day, ist_naive, parse_date};
use crate::brokers::common::ratelimit::Pacer;
use crate::clock::Clock;
use crate::db::duckdb::{HistorifyDb, STORE_UNAVAILABLE};
use crate::error::AppError;
use crate::services::core::Reply;
use async_trait::async_trait;
use chrono::{Duration as CDuration, NaiveDate};
use parking_lot::{Mutex, RwLock};
use rand::Rng;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{watch, Mutex as AsyncMutex, Semaphore};
use tokio::task::JoinSet;

/// What a retry of a download that still has a processor is told.
pub const RETRY_BUSY_MESSAGE: &str =
    "This download is already running. Wait for it to finish, then retry the failed symbols.";

const SAVE_FAILED: &str =
    "The downloaded candles could not be saved. Check free disk space, then retry.";

/// Pacing and concurrency.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Jobs downloading at the same time (web `HISTORIFY_MAX_WORKERS`).
    pub max_concurrent_jobs: usize,
    /// Random wait between symbols (web `HISTORIFY_DELAY_MIN/MAX`).
    pub delay_min: Duration,
    pub delay_max: Duration,
    /// Every `batch_size` symbols, a longer cooldown so per-minute broker
    /// limits reset.
    pub batch_size: usize,
    pub batch_cooldown_min: Duration,
    pub batch_cooldown_max: Duration,
    /// Minimum spacing of broker history calls across all jobs.
    pub history_interval: Duration,
    /// How often a paused job repeats `historify_job_paused`.
    pub pause_heartbeat: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: 5,
            delay_min: Duration::from_secs(1),
            delay_max: Duration::from_secs(3),
            batch_size: 10,
            batch_cooldown_min: Duration::from_secs(5),
            batch_cooldown_max: Duration::from_secs(10),
            history_interval: Duration::from_millis(350),
            pause_heartbeat: Duration::from_secs(1),
        }
    }
}

impl EngineConfig {
    /// No waits (tests).
    pub fn immediate() -> Self {
        Self {
            delay_min: Duration::ZERO,
            delay_max: Duration::ZERO,
            batch_cooldown_min: Duration::ZERO,
            batch_cooldown_max: Duration::ZERO,
            history_interval: Duration::ZERO,
            pause_heartbeat: Duration::from_millis(50),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctl {
    Run,
    Pause,
    Cancel,
}

/// A processor finished (schedule bookkeeping).
#[derive(Debug, Clone)]
pub struct Finished {
    pub job_id: String,
    pub config: Value,
    pub status: String,
    pub total: i64,
    pub completed: i64,
    pub failed: i64,
}

#[async_trait]
pub trait JobHook: Send + Sync {
    async fn finished(&self, f: Finished);
}

/// A job slot's generation, control sender and status lock.
type SlotRef = (u64, Arc<watch::Sender<Ctl>>, Arc<AsyncMutex<()>>);

struct Slot {
    gen: u64,
    ctl: Arc<watch::Sender<Ctl>>,
    status: Arc<AsyncMutex<()>>,
}

struct Inner {
    db: HistorifyDb,
    source: Arc<dyn HistorySource>,
    notify: Arc<dyn Notifier>,
    clock: Arc<dyn Clock>,
    cfg: EngineConfig,
    permits: Arc<Semaphore>,
    pacer: Pacer,
    slots: Mutex<HashMap<String, Slot>>,
    tasks: Mutex<JoinSet<()>>,
    next_gen: AtomicU64,
    closed: AtomicBool,
    hook: RwLock<Option<Weak<dyn JobHook>>>,
}

/// A job to create.
#[derive(Debug, Clone, Default)]
pub struct CreateJob {
    pub job_type: String,
    pub symbols: Vec<(String, String)>,
    pub interval: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub config: Value,
    pub incremental: bool,
}

#[derive(Clone)]
pub struct JobEngine {
    inner: Arc<Inner>,
}

fn store_failed(what: &str, e: AppError) -> Reply {
    tracing::error!("Historify {} failed: {}", what, e);
    Reply::error(500, STORE_UNAVAILABLE)
}

fn jitter(min: Duration, max: Duration) -> Duration {
    if max <= min {
        min
    } else {
        rand::thread_rng().gen_range(min..=max)
    }
}

/// Wait until the control value is `Cancel` (or the sender is gone).
async fn until_cancel(mut rx: watch::Receiver<Ctl>) {
    loop {
        if *rx.borrow_and_update() == Ctl::Cancel {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

enum Interrupted {
    Cancelled,
}

/// Releases a claimed slot unless it is handed over to a processor.
struct ClaimGuard<'a> {
    engine: &'a JobEngine,
    job_id: &'a str,
    gen: u64,
}

impl ClaimGuard<'_> {
    /// The processor now owns the slot and releases it when it ends.
    fn hand_over(self) {
        std::mem::forget(self);
    }
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        self.engine.release(self.job_id, self.gen);
    }
}

impl JobEngine {
    /// Build the engine and mark jobs a previous run left mid-download as
    /// paused, so they can be resumed.
    pub fn new(
        db: HistorifyDb,
        source: Arc<dyn HistorySource>,
        notify: Arc<dyn Notifier>,
        clock: Arc<dyn Clock>,
        cfg: EngineConfig,
    ) -> Self {
        let permits = Arc::new(Semaphore::new(cfg.max_concurrent_jobs.max(1)));
        let pacer = Pacer::with_interval(cfg.history_interval);
        let e = Self {
            inner: Arc::new(Inner {
                db,
                source,
                notify,
                clock,
                cfg,
                permits,
                pacer,
                slots: Mutex::new(HashMap::new()),
                tasks: Mutex::new(JoinSet::new()),
                next_gen: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                hook: RwLock::new(None),
            }),
        };
        if let Err(err) = e.recover() {
            tracing::error!("Could not recover interrupted Historify jobs: {}", err);
        }
        e
    }

    pub fn set_hook(&self, hook: Weak<dyn JobHook>) {
        *self.inner.hook.write() = Some(hook);
    }

    pub fn db(&self) -> &HistorifyDb {
        &self.inner.db
    }

    pub fn source(&self) -> &Arc<dyn HistorySource> {
        &self.inner.source
    }

    fn now(&self) -> chrono::NaiveDateTime {
        ist_naive(self.inner.clock.now())
    }

    /// Jobs that were running or queued when the app last stopped become
    /// `paused`; their interrupted items go back to `pending`.
    fn recover(&self) -> crate::error::Result<usize> {
        self.inner.db.mutate(|c| {
            let ids = db::job_ids_with_status(c, &["running", "pending"])?;
            for id in &ids {
                db::set_job_status(c, id, "paused", None, Default::default())?;
                db::reset_items(c, id, &["downloading"])?;
            }
            if !ids.is_empty() {
                tracing::info!(
                    "{} interrupted Historify download(s) are paused and can be resumed",
                    ids.len()
                );
            }
            Ok(ids.len())
        })
    }

    /// Jobs with a live processor.
    pub fn active_jobs(&self) -> usize {
        self.inner.slots.lock().len()
    }

    /// Owned tasks still alive (finished ones reaped).
    pub fn task_count(&self) -> usize {
        let mut t = self.inner.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    pub fn is_active(&self, job_id: &str) -> bool {
        self.inner.slots.lock().contains_key(job_id)
    }

    /// Claim the job's slot; `None` when a processor already holds it.
    fn claim(&self, job_id: &str) -> Option<(u64, watch::Receiver<Ctl>, Arc<AsyncMutex<()>>)> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return None;
        }
        let mut slots = self.inner.slots.lock();
        if slots.contains_key(job_id) {
            return None;
        }
        let gen = self.inner.next_gen.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = watch::channel(Ctl::Run);
        let status = Arc::new(AsyncMutex::new(()));
        slots.insert(
            job_id.to_string(),
            Slot {
                gen,
                ctl: Arc::new(tx),
                status: status.clone(),
            },
        );
        Some((gen, rx, status))
    }

    fn slot(&self, job_id: &str) -> Option<SlotRef> {
        self.inner
            .slots
            .lock()
            .get(job_id)
            .map(|s| (s.gen, s.ctl.clone(), s.status.clone()))
    }

    fn spawn(&self, job_id: String, gen: u64, rx: watch::Receiver<Ctl>, st: Arc<AsyncMutex<()>>) {
        let me = self.clone();
        let mut tasks = self.inner.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move { me.process(job_id, gen, rx, st).await });
    }

    fn release(&self, job_id: &str, gen: u64) {
        let mut slots = self.inner.slots.lock();
        if slots.get(job_id).map(|s| s.gen) == Some(gen) {
            slots.remove(job_id);
        }
    }

    async fn job_status(&self, job_id: &str) -> Result<Option<String>, Reply> {
        let id = job_id.to_string();
        match self.inner.db.run(move |c| db::job(c, &id)).await {
            Ok(j) => Ok(j.and_then(|j| j["status"].as_str().map(str::to_string))),
            Err(e) => Err(store_failed("reading a job", e)),
        }
    }

    async fn write_status(&self, job_id: &str, status: &'static str, error: Option<String>) {
        let (id, now) = (job_id.to_string(), self.now());
        if let Err(e) = self
            .inner
            .db
            .write(move |c| db::set_job_status(c, &id, status, error.as_deref(), now))
            .await
        {
            tracing::error!(
                "Could not record Historify job {} as {}: {}",
                job_id,
                status,
                e
            );
        }
    }

    // ------------------------------------------------------------ create

    /// Web `create_and_start_job`.
    pub async fn create_and_start(&self, req: CreateJob) -> Reply {
        if req.symbols.is_empty() {
            return Reply::error(400, "No symbols provided");
        }
        if self.inner.closed.load(Ordering::SeqCst) {
            return Reply::error(
                503,
                "OpenAlgo is shutting down. Start the download again after restarting.",
            );
        }
        let job_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let mut config = match req.config {
            Value::Object(m) => Value::Object(m),
            _ => json!({}),
        };
        config["incremental"] = json!(req.incremental);
        let total = req.symbols.len();
        let (id, now) = (job_id.clone(), self.now());
        let r = self
            .inner
            .db
            .write(move |c| {
                db::create_job(
                    c,
                    &NewJob {
                        id: &id,
                        job_type: &req.job_type,
                        symbols: &req.symbols,
                        interval: &req.interval,
                        start_date: req.start_date.as_deref(),
                        end_date: req.end_date.as_deref(),
                        config: &config,
                    },
                    now,
                )
            })
            .await;
        if let Err(e) = r {
            return store_failed("creating a job", e);
        }
        match self.claim(&job_id) {
            Some((gen, rx, st)) => self.spawn(job_id.clone(), gen, rx, st),
            None => {
                self.write_status(
                    &job_id,
                    "failed",
                    Some("Could not start background download".into()),
                )
                .await;
                return Reply::error(
                    503,
                    "OpenAlgo is shutting down. Start the download again after restarting.",
                );
            }
        }
        Reply::ok(json!({
            "status": "success",
            "message": format!("Job started with {} symbols", total),
            "job_id": job_id,
            "total_symbols": total,
            "incremental": req.incremental,
        }))
    }

    // ----------------------------------------------------------- control

    /// Web `get_job_status`.
    pub async fn status(&self, job_id: &str) -> Reply {
        let id = job_id.to_string();
        match self
            .inner
            .db
            .run(move |c| Ok((db::job(c, &id)?, db::job_items(c, &id, None)?)))
            .await
        {
            Ok((Some(job), items)) => {
                Reply::ok(json!({"status": "success", "job": job, "items": items}))
            }
            Ok((None, _)) => Reply::error(404, "Job not found"),
            Err(e) => store_failed("reading a job", e),
        }
    }

    /// Web `get_all_jobs`.
    pub async fn list(&self, status: Option<String>, limit: i64) -> Reply {
        match self
            .inner
            .db
            .run(move |c| db::jobs(c, status.as_deref(), limit))
            .await
        {
            Ok(jobs) => Reply::ok(json!({"status": "success", "count": jobs.len(), "data": jobs})),
            Err(e) => store_failed("listing jobs", e),
        }
    }

    /// Web `pause_job`.
    pub async fn pause(&self, job_id: &str) -> Reply {
        match self.job_status(job_id).await {
            Err(r) => return r,
            Ok(None) => return Reply::error(404, "Job not found"),
            Ok(Some(s)) if s != "running" => {
                return Reply::error(400, format!("Job is not running (status: {})", s))
            }
            Ok(Some(_)) => {}
        }
        let Some((gen, ctl, st)) = self.slot(job_id) else {
            return Reply::error(400, "Job not found in running jobs");
        };
        let _g = st.lock().await;
        if self.slot(job_id).map(|s| s.0) != Some(gen) {
            return self.not_running_now(job_id).await;
        }
        let current = *ctl.borrow();
        match current {
            Ctl::Pause => return Reply::error(400, "Job is already paused"),
            Ctl::Cancel => return self.not_running_now(job_id).await,
            Ctl::Run => {}
        }
        ctl.send_replace(Ctl::Pause);
        self.write_status(job_id, "paused", None).await;
        tracing::info!("Historify job {} paused", job_id);
        Reply::ok(json!({"status": "success", "message": "Job paused"}))
    }

    async fn not_running_now(&self, job_id: &str) -> Reply {
        match self.job_status(job_id).await {
            Ok(Some(s)) => Reply::error(400, format!("Job is not running (status: {})", s)),
            Ok(None) => Reply::error(404, "Job not found"),
            Err(r) => r,
        }
    }

    /// Web `resume_job`. A job paused by a restart (no processor) is
    /// started again from its pending items.
    pub async fn resume(&self, job_id: &str) -> Reply {
        match self.job_status(job_id).await {
            Err(r) => return r,
            Ok(None) => return Reply::error(404, "Job not found"),
            Ok(Some(s)) if s != "paused" => {
                return Reply::error(400, format!("Job is not paused (status: {})", s))
            }
            Ok(Some(_)) => {}
        }
        if let Some((gen, ctl, st)) = self.slot(job_id) {
            let _g = st.lock().await;
            if self.slot(job_id).map(|s| s.0) == Some(gen) && *ctl.borrow() == Ctl::Pause {
                self.write_status(job_id, "running", None).await;
                ctl.send_replace(Ctl::Run);
                tracing::info!("Historify job {} resumed", job_id);
                return Reply::ok(json!({"status": "success", "message": "Job resumed"}));
            }
            return Reply::error(400, "Job status is changing. Try again in a moment.");
        }
        // Interrupted by a restart: start a processor for what is left.
        let Some((gen, rx, st)) = self.claim(job_id) else {
            return Reply::error(409, RETRY_BUSY_MESSAGE);
        };
        let id = job_id.to_string();
        let r = self
            .inner
            .db
            .write(move |c| {
                db::reset_items(c, &id, &["downloading"])?;
                db::set_job_status(c, &id, "pending", None, Default::default())
            })
            .await;
        if let Err(e) = r {
            self.release(job_id, gen);
            return store_failed("resuming a job", e);
        }
        self.spawn(job_id.to_string(), gen, rx, st);
        tracing::info!("Historify job {} resumed after a restart", job_id);
        Reply::ok(json!({"status": "success", "message": "Job resumed"}))
    }

    /// Web `cancel_job`. A queued job and one paused by a restart can be
    /// cancelled too.
    pub async fn cancel(&self, job_id: &str) -> Reply {
        let status = match self.job_status(job_id).await {
            Err(r) => return r,
            Ok(None) => return Reply::error(404, "Job not found"),
            Ok(Some(s)) => s,
        };
        if !matches!(status.as_str(), "running" | "paused" | "pending") {
            return Reply::error(
                400,
                format!("Job is not running or paused (status: {})", status),
            );
        }
        match self.slot(job_id) {
            Some((gen, ctl, st)) => {
                let _g = st.lock().await;
                if self.slot(job_id).map(|s| s.0) != Some(gen) {
                    return match self.job_status(job_id).await {
                        Ok(Some(s)) => Reply::error(
                            400,
                            format!("Job is not running or paused (status: {})", s),
                        ),
                        Ok(None) => Reply::error(404, "Job not found"),
                        Err(r) => r,
                    };
                }
                ctl.send_replace(Ctl::Cancel);
                self.write_status(job_id, "cancelled", None).await;
            }
            None => self.write_status(job_id, "cancelled", None).await,
        }
        tracing::info!("Historify job {} cancelled", job_id);
        self.inner.notify.notify(
            "historify_job_cancelled",
            json!({"job_id": job_id, "status": "cancelled"}),
        );
        Reply::ok(json!({"status": "success", "message": "Job cancelled"}))
    }

    /// Web `retry_failed_items`.
    ///
    /// The claim is taken before the first await, so of two retries racing
    /// for the same job exactly one proceeds and the other is refused. The
    /// checks that follow run under that claim; it is released when they
    /// decline, when a store call fails, or when the request is dropped
    /// before the processor starts.
    pub async fn retry(&self, job_id: &str) -> Reply {
        let Some((gen, rx, st)) = self.claim(job_id) else {
            return match self.job_status(job_id).await {
                Err(r) => r,
                Ok(None) => Reply::error(404, "Job not found"),
                Ok(Some(s)) if s == "running" => Reply::error(400, "Job is already running"),
                Ok(Some(_)) => Reply::error(409, RETRY_BUSY_MESSAGE),
            };
        };
        let claim = ClaimGuard {
            engine: self,
            job_id,
            gen,
        };
        match self.job_status(job_id).await {
            Err(r) => return r,
            Ok(None) => return Reply::error(404, "Job not found"),
            Ok(Some(s)) if s == "running" => return Reply::error(400, "Job is already running"),
            Ok(Some(_)) => {}
        }
        let id = job_id.to_string();
        let failed = match self
            .inner
            .db
            .run(move |c| {
                Ok(db::items(c, &id)?
                    .into_iter()
                    .filter(|i| i.status == "error")
                    .count())
            })
            .await
        {
            Ok(n) => n,
            Err(e) => return store_failed("reading job items", e),
        };
        if failed == 0 {
            return Reply::ok(json!({"status": "success", "message": "No failed items to retry"}));
        }
        let id = job_id.to_string();
        let r = self
            .inner
            .db
            .write(move |c| {
                let n = db::reset_items(c, &id, &["error"])?;
                db::set_job_status(c, &id, "pending", None, Default::default())?;
                Ok(n)
            })
            .await;
        let n = match r {
            Ok(n) => n,
            Err(e) => return store_failed("retrying a job", e),
        };
        claim.hand_over();
        self.spawn(job_id.to_string(), gen, rx, st);
        Reply::ok(json!({
            "status": "success",
            "message": format!("Retrying {} failed items", n),
            "retry_count": n,
        }))
    }

    /// Web `delete_job`. A paused or queued job's processor is stopped first.
    pub async fn delete(&self, job_id: &str) -> Reply {
        match self.job_status(job_id).await {
            Err(r) => return r,
            Ok(None) => return Reply::error(404, "Job not found"),
            Ok(Some(s)) if s == "running" => {
                return Reply::error(400, "Cannot delete running job. Cancel it first.")
            }
            Ok(Some(_)) => {}
        }
        if let Some((_, ctl, _)) = self.slot(job_id) {
            ctl.send_replace(Ctl::Cancel);
        }
        let id = job_id.to_string();
        match self.inner.db.write(move |c| db::delete_job(c, &id)).await {
            Ok(()) => {
                tracing::info!("Deleted Historify job {}", job_id);
                Reply::ok(
                    json!({"status": "success", "message": format!("Job {} deleted", job_id)}),
                )
            }
            Err(e) => store_failed("deleting a job", e),
        }
    }

    /// Stop every processor without touching job statuses (they come back
    /// paused on the next start). Waits for the tasks to end.
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        for s in self.inner.slots.lock().values() {
            s.ctl.send_replace(Ctl::Cancel);
        }
        let mut tasks = std::mem::take(&mut *self.inner.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.inner.slots.lock().clear();
        *self.inner.hook.write() = None;
    }

    // --------------------------------------------------------- processor

    async fn process(
        self,
        job_id: String,
        gen: u64,
        rx: watch::Receiver<Ctl>,
        st: Arc<AsyncMutex<()>>,
    ) {
        let mut fin = Finished {
            job_id: job_id.clone(),
            config: Value::Null,
            status: "failed".into(),
            total: 0,
            completed: 0,
            failed: 0,
        };
        let r = self.run(&job_id, gen, rx, &st, &mut fin).await;
        if let Err(e) = r {
            tracing::error!("Historify job {} stopped: {}", job_id, e);
            fin.status = "failed".into();
            self.write_status(
                &job_id,
                "failed",
                Some("The download stopped unexpectedly. Retry the job.".into()),
            )
            .await;
        }
        self.release(&job_id, gen);
        if fin
            .config
            .get("schedule_id")
            .and_then(Value::as_str)
            .is_some()
        {
            let hook = self.inner.hook.read().as_ref().and_then(Weak::upgrade);
            if let Some(h) = hook {
                h.finished(fin).await;
            }
        }
    }

    fn emit_paused(&self, job_id: &str, current: i64, total: i64) {
        self.inner.notify.notify(
            "historify_job_paused",
            json!({"job_id": job_id, "current": current, "total": total, "status": "paused"}),
        );
    }

    /// Wait out a pause (or return the cancel). `Ok(())` when running.
    async fn wait_running(
        &self,
        job_id: &str,
        rx: &mut watch::Receiver<Ctl>,
        current: i64,
        total: i64,
    ) -> Result<(), Interrupted> {
        loop {
            let c = *rx.borrow_and_update();
            match c {
                Ctl::Run => return Ok(()),
                Ctl::Cancel => return Err(Interrupted::Cancelled),
                Ctl::Pause => {
                    self.emit_paused(job_id, current, total);
                    tokio::select! {
                        r = rx.changed() => if r.is_err() { return Err(Interrupted::Cancelled) },
                        _ = tokio::time::sleep(self.inner.cfg.pause_heartbeat) => {}
                    }
                }
            }
        }
    }

    async fn run(
        &self,
        job_id: &str,
        gen: u64,
        mut rx: watch::Receiver<Ctl>,
        st: &Arc<AsyncMutex<()>>,
        fin: &mut Finished,
    ) -> crate::error::Result<()> {
        // Queue for a download slot; a cancel while queued ends the job.
        let _permit = tokio::select! {
            p = self.inner.permits.clone().acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => return Ok(()),
            },
            _ = until_cancel(rx.clone()) => {
                fin.status = "cancelled".into();
                return Ok(());
            }
        };

        let id = job_id.to_string();
        let Some(job) = self.inner.db.run(move |c| db::job(c, &id)).await? else {
            tracing::error!("Historify job {} not found", job_id);
            return Ok(());
        };
        fin.config = job
            .get("config")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null);
        {
            let _g = st.lock().await;
            if *rx.borrow() == Ctl::Cancel {
                fin.status = "cancelled".into();
                return Ok(());
            }
            self.write_status(job_id, "running", None).await;
        }

        let id = job_id.to_string();
        let items = self.inner.db.run(move |c| db::items(c, &id)).await?;
        if items.is_empty() {
            self.write_status(job_id, "failed", Some("No items to process".into()))
                .await;
            return Ok(());
        }
        let total = items.len() as i64;
        let already_completed = items.iter().filter(|i| i.status == "success").count() as i64;
        let already_failed = items.iter().filter(|i| i.status == "error").count() as i64;
        let (mut completed, mut failed) = (already_completed, already_failed);
        let mut processed = already_completed + already_failed;
        fin.total = total;
        fin.completed = completed;
        fin.failed = failed;

        let incremental = fin
            .config
            .get("incremental")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let interval = job["interval"].as_str().unwrap_or("D").to_string();
        let start = job["start_date"].as_str().and_then(parse_date);
        let end = job["end_date"].as_str().and_then(parse_date);

        let pending: Vec<Item> = items
            .into_iter()
            .filter(|i| i.status == "pending" || i.status == "downloading")
            .collect();

        for item in pending {
            if self
                .wait_running(job_id, &mut rx, processed, total)
                .await
                .is_err()
            {
                tracing::info!("Historify job {} cancelled", job_id);
                fin.status = "cancelled".into();
                return Ok(());
            }

            let (iid, now) = (item.id, self.now());
            self.inner
                .db
                .write(move |c| db::set_item_status(c, iid, "downloading", 0, None, now))
                .await?;
            processed += 1;
            self.inner.notify.notify(
                "historify_progress",
                json!({
                    "job_id": job_id,
                    "current": processed,
                    "total": total,
                    "symbol": item.symbol,
                    "percent": ((processed as f64 / total as f64) * 1000.0).round() / 10.0,
                }),
            );

            let outcome = self
                .download_item(&rx, &item, &interval, start, end, incremental)
                .await;
            let (status, records, error) = match outcome {
                Err(Interrupted::Cancelled) => {
                    // Not finished: leave it for a retry or resume.
                    let iid = item.id;
                    self.inner
                        .db
                        .write(move |c| {
                            db::set_item_status(c, iid, "pending", 0, None, Default::default())
                        })
                        .await?;
                    fin.status = "cancelled".into();
                    return Ok(());
                }
                Ok(ItemOutcome::Skipped(msg)) => ("skipped", 0, Some(msg)),
                Ok(ItemOutcome::Done(Ok(n))) => {
                    completed += 1;
                    ("success", n, None)
                }
                Ok(ItemOutcome::Done(Err((n, msg)))) => {
                    failed += 1;
                    ("error", n, Some(msg))
                }
            };
            let (iid, id, now) = (item.id, job_id.to_string(), self.now());
            let progress = status != "skipped";
            self.inner
                .db
                .write(move |c| {
                    db::set_item_status(c, iid, status, records, error.as_deref(), now)?;
                    if progress {
                        db::set_job_progress(c, &id, completed, failed)?;
                    }
                    Ok(())
                })
                .await?;
            fin.completed = completed;
            fin.failed = failed;
            if !progress {
                tracing::info!(
                    "Skipping {} - data already covers requested range",
                    item.symbol
                );
                continue;
            }

            let cfg = &self.inner.cfg;
            self.pause_between(&rx, jitter(cfg.delay_min, cfg.delay_max))
                .await;
            let done_this_run = processed - already_completed - already_failed;
            if cfg.batch_size > 0 && done_this_run > 0 && done_this_run % cfg.batch_size as i64 == 0
            {
                let d = jitter(cfg.batch_cooldown_min, cfg.batch_cooldown_max);
                tracing::info!(
                    "Historify job {}: cooldown after {} symbols, waiting {:.1}s",
                    job_id,
                    done_this_run,
                    d.as_secs_f64()
                );
                self.pause_between(&rx, d).await;
            }
        }

        // Claim the terminal state under the status lock; honour a pause
        // that landed after the last item.
        loop {
            let g = st.lock().await;
            let c = *rx.borrow_and_update();
            match c {
                Ctl::Cancel => {
                    fin.status = "cancelled".into();
                    return Ok(());
                }
                Ctl::Pause => {
                    drop(g);
                    self.emit_paused(job_id, processed, total);
                    tokio::select! {
                        r = rx.changed() => if r.is_err() {
                            fin.status = "cancelled".into();
                            return Ok(());
                        },
                        _ = tokio::time::sleep(self.inner.cfg.pause_heartbeat) => {}
                    }
                }
                Ctl::Run => {
                    let status = if failed == 0 {
                        "completed"
                    } else {
                        "completed_with_errors"
                    };
                    self.write_status(job_id, status, None).await;
                    self.release(job_id, gen);
                    drop(g);
                    fin.status = status.into();
                    self.inner.notify.notify(
                        "historify_job_complete",
                        json!({
                            "job_id": job_id,
                            "completed": completed,
                            "failed": failed,
                            "total": total,
                            "status": status,
                        }),
                    );
                    tracing::info!(
                        "Historify job {} completed: {} success, {} failed",
                        job_id,
                        completed,
                        failed
                    );
                    return Ok(());
                }
            }
        }
    }

    /// Sleep `d`, ending early on a cancel.
    async fn pause_between(&self, rx: &watch::Receiver<Ctl>, d: Duration) {
        if d.is_zero() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = until_cancel(rx.clone()) => {}
        }
    }

    /// One broker download into the store: `Ok(records)` or the reason.
    async fn download_range(
        &self,
        rx: &watch::Receiver<Ctl>,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Result<i64, String>, Interrupted> {
        if !interval::is_storage(interval) {
            return Ok(Err(format!(
                "Only {} intervals can be downloaded. Other timeframes ({}) are computed from 1m data.",
                interval::sorted(interval::STORAGE_INTERVALS).join(", "),
                interval::sorted(interval::COMPUTED_INTERVALS).join(", ")
            )));
        }
        let fetch = async {
            self.inner.pacer.acquire().await;
            self.inner
                .source
                .fetch(symbol, exchange, interval, start, end)
                .await
        };
        let candles = tokio::select! {
            r = fetch => r,
            _ = until_cancel(rx.clone()) => return Err(Interrupted::Cancelled),
        };
        let candles = match candles {
            Ok(c) => c,
            Err(m) => return Ok(Err(m)),
        };
        if candles.is_empty() {
            return Ok(Ok(0));
        }
        let (s, e, i, now) = (
            symbol.to_string(),
            exchange.to_string(),
            interval.to_string(),
            self.now(),
        );
        match self
            .inner
            .db
            .write(move |c| db::upsert_bars(c, &s, &e, &i, &candles, now))
            .await
        {
            Ok(n) => Ok(Ok(n as i64)),
            Err(err) => {
                tracing::error!(
                    "Saving {}:{} {} candles failed: {}",
                    exchange,
                    symbol,
                    interval,
                    err
                );
                Ok(Err(SAVE_FAILED.to_string()))
            }
        }
    }

    /// Web item processing, including the incremental before/after ranges.
    async fn download_item(
        &self,
        rx: &watch::Receiver<Ctl>,
        item: &Item,
        interval: &str,
        start: Option<NaiveDate>,
        end: Option<NaiveDate>,
        incremental: bool,
    ) -> Result<ItemOutcome, Interrupted> {
        let (Some(start), Some(end)) = (start, end) else {
            return Ok(ItemOutcome::Done(Err((
                0,
                "This download has no valid date range. Create it again with a start and end date."
                    .into(),
            ))));
        };
        let (sym, exch) = (item.symbol.as_str(), item.exchange.as_str());
        if incremental {
            let (s, e, i) = (sym.to_string(), exch.to_string(), interval.to_string());
            let range = self
                .inner
                .db
                .run(move |c| db::data_range(c, &s, &e, &i))
                .await
                .unwrap_or_else(|err| {
                    tracing::error!(
                        "Reading the stored range of {}:{} failed: {}",
                        exch,
                        sym,
                        err
                    );
                    None
                });
            if let Some((Some(first_ts), Some(last_ts), _)) =
                range.filter(|r| r.0.is_some_and(|v| v != 0) && r.1.is_some_and(|v| v != 0))
            {
                let (first, last) = (ist_day(first_ts), ist_day(last_ts));
                let minute = interval == "1m";
                let need_before = start < first;
                let need_after = if minute { end >= last } else { end > last };
                if !need_before && !need_after {
                    return Ok(ItemOutcome::Skipped(
                        "Data already covers requested range".into(),
                    ));
                }
                let mut records = 0i64;
                let mut error: Option<String> = None;
                if need_before {
                    let before_end = if minute {
                        first
                    } else {
                        first - CDuration::days(1)
                    };
                    if start <= before_end {
                        match self
                            .download_range(rx, sym, exch, interval, start, before_end)
                            .await?
                        {
                            Ok(n) => records += n,
                            Err(m) => error = Some(m),
                        }
                    }
                }
                if need_after && error.is_none() {
                    let after_start = if minute {
                        last
                    } else {
                        last + CDuration::days(1)
                    };
                    if after_start <= end {
                        match self
                            .download_range(rx, sym, exch, interval, after_start, end)
                            .await?
                        {
                            Ok(n) => records += n,
                            Err(m) => error = Some(m),
                        }
                    }
                }
                return Ok(ItemOutcome::Done(match error {
                    Some(m) => Err((records, m)),
                    None => Ok(records),
                }));
            }
        }
        Ok(ItemOutcome::Done(
            self.download_range(rx, sym, exch, interval, start, end)
                .await?
                .map_err(|m| (0, m)),
        ))
    }

    /// Web `download_data` for one symbol, outside any job (`/api/download`).
    pub async fn download_now(
        &self,
        symbol: &str,
        exchange: &str,
        interval: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<i64, String> {
        let (_tx, rx) = watch::channel(Ctl::Run);
        match self
            .download_range(&rx, symbol, exchange, interval, start, end)
            .await
        {
            Ok(r) => r,
            Err(Interrupted::Cancelled) => Err("The download was stopped.".into()),
        }
    }
}

enum ItemOutcome {
    Skipped(String),
    Done(Result<i64, (i64, String)>),
}
