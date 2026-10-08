//! IST scheduling for strategies (web `services/strategy_module/scheduler.py`).
//!
//! A strategy's `scheduler` config (`{enabled, days, start_time,
//! auto_stop_time, default_mode}`) becomes at most two jobs:
//!
//! ```text
//! strategy:{id}:start   start a run at start_time on the configured days
//! strategy:{id}:stop    square the run off at auto_stop_time
//! ```
//!
//! Kept from the web, each a defect the other schedulers had:
//!
//! * **Every job is on Asia/Kolkata**, never server-local time.
//! * **Job defaults**: misfire grace 60 s (not 1 s), coalesce, one instance.
//! * **Jobs are plain values** (`JobFunc` plus a strategy id), never closures.
//! * **`exit_time` installs the square-off** when no `auto_stop_time` is set,
//!   and when the scheduler is switched off entirely (weekdays): the default
//!   intraday configuration, started by an alert and squared off by the clock.
//!
//! The job table is a projection of the database, rebuilt at start and on
//! every CRUD write. One owned task evaluates due jobs each second against
//! the injected clock and retries durable pending stops every 5 s.

use super::store::StrategyRow;
use super::StrategyModule;
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

pub const TIMEZONE: &str = "Asia/Kolkata";
pub const MISFIRE_GRACE: Duration = Duration::from_secs(60);
pub const COALESCE: bool = true;
pub const MAX_INSTANCES: u32 = 1;
pub const JOB_PREFIX: &str = "strategy:";
pub const PENDING_STOP_RECONCILE: Duration = Duration::from_secs(5);

const DAYS: &[(&str, Weekday)] = &[
    ("MON", Weekday::Mon),
    ("TUE", Weekday::Tue),
    ("WED", Weekday::Wed),
    ("THU", Weekday::Thu),
    ("FRI", Weekday::Fri),
    ("SAT", Weekday::Sat),
    ("SUN", Weekday::Sun),
];
const WEEKDAYS: &[Weekday] = &[
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
];

/// What a job does. A plain value, never a closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobFunc {
    RunScheduledStart,
    RunScheduledStop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub func: JobFunc,
    /// The job's only argument.
    pub strategy_id: i64,
    pub days: Vec<Weekday>,
    pub hour: u32,
    pub minute: u32,
    pub timezone: &'static str,
    pub misfire_grace: Duration,
    pub coalesce: bool,
    pub max_instances: u32,
}

impl Job {
    /// The next IST fire time at or after `now`.
    pub fn next_run_time(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let ist = now.with_timezone(&Kolkata);
        for add in 0..8 {
            let date = ist.date_naive() + chrono::Duration::days(add);
            if !self.days.contains(&date.weekday()) {
                continue;
            }
            let at = Kolkata
                .from_local_datetime(&date.and_time(NaiveTime::from_hms_opt(
                    self.hour,
                    self.minute,
                    0,
                )?))
                .single()?
                .with_timezone(&Utc);
            if at >= now {
                return Some(at);
            }
        }
        None
    }

    pub fn to_dict(&self, now: DateTime<Utc>) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "strategy_id": self.strategy_id,
            "trigger": format!(
                "cron[day_of_week='{}', hour='{}', minute='{}', timezone='{}']",
                self.days.iter().map(|d| d.to_string().to_ascii_lowercase()).collect::<Vec<_>>().join(","),
                self.hour, self.minute, self.timezone
            ),
            "next_run_time": self.next_run_time(now).map(|t| t.with_timezone(&Kolkata).to_rfc3339()),
        })
    }
}

pub fn start_job_id(strategy_id: i64) -> String {
    format!("{}{}:start", JOB_PREFIX, strategy_id)
}

pub fn stop_job_id(strategy_id: i64) -> String {
    format!("{}{}:stop", JOB_PREFIX, strategy_id)
}

fn parse_hhmm(v: &Value) -> Option<(u32, u32)> {
    let text = v.as_str()?.trim();
    let (h, m) = text.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 23 && m <= 59).then_some((h, m))
}

/// An unrecognised day rejects the whole list: silently trading four days of
/// a five-day schedule is worse than not trading.
fn cron_days(raw: &Value) -> Option<Vec<Weekday>> {
    let list = raw.as_array().filter(|a| !a.is_empty())?;
    let mut days = Vec::new();
    for d in list {
        let name = d.as_str()?.trim().to_ascii_uppercase();
        let (_, wd) = DAYS.iter().find(|(n, _)| *n == name)?;
        if !days.contains(wd) {
            days.push(*wd);
        }
    }
    days.sort_by_key(|d| d.num_days_from_monday());
    Some(days)
}

fn job(
    func: JobFunc,
    strategy_id: i64,
    name: String,
    days: Vec<Weekday>,
    (hour, minute): (u32, u32),
) -> Job {
    Job {
        id: match func {
            JobFunc::RunScheduledStart => start_job_id(strategy_id),
            JobFunc::RunScheduledStop => stop_job_id(strategy_id),
        },
        name,
        func,
        strategy_id,
        days,
        hour,
        minute,
        timezone: TIMEZONE,
        misfire_grace: MISFIRE_GRACE,
        coalesce: COALESCE,
        max_instances: MAX_INSTANCES,
    }
}

/// What should be installed for one strategy.
pub fn planned_jobs(row: &StrategyRow) -> Vec<Job> {
    let config = row.scheduler.as_object();
    let enabled = config
        .and_then(|c| c.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let days = if enabled {
        config.and_then(|c| cron_days(c.get("days").unwrap_or(&Value::Null)))
    } else {
        None
    };
    let exit_at = row.exit_time.as_ref().and_then(|t| parse_hhmm(&json!(t)));

    // The exit_time square-off survives a scheduler that is off.
    let Some(days) = days else {
        return match exit_at {
            Some(at) => vec![job(
                JobFunc::RunScheduledStop,
                row.id,
                format!("Strategy {} scheduled square-off (exit_time)", row.id),
                WEEKDAYS.to_vec(),
                at,
            )],
            None => vec![],
        };
    };
    let config = config.cloned().unwrap_or_default();
    let mut planned = Vec::new();
    match parse_hhmm(config.get("start_time").unwrap_or(&Value::Null)) {
        Some(at) => planned.push(job(
            JobFunc::RunScheduledStart,
            row.id,
            format!("Strategy {} scheduled start", row.id),
            days.clone(),
            at,
        )),
        None => tracing::warn!(
            "strategy {} scheduler has no usable start_time; no start job was installed",
            row.id
        ),
    }
    let (stop_at, source) = match parse_hhmm(config.get("auto_stop_time").unwrap_or(&Value::Null)) {
        Some(at) => (Some(at), "scheduler.auto_stop_time"),
        None => (exit_at, "exit_time"),
    };
    if let Some(at) = stop_at {
        planned.push(job(
            JobFunc::RunScheduledStop,
            row.id,
            format!("Strategy {} scheduled square-off ({})", row.id, source),
            days,
            at,
        ));
    }
    planned
}

/// The installed job table and when each job last fired.
#[derive(Default)]
pub struct Scheduler {
    jobs: Mutex<BTreeMap<String, Job>>,
    fired: Mutex<HashMap<String, NaiveDate>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, job_id: &str) -> Option<Job> {
        self.jobs.lock().get(job_id).cloned()
    }

    pub fn jobs(&self) -> Vec<Job> {
        self.jobs.lock().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.jobs.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Jobs due at `now`: inside their slot plus the misfire grace, and not
    /// yet fired for that IST day (coalesced: once per slot).
    pub fn due(&self, now: DateTime<Utc>) -> Vec<Job> {
        let ist = now.with_timezone(&Kolkata);
        let today = ist.date_naive();
        let jobs = self.jobs.lock().clone();
        let mut fired = self.fired.lock();
        fired.retain(|id, _| jobs.contains_key(id));
        let mut out = Vec::new();
        for job in jobs.values() {
            if !job.days.contains(&today.weekday()) {
                continue;
            }
            let Some(at) = NaiveTime::from_hms_opt(job.hour, job.minute, 0)
                .and_then(|t| Kolkata.from_local_datetime(&today.and_time(t)).single())
                .map(|d| d.with_timezone(&Utc))
            else {
                continue;
            };
            let late = now.signed_duration_since(at);
            if late < chrono::Duration::zero()
                || late >= chrono::Duration::from_std(job.misfire_grace).unwrap_or_default()
            {
                continue;
            }
            if fired.get(&job.id) == Some(&today) {
                continue;
            }
            fired.insert(job.id.clone(), today);
            out.push(job.clone());
        }
        out
    }
}

impl StrategyModule {
    /// Rebuild one strategy's jobs from its stored config. Returns the ids
    /// now installed.
    pub fn sync_strategy_jobs(&self, strategy_id: i64) -> Vec<String> {
        let row = self.store.get_strategy_unscoped(strategy_id).ok().flatten();
        let planned = row.as_ref().map(planned_jobs).unwrap_or_default();
        let mut jobs = self.scheduler.jobs.lock();
        jobs.remove(&start_job_id(strategy_id));
        jobs.remove(&stop_job_id(strategy_id));
        let mut installed = Vec::new();
        for j in planned {
            tracing::info!("Scheduled {} at {:02}:{:02} IST", j.id, j.hour, j.minute);
            installed.push(j.id.clone());
            jobs.insert(j.id.clone(), j);
        }
        installed
    }

    /// Rebuild every job; drop jobs whose strategy no longer exists.
    pub fn sync_all_jobs(&self) -> Value {
        let ids = self.store.all_strategy_ids().unwrap_or_default();
        let mut installed = 0;
        for id in &ids {
            installed += self.sync_strategy_jobs(*id).len();
        }
        let mut orphans = 0;
        {
            let mut jobs = self.scheduler.jobs.lock();
            let before = jobs.len();
            jobs.retain(|_, j| ids.contains(&j.strategy_id));
            orphans += before - jobs.len();
        }
        json!({"strategies": ids.len(), "installed": installed, "orphans_removed": orphans})
    }

    pub fn remove_strategy_jobs(&self, strategy_id: i64) -> usize {
        let mut jobs = self.scheduler.jobs.lock();
        [start_job_id(strategy_id), stop_job_id(strategy_id)]
            .iter()
            .filter(|id| jobs.remove(*id).is_some())
            .count()
    }

    /// Start a strategy on its schedule. Fails closed: a disabled scheduler,
    /// a running strategy or a live start without the opt-in does nothing.
    pub async fn run_scheduled_start(&self, strategy_id: i64) {
        let Ok(Some(row)) = self.store.get_strategy_unscoped(strategy_id) else {
            tracing::warn!(
                "Scheduled start skipped: strategy {} no longer exists",
                strategy_id
            );
            return;
        };
        let config = row.scheduler.as_object().cloned().unwrap_or_default();
        if !config
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return;
        }
        if row.status == "running" {
            return;
        }
        let mode = config
            .get("default_mode")
            .and_then(Value::as_str)
            .unwrap_or("sandbox")
            .to_string();
        if mode != "live" && mode != "sandbox" {
            tracing::error!(
                "Scheduled start skipped: strategy {} has an unknown default mode",
                strategy_id
            );
            return;
        }
        if mode == "live" && !row.live_enabled {
            self.emit(
                strategy_id,
                &row.user_id,
                "live_disabled",
                "Scheduled live start refused: live trading is not enabled for this strategy",
                super::store::EventFields {
                    severity: Some("warn"),
                    payload: Some(json!({"trigger_source": "scheduler", "mode": mode})),
                    ..Default::default()
                },
            )
            .await;
            return;
        }
        let r = self
            .start_run(strategy_id, &row.user_id, &mode, "scheduler", None)
            .await;
        if !r.ok {
            tracing::error!(
                "Scheduled start of strategy {} failed: {}",
                strategy_id,
                r.error.unwrap_or_default()
            );
        }
    }

    /// Square a strategy off on its schedule. Not gated on `enabled`: a run
    /// that is open must be closed whatever the config now says.
    pub async fn run_scheduled_stop(&self, strategy_id: i64) {
        let Ok(Some(row)) = self.store.get_strategy_unscoped(strategy_id) else {
            return;
        };
        let (true, Some(run_id)) = (row.status == "running", row.current_run_id) else {
            return;
        };
        let r = self.stop_run(run_id, &row.user_id, "scheduler").await;
        if !r.ok {
            tracing::error!(
                "Scheduled square-off of strategy {} {}: {}",
                strategy_id,
                if r.stop_pending {
                    "refused; the stop remains pending and is retried"
                } else {
                    "failed"
                },
                r.error.unwrap_or_default()
            );
        }
    }

    /// Repair lost acknowledgements, then retry every durable pending stop.
    pub async fn reconcile_pending_stops(&self) -> Value {
        let mut counts = (0, 0, 0, 0);
        let runs = self.store.list_open_runs().unwrap_or_default();
        for run in &runs {
            self.reconcile_acks(run.id, true).await;
        }
        for run in runs.iter().filter(|r| r.stop_requested_reason.is_some()) {
            counts.0 += 1;
            match self.reconcile_pending_stop(run.id).await {
                Some(o) if o.stop_pending => {
                    counts.1 += 1;
                    if !o.ok {
                        counts.3 += 1;
                    }
                }
                Some(o) if o.ok => counts.2 += 1,
                _ => counts.3 += 1,
            }
        }
        json!({"examined": counts.0, "pending": counts.1, "finalised": counts.2, "failed": counts.3})
    }

    /// Fire every job due at the clock's now (one scheduler pass).
    pub async fn run_due_jobs(&self) -> Vec<String> {
        let mut fired = Vec::new();
        for job in self.scheduler.due(self.clock.now()) {
            match job.func {
                JobFunc::RunScheduledStart => self.run_scheduled_start(job.strategy_id).await,
                JobFunc::RunScheduledStop => self.run_scheduled_stop(job.strategy_id).await,
            }
            fired.push(job.id);
        }
        fired
    }
}

/// Start the owned scheduler task; it ends on module shutdown.
pub fn start(module: &Arc<StrategyModule>) {
    module.sync_all_jobs();
    let weak = Arc::downgrade(module);
    let token = module.shutdown_token();
    module.spawn(async move {
        let mut second = tokio::time::interval(Duration::from_secs(1));
        second.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reconcile = tokio::time::interval(PENDING_STOP_RECONCILE);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = second.tick() => {
                    let Some(m) = weak.upgrade() else { break };
                    m.run_due_jobs().await;
                }
                _ = reconcile.tick() => {
                    let Some(m) = weak.upgrade() else { break };
                    m.reconcile_pending_stops().await;
                }
            }
        }
    });
}
