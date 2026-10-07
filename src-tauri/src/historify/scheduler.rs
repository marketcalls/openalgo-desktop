//! Scheduled downloads (web `historify_scheduler_service.py`, APScheduler).
//!
//! Each enabled, unpaused schedule has an in-memory entry with its trigger
//! and next fire time (mirrored to `historify_schedules.next_run_at`, so a
//! restart picks up where it left off). Interval schedules fire every N
//! minutes or hours from when they were added; daily schedules at HH:MM
//! IST. Misfires follow the web's APScheduler settings: a run that is late
//! by up to five minutes still runs once (missed runs coalesce), a later
//! one is skipped and the next fire time is computed from now.
//!
//! [`Scheduler::tick`] does the work for a given instant, so tests drive it
//! with an injected clock; the driver task only sleeps until the next due
//! time and calls it.

use super::db::{self, ExecutionUpdate, NewSchedule, ScheduleUpdate};
use super::jobs::{CreateJob, Finished, JobEngine, JobHook};
use super::source::Notifier;
use super::time::{from_ist, iso_ist, ist_naive};
use crate::clock::Clock;
use crate::db::duckdb::HistorifyDb;
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// APScheduler `misfire_grace_time`.
pub const MISFIRE_GRACE_SECS: i64 = 300;
/// The driver never sleeps longer than this, so a clock change is noticed.
const MAX_SLEEP_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Every `every`, on the grid that starts at `anchor`.
    Interval {
        every: Duration,
        anchor: DateTime<Utc>,
    },
    /// Every day at this IST wall-clock time.
    Daily { at: NaiveTime },
}

impl Trigger {
    /// From a schedule row; `None` when it cannot fire.
    pub fn from_row(row: &Value, now: DateTime<Utc>) -> Option<Self> {
        match row["schedule_type"].as_str()? {
            "interval" => {
                let v = row["interval_value"].as_i64().unwrap_or(1).max(1);
                let every = if row["interval_unit"].as_str() == Some("hours") {
                    Duration::hours(v)
                } else {
                    Duration::minutes(v)
                };
                // APScheduler's IntervalTrigger starts one interval from now.
                Some(Trigger::Interval {
                    every,
                    anchor: now + every,
                })
            }
            "daily" => {
                let t = row["time_of_day"].as_str().unwrap_or("09:15");
                Some(Trigger::Daily { at: parse_hhmm(t)? })
            }
            _ => None,
        }
    }

    /// The first fire time at or after `now`.
    pub fn next_at_or_after(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match *self {
            Trigger::Interval { every, anchor } => {
                if now <= anchor {
                    return anchor;
                }
                let step = every.num_seconds().max(1);
                let behind = (now - anchor).num_seconds();
                let k = (behind + step - 1) / step;
                anchor + Duration::seconds(k * step)
            }
            Trigger::Daily { at } => {
                let today = now.with_timezone(&Kolkata).date_naive();
                let t = from_ist(today.and_time(at));
                if t >= now {
                    t
                } else {
                    from_ist((today + Duration::days(1)).and_time(at))
                }
            }
        }
    }

    /// The next fire time strictly after `now`.
    pub fn next_after(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        self.next_at_or_after(now + Duration::seconds(1))
    }
}

/// `HH:MM` with the web's checks (hour 0-23, minute 0-59).
pub fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let mut parts = s.split(':');
    let (h, m) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (h, m): (u32, u32) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    if h > 23 || m > 59 {
        return None;
    }
    NaiveTime::from_hms_opt(h, m, 0)
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    trigger: Trigger,
    /// `None` while paused.
    next: Option<DateTime<Utc>>,
}

/// What [`Scheduler::tick`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    pub fired: Vec<String>,
    pub missed: Vec<String>,
}

struct Inner {
    db: HistorifyDb,
    engine: JobEngine,
    notify: Arc<dyn Notifier>,
    clock: Arc<dyn Clock>,
    entries: Mutex<HashMap<String, Entry>>,
    active_runs: Mutex<HashSet<String>>,
    wake: Notify,
    stop: CancellationToken,
    driver: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

/// Fields of a new schedule, already validated by the route.
#[derive(Debug, Clone, Default)]
pub struct AddSchedule {
    pub name: String,
    pub schedule_type: String,
    pub data_interval: String,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub time_of_day: Option<String>,
    pub lookback_days: i64,
    pub description: Option<String>,
}

impl Scheduler {
    pub fn new(
        db: HistorifyDb,
        engine: JobEngine,
        notify: Arc<dyn Notifier>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let s = Self {
            inner: Arc::new(Inner {
                db,
                engine: engine.clone(),
                notify,
                clock,
                entries: Mutex::new(HashMap::new()),
                active_runs: Mutex::new(HashSet::new()),
                wake: Notify::new(),
                stop: CancellationToken::new(),
                driver: Mutex::new(None),
            }),
        };
        // The engine holds a weak reference to the same allocation as
        // `inner`, so the hook lives exactly as long as the scheduler.
        let hook: Arc<dyn JobHook> = s.inner.clone();
        engine.set_hook(Arc::downgrade(&hook));
        s.restore();
        s
    }

    fn now(&self) -> DateTime<Utc> {
        self.inner.clock.now()
    }

    /// Web `_restore_schedules`, plus clearing a `running` status a previous
    /// run left behind.
    fn restore(&self) {
        let now = self.now();
        let rows = match self.inner.db.mutate(|c| {
            c.execute(
                "UPDATE historify_schedules SET status = 'idle' WHERE status = 'running'",
                [],
            )?;
            db::schedules(c, true)
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Could not restore Historify schedules: {}", e);
                return;
            }
        };
        let mut restored = 0;
        for row in rows {
            let Some(id) = row["id"].as_str() else {
                continue;
            };
            let Some(mut trigger) = Trigger::from_row(&row, now) else {
                tracing::warn!("Historify schedule {} has an invalid trigger", id);
                continue;
            };
            let stored = row["next_run_at"]
                .as_str()
                .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
                .map(from_ist);
            if let (Trigger::Interval { anchor, .. }, Some(t)) = (&mut trigger, stored) {
                *anchor = t;
            }
            let next = stored.unwrap_or_else(|| trigger.next_at_or_after(now));
            self.inner.entries.lock().insert(
                id.to_string(),
                Entry {
                    trigger,
                    next: Some(next),
                },
            );
            restored += 1;
        }
        if restored > 0 {
            tracing::info!("Restored {} Historify schedules", restored);
        }
    }

    /// Start the driver task (owned; stopped by [`Self::shutdown`]).
    pub fn start(&self) {
        let me = self.clone();
        let h = tokio::spawn(async move { me.drive().await });
        if let Some(old) = self.inner.driver.lock().replace(h) {
            old.abort();
        }
    }

    async fn drive(&self) {
        loop {
            self.tick(self.now()).await;
            let now = self.now();
            let next = self
                .inner
                .entries
                .lock()
                .values()
                .filter_map(|e| e.next)
                .min();
            let secs = next
                .map(|t| (t - now).num_seconds().clamp(0, MAX_SLEEP_SECS))
                .unwrap_or(MAX_SLEEP_SECS);
            let d = std::time::Duration::from_secs(secs as u64)
                .max(std::time::Duration::from_millis(200));
            tokio::select! {
                _ = tokio::time::sleep(d) => {}
                _ = self.inner.wake.notified() => {}
                _ = self.inner.stop.cancelled() => return,
            }
        }
    }

    pub async fn shutdown(&self) {
        self.inner.stop.cancel();
        let h = self.inner.driver.lock().take();
        if let Some(h) = h {
            h.abort();
            let _ = h.await;
        }
    }

    pub fn driver_running(&self) -> bool {
        self.inner
            .driver
            .lock()
            .as_ref()
            .is_some_and(|h| !h.is_finished())
    }

    /// Fire every due schedule at `now`; skip the ones late beyond grace.
    pub async fn tick(&self, now: DateTime<Utc>) -> TickReport {
        let mut report = TickReport::default();
        let due: Vec<(String, DateTime<Utc>, Trigger)> = self
            .inner
            .entries
            .lock()
            .iter()
            .filter_map(|(id, e)| match e.next {
                Some(t) if t <= now => Some((id.clone(), t, e.trigger)),
                _ => None,
            })
            .collect();
        for (id, scheduled, trigger) in due {
            let next = trigger.next_after(now);
            if let Some(e) = self.inner.entries.lock().get_mut(&id) {
                e.next = Some(next);
            }
            self.persist_next(&id, Some(next));
            let late = (now - scheduled).num_seconds();
            if late > MISFIRE_GRACE_SECS {
                tracing::warn!(
                    "Historify schedule {} missed its run at {} by {} s; next run {}",
                    id,
                    iso_ist(scheduled),
                    late,
                    iso_ist(next)
                );
                report.missed.push(id);
                continue;
            }
            self.execute(&id).await;
            report.fired.push(id);
        }
        report
    }

    fn persist_next(&self, id: &str, next: Option<DateTime<Utc>>) {
        let id = id.to_string();
        let r = self.inner.db.mutate(|c| match next {
            Some(t) => db::update_schedule(
                c,
                &id,
                &ScheduleUpdate {
                    next_run_at: Some(ist_naive(t)),
                    ..Default::default()
                },
            )
            .map(|_| ()),
            None => db::clear_next_run(c, &id),
        });
        if let Err(e) = r {
            tracing::error!("Could not record the next run of schedule {}: {}", id, e);
        }
    }

    /// (Re)add a schedule's entry from its row (web `_add_schedule_job`).
    fn add_entry(&self, row: &Value) -> bool {
        let Some(id) = row["id"].as_str() else {
            return false;
        };
        let now = self.now();
        let Some(trigger) = Trigger::from_row(row, now) else {
            tracing::error!("Invalid trigger for Historify schedule {}", id);
            return false;
        };
        let next = trigger.next_at_or_after(now);
        self.inner.entries.lock().insert(
            id.to_string(),
            Entry {
                trigger,
                next: Some(next),
            },
        );
        let id_s = id.to_string();
        let job_id = format!("historify_schedule_{}", id);
        if let Err(e) = self.inner.db.mutate(|c| {
            db::update_schedule(
                c,
                &id_s,
                &ScheduleUpdate {
                    next_run_at: Some(ist_naive(next)),
                    apscheduler_job_id: Some(job_id),
                    ..Default::default()
                },
            )
            .map(|_| ())
        }) {
            tracing::error!("Could not record the schedule's next run: {}", e);
        }
        self.inner.wake.notify_one();
        true
    }

    fn remove_entry(&self, id: &str) {
        self.inner.entries.lock().remove(id);
    }

    /// The next fire time held for a schedule (web `get_next_run_time`).
    pub fn next_run_time(&self, id: &str) -> Option<DateTime<Utc>> {
        self.inner.entries.lock().get(id).and_then(|e| e.next)
    }

    /// Schedule rows with the live next run time, as the routes serve them.
    pub fn enrich(&self, mut row: Value) -> Value {
        if let Some(id) = row["id"].as_str().map(str::to_string) {
            if let Some(t) = self.next_run_time(&id) {
                row["next_run_at"] = json!(iso_ist(t));
            }
        }
        row
    }

    fn emit(&self, event: &'static str, id: &str) {
        self.inner.notify.notify(event, json!({"schedule_id": id}));
    }

    async fn get(&self, id: &str) -> Option<Value> {
        let id = id.to_string();
        match self.inner.db.run(move |c| db::schedule(c, &id)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Reading a Historify schedule failed: {}", e);
                None
            }
        }
    }

    pub async fn schedule(&self, id: &str) -> crate::error::Result<Option<Value>> {
        let id = id.to_string();
        let row = self.inner.db.run(move |c| db::schedule(c, &id)).await?;
        Ok(row.map(|r| self.enrich(r)))
    }

    pub async fn all(&self) -> crate::error::Result<Vec<Value>> {
        let rows = self.inner.db.run(|c| db::schedules(c, false)).await?;
        Ok(rows.into_iter().map(|r| self.enrich(r)).collect())
    }

    async fn update_db(&self, id: &str, u: ScheduleUpdate) -> Result<(), String> {
        let id = id.to_string();
        match self
            .inner
            .db
            .write(move |c| db::update_schedule(c, &id, &u))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Updating a Historify schedule failed: {}", e);
                Err(crate::db::duckdb::STORE_UNAVAILABLE.to_string())
            }
        }
    }

    /// Web `add_schedule`. `Ok(message)` or `Err(message)`.
    pub async fn add(&self, id: &str, s: AddSchedule) -> Result<String, String> {
        let (idc, now) = (id.to_string(), ist_naive(self.now()));
        let name = s.name.clone();
        let created = self
            .inner
            .db
            .write(move |c| {
                db::create_schedule(
                    c,
                    &NewSchedule {
                        id: &idc,
                        name: &s.name,
                        description: s.description.as_deref(),
                        schedule_type: &s.schedule_type,
                        interval_value: s.interval_value,
                        interval_unit: s.interval_unit.as_deref(),
                        time_of_day: s.time_of_day.as_deref(),
                        data_interval: &s.data_interval,
                        lookback_days: s.lookback_days,
                    },
                    now,
                )
            })
            .await;
        match created {
            Ok(Ok(())) => {}
            Ok(Err(m)) => return Err(m),
            Err(e) => {
                tracing::error!("Creating a Historify schedule failed: {}", e);
                return Err(crate::db::duckdb::STORE_UNAVAILABLE.to_string());
            }
        }
        let Some(row) = self.get(id).await else {
            return Err("Failed to retrieve created schedule".into());
        };
        if !self.add_entry(&row) {
            return Err("Failed to add schedule to scheduler".into());
        }
        self.emit("historify_schedule_created", id);
        Ok(format!("Schedule '{}' created successfully", name))
    }

    /// Web `update_schedule`.
    pub async fn update(&self, id: &str, u: ScheduleUpdate) -> Result<String, String> {
        let trigger_changed = u.changes_trigger();
        self.update_db(id, u).await?;
        let Some(row) = self.get(id).await else {
            return Err("Schedule not found".into());
        };
        let active = row["is_enabled"].as_bool().unwrap_or(false)
            && !row["is_paused"].as_bool().unwrap_or(false);
        if trigger_changed && active && !self.add_entry(&row) {
            return Err("Failed to update scheduler job".into());
        }
        self.emit("historify_schedule_updated", id);
        Ok("Schedule updated successfully".into())
    }

    /// Web `delete_schedule`.
    pub async fn delete(&self, id: &str) -> Result<String, String> {
        self.remove_entry(id);
        let idc = id.to_string();
        if let Err(e) = self
            .inner
            .db
            .write(move |c| db::delete_schedule(c, &idc))
            .await
        {
            tracing::error!("Deleting a Historify schedule failed: {}", e);
            return Err(crate::db::duckdb::STORE_UNAVAILABLE.to_string());
        }
        self.emit("historify_schedule_deleted", id);
        Ok("Schedule deleted successfully".into())
    }

    pub async fn enable(&self, id: &str) -> Result<String, String> {
        self.update_db(
            id,
            ScheduleUpdate {
                is_enabled: Some(true),
                ..Default::default()
            },
        )
        .await?;
        if let Some(row) = self.get(id).await {
            if !row["is_paused"].as_bool().unwrap_or(false) {
                self.add_entry(&row);
            }
        }
        self.emit("historify_schedule_updated", id);
        Ok("Schedule enabled".into())
    }

    pub async fn disable(&self, id: &str) -> Result<String, String> {
        self.remove_entry(id);
        self.update_db(
            id,
            ScheduleUpdate {
                is_enabled: Some(false),
                ..Default::default()
            },
        )
        .await?;
        self.emit("historify_schedule_updated", id);
        Ok("Schedule disabled".into())
    }

    pub async fn pause(&self, id: &str) -> Result<String, String> {
        if let Some(e) = self.inner.entries.lock().get_mut(id) {
            e.next = None;
        }
        self.update_db(
            id,
            ScheduleUpdate {
                is_paused: Some(true),
                ..Default::default()
            },
        )
        .await?;
        self.emit("historify_schedule_updated", id);
        Ok("Schedule paused".into())
    }

    pub async fn resume(&self, id: &str) -> Result<String, String> {
        let now = self.now();
        let resumed = {
            let mut entries = self.inner.entries.lock();
            match entries.get_mut(id) {
                Some(e) => {
                    let n = e.trigger.next_at_or_after(now);
                    e.next = Some(n);
                    Some(n)
                }
                None => None,
            }
        };
        match resumed {
            Some(n) => {
                self.persist_next(id, Some(n));
                self.inner.wake.notify_one();
            }
            None => {
                if let Some(row) = self.get(id).await {
                    self.add_entry(&row);
                }
            }
        }
        self.update_db(
            id,
            ScheduleUpdate {
                is_paused: Some(false),
                ..Default::default()
            },
        )
        .await?;
        self.emit("historify_schedule_updated", id);
        Ok("Schedule resumed".into())
    }

    /// Web `trigger_schedule`: run now, whatever the timetable says.
    pub async fn trigger(&self, id: &str) -> Result<String, String> {
        if self.get(id).await.is_none() {
            return Err("Schedule not found".into());
        }
        self.execute(id).await;
        Ok("Schedule triggered".into())
    }

    fn claim_run(&self, id: &str) -> bool {
        self.inner.active_runs.lock().insert(id.to_string())
    }

    fn release_run(&self, id: &str) {
        self.inner.active_runs.lock().remove(id);
    }

    async fn set_state(&self, id: &str, status: &str, last: Option<&str>) {
        let _ = self
            .update_db(
                id,
                ScheduleUpdate {
                    status: Some(status.into()),
                    last_run_status: last.map(str::to_string),
                    ..Default::default()
                },
            )
            .await;
    }

    /// Web `execute_schedule`.
    pub async fn execute(&self, id: &str) {
        if !self.claim_run(id) {
            tracing::info!(
                "Scheduled download {} is already running; skipping overlap",
                id
            );
            return;
        }
        let started = self.execute_claimed(id).await;
        if !started {
            self.release_run(id);
        }
    }

    async fn execute_claimed(&self, id: &str) -> bool {
        let Some(schedule) = self.get(id).await else {
            tracing::error!("Schedule not found: {}", id);
            return false;
        };
        if !schedule["is_enabled"].as_bool().unwrap_or(false)
            || schedule["is_paused"].as_bool().unwrap_or(false)
        {
            tracing::info!("Schedule {} is disabled or paused, skipping", id);
            return false;
        }
        self.set_state(id, "running", None).await;
        if let Err(m) = self.inner.engine.source().ready() {
            tracing::warn!("Scheduled download {} not started: {}", id, m);
            self.set_state(id, "idle", Some("no_broker_session")).await;
            return false;
        }
        let symbols = match self.inner.db.run(db::watchlist_symbols).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Reading the Historify watchlist failed: {}", e);
                Vec::new()
            }
        };
        if symbols.is_empty() {
            tracing::warn!("No symbols found for schedule {}", id);
            self.set_state(id, "idle", Some("no_symbols")).await;
            return false;
        }
        let lookback = schedule["lookback_days"].as_i64().unwrap_or(1);
        let today = self.now().with_timezone(&Kolkata).date_naive();
        let end = today.format("%Y-%m-%d").to_string();
        let start = (today - Duration::days(lookback))
            .format("%Y-%m-%d")
            .to_string();
        let (idc, now) = (id.to_string(), ist_naive(self.now()));
        let execution_id = match self
            .inner
            .db
            .write(move |c| db::create_execution(c, &idc, now))
            .await
        {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::error!("Could not record a schedule execution: {}", e);
                None
            }
        };
        let total = symbols.len() as i64;
        let reply = self
            .inner
            .engine
            .create_and_start(CreateJob {
                job_type: "scheduled".into(),
                symbols,
                interval: schedule["data_interval"].as_str().unwrap_or("D").into(),
                start_date: Some(start),
                end_date: Some(end),
                config: json!({"schedule_id": id, "schedule_execution_id": execution_id}),
                incremental: true,
            })
            .await;
        if reply.is_success() {
            let job_id = reply.body["job_id"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if let Some(ex) = execution_id {
                let jid = job_id.clone();
                let _ = self
                    .inner
                    .db
                    .write(move |c| {
                        db::update_execution(
                            c,
                            ex,
                            &ExecutionUpdate {
                                download_job_id: Some(jid),
                                symbols_processed: Some(total),
                                ..Default::default()
                            },
                        )
                    })
                    .await;
            }
            tracing::info!("Scheduled download started: {} ({} symbols)", job_id, total);
            self.inner.notify.notify(
                "historify_schedule_execution_started",
                json!({"schedule_id": id, "execution_id": execution_id, "job_id": job_id}),
            );
            true
        } else {
            let msg = reply.message();
            if let Some(ex) = execution_id {
                let (m, now) = (msg.clone(), ist_naive(self.now()));
                let _ = self
                    .inner
                    .db
                    .write(move |c| {
                        db::update_execution(
                            c,
                            ex,
                            &ExecutionUpdate {
                                status: Some("failed".into()),
                                completed_at: Some(now),
                                error_message: Some(m),
                                ..Default::default()
                            },
                        )
                    })
                    .await;
            }
            self.set_state(id, "idle", Some("failed")).await;
            let (idc, now) = (id.to_string(), ist_naive(self.now()));
            let _ = self
                .inner
                .db
                .write(move |c| db::count_run(c, &idc, false, now))
                .await;
            tracing::error!("Scheduled download failed: {}", msg);
            false
        }
    }

    /// Schedules with an entry (tests).
    pub fn entry_count(&self) -> usize {
        self.inner.entries.lock().len()
    }

    pub fn running_claims(&self) -> usize {
        self.inner.active_runs.lock().len()
    }
}

#[async_trait]
impl JobHook for Inner {
    /// The schedule half of the web's job `finally` block.
    async fn finished(&self, f: Finished) {
        let Some(schedule_id) = f.config["schedule_id"].as_str().map(str::to_string) else {
            return;
        };
        let execution_id = f.config["schedule_execution_id"].as_i64();
        let now = ist_naive(self.clock.now());
        let (sid, status, job_id) = (schedule_id.clone(), f.status.clone(), f.job_id.clone());
        let r = self
            .db
            .write(move |c| {
                if let Some(ex) = execution_id {
                    let records = db::job_records(c, &job_id)?;
                    db::update_execution(
                        c,
                        ex,
                        &ExecutionUpdate {
                            status: Some(status.clone()),
                            completed_at: Some(now),
                            symbols_processed: Some(f.total),
                            symbols_success: Some(f.completed),
                            symbols_failed: Some(f.failed),
                            records_downloaded: Some(records),
                            ..Default::default()
                        },
                    )?;
                }
                db::update_schedule(
                    c,
                    &sid,
                    &ScheduleUpdate {
                        status: Some("idle".into()),
                        last_run_status: Some(status.clone()),
                        ..Default::default()
                    },
                )?
                .ok();
                db::count_run(c, &sid, status == "completed", now)
            })
            .await;
        if let Err(e) = r {
            tracing::error!("Could not finalize Historify schedule run: {}", e);
        }
        self.notify.notify(
            "historify_schedule_execution_complete",
            json!({"schedule_id": schedule_id, "execution_id": execution_id, "status": f.status}),
        );
        self.active_runs.lock().remove(&schedule_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(y, m, d, h, mi, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn daily_trigger_crosses_days_in_ist() {
        let t = Trigger::Daily {
            at: parse_hhmm("09:15").unwrap(),
        };
        assert_eq!(
            t.next_at_or_after(ist(2026, 10, 7, 8, 0)),
            ist(2026, 10, 7, 9, 15)
        );
        assert_eq!(
            t.next_after(ist(2026, 10, 7, 9, 15)),
            ist(2026, 10, 8, 9, 15)
        );
        assert_eq!(
            t.next_at_or_after(ist(2026, 10, 7, 23, 59)),
            ist(2026, 10, 8, 9, 15)
        );
    }

    #[test]
    fn interval_trigger_stays_on_its_grid() {
        let anchor = ist(2026, 10, 7, 10, 0);
        let t = Trigger::Interval {
            every: Duration::minutes(15),
            anchor,
        };
        assert_eq!(t.next_at_or_after(ist(2026, 10, 7, 9, 0)), anchor);
        assert_eq!(
            t.next_after(ist(2026, 10, 7, 10, 0)),
            ist(2026, 10, 7, 10, 15)
        );
        assert_eq!(
            t.next_after(ist(2026, 10, 7, 11, 7)),
            ist(2026, 10, 7, 11, 15)
        );
    }

    #[test]
    fn time_of_day_validation_matches_the_web() {
        assert!(parse_hhmm("09:15").is_some());
        assert!(parse_hhmm("9:5").is_some());
        assert!(parse_hhmm("24:00").is_none());
        assert!(parse_hhmm("09:60").is_none());
        assert!(parse_hhmm("09:15:00").is_none());
        assert!(parse_hhmm("0915").is_none());
    }
}
