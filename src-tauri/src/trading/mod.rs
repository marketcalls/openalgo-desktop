//! The /trading terminal's server side: the trader's custom chart indicators,
//! their OpenScript files, the instrument facts the engine reads, and the
//! OpenScript live runner.
//!
//! Everything the trader writes lives in the app data directory:
//! `indicators/` (chart modules, served at runtime, never bundled),
//! `openscript/` (sources and the compiled program beside each) and
//! `openscript_logs/` (one log per run).

pub mod indicators;
pub mod instrument;
pub mod names;
pub mod runner;
pub mod scripts;

use crate::clock::Clock;
use crate::db::sqlite::SqliteDb;
use crate::events::{Event, Lane, SessionEndReason, Subscriber, Topic};
use runner::services::RunnerServices;
use runner::Runner;
use scripts::ScriptStore;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

pub struct Trading {
    pub indicators_dir: PathBuf,
    pub scripts: Arc<ScriptStore>,
    pub facts: instrument::FactsCache,
    pub runner: Arc<Runner>,
}

impl Trading {
    pub fn new(
        data_dir: &Path,
        db: Arc<SqliteDb>,
        clock: Arc<dyn Clock>,
        services: Arc<dyn RunnerServices>,
    ) -> Self {
        let scripts = Arc::new(ScriptStore::new(data_dir.join("openscript")));
        Self {
            indicators_dir: data_dir.join("indicators"),
            runner: Runner::new(
                db,
                scripts.clone(),
                data_dir.join("openscript_logs"),
                clock,
                services,
            ),
            scripts,
            facts: instrument::FactsCache::default(),
        }
    }

    /// Start the schedule task and prune old order rows.
    pub fn start(&self, now: chrono::DateTime<chrono::Utc>, db: &SqliteDb) {
        if let Ok(conn) = db.conn() {
            if let Err(e) = runner::store::prune_orders(&conn, now) {
                tracing::warn!("Could not prune old OpenScript orders: {}", e);
            }
        }
        self.runner.start_scheduler();
    }

    /// Pause every run and stop the scheduler.
    pub async fn shutdown(&self) {
        self.runner.shutdown().await;
    }
}

/// Pauses every run when the trader signs out: a run started by a person is
/// not left trading after that person has left.
pub struct LogoutTeardown {
    runner: Weak<Runner>,
}

impl LogoutTeardown {
    pub fn new(runner: &Arc<Runner>) -> Self {
        Self {
            runner: Arc::downgrade(runner),
        }
    }
}

#[async_trait::async_trait]
impl Subscriber for LogoutTeardown {
    fn name(&self) -> &'static str {
        "openscript_logout_teardown"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::ForceLogout, Topic::BrokerSessionEnded]
    }

    async fn handle(&self, event: Arc<Event>) {
        let logout = match &*event {
            Event::ForceLogout { .. } => true,
            Event::BrokerSessionEnded { reason } => matches!(reason, SessionEndReason::Logout),
            _ => false,
        };
        if !logout {
            return;
        }
        if let Some(r) = self.runner.upgrade() {
            let paused = r.pause_all("Paused because you signed out");
            if !paused.is_empty() {
                tracing::info!("Paused {} OpenScript run(s) on sign-out", paused.len());
            }
        }
    }
}

/// Register the trading subscribers on the app's bus.
pub fn register(ctx: &Arc<crate::state::AppState>) {
    ctx.bus.subscribe(
        Arc::new(LogoutTeardown::new(&ctx.trading.runner)),
        Lane::Critical,
    );
}
