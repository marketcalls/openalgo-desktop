//! One report computed at a time, shared by every request that arrives
//! while it is computed (single flight), for the report pages a browser
//! keeps refreshing (Latency, Traffic), and the same per key for the
//! reads the trading pages repeat while they poll ([`KeyedFlight`]). Each report holds a logs
//! connection for its whole computation, and the logs pool has four, so
//! four refreshes computing at once left none for the traffic, latency,
//! error and order log writers. Shared, they hold one.

use crate::error::Result;
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct ReportFlight {
    /// The last report and when it finished; locked while one is computed.
    last: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

impl ReportFlight {
    /// The report `compute` makes, on the blocking pool (off the async
    /// workers) as one of the log reports built at once
    /// (`AppState::report`). A request that arrives while one is computed waits for it
    /// and answers with it; one that arrives after it computes its own.
    pub async fn share<F>(&self, ctx: &Arc<AppState>, compute: F) -> Result<Value>
    where
        F: FnOnce(&AppState) -> Result<Value> + Send + 'static,
    {
        let asked = Instant::now();
        let mut last = self.last.lock().await;
        if let Some((done, report)) = last.as_ref() {
            if *done >= asked {
                return Ok(report.clone());
            }
        }
        let report = ctx.report(compute).await?;
        *last = Some((Instant::now(), report.clone()));
        Ok(report)
    }
}

/// The last read of a key: when it began, when it was done, its value.
type Slot<V> = Arc<tokio::sync::Mutex<Option<(Instant, Instant, V)>>>;

/// A key being read: its slot and how many requests hold it.
struct Held<V> {
    requests: usize,
    slot: Slot<V>,
}

/// [`ReportFlight`] per key, for the reads a page repeats while it polls
/// (a strategy's order history behind its three books, a deployment's
/// ledger, a user's Action Center): one read at a time for each key,
/// shared by the requests for that key that wait on it. A key's slot is
/// dropped when its last request is answered, so the map holds only keys
/// being read.
pub struct KeyedFlight<K, V> {
    /// What is read, for the debug line each read writes.
    name: &'static str,
    /// Share only a read begun after the request asked
    /// ([`KeyedFlight::fresh`]).
    fresh: bool,
    slots: parking_lot::Mutex<HashMap<K, Held<V>>>,
}

impl<K: Eq + Hash + Clone, V: Clone> KeyedFlight<K, V> {
    /// For what a page polls: a request that arrives while a read runs
    /// waits for it and answers with it, so the answer may have begun to be
    /// read up to one read before the request (the next poll is newer).
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            fresh: false,
            slots: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// For an answer that must show every change made before it was asked
    /// (the Action Center reloads once after an approval, and may not be
    /// told again): a request shares only a read begun after it asked, so
    /// the requests waiting on a read share the next one.
    pub fn fresh(name: &'static str) -> Self {
        Self {
            fresh: true,
            ..Self::new(name)
        }
    }

    /// The value `read` answers for `key`, read once for the requests that
    /// wait on the same key (see [`KeyedFlight::new`] and
    /// [`KeyedFlight::fresh`]); one that arrives after a read reads again.
    /// A failed read is not shared.
    pub async fn share<F, Fut>(&self, key: K, read: F) -> Result<V>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V>>,
    {
        let asked = Instant::now();
        let slot = {
            let mut slots = self.slots.lock();
            let held = slots.entry(key.clone()).or_insert_with(|| Held {
                requests: 0,
                slot: Arc::default(),
            });
            held.requests += 1;
            held.slot.clone()
        };
        let _leave = Leave { flight: self, key };
        let mut last = slot.lock().await;
        if let Some((begun, done, value)) = last.as_ref() {
            let since = if self.fresh { begun } else { done };
            if *since >= asked {
                return Ok(value.clone());
            }
        }
        tracing::debug!("Reading {} for the requests waiting on it", self.name);
        let begun = Instant::now();
        let value = read().await?;
        *last = Some((begun, Instant::now(), value.clone()));
        Ok(value)
    }
}

/// Drops a key's slot when the last request holding it is done (answered
/// or abandoned). The requests are counted under the map's lock, so two
/// that end together cannot each see the other still holding it (a slot
/// left behind keeps a whole answer: an Action Center, a strategy's
/// history).
struct Leave<'a, K: Eq + Hash, V> {
    flight: &'a KeyedFlight<K, V>,
    key: K,
}

impl<K: Eq + Hash, V> Drop for Leave<'_, K, V> {
    fn drop(&mut self) {
        let mut slots = self.flight.slots.lock();
        if let Some(held) = slots.get_mut(&self.key) {
            held.requests = held.requests.saturating_sub(1);
            if held.requests == 0 {
                slots.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A read that counts itself and takes a moment, so the requests
    /// polled with it ask while it runs.
    async fn slow_read(reads: &AtomicUsize, answer: u32) -> Result<u32> {
        reads.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        Ok(answer)
    }

    #[tokio::test]
    async fn requests_that_arrive_during_a_read_share_it() {
        let flight = KeyedFlight::<&str, u32>::new("test");
        let reads = AtomicUsize::new(0);
        let ask = || flight.share("k", || slow_read(&reads, 7));
        let (a, b, c) = tokio::join!(ask(), ask(), ask());
        assert_eq!((a.unwrap(), b.unwrap(), c.unwrap()), (7, 7, 7));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert!(
            flight.slots.lock().is_empty(),
            "no key is held once answered"
        );
    }

    #[tokio::test]
    async fn a_request_after_a_read_reads_again_and_keys_do_not_share() {
        let flight = KeyedFlight::<&str, u32>::new("test");
        let reads = AtomicUsize::new(0);
        flight.share("k", || slow_read(&reads, 1)).await.unwrap();
        let again = flight.share("k", || slow_read(&reads, 2)).await.unwrap();
        assert_eq!(
            again, 2,
            "a read finished before it was asked is not reused"
        );
        let (x, y) = tokio::join!(
            flight.share("x", || slow_read(&reads, 3)),
            flight.share("y", || slow_read(&reads, 4)),
        );
        assert_eq!((x.unwrap(), y.unwrap()), (3, 4));
        assert_eq!(reads.load(Ordering::SeqCst), 4);
    }

    /// A fresh flight never answers with a read begun before the request:
    /// the requests that waited on the first read share the next one.
    #[tokio::test]
    async fn a_fresh_flight_shares_only_a_read_begun_after_asking() {
        let flight = KeyedFlight::<&str, usize>::fresh("test");
        let reads = AtomicUsize::new(0);
        let read = || async {
            let n = reads.fetch_add(1, Ordering::SeqCst) + 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(n)
        };
        let (a, b, c) = tokio::join!(
            flight.share("k", read),
            flight.share("k", read),
            flight.share("k", read),
        );
        assert_eq!((a.unwrap(), b.unwrap(), c.unwrap()), (1, 2, 2));
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    /// A request's key, whose own copy takes a moment to drop: it stands
    /// in for a thread preempted as the request leaves, and holds the
    /// requests that end with it together. The copies the flight keeps
    /// drop at once, so only the leaving is slowed.
    struct LeavingKey {
        id: u32,
        slow: bool,
    }

    impl Clone for LeavingKey {
        fn clone(&self) -> Self {
            Self {
                id: self.id,
                slow: false,
            }
        }
    }

    impl PartialEq for LeavingKey {
        fn eq(&self, other: &Self) -> bool {
            self.id == other.id
        }
    }

    impl Eq for LeavingKey {}

    impl Hash for LeavingKey {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.id.hash(state);
        }
    }

    impl Drop for LeavingKey {
        fn drop(&mut self) {
            if self.slow {
                std::thread::sleep(Duration::from_micros(500));
            }
        }
    }

    /// Requests that end together on several threads leave no key behind.
    /// Counting the slot's references while each leaving request still
    /// held its own let the last two each see the other and keep the key,
    /// with its whole answer (an Action Center, a strategy's history).
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn requests_that_end_together_leave_no_key_behind() {
        let flight = Arc::new(KeyedFlight::<LeavingKey, u32>::new("test"));
        for round in 0..20u32 {
            let start = Arc::new(tokio::sync::Barrier::new(16));
            let requests: Vec<_> = (0..16u32)
                .map(|i| {
                    let (flight, start) = (flight.clone(), start.clone());
                    tokio::spawn(async move {
                        start.wait().await;
                        let key = LeavingKey {
                            id: i % 2,
                            slow: true,
                        };
                        flight.share(key, || async move { Ok(round) }).await
                    })
                })
                .collect();
            for r in requests {
                r.await.unwrap().unwrap();
            }
        }
        let left = flight.slots.lock().len();
        assert_eq!(left, 0, "keys left behind");
    }

    #[tokio::test]
    async fn a_failed_read_is_not_shared() {
        let flight = KeyedFlight::<&str, u32>::new("test");
        let reads = AtomicUsize::new(0);
        let failing = || async {
            reads.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            Err::<u32, _>(AppError::Internal("unreadable".into()))
        };
        let (a, b) = tokio::join!(
            flight.share("k", failing),
            flight.share("k", || slow_read(&reads, 5)),
        );
        assert!(a.is_err());
        assert_eq!(b.unwrap(), 5, "a waiter reads for itself after a failure");
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert!(flight.slots.lock().is_empty());
    }
}
