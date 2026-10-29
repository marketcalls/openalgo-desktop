//! The Flow webhook's lockout after wrong secrets (lead decision 13, a
//! desktop hardening the web does not have; modelled on the Chartink
//! webhook's guard, `chartink/webhook.rs`).
//!
//! A known token with a wrong or missing secret counts a failure against
//! its workflow, from any number of addresses; [`LOCKOUT_FAILURES`] inside
//! [`LOCKOUT_WINDOW`] lock the webhook for [`LOCK_DURATION`] (403 even with
//! the right secret), until the trader regenerates the secret or turns the
//! webhook off and on again. Bounded: at most [`MAX_TRACKED`] workflows per
//! map, expired entries swept on every write.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Wrong secrets against one webhook that lock it.
pub const LOCKOUT_FAILURES: usize = 10;
pub const LOCKOUT_WINDOW: Duration = Duration::from_secs(600);
/// How long a locked webhook stays locked unless the trader unlocks it.
pub const LOCK_DURATION: Duration = Duration::from_secs(900);
/// Bound on each map.
pub const MAX_TRACKED: usize = 4096;

/// What a locked webhook answers (403).
pub const LOCKED: &str = "This Flow webhook is locked after repeated alerts with a wrong secret. Check who has its address, then regenerate the secret, or turn the webhook off and on again, in the workflow's webhook settings to unlock it.";

#[derive(Default)]
pub struct WebhookGuard {
    failures: Mutex<HashMap<i64, VecDeque<Instant>>>,
    locked: Mutex<HashMap<i64, Instant>>,
    offset: Mutex<Duration>,
}

impl WebhookGuard {
    fn now(&self) -> Instant {
        Instant::now() + *self.offset.lock()
    }

    /// Move the guard's clock forward (tests).
    pub fn advance(&self, by: Duration) {
        *self.offset.lock() += by;
    }

    /// Workflows held across both maps (the hygiene tests).
    pub fn tracked(&self) -> usize {
        self.failures.lock().len() + self.locked.lock().len()
    }

    /// Whether the workflow's webhook is locked now (an expired lock is
    /// dropped).
    pub fn is_locked(&self, workflow_id: i64) -> bool {
        let now = self.now();
        let mut locked = self.locked.lock();
        match locked.get(&workflow_id) {
            Some(at) if now.saturating_duration_since(*at) < LOCK_DURATION => true,
            Some(_) => {
                locked.remove(&workflow_id);
                false
            }
            None => false,
        }
    }

    /// Count one wrong secret; `true` when this one locked the webhook.
    pub fn record_failure(&self, workflow_id: i64) -> bool {
        let now = self.now();
        let recent = |t: &Instant| now.saturating_duration_since(*t) < LOCKOUT_WINDOW;
        let mut map = self.failures.lock();
        map.retain(|_, q| q.back().is_some_and(recent));
        if map.len() >= MAX_TRACKED && !map.contains_key(&workflow_id) {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, q)| q.back().copied())
                .map(|(k, _)| *k)
            {
                map.remove(&oldest);
            }
        }
        let q = map.entry(workflow_id).or_default();
        while q.front().is_some_and(|t| !recent(t)) {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() < LOCKOUT_FAILURES {
            return false;
        }
        map.remove(&workflow_id);
        drop(map);
        let mut locked = self.locked.lock();
        locked.retain(|_, at| now.saturating_duration_since(*at) < LOCK_DURATION);
        if locked.len() >= MAX_TRACKED {
            locked.clear();
        }
        locked.insert(workflow_id, now);
        tracing::warn!(
            "Flow webhook of workflow {} locked after {} wrong secrets",
            workflow_id,
            LOCKOUT_FAILURES
        );
        true
    }

    /// The trader unlocked the webhook (a new secret, or off and on).
    pub fn unlock(&self, workflow_id: i64) {
        self.failures.lock().remove(&workflow_id);
        self.locked.lock().remove(&workflow_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten wrong secrets inside ten minutes lock the webhook for fifteen;
    /// older failures do not count; an unlock clears it at once.
    #[test]
    fn wrong_secrets_lock_the_webhook_until_unlocked_or_expired() {
        let g = WebhookGuard::default();
        for _ in 0..LOCKOUT_FAILURES - 1 {
            assert!(!g.record_failure(1));
        }
        g.advance(LOCKOUT_WINDOW);
        assert!(!g.record_failure(1), "the old failures left the window");
        for _ in 0..LOCKOUT_FAILURES - 2 {
            assert!(!g.record_failure(1));
        }
        assert!(g.record_failure(1));
        assert!(g.is_locked(1));
        assert!(!g.is_locked(2), "per workflow");
        g.advance(LOCK_DURATION);
        assert!(!g.is_locked(1), "a lock expires");
        for _ in 0..LOCKOUT_FAILURES {
            g.record_failure(1);
        }
        assert!(g.is_locked(1));
        g.unlock(1);
        assert!(!g.is_locked(1));
        assert_eq!(g.tracked(), 0);
    }

    /// The maps stay bounded however many workflows fail.
    #[test]
    fn the_guard_is_bounded() {
        let g = WebhookGuard::default();
        for id in 0..(MAX_TRACKED as i64 + 50) {
            g.record_failure(id);
        }
        assert!(g.tracked() <= MAX_TRACKED);
    }
}
