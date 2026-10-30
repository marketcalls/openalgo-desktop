//! Flow's trigger side: the scheduler, the price monitor, the order-update
//! monitor and the feed the subscribe nodes read (the web's
//! `flow_scheduler_service.py`, `flow_price_monitor_service.py`,
//! `flow_order_update_monitor_service.py` and the blueprint's
//! `_register_trigger` / `_unregister_trigger`), installed on the core's
//! [`crate::flow::FlowModule`] as its [`TriggerHooks`] at start-up.
//!
//! | File | What |
//! |---|---|
//! | `scheduler` | schedule types, next fire times in IST, the jobs, `tick` |
//! | `price_monitor` | price alerts: conditions, latch, expiry, one run at a time |
//! | `order_monitor` | order-update watches: matching, claim, follow-up |
//! | `feed` | the market feed the subscribe nodes read |
//! | `drive` | the scheduler driver, the 5 s price poll, the order-update subscriber |
//! | `runs` | starting a triggered run, and the skip while OpenAlgo is signed out |
//! | `restore` | re-registering the active workflows at start-up |
//! | `store` | the trigger side's reads and writes of `flow_workflows` |
//!
//! Every task here is owned ([`TriggerSide::spawn`]) and ends at app exit
//! ([`TriggerHooks::stop`]); a schedule, alert or watch lives as long as its
//! workflow is active. Signing out does not stand a workflow down: its
//! triggers skip, each skip written to the workflow's run history with the
//! reason, and run again after sign-in; with no broker logged in they run
//! and each broker call fails at its node (lead decision 16, `runs`). A
//! one-time trigger that has fired, an
//! expired alert and a trigger that cannot be restored stand the workflow
//! down through one path that also gives back its feed subscriptions
//! (T-043).

pub mod drive;
pub mod feed;
pub mod order_monitor;
pub mod price_monitor;
pub mod restore;
pub mod runs;
pub mod scheduler;
pub mod store;

use crate::clock::Clock;
use crate::flow::contract::subscriptions::FeedRelease;
use crate::flow::contract::triggers::{TriggerError, TriggerHooks};
use crate::state::AppState;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Weak};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// The trigger side.
pub struct TriggerSide {
    app: Weak<AppState>,
    clock: Arc<dyn Clock>,
    pub scheduler: scheduler::Scheduler,
    pub alerts: price_monitor::Alerts,
    pub watches: order_monitor::Watches,
    pub feed: Arc<feed::AppFeed>,
    /// The driver, the poll and every monitor run.
    tasks: Mutex<JoinSet<()>>,
    stop: CancellationToken,
    /// A job was added: the driver recomputes its sleep.
    wake_scheduler: Notify,
    /// An alert was added: the poll starts.
    wake_prices: Notify,
    /// The web's worker pools: at most this many monitor runs at once.
    price_runs: Arc<Semaphore>,
    order_runs: Arc<Semaphore>,
}

impl TriggerSide {
    pub fn new(app: Weak<AppState>, clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            feed: Arc::new(feed::AppFeed::new(app.clone())),
            app,
            clock,
            scheduler: Default::default(),
            alerts: Default::default(),
            watches: Default::default(),
            tasks: Mutex::new(JoinSet::new()),
            stop: CancellationToken::new(),
            wake_scheduler: Notify::new(),
            wake_prices: Notify::new(),
            price_runs: Arc::new(Semaphore::new(price_monitor::WORKERS)),
            order_runs: Arc::new(Semaphore::new(order_monitor::WORKERS)),
        })
    }

    /// Install on the app at start-up (after the database is ready): the
    /// hooks, the feed, the order-update subscriber, the driver and the
    /// poll, then the restore of the active workflows (`app.py` 850-880).
    pub fn install(app: &Arc<AppState>) -> Arc<Self> {
        let side = Self::new(Arc::downgrade(app), app.clock.clone());
        app.flow.set_triggers(side.clone());
        let feed = side.feed.clone();
        app.flow.install_env(|e| e.feed = feed);
        drive::subscribe_order_updates(app, &side);
        side.watches.attach();
        side.start_drivers();
        let me = side.clone();
        side.spawn(async move { me.restore().await });
        side
    }

    /// The scheduler driver and the price poll.
    pub fn start_drivers(self: &Arc<Self>) {
        let me = self.clone();
        self.spawn(async move { me.drive_schedules().await });
        let me = self.clone();
        self.spawn(async move { me.poll_prices().await });
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }

    fn app(&self) -> Option<Arc<AppState>> {
        self.app.upgrade()
    }

    /// Run `fut` as a task this side owns (ended at app exit).
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if self.stop.is_cancelled() {
            return;
        }
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    /// Tasks alive now (the leak audit).
    pub fn task_count(&self) -> usize {
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.len()
    }

    /// Stand a workflow down for `reason` (a one-time trigger that fired,
    /// an expired alert, a trigger that could not be restored): its feed
    /// subscriptions given back, every registration removed, the row
    /// inactive. One path for every cause (T-043; the web skipped the
    /// subscriptions here).
    pub async fn stand_down(&self, workflow_id: i64, reason: &str) {
        let failed = format!("Could not deactivate workflow {} ({})", workflow_id, reason);
        self.deactivate_logged(workflow_id, reason, &failed).await;
    }

    /// A one-shot `what` (`price alert`, `order-update watch`) that fired:
    /// stood down with the web's sentences for both outcomes.
    pub async fn spend_one_shot(&self, workflow_id: i64, what: &str) {
        let done = format!(
            "One-shot {} for workflow {} consumed; workflow deactivated.",
            what, workflow_id
        );
        let failed = format!(
            "Could not deactivate workflow {} after its one-shot {} fired",
            workflow_id, what
        );
        self.deactivate_logged(workflow_id, &done, &failed).await;
    }

    async fn deactivate_logged(&self, workflow_id: i64, reason: &str, failed: &str) {
        let Some(app) = self.app() else {
            return;
        };
        app.flow.subscriptions.release(workflow_id, self).await;
        self.scheduler.remove(workflow_id);
        self.alerts.remove(workflow_id);
        self.watches.remove(workflow_id);
        let now = self.now();
        let done = app
            .blocking(move |c| store::deactivate(&*c.sqlite.conn()?, workflow_id, now))
            .await;
        match done {
            Ok(_) => tracing::info!("{}", reason),
            Err(e) => tracing::error!("{}: {}", failed, e),
        }
    }

    /// Record a scheduler job's id and next run; a failure undoes the job
    /// (`set_schedule_job_id` falsy: nothing could find the job again).
    async fn record_job(
        &self,
        workflow_id: i64,
        next: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), TriggerError> {
        let Some(app) = self.app() else {
            return Err(TriggerError::Failed("OpenAlgo is closing".into()));
        };
        let id = scheduler::job_id(workflow_id);
        let saved = app
            .blocking(move |c| {
                let conn = c.sqlite.conn()?;
                let found = store::set_schedule_job_id(&conn, workflow_id, Some(&id), c.now())?;
                store::set_next_run(&conn, workflow_id, next)?;
                Ok(found)
            })
            .await;
        if !matches!(saved, Ok(true)) {
            if let Err(e) = saved {
                tracing::error!(
                    "Could not record the schedule of workflow {}: {}",
                    workflow_id,
                    e
                );
            }
            self.scheduler.remove(workflow_id);
            return Err(TriggerError::Failed(
                "Could not record the scheduler job id for this workflow".into(),
            ));
        }
        Ok(())
    }

    /// `_register_trigger` for a start node.
    async fn register_schedule(
        &self,
        workflow_id: i64,
        data: &Map<String, Value>,
    ) -> Result<(), TriggerError> {
        if scheduler::scheduled_kind(data).is_none() {
            return Ok(());
        }
        // `add_workflow_job` clears the workflow's previous job first.
        self.scheduler.remove(workflow_id);
        let now = self.now();
        let Some(schedule) =
            scheduler::Schedule::from_node(data, now).map_err(TriggerError::Invalid)?
        else {
            return Ok(());
        };
        let gated = data
            .get("marketHoursOnly")
            .is_some_and(crate::flow::contract::pyfmt::truthy);
        let next = self.scheduler.add(workflow_id, schedule, gated, now);
        self.record_job(workflow_id, next).await?;
        self.wake_scheduler.notify_one();
        Ok(())
    }
}

#[async_trait]
impl FeedRelease for TriggerSide {
    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> bool {
        crate::flow::contract::env::MarketFeed::unsubscribe(
            self.feed.as_ref(),
            symbol,
            exchange,
            mode,
        )
        .await
        .is_ok()
    }
}

#[async_trait]
impl TriggerHooks for TriggerSide {
    async fn register(
        &self,
        workflow_id: i64,
        trigger_type: &str,
        data: &Map<String, Value>,
    ) -> Result<(), TriggerError> {
        match trigger_type {
            "start" => self.register_schedule(workflow_id, data).await,
            "priceAlert" => {
                let alert = price_monitor::Alert::from_node(workflow_id, data, self.now())
                    .map_err(TriggerError::Invalid)?;
                self.alerts.add(alert);
                self.wake_prices.notify_one();
                Ok(())
            }
            "orderUpdateTrigger" => {
                let watch = order_monitor::Watch::from_node(workflow_id, data)
                    .map_err(TriggerError::Invalid)?;
                self.watches.add(watch);
                Ok(())
            }
            // A webhook trigger registers nothing: the webhook is its own
            // switch (`/webhook/enable`).
            _ => Ok(()),
        }
    }

    async fn unregister(&self, workflow_id: i64) -> Result<(), String> {
        self.scheduler.remove(workflow_id);
        if self.alerts.remove(workflow_id) {
            self.wake_prices.notify_one();
        }
        self.watches.remove(workflow_id);
        if let Some(app) = self.app() {
            let cleared = app
                .blocking(move |c| store::set_next_run(&*c.sqlite.conn()?, workflow_id, None))
                .await;
            if let Err(e) = cleared {
                tracing::warn!(
                    "Could not clear the next run of workflow {}: {}",
                    workflow_id,
                    e
                );
            }
        }
        Ok(())
    }

    fn status(&self) -> Value {
        json!({
            "running": !self.alerts.is_empty() && !self.stop.is_cancelled(),
            "alerts_count": self.alerts.len(),
            "poll_interval": price_monitor::POLL_INTERVAL.as_secs(),
            "alerts": self.alerts.status(),
            "order_updates": self.watches.status(),
        })
    }

    async fn reset(&self) {
        self.scheduler.clear();
        self.alerts.clear();
        self.watches.clear();
        let Some(app) = self.app() else {
            return;
        };
        let ids = app
            .blocking(|c| crate::flow::store::workflows::list(&*c.sqlite.conn()?))
            .await
            .map(|rows| rows.into_iter().map(|w| w.id).collect::<Vec<_>>())
            .unwrap_or_default();
        for id in ids {
            app.flow.subscriptions.release(id, self).await;
        }
    }

    async fn stop(&self) {
        // The web's shutdowns are idempotent: a second stop writes nothing.
        let first = !self.stop.is_cancelled();
        self.stop.cancel();
        self.watches.detach();
        self.scheduler.clear();
        self.alerts.clear();
        // The runs first, so a monitor's run ends and its one-shot is
        // retired before the monitor's own task is asked to finish.
        if let Some(app) = self.app() {
            app.flow.stop_all(crate::flow::STOP_GRACE).await;
        }
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        let drained = tokio::time::timeout(crate::flow::STOP_GRACE, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        // After the poll's own `Price monitoring stopped`, as the web's
        // `_stop_monitoring` comes before its shutdown line.
        if first {
            tracing::info!("Flow Scheduler shutdown");
            tracing::info!("FlowPriceMonitor shutdown");
            tracing::info!("FlowOrderUpdateMonitor shutdown");
        }
    }
}

#[cfg(test)]
mod tests;
