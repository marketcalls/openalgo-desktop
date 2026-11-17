//! The next run a scheduler records for a restart to read, kept in memory
//! while the database will not take it (web ceb116807,
//! `utils/resilient_scheduler.py`, `LockTolerantJobStore`). Used by Flow's
//! scheduler (`flow::triggers`) and Historify's (`historify::scheduler`).
//!
//! The job in memory is what fires, so a refused save changes nothing
//! now. Left unsaved, a restart reads the fire just made as still due and
//! runs it again, so the save is not dropped:
//!
//! - Each job's newest next run is held here, replacing an older one not
//!   yet written (the web's `_held`).
//! - One saver writes them, one at a time: a save per due job, each on a
//!   pooled connection waiting out the busy budget, took the whole pool.
//! - Each write checks, under the database's write lock, that the job
//!   still has that next run, and writes nothing otherwise
//!   ([`Saved::Superseded`]). An older save can then never land after a
//!   newer one, or over a schedule changed or removed meanwhile.
//! - A round stops at the first refusal, which has already waited out the
//!   lock, and comes back with a capped backoff.
//! - At stop one last round runs, and whatever is still held is named in a
//!   warning, in the web's words.

use crate::error::{AppError, Result};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::future::Future;
use std::hash::Hash;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The wait after a refused round, doubled each time up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// How long a stop waits for the last round, a round still running
/// included: just past the SQLite pool's busy budget (5 s), so a write
/// waiting for another writer's lock can still finish.
const LAST_ROUND: Duration = Duration::from_secs(6);

/// A job's next fire; `None` when it has none.
pub type NextRun = Option<DateTime<Utc>>;

/// What one write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    Written,
    /// The job no longer has this next run (a later fire, a new schedule,
    /// removed): nothing was written.
    Superseded,
}

pub struct NextRunSaves<K> {
    /// How a job is named in the log ("workflow", "Historify schedule").
    what: &'static str,
    /// How recent a run a restart may make again ("in the last minute"):
    /// the scheduler's misfire grace.
    window: &'static str,
    held: Mutex<Held<K>>,
    /// One round at a time.
    lane: tokio::sync::Mutex<()>,
}

struct Held<K> {
    /// The newest next run of each job not written yet.
    next: HashMap<K, NextRun>,
    /// Jobs whose save was refused and is not written yet (logged once).
    refused: HashSet<K>,
    /// A saver task is running.
    saving: bool,
}

impl<K: Clone + Eq + Hash + Display> NextRunSaves<K> {
    pub fn new(what: &'static str, window: &'static str) -> Self {
        Self {
            what,
            window,
            held: Mutex::new(Held {
                next: HashMap::new(),
                refused: HashSet::new(),
                saving: false,
            }),
            lane: tokio::sync::Mutex::new(()),
        }
    }

    /// Hold `next` as the next run of `job` to write, in place of any
    /// older one. `true` when no saver is running: the caller starts one
    /// ([`Self::save`]) as a task it owns.
    #[must_use]
    pub fn hold(&self, job: K, next: NextRun) -> bool {
        let mut held = self.held.lock();
        held.next.insert(job, next);
        !std::mem::replace(&mut held.saving, true)
    }

    /// Nothing held and no saver running.
    #[cfg(test)]
    pub(crate) fn is_settled(&self) -> bool {
        let held = self.held.lock();
        held.next.is_empty() && !held.saving
    }

    /// The saver: rounds until nothing is held, or until `stop`.
    pub async fn save<W, F>(&self, stop: &CancellationToken, write: W)
    where
        W: Fn(K, NextRun) -> F,
        F: Future<Output = Result<Saved>>,
    {
        let mut wait = RETRY_FIRST;
        while !stop.is_cancelled() {
            if self.round(&write).await {
                let mut held = self.held.lock();
                if held.next.is_empty() {
                    held.saving = false;
                    return;
                }
                // More came in during the round.
                wait = RETRY_FIRST;
                continue;
            }
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = stop.cancelled() => break,
            }
            wait = (wait * 2).min(RETRY_MAX);
        }
        self.held.lock().saving = false;
    }

    /// One pass over what is held, stopping at the first refusal; `true`
    /// when nothing was refused.
    async fn round<W, F>(&self, write: &W) -> bool
    where
        W: Fn(K, NextRun) -> F,
        F: Future<Output = Result<Saved>>,
    {
        let _lane = self.lane.lock().await;
        let pending: Vec<(K, NextRun)> = self
            .held
            .lock()
            .next
            .iter()
            .map(|(job, next)| (job.clone(), *next))
            .collect();
        for (job, next) in pending {
            match write(job.clone(), next).await {
                Ok(saved) => self.settle(&job, next, saved),
                Err(e) => {
                    self.refuse(job, &e);
                    return false;
                }
            }
        }
        true
    }

    /// `next` of `job` is written or superseded: forgotten, unless a newer
    /// one was held meanwhile.
    fn settle(&self, job: &K, next: NextRun, saved: Saved) {
        let mut held = self.held.lock();
        if held.next.get(job) == Some(&next) {
            held.next.remove(job);
        }
        let caught_up = match saved {
            Saved::Written => held.refused.remove(job),
            Saved::Superseded => {
                if !held.next.contains_key(job) {
                    held.refused.remove(job);
                }
                false
            }
        };
        drop(held);
        if caught_up {
            tracing::info!(
                "Scheduler: the next run of {} {} has been saved.",
                self.what,
                job
            );
        }
    }

    fn refuse(&self, job: K, e: &AppError) {
        let first = self.held.lock().refused.insert(job.clone());
        if first {
            let reason = e.to_string();
            tracing::warn!(
                "Scheduler: the database could not save the next run of {} {} ({}). It is being kept in memory and will be saved as soon as the database allows. Its runs continue on time.",
                self.what,
                job,
                reason.lines().next().unwrap_or_default()
            );
        }
    }

    /// At stop, with the saver told to stop and before the jobs are
    /// cleared (the writes check them): one last round, after a round
    /// still running, both within [`LAST_ROUND`]; then a warning naming
    /// whatever is still unsaved (the web's shutdown makes one more pass
    /// and says the same).
    pub async fn finish<W, F>(&self, write: W)
    where
        W: Fn(K, NextRun) -> F,
        F: Future<Output = Result<Saved>>,
    {
        let last = async {
            if !self.held.lock().next.is_empty() {
                self.round(&write).await;
            }
        };
        if tokio::time::timeout(LAST_ROUND, last).await.is_err() {
            tracing::debug!("The last save of the scheduler's next runs did not finish in time");
        }
        let mut unsaved: Vec<String> = self
            .held
            .lock()
            .next
            .keys()
            .map(|job| format!("{} {}", self.what, job))
            .collect();
        if unsaved.is_empty() {
            return;
        }
        unsaved.sort();
        tracing::warn!(
            "Scheduler: stopped before the database took the next run of {}. A run of theirs made {} may be made again on the next start.",
            unsaved.join(", "),
            self.window
        );
    }
}
