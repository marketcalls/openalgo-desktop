//! Flow's scheduler (the web's `FlowScheduler`, `flow_scheduler_service.py`
//! 1-336, on APScheduler), modelled on the Historify scheduler: the schedule
//! types, their next fire times in IST, one job per workflow, and a `tick`
//! the driver (and the tests, with their own clock) calls.
//!
//! APScheduler's settings, kept: `coalesce` (missed fires collapse into
//! one), `max_instances=1` (a fire while the workflow runs meets the
//! executor's `already_running`) and `misfire_grace_time=60` (a fire more
//! than 60 s late, including one that fell due while the app was closed,
//! is skipped and the next one computed).
//!
//! Departures from the web, each decided:
//!
//! - Every wall-clock field is IST, whatever the computer's zone (T-014,
//!   X-347): the web's scheduler had no time zone, so a UTC server ran a
//!   09:15 schedule at 14:45 IST.
//! - `once` with a date and no time takes the node's Time field (default
//!   09:15) in IST (T-009): the web read the editor's date as midnight,
//!   which the default market-hours gate then skipped, so a one-time
//!   schedule built in the editor never ran. A time already past is
//!   refused at activation.
//! - `daily` honours its days when it has them (T-012): the web's daily
//!   schedule fired at weekends too.
//! - The job's arguments never hold the API key (T-015): a run reads the
//!   trader's current key when it fires.

use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc,
    Weekday,
};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// APScheduler `misfire_grace_time`.
pub const MISFIRE_GRACE_SECS: i64 = 60;
/// Seconds past an aligned boundary that an interval job fires at
/// (`FLOW_INTERVAL_ALIGN_OFFSET`, fixed: no `.env`).
pub const INTERVAL_ALIGN_OFFSET_SECONDS: i64 = 2;
/// The driver never sleeps longer than this, so a clock change is noticed.
pub const MAX_SLEEP_SECS: i64 = 60;

/// The job id the web gives a workflow's schedule.
pub fn job_id(workflow_id: i64) -> String {
    format!("flow_workflow_{}", workflow_id)
}

/// A schedule and how it fires.
#[derive(Debug, Clone, PartialEq)]
pub enum Schedule {
    /// Every `every`, on the grid that starts at `start` (aligned to the
    /// IST clock at activation).
    Interval {
        every: Duration,
        start: DateTime<Utc>,
    },
    /// Once, at this instant.
    Once { at: DateTime<Utc> },
    /// Every day at this IST time; only on `days` when given.
    Daily {
        at: NaiveTime,
        days: Option<Vec<Weekday>>,
    },
    /// On `days` at this IST time.
    Weekly { at: NaiveTime, days: Vec<Weekday> },
}

/// IST wall time to an instant (the later one when a local time repeats;
/// IST has no daylight saving, so this never happens).
fn from_ist(t: NaiveDateTime) -> DateTime<Utc> {
    Kolkata
        .from_local_datetime(&t)
        .latest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| t.and_utc())
}

fn ist(t: DateTime<Utc>) -> NaiveDateTime {
    t.with_timezone(&Kolkata).naive_local()
}

/// `hour, minute = map(int, time.split(":"))`.
fn parse_time(text: &str) -> Option<NaiveTime> {
    let (h, m) = text.split_once(':')?;
    let (h, m): (i64, i64) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    NaiveTime::from_hms_opt(u32::try_from(h).ok()?, u32::try_from(m).ok()?, 0)
}

/// `days` (0 = Monday .. 6 = Sunday), unknown entries dropped as the web
/// drops them.
fn weekdays(days: &Value) -> Vec<Weekday> {
    let mut out: Vec<Weekday> = days
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let f = d.as_f64().filter(|f| f.fract() == 0.0)?;
            match f as i64 {
                0 => Some(Weekday::Mon),
                1 => Some(Weekday::Tue),
                2 => Some(Weekday::Wed),
                3 => Some(Weekday::Thu),
                4 => Some(Weekday::Fri),
                5 => Some(Weekday::Sat),
                6 => Some(Weekday::Sun),
                _ => None,
            }
        })
        .collect();
    out.sort_by_key(Weekday::num_days_from_monday);
    out.dedup();
    out
}

/// The first interval fire after `now`: on the clock's boundary plus the
/// offset (`_next_aligned_start`), in IST. Seconds are not aligned.
fn aligned_start(value: i64, unit: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    if unit == "seconds" {
        return now + Duration::seconds(1);
    }
    let step = if unit == "hours" {
        Duration::hours(value)
    } else {
        Duration::minutes(value)
    };
    let local = ist(now);
    let hour = local
        .date()
        .and_hms_opt(local.hour(), 0, 0)
        .unwrap_or(local);
    let anchor = if unit == "hours" {
        hour
    } else {
        let minute = i64::from(local.minute());
        hour + Duration::minutes(minute - minute % value)
    };
    let mut start = from_ist(anchor) + Duration::seconds(INTERVAL_ALIGN_OFFSET_SECONDS);
    while start <= now {
        start += step;
    }
    start
}

/// `executeAt` as an instant: a full ISO datetime as given (naive = IST,
/// `Z` or an offset honoured); a date alone at the node's Time (default
/// 09:15) in IST (T-009).
fn once_at(execute_at: &str, time: &str) -> Option<DateTime<Utc>> {
    let text = execute_at.trim();
    if let Ok(d) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Some(from_ist(d.and_time(parse_time(time)?)));
    }
    let text = text.replace('Z', "+00:00");
    if let Ok(t) = DateTime::parse_from_rfc3339(&text) {
        return Some(t.with_timezone(&Utc));
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(&text, f).ok())
    .map(from_ist)
    .or_else(|| {
        [
            "%Y-%m-%dT%H:%M:%S%.f%:z",
            "%Y-%m-%dT%H:%M%:z",
            "%Y-%m-%d %H:%M:%S%.f%:z",
        ]
        .iter()
        .find_map(|f| DateTime::parse_from_str(&text, f).ok())
        .map(|t| t.with_timezone(&Utc))
    })
}

/// A field as text: a string as is, anything else as Python prints it.
fn field_text(data: &Map<String, Value>, key: &str, default: &str) -> String {
    match data.get(key) {
        None | Some(Value::Null) => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => crate::flow::contract::pyfmt::str(v),
    }
}

impl Schedule {
    /// The start node's schedule at `now` (`add_workflow_job`); `Ok(None)`
    /// for a manual (or absent) schedule, which registers nothing. The
    /// error is the web's sentence, answered 400 by activation.
    pub fn from_node(
        data: &Map<String, Value>,
        now: DateTime<Utc>,
    ) -> Result<Option<Schedule>, String> {
        Self::build(data, now, true)
    }

    /// The schedule an active workflow had, rebuilt at start-up: a
    /// one-time fire already past is kept, so the tick runs it within the
    /// grace or retires it.
    pub fn restored(
        data: &Map<String, Value>,
        now: DateTime<Utc>,
    ) -> Result<Option<Schedule>, String> {
        Self::build(data, now, false)
    }

    fn build(
        data: &Map<String, Value>,
        now: DateTime<Utc>,
        refuse_past: bool,
    ) -> Result<Option<Schedule>, String> {
        let kind = match data.get("scheduleType") {
            Some(v) if crate::flow::contract::pyfmt::truthy(v) => {
                crate::flow::contract::pyfmt::str(v)
            }
            _ => return Ok(None),
        };
        if kind == "manual" {
            return Ok(None);
        }
        let time = field_text(data, "time", "09:15");
        let invalid = || format!("Invalid schedule configuration: type={}", kind);
        let days = data.get("days").cloned().unwrap_or(Value::Null);
        let schedule = match kind.as_str() {
            "interval" => {
                let value = data
                    .get("intervalValue")
                    .filter(|v| crate::flow::contract::pyfmt::truthy(v))
                    .map(|v| crate::flow::contract::pyfmt::to_int(v).filter(|n| *n > 0))
                    .unwrap_or(Some(1))
                    .ok_or_else(invalid)?;
                // `interval_unit or "minutes"`: the log names the unit given;
                // anything but seconds or hours runs as minutes.
                let given = data
                    .get("intervalUnit")
                    .filter(|v| crate::flow::contract::pyfmt::truthy(v))
                    .map(crate::flow::contract::pyfmt::str)
                    .unwrap_or_else(|| "minutes".into());
                let unit = match given.as_str() {
                    "seconds" => "seconds",
                    "hours" => "hours",
                    _ => "minutes",
                };
                let start = aligned_start(value, unit, now);
                tracing::info!(
                    "Creating interval trigger: every {} {}, first run {}",
                    value,
                    given,
                    ist(start).format("%H:%M:%S")
                );
                let every = match unit {
                    "seconds" => Duration::seconds(value),
                    "hours" => Duration::hours(value),
                    _ => Duration::minutes(value),
                };
                Schedule::Interval { every, start }
            }
            "once" => {
                let at = data
                    .get("executeAt")
                    .filter(|v| crate::flow::contract::pyfmt::truthy(v))
                    .map(crate::flow::contract::pyfmt::str)
                    .ok_or_else(invalid)?;
                let when = once_at(&at, &time).ok_or_else(|| {
                    tracing::error!("Invalid execute_at datetime: {}", at);
                    format!("Invalid datetime format: {}", at)
                })?;
                if refuse_past && when + Duration::seconds(MISFIRE_GRACE_SECS) < now {
                    return Err(format!(
                        "The one-time schedule is set for {} IST, which has already passed. Set a later date and time, then activate it again.",
                        ist(when).format("%Y-%m-%d %H:%M")
                    ));
                }
                tracing::info!("Creating one-time trigger: {}", ist(when));
                Schedule::Once { at: when }
            }
            "daily" => {
                let at = parse_time(&time).ok_or_else(|| {
                    tracing::error!("Invalid time format: {}", time);
                    format!("Invalid time format: {}", time)
                })?;
                let wanted = crate::flow::contract::pyfmt::truthy(&days);
                let chosen = weekdays(&days);
                if wanted && chosen.is_empty() {
                    return Err(invalid());
                }
                tracing::info!("Creating daily trigger: {}", time);
                Schedule::Daily {
                    at,
                    days: wanted.then_some(chosen),
                }
            }
            "weekly" if crate::flow::contract::pyfmt::truthy(&days) => {
                let chosen = weekdays(&days);
                let at = parse_time(&time)
                    .filter(|_| !chosen.is_empty())
                    .ok_or_else(|| {
                        tracing::error!("Invalid weekly schedule config: {} {}", time, days);
                        "Invalid weekly schedule configuration".to_string()
                    })?;
                tracing::info!("Creating weekly trigger: {:?} at {}", chosen, time);
                Schedule::Weekly { at, days: chosen }
            }
            _ => return Err(invalid()),
        };
        Ok(Some(schedule))
    }

    fn days(&self) -> Option<&[Weekday]> {
        match self {
            Schedule::Daily { days, .. } => days.as_deref(),
            Schedule::Weekly { days, .. } => Some(days),
            _ => None,
        }
    }

    fn daily_at(&self) -> Option<NaiveTime> {
        match self {
            Schedule::Daily { at, .. } | Schedule::Weekly { at, .. } => Some(*at),
            _ => None,
        }
    }

    /// The day's fire on IST date `d`, when it fires that day.
    fn on(&self, d: NaiveDate) -> Option<DateTime<Utc>> {
        let at = self.daily_at()?;
        let fires = self.days().is_none_or(|days| days.contains(&d.weekday()));
        fires.then(|| from_ist(d.and_time(at)))
    }

    /// The first fire at or after `t`; `None` when there is none (a past
    /// `once`).
    pub fn next_at_or_after(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Interval { every, start } => {
                if t <= *start {
                    return Some(*start);
                }
                let step = every.num_seconds().max(1);
                let k = ((t - *start).num_seconds() + step - 1) / step;
                Some(*start + Duration::seconds(k * step))
            }
            Schedule::Once { at } => (*at >= t).then_some(*at),
            _ => {
                let today = ist(t).date();
                (0..8)
                    .filter_map(|i| self.on(today + Duration::days(i)))
                    .find(|f| *f >= t)
            }
        }
    }

    /// The latest fire at or before `t` (what a coalesced misfire is
    /// judged on).
    pub fn latest_at_or_before(&self, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Interval { every, start } => {
                if t < *start {
                    return None;
                }
                let step = every.num_seconds().max(1);
                let k = (t - *start).num_seconds() / step;
                Some(*start + Duration::seconds(k * step))
            }
            Schedule::Once { at } => (*at <= t).then_some(*at),
            _ => {
                let today = ist(t).date();
                (0..8)
                    .filter_map(|i| self.on(today - Duration::days(i)))
                    .find(|f| *f <= t)
            }
        }
    }

    pub fn is_once(&self) -> bool {
        matches!(self, Schedule::Once { .. })
    }
}

/// One workflow's job.
#[derive(Debug, Clone)]
struct Job {
    schedule: Schedule,
    next: Option<DateTime<Utc>>,
    /// The market-hours flag at activation, used only when the graph cannot
    /// be read at fire time (the web's stored job argument).
    market_hours_only: bool,
}

/// A fire [`Scheduler::tick`] found due.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    pub workflow_id: i64,
    /// When it was due (the latest missed fire, coalesced).
    pub at: DateTime<Utc>,
    /// More than the grace late: skipped.
    pub missed: bool,
    /// A `once` schedule: retired after this fire.
    pub once: bool,
    pub market_hours_only: bool,
    /// The next fire, `None` when the job is gone.
    pub next: Option<DateTime<Utc>>,
}

/// The jobs, one per workflow (`flow_workflow_<id>`).
#[derive(Default)]
pub struct Scheduler {
    jobs: Mutex<HashMap<i64, Job>>,
}

impl Scheduler {
    /// Replace the workflow's job; the first fire, `None` for a past once.
    pub fn add(
        &self,
        workflow_id: i64,
        schedule: Schedule,
        market_hours_only: bool,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        let next = schedule.next_at_or_after(now);
        self.jobs.lock().insert(
            workflow_id,
            Job {
                schedule,
                next,
                market_hours_only,
            },
        );
        tracing::info!("Added job {}", job_id(workflow_id));
        next
    }

    /// Add a job restored at start-up, catching up a fire that fell due
    /// while the app was closed: due at once when the latest missed fire
    /// since `recorded` is within the grace, else skipped (`missed`).
    pub fn restore(
        &self,
        workflow_id: i64,
        schedule: Schedule,
        market_hours_only: bool,
        recorded: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        // The latest fire since the one recorded, if the app missed any.
        let missed = recorded.filter(|r| *r <= now).map(|r| {
            schedule
                .latest_at_or_before(now)
                .filter(|l| *l >= r)
                .unwrap_or(r)
        });
        let next = match (missed, &schedule) {
            (Some(m), _) if (now - m).num_seconds() <= MISFIRE_GRACE_SECS => Some(m),
            // A past one-time fire stays due, so the tick retires it.
            (_, Schedule::Once { at }) => Some(*at),
            (Some(m), _) => {
                tracing::warn!(
                    "Flow schedule {} missed its run at {} IST while OpenAlgo was closed; it is skipped",
                    job_id(workflow_id),
                    ist(m)
                );
                schedule.next_at_or_after(now)
            }
            (None, _) => schedule.next_at_or_after(now),
        };
        self.jobs.lock().insert(
            workflow_id,
            Job {
                schedule,
                next,
                market_hours_only,
            },
        );
        next
    }

    /// Remove the workflow's job; `false` when there was none.
    pub fn remove(&self, workflow_id: i64) -> bool {
        let removed = self.jobs.lock().remove(&workflow_id).is_some();
        if removed {
            tracing::info!("Removed job {}", job_id(workflow_id));
        } else {
            tracing::debug!("No scheduler job {} to remove", job_id(workflow_id));
        }
        removed
    }

    pub fn has(&self, workflow_id: i64) -> bool {
        self.jobs.lock().contains_key(&workflow_id)
    }

    /// The workflows with a job.
    pub fn workflow_ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.jobs.lock().keys().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn clear(&self) {
        self.jobs.lock().clear();
    }

    /// How long the driver may sleep from `now`.
    pub fn sleep_for(&self, now: DateTime<Utc>) -> std::time::Duration {
        let next = self.jobs.lock().values().filter_map(|j| j.next).min();
        let secs = next
            .map(|t| (t - now).num_milliseconds().clamp(0, MAX_SLEEP_SECS * 1000))
            .unwrap_or(MAX_SLEEP_SECS * 1000);
        std::time::Duration::from_millis(secs.max(50) as u64)
    }

    /// Every fire due at `now`, coalesced per job (the latest missed fire),
    /// each job moved to its next fire after `now`; a `once` job is removed.
    pub fn tick(&self, now: DateTime<Utc>) -> Vec<Due> {
        let mut jobs = self.jobs.lock();
        let mut due = Vec::new();
        for (id, job) in jobs.iter_mut() {
            let Some(next) = job.next.filter(|n| *n <= now) else {
                continue;
            };
            let at = job
                .schedule
                .latest_at_or_before(now)
                .filter(|l| *l >= next)
                .unwrap_or(next);
            let once = job.schedule.is_once();
            job.next = if once {
                None
            } else {
                job.schedule
                    .next_at_or_after(now + Duration::milliseconds(1))
            };
            due.push(Due {
                workflow_id: *id,
                at,
                missed: (now - at).num_seconds() > MISFIRE_GRACE_SECS,
                once,
                market_hours_only: job.market_hours_only,
                next: job.next,
            });
        }
        jobs.retain(|_, j| j.next.is_some());
        due.sort_by_key(|d| d.workflow_id);
        due
    }
}

#[cfg(test)]
mod tests;
