//! In-process event bus with two lanes, like the web's `utils/event_bus.py`.
//!
//! Each subscriber gets its own bounded queue and its own worker task, so:
//! * a slow subscriber (an alert sender) never delays another (the log);
//! * events reach a subscriber in publish order;
//! * memory is bounded: a full best-effort queue sheds the event (sampled
//!   warning), a full critical queue logs an error every time. The critical
//!   cap is ten times larger, for subscribers on the money path.
//!
//! `publish` never blocks and never awaits. Workers are owned by the bus in a
//! `JoinSet`; `shutdown` closes the queues, lets workers drain for a short
//! grace period, then aborts what is left.

use super::{Event, Topic};
use futures_util::FutureExt;
use parking_lot::{Mutex, RwLock};
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

pub const BEST_EFFORT_CAP: usize = 1000;
pub const CRITICAL_CAP: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    BestEffort,
    Critical,
}

#[async_trait::async_trait]
pub trait Subscriber: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Topics this subscriber receives.
    fn topics(&self) -> Vec<Topic>;
    async fn handle(&self, event: Arc<Event>);
}

struct Registered {
    name: &'static str,
    topics: HashSet<Topic>,
    lane: Lane,
    tx: mpsc::Sender<Arc<Event>>,
}

#[derive(Debug, Default)]
pub struct BusStats {
    pub published: AtomicU64,
    pub dropped: AtomicU64,
    pub critical_dropped: AtomicU64,
    /// Deliveries to a subscriber whose queue was already closed.
    pub closed_sends: AtomicU64,
    /// Subscriber panics, every lane.
    pub panics: AtomicU64,
    /// ... of them on the critical lane (an order fact may be lost).
    pub critical_panics: AtomicU64,
    /// Order updates from an earlier broker session, ignored (EV-02).
    pub stale_dropped: AtomicU64,
    /// Order updates the broker relay skipped because it fell behind.
    pub relay_lagged: AtomicU64,
}

/// A fault that may have lost an order fact (ARCH-01, EV-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactLoss {
    /// A critical delivery was dropped, refused by a closed queue, or its
    /// subscriber failed on it.
    Event,
    /// The broker order relay fell behind and skipped updates.
    Relay,
}

/// One subscriber queue, for health.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueDepth {
    pub name: &'static str,
    pub lane: Lane,
    pub depth: usize,
    pub capacity: usize,
}

pub struct EventBus {
    subs: RwLock<Vec<Registered>>,
    tasks: Mutex<JoinSet<()>>,
    stats: Arc<BusStats>,
    best_effort_cap: usize,
    critical_cap: usize,
    /// Faults that may have lost an order fact; the order reconciler wakes
    /// on `fact_loss` and repairs from the broker's order book.
    fact_losses: Arc<AtomicU64>,
    last_loss_relay: Arc<std::sync::atomic::AtomicBool>,
    fact_loss: Arc<tokio::sync::Notify>,
    /// The current broker session's generation (EV-02). An order update
    /// stamped with another (non-zero) generation is from an earlier
    /// session and is not delivered.
    session_generation: Arc<AtomicU64>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_caps(BEST_EFFORT_CAP, CRITICAL_CAP)
    }

    pub fn with_caps(best_effort_cap: usize, critical_cap: usize) -> Self {
        Self {
            subs: RwLock::new(Vec::new()),
            tasks: Mutex::new(JoinSet::new()),
            stats: Arc::new(BusStats::default()),
            best_effort_cap: best_effort_cap.max(1),
            critical_cap: critical_cap.max(1),
            fact_losses: Arc::new(AtomicU64::new(0)),
            last_loss_relay: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fact_loss: Arc::new(tokio::sync::Notify::new()),
            session_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn stats(&self) -> &BusStats {
        &self.stats
    }

    /// Record a fault that may have lost an order fact and wake whoever
    /// repairs it.
    pub fn report_fact_loss(&self, kind: FactLoss) {
        note_loss(
            &self.fact_losses,
            &self.last_loss_relay,
            &self.fact_loss,
            kind,
        );
    }

    /// The broker relay skipped `n` order updates.
    pub fn report_relay_lag(&self, n: u64) {
        self.stats.relay_lagged.fetch_add(n, Ordering::Relaxed);
        self.report_fact_loss(FactLoss::Relay);
    }

    /// Faults that may have lost an order fact, since start.
    pub fn gap_count(&self) -> u64 {
        self.fact_losses.load(Ordering::SeqCst)
    }

    /// Whether the latest such fault was the relay falling behind.
    pub fn last_loss_was_relay(&self) -> bool {
        self.last_loss_relay.load(Ordering::SeqCst)
    }

    /// Resolves after the next fact loss (or at once, for one reported
    /// while nobody waited).
    pub async fn gap_notified(&self) {
        self.fact_loss.notified().await
    }

    /// Start a new broker session's generation and return it.
    pub fn begin_session(&self) -> u64 {
        self.session_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn session_generation(&self) -> u64 {
        self.session_generation.load(Ordering::SeqCst)
    }

    /// Each subscriber's queue depth, for health.
    pub fn queue_depths(&self) -> Vec<QueueDepth> {
        self.subs
            .read()
            .iter()
            .map(|s| QueueDepth {
                name: s.name,
                lane: s.lane,
                depth: s.tx.max_capacity() - s.tx.capacity(),
                capacity: s.tx.max_capacity(),
            })
            .collect()
    }

    /// Register a subscriber. Must be called from inside a Tokio runtime.
    pub fn subscribe(&self, sub: Arc<dyn Subscriber>, lane: Lane) {
        let cap = match lane {
            Lane::BestEffort => self.best_effort_cap,
            Lane::Critical => self.critical_cap,
        };
        let (tx, mut rx) = mpsc::channel::<Arc<Event>>(cap);
        let name = sub.name();
        let worker = sub.clone();
        let stats = self.stats.clone();
        let generation = self.session_generation.clone();
        let (losses, relay, notify) = (
            self.fact_losses.clone(),
            self.last_loss_relay.clone(),
            self.fact_loss.clone(),
        );
        self.tasks.lock().spawn(async move {
            while let Some(ev) = rx.recv().await {
                let topic = ev.topic().as_str();
                if let Event::OrderUpdate(u) = &*ev {
                    // EV-02: a frame from an earlier broker session is not
                    // this session's fact.
                    let current = generation.load(Ordering::SeqCst);
                    if u.session_generation != 0 && u.session_generation != current {
                        stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            "Ignored an order update for '{}' from an earlier broker session",
                            name
                        );
                        continue;
                    }
                }
                if AssertUnwindSafe(worker.handle(ev))
                    .catch_unwind()
                    .await
                    .is_err()
                {
                    stats.panics.fetch_add(1, Ordering::Relaxed);
                    tracing::error!("Event subscriber '{}' failed on '{}'", name, topic);
                    if lane == Lane::Critical {
                        stats.critical_panics.fetch_add(1, Ordering::Relaxed);
                        note_loss(&losses, &relay, &notify, FactLoss::Event);
                    }
                }
            }
        });
        self.subs.write().push(Registered {
            name,
            topics: sub.topics().into_iter().collect(),
            lane,
            tx,
        });
        tracing::debug!("EventBus: subscribed '{}' ({:?} lane)", name, lane);
    }

    /// Publish without blocking. Returns the number of subscribers reached.
    pub fn publish(&self, event: Event) -> usize {
        self.stats.published.fetch_add(1, Ordering::Relaxed);
        let topic = event.topic();
        let ev = Arc::new(event);
        let subs = self.subs.read();
        let mut delivered = 0;
        for s in subs.iter().filter(|s| s.topics.contains(&topic)) {
            match s.tx.try_send(ev.clone()) {
                Ok(()) => delivered += 1,
                Err(mpsc::error::TrySendError::Full(_)) => match s.lane {
                    Lane::BestEffort => {
                        let n = self.stats.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        if n == 1 || n.is_multiple_of(100) {
                            tracing::warn!(
                                "EventBus at capacity; dropped '{}' for '{}'. {} dropped since start.",
                                topic.as_str(),
                                s.name,
                                n
                            );
                        }
                    }
                    Lane::Critical => {
                        let n = self.stats.critical_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        tracing::error!(
                            "EventBus critical lane full; '{}' did not receive '{}'. {} critical drops since start.",
                            s.name,
                            topic.as_str(),
                            n
                        );
                        self.report_fact_loss(FactLoss::Event);
                    }
                },
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.stats.closed_sends.fetch_add(1, Ordering::Relaxed);
                    if s.lane == Lane::Critical {
                        tracing::error!(
                            "EventBus critical subscriber '{}' has stopped; '{}' was not delivered",
                            s.name,
                            topic.as_str()
                        );
                        self.report_fact_loss(FactLoss::Event);
                    }
                }
            }
        }
        delivered
    }

    /// Close every queue, give workers `grace` to drain, abort the rest.
    pub async fn shutdown(&self, grace: Duration) {
        self.subs.write().clear();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        let drained =
            tokio::time::timeout(grace, async { while tasks.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }

    pub fn subscriber_count(&self) -> usize {
        self.subs.read().len()
    }
}

fn note_loss(
    losses: &AtomicU64,
    relay: &std::sync::atomic::AtomicBool,
    notify: &tokio::sync::Notify,
    kind: FactLoss,
) {
    relay.store(kind == FactLoss::Relay, Ordering::SeqCst);
    losses.fetch_add(1, Ordering::SeqCst);
    // A stored permit: a loss reported while nobody waits is not missed.
    notify.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::SessionEndReason;
    use tokio::sync::Notify;

    struct Recorder {
        seen: Mutex<Vec<String>>,
        gate: Option<Arc<Notify>>,
    }

    #[async_trait::async_trait]
    impl Subscriber for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::ForceLogout]
        }
        async fn handle(&self, event: Arc<Event>) {
            if let Some(g) = &self.gate {
                g.notified().await;
            }
            if let Event::ForceLogout { message } = &*event {
                self.seen.lock().push(message.clone());
            }
        }
    }

    fn ev(i: usize) -> Event {
        Event::ForceLogout {
            message: i.to_string(),
        }
    }

    #[tokio::test]
    async fn delivers_in_publish_order() {
        let bus = EventBus::new();
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        for i in 0..200 {
            bus.publish(ev(i));
        }
        bus.shutdown(Duration::from_secs(5)).await;
        let seen = r.seen.lock().clone();
        assert_eq!(seen, (0..200).map(|i| i.to_string()).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn only_subscribed_topics_are_delivered() {
        let bus = EventBus::new();
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        assert_eq!(
            bus.publish(Event::BrokerSessionEnded {
                reason: SessionEndReason::Logout
            }),
            0
        );
        assert_eq!(bus.publish(ev(1)), 1);
        bus.shutdown(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn best_effort_lane_sheds_when_full() {
        let bus = EventBus::with_caps(4, 100);
        let gate = Arc::new(Notify::new());
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: Some(gate.clone()),
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        for i in 0..20 {
            bus.publish(ev(i));
            tokio::task::yield_now().await;
        }
        let dropped = bus.stats().dropped.load(Ordering::Relaxed);
        // Queue holds 4, at most one more is in the worker's hands.
        assert!(dropped >= 15, "dropped {}", dropped);
        assert_eq!(bus.stats().critical_dropped.load(Ordering::Relaxed), 0);
        bus.shutdown(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn critical_lane_never_drops_below_cap() {
        let cap = 500;
        let bus = EventBus::with_caps(4, cap);
        let gate = Arc::new(Notify::new());
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: Some(gate.clone()),
        });
        bus.subscribe(r.clone(), Lane::Critical);
        for i in 0..cap {
            bus.publish(ev(i));
        }
        assert_eq!(bus.stats().critical_dropped.load(Ordering::Relaxed), 0);
        assert_eq!(bus.stats().dropped.load(Ordering::Relaxed), 0);
        // Release the worker; everything published is delivered in order.
        for _ in 0..cap {
            gate.notify_one();
            tokio::task::yield_now().await;
        }
        for _ in 0..50 {
            if r.seen.lock().len() == cap {
                break;
            }
            gate.notify_one();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(r.seen.lock().len(), cap);
        bus.shutdown(Duration::from_secs(1)).await;
    }

    struct Panicker;
    #[async_trait::async_trait]
    impl Subscriber for Panicker {
        fn name(&self) -> &'static str {
            "panicker"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::ForceLogout]
        }
        async fn handle(&self, _event: Arc<Event>) {
            panic!("boom");
        }
    }

    /// Records order updates; blocks on `gate` first when set.
    struct Orders {
        seen: Mutex<Vec<String>>,
        gate: Arc<Notify>,
        gated: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Subscriber for Orders {
        fn name(&self) -> &'static str {
            "orders"
        }
        fn topics(&self) -> Vec<Topic> {
            vec![Topic::OrderUpdate]
        }
        async fn handle(&self, event: Arc<Event>) {
            if self.gated.load(Ordering::SeqCst) {
                self.gate.notified().await;
            }
            if let Event::OrderUpdate(u) = &*event {
                self.seen.lock().push(u.orderid.clone());
            }
        }
    }

    fn update(id: &str, generation: u64) -> Event {
        Event::OrderUpdate(crate::events::OrderUpdate {
            orderid: id.into(),
            session_generation: generation,
            ..Default::default()
        })
    }

    /// ARCH-01, EV-01: a full critical queue sheds the fill, and that is
    /// reported as a fact loss the reconciler wakes on.
    #[tokio::test]
    async fn a_dropped_critical_delivery_is_reported_as_a_fact_loss() {
        let bus = EventBus::with_caps(1, 1);
        let r = Arc::new(Orders {
            seen: Mutex::new(vec![]),
            gate: Arc::new(Notify::new()),
            gated: std::sync::atomic::AtomicBool::new(true),
        });
        bus.subscribe(r.clone(), Lane::Critical);
        assert_eq!(bus.gap_count(), 0);
        for i in 0..4 {
            bus.publish(update(&format!("O{}", i), 0));
            tokio::task::yield_now().await;
        }
        assert!(bus.stats().critical_dropped.load(Ordering::Relaxed) >= 1);
        assert!(bus.gap_count() >= 1);
        assert!(!bus.last_loss_was_relay());
        // The reconciler's wake-up was stored, not missed.
        tokio::time::timeout(Duration::from_secs(1), bus.gap_notified())
            .await
            .expect("a fact loss wakes the reconciler");
        assert_eq!(bus.queue_depths()[0].capacity, 1);
        r.gated.store(false, Ordering::SeqCst);
        r.gate.notify_waiters();
        bus.shutdown(Duration::from_millis(100)).await;
    }

    /// EV-02: a frame queued in one broker session is not delivered once
    /// another session has begun; it is counted.
    #[tokio::test]
    async fn an_order_update_from_an_earlier_session_is_ignored_and_counted() {
        let bus = EventBus::new();
        let a = bus.begin_session();
        let r = Arc::new(Orders {
            seen: Mutex::new(vec![]),
            gate: Arc::new(Notify::new()),
            gated: std::sync::atomic::AtomicBool::new(true),
        });
        bus.subscribe(r.clone(), Lane::Critical);
        // The first frame holds the worker; the second waits in the queue.
        bus.publish(update("A1", a));
        bus.publish(update("A2", a));
        tokio::task::yield_now().await;
        // Session B starts (same broker) before A2 is handled.
        let b = bus.begin_session();
        assert_ne!(a, b);
        r.gated.store(false, Ordering::SeqCst);
        r.gate.notify_one();
        bus.publish(update("B1", b));
        // A sandbox update (no session) is always delivered.
        bus.publish(update("S1", 0));
        bus.shutdown(Duration::from_secs(2)).await;
        let seen = r.seen.lock().clone();
        assert_eq!(seen, vec!["A1", "B1", "S1"], "A2 is stale");
        assert_eq!(bus.stats().stale_dropped.load(Ordering::Relaxed), 1);
    }

    /// EV-03: a critical subscriber that fails on an event may have lost an
    /// order fact; a best-effort one only counts.
    #[tokio::test]
    async fn a_failing_critical_subscriber_is_a_fact_loss() {
        async fn until(f: impl Fn() -> bool) {
            for _ in 0..400 {
                if f() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        let bus = EventBus::new();
        bus.subscribe(Arc::new(Panicker), Lane::BestEffort);
        bus.publish(ev(1));
        until(|| bus.stats().panics.load(Ordering::Relaxed) == 1).await;
        assert_eq!(bus.stats().panics.load(Ordering::Relaxed), 1);
        assert_eq!(bus.gap_count(), 0, "best effort: counted only");
        bus.subscribe(Arc::new(Panicker), Lane::Critical);
        bus.publish(ev(2));
        until(|| bus.stats().critical_panics.load(Ordering::Relaxed) == 1).await;
        assert_eq!(bus.stats().critical_panics.load(Ordering::Relaxed), 1);
        assert_eq!(bus.gap_count(), 1);
        bus.shutdown(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn a_panicking_subscriber_keeps_its_worker() {
        let bus = EventBus::new();
        bus.subscribe(Arc::new(Panicker), Lane::BestEffort);
        let r = Arc::new(Recorder {
            seen: Mutex::new(vec![]),
            gate: None,
        });
        bus.subscribe(r.clone(), Lane::BestEffort);
        bus.publish(ev(1));
        bus.publish(ev(2));
        bus.shutdown(Duration::from_secs(5)).await;
        assert_eq!(r.seen.lock().len(), 2);
    }
}
