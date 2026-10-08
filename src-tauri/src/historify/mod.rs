//! Historify: the local DuckDB history store, its download jobs and
//! schedules (web `services/historify_service.py`,
//! `services/historify_scheduler_service.py`, `database/historify_db.py`).
//!
//! [`Historify`] owns everything that lives for the process: the store
//! handle, the job engine (one owned task per active job), the scheduler
//! driver and the exports waiting for download. [`Historify::shutdown`]
//! stops the tasks, waits for in-flight database work, deletes pending
//! export files and closes the database.

pub mod db;
pub mod export;
pub mod import;
pub mod interval;
pub mod jobs;
pub mod scheduler;
pub mod service;
pub mod source;
pub mod time;

use crate::clock::Clock;
use crate::db::duckdb::HistorifyDb;
use export::ExportSlots;
use jobs::{EngineConfig, JobEngine};
use scheduler::Scheduler;
use source::{HistorySource, Notifier};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Exchanges Historify accepts (web `SUPPORTED_EXCHANGES`).
pub const SUPPORTED_EXCHANGES: &[&str] = &[
    "NSE",
    "BSE",
    "NFO",
    "BFO",
    "MCX",
    "CDS",
    "BCD",
    "NCO",
    "NSE_INDEX",
    "BSE_INDEX",
    "MCX_INDEX",
    "GLOBAL_INDEX",
    "CRYPTO",
];

pub struct Historify {
    pub db: HistorifyDb,
    pub jobs: JobEngine,
    pub scheduler: Scheduler,
    pub exports: ExportSlots,
    work_dir: PathBuf,
    clock: Arc<dyn Clock>,
}

impl Historify {
    /// Build on an open store. Interrupted jobs come back paused and
    /// active schedules are restored; call [`Self::start`] for the driver.
    pub fn new(
        db: HistorifyDb,
        source: Arc<dyn HistorySource>,
        notify: Arc<dyn Notifier>,
        clock: Arc<dyn Clock>,
        work_dir: &Path,
        cfg: EngineConfig,
    ) -> Self {
        // Exports and uploads left by a previous run are not wanted.
        if work_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(work_dir) {
                tracing::warn!("Could not clear old Historify work files: {}", e);
            }
        }
        let jobs = JobEngine::new(db.clone(), source, notify.clone(), clock.clone(), cfg);
        let scheduler = Scheduler::new(db.clone(), jobs.clone(), notify, clock.clone());
        Self {
            db,
            jobs,
            scheduler,
            exports: ExportSlots::default(),
            work_dir: work_dir.to_path_buf(),
            clock,
        }
    }

    /// Start the scheduler driver (inside a Tokio runtime).
    pub fn start(&self) {
        self.scheduler.start();
    }

    /// Directory for export and upload files (created on demand).
    pub fn work_dir(&self) -> std::io::Result<&Path> {
        std::fs::create_dir_all(&self.work_dir)?;
        Ok(&self.work_dir)
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    /// Stop the scheduler and every job, delete pending exports, refuse new
    /// database work, wait (bounded) for work still running on the blocking
    /// pool, close the store.
    pub async fn shutdown(&self) {
        self.scheduler.shutdown().await;
        self.jobs.shutdown().await;
        self.exports.clear();
        // Database work queued by an aborted task may still run on the
        // blocking pool; sealed, it cannot borrow a connection after the
        // wait below.
        self.db.seal();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.db.open_connections() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if self.db.open_connections() > 0 {
            tracing::warn!(
                "{} Historify database connection(s) still busy at shutdown",
                self.db.open_connections()
            );
        }
        self.db.close();
    }
}
