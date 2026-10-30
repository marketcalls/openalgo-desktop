//! The feed subscriptions each workflow opened, so they are given back when
//! it is deactivated or deleted (the web's `record_workflow_subscription`,
//! `release_workflow_subscriptions`, `has_workflow_subscriptions`,
//! `flow_executor_service.py` 222-366).
//!
//! The subscribe nodes open a subscription on the shared feed that outlives
//! the run. Without this record a workflow reading `{{webhook.symbol}}`
//! gathered one per distinct symbol until the adapter's ceiling, after which
//! `/trading`, the sandbox and the API could not subscribe.
//!
//! A release that the feed did not confirm keeps the entry, so the same
//! deactivate or delete can be retried; a subscription another workflow
//! still holds is not unsubscribed, only this workflow's claim is dropped.
//! Each record carries a version, so a release still waiting on the feed
//! never forgets a subscription recorded again in the meantime.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;

/// One subscription: symbol, exchange and mode (`LTP`, `Quote`, `Depth`).
pub type Entry = (String, String, String);

/// What gives a subscription back to the feed (the triggers lane's feed
/// side). `true` when the feed confirmed it.
#[async_trait]
pub trait FeedRelease: Send + Sync {
    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> bool;
}

/// The per-workflow record, bounded by what the workflows subscribed and
/// emptied by their release.
#[derive(Default)]
pub struct Subscriptions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// workflow id -> entry -> the version it was recorded at.
    held: HashMap<i64, HashMap<Entry, u64>>,
    version: u64,
}

impl Subscriptions {
    /// Remember a subscription for `workflow_id`; a run with no workflow
    /// (a bare test context) records nothing.
    pub fn record(&self, workflow_id: Option<i64>, symbol: &str, exchange: &str, mode: &str) {
        let Some(id) = workflow_id else {
            return;
        };
        let mut inner = self.inner.lock();
        inner.version += 1;
        let version = inner.version;
        inner
            .held
            .entry(id)
            .or_default()
            .insert((symbol.into(), exchange.into(), mode.into()), version);
    }

    /// Whether a release still has to be retried before the workflow's
    /// cleanup can finish.
    pub fn has(&self, workflow_id: i64) -> bool {
        self.inner
            .lock()
            .held
            .get(&workflow_id)
            .is_some_and(|m| !m.is_empty())
    }

    /// The entries recorded for `workflow_id`, sorted.
    pub fn entries(&self, workflow_id: i64) -> Vec<Entry> {
        let mut v: Vec<Entry> = self
            .inner
            .lock()
            .held
            .get(&workflow_id)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Forget `entry` for `workflow_id` if it is still the version that was
    /// released.
    fn forget(&self, workflow_id: i64, entry: &Entry, version: u64) {
        let mut inner = self.inner.lock();
        let Some(map) = inner.held.get_mut(&workflow_id) else {
            return;
        };
        if map.get(entry) != Some(&version) {
            return;
        }
        map.remove(entry);
        if map.is_empty() {
            inner.held.remove(&workflow_id);
        }
    }

    /// Whether another workflow holds `entry` too (its feed stays).
    fn shared(&self, workflow_id: i64, entry: &Entry) -> bool {
        self.inner
            .lock()
            .held
            .iter()
            .any(|(other, m)| *other != workflow_id && m.contains_key(entry))
    }

    /// Give back every subscription of `workflow_id`; failures stay recorded
    /// for a later retry. Safe for a workflow that never subscribed and
    /// safe to call twice. Returns how many were released.
    pub async fn release(&self, workflow_id: i64, feed: &dyn FeedRelease) -> usize {
        let snapshot: Vec<(Entry, u64)> = self
            .inner
            .lock()
            .held
            .get(&workflow_id)
            .map(|m| m.iter().map(|(e, v)| (e.clone(), *v)).collect())
            .unwrap_or_default();
        let total = snapshot.len();
        let mut released = 0;
        for (entry, version) in snapshot {
            if self.shared(workflow_id, &entry) {
                self.forget(workflow_id, &entry, version);
                released += 1;
                continue;
            }
            let (symbol, exchange, mode) = &entry;
            if feed.unsubscribe(symbol, exchange, mode).await {
                self.forget(workflow_id, &entry, version);
                released += 1;
            } else {
                tracing::warn!(
                    "Flow could not give back the {} subscription on {}:{}; it is kept for a retry",
                    mode,
                    exchange,
                    symbol
                );
            }
        }
        if total > 0 {
            tracing::info!(
                "Released {}/{} subscription(s) for workflow {}",
                released,
                total,
                workflow_id
            );
        }
        released
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Feed {
        ok: AtomicBool,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl FeedRelease for Feed {
        async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ok.load(Ordering::SeqCst)
        }
    }

    fn feed(ok: bool) -> Feed {
        Feed {
            ok: AtomicBool::new(ok),
            calls: AtomicUsize::new(0),
        }
    }

    #[tokio::test]
    async fn a_subscription_is_recorded_against_its_workflow() {
        let s = Subscriptions::default();
        s.record(Some(9001), "RELIANCE", "NSE", "LTP");
        s.record(Some(9001), "TCS", "NSE", "Quote");
        assert_eq!(
            s.entries(9001),
            vec![
                ("RELIANCE".into(), "NSE".into(), "LTP".into()),
                ("TCS".into(), "NSE".into(), "Quote".into()),
            ]
        );
        assert!(s.has(9001));
    }

    #[tokio::test]
    async fn releasing_clears_the_record() {
        let s = Subscriptions::default();
        s.record(Some(9002), "RELIANCE", "NSE", "LTP");
        assert_eq!(s.release(9002, &feed(true)).await, 1);
        assert!(!s.has(9002));
    }

    #[tokio::test]
    async fn releasing_an_unknown_workflow_is_a_no_op() {
        let s = Subscriptions::default();
        let f = feed(true);
        assert_eq!(s.release(9003, &f).await, 0);
        assert_eq!(s.release(9003, &f).await, 0);
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_run_without_a_workflow_id_records_nothing() {
        let s = Subscriptions::default();
        s.record(None, "RELIANCE", "NSE", "LTP");
        assert!(s.inner.lock().held.is_empty());
    }

    #[tokio::test]
    async fn an_unconfirmed_release_is_kept_for_a_retry() {
        let s = Subscriptions::default();
        s.record(Some(1), "SBIN", "NSE", "LTP");
        let f = feed(false);
        assert_eq!(s.release(1, &f).await, 0);
        assert!(s.has(1));
        f.ok.store(true, Ordering::SeqCst);
        assert_eq!(s.release(1, &f).await, 1);
        assert!(!s.has(1));
    }

    #[tokio::test]
    async fn a_feed_another_workflow_uses_stays_subscribed() {
        let s = Subscriptions::default();
        s.record(Some(1), "SBIN", "NSE", "LTP");
        s.record(Some(2), "SBIN", "NSE", "LTP");
        let f = feed(true);
        assert_eq!(s.release(1, &f).await, 1);
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            0,
            "workflow 2 still uses it"
        );
        assert!(!s.has(1));
        assert!(s.has(2));
    }

    #[test]
    fn a_newer_record_survives_an_older_release() {
        let s = Subscriptions::default();
        s.record(Some(1), "SBIN", "NSE", "LTP");
        let entry: Entry = ("SBIN".into(), "NSE".into(), "LTP".into());
        let old = s.inner.lock().held[&1][&entry];
        s.record(Some(1), "SBIN", "NSE", "LTP");
        s.forget(1, &entry, old);
        assert!(s.has(1));
    }
}
