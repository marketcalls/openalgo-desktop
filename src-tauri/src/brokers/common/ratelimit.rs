//! Outbound pacing for broker APIs.
//!
//! Brokers publish per-category limits (Kite: 10 orders/s, 1 quote/s for
//! the batch endpoint, 3 historical calls/s). A `Pacer` hands out send slots
//! no closer together than `1 / rate`, so a burst from one caller queues
//! instead of earning a 429. State is one `Instant` per category: nothing
//! grows with traffic. A `Budget` holds rolling windows, a daily cap and a
//! broker-imposed pause for brokers that publish several limits at once.

use chrono::NaiveDate;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Longest a request waits for its turn before it is refused (web
/// `BROKER_MAX_QUEUE_WAIT_SECONDS` and `..._ORDER_...`, 10 s): an order or a
/// quote sent long after it was asked for is worse than a refusal the
/// trader can retry.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(10);

/// Order pacing for a broker that publishes no order rate (RATE-03): the
/// exchanges' threshold of 10 orders a second per client, above which a
/// retail API client's algorithm must be registered (SEBI's 2025 retail
/// algo framework), and the rate the other adapters' order pacers use.
/// Orders get their own lane, so data calls never queue ahead of an exit
/// (RATE-02). The web paces orders on none of the brokers that use this
/// (Samco, Tradejini, HDFC Sky, HDFC Securities, Pocketful, 5paisa): a
/// deliberate desktop deviation (maintainer decision 2026-10-16).
pub const ORDERS_PER_SECOND: f64 = 10.0;

/// What the trader reads when a request is refused before sending because
/// its turn was too far away (web `BROKER_BUSY_MESSAGE`).
pub const BUSY_MESSAGE: &str = "OpenAlgo is pacing requests to stay within your broker's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.";

/// Minimum-interval limiter for one request category.
///
/// Callers take turns in arrival order (a fair async lock) and a slot is
/// booked only when its caller's wait is over, just before it sends: a
/// caller dropped while it waits (a closed page, a cancelled request)
/// books nothing, and the next caller takes its place (BRL-05). Waiting
/// is bounded: a caller whose turn would come more than its maximum wait
/// away is refused at once, without queueing, and delays nobody.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    /// The earliest instant the next send may go.
    next: parking_lot::Mutex<Option<Instant>>,
    /// Turns, first come first served.
    turn: Mutex<()>,
    /// Callers queued for a turn or waiting for their slot.
    waiting: AtomicUsize,
}

/// Counts a caller as waiting until it is done or dropped.
struct Queued<'a>(&'a AtomicUsize);

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Pacer {
    /// At most `per_second` requests per second (values below 1 are clamped).
    pub fn per_second(per_second: f64) -> Self {
        let rate = if per_second.is_finite() && per_second > 0.0 {
            per_second
        } else {
            1.0
        };
        Self::with_interval(Duration::from_secs_f64(1.0 / rate))
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            next: parking_lot::Mutex::new(None),
            turn: Mutex::new(()),
            waiting: AtomicUsize::new(0),
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Wait for a send slot, at most [`MAX_QUEUE_WAIT`]. Past it the
    /// request is refused before anything is sent: a definite refusal
    /// inside an order placement, never an uncertain outcome.
    pub async fn acquire(&self) -> crate::error::Result<()> {
        self.acquire_within(MAX_QUEUE_WAIT).await.map_err(|wait| {
            tracing::warn!(
                "A broker request was refused before sending: its turn was {:?} away",
                wait
            );
            super::outcome::refusal(BUSY_MESSAGE)
        })
    }

    /// Wait for a send slot however long it takes: background work (a
    /// Historify download) is cancelled when no longer wanted, never refused
    /// for the queue bound interactive requests have.
    pub async fn acquire_unbounded(&self) {
        // Without a bound nothing is refused: the wait cannot exceed it.
        let _ = self.acquire_within(Duration::MAX).await;
    }

    /// Wait for a send slot for at most `max_wait`. When the caller's turn
    /// would come later it is refused at once with that wait, booking
    /// nothing, so it delays nobody behind it (web
    /// `utils/broker_backpressure.py` `check_queue_wait`).
    pub async fn acquire_within(&self, max_wait: Duration) -> Result<(), Duration> {
        let start = Instant::now();
        let ahead = u32::try_from(self.waiting.load(Ordering::SeqCst)).unwrap_or(u32::MAX);
        let until_next = self
            .next
            .lock()
            .map(|t| t.saturating_duration_since(start))
            .unwrap_or_default();
        let estimate = until_next.saturating_add(self.interval.saturating_mul(ahead));
        if estimate > max_wait {
            return Err(estimate);
        }
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let _queued = Queued(&self.waiting);
        let deadline = start.checked_add(max_wait);
        let _turn = match deadline {
            Some(d) => tokio::time::timeout_at(d, self.turn.lock())
                .await
                .map_err(|_| max_wait)?,
            None => self.turn.lock().await,
        };
        let now = Instant::now();
        let slot = (*self.next.lock()).filter(|t| *t > now).unwrap_or(now);
        if deadline.is_some_and(|d| slot > d) {
            return Err(slot.saturating_duration_since(start));
        }
        tokio::time::sleep_until(slot).await;
        *self.next.lock() = Some(slot + self.interval);
        Ok(())
    }
}

/// One rolling limit: at most `max` sends in any `span`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rolling {
    pub max: u32,
    pub span: Duration,
}

impl Rolling {
    pub const fn per_second(max: u32) -> Self {
        Self {
            max,
            span: Duration::from_secs(1),
        }
    }

    pub const fn per_minute(max: u32) -> Self {
        Self {
            max,
            span: Duration::from_secs(60),
        }
    }
}

/// The limits of one budget: rolling windows plus an optional cap per
/// calendar day (IST).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    pub windows: Vec<Rolling>,
    pub per_day: Option<u32>,
}

/// Why a send was not claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// The earliest permitted send is this far away, past the caller's
    /// deadline.
    Wait(Duration),
    /// Today's cap of this many sends is used up.
    Daily(u32),
}

#[derive(Debug, Default)]
struct BudgetState {
    /// Send instants inside the longest window, oldest first. Never more
    /// than the largest window's `max`: a send is claimed only when every
    /// window has room.
    sent: VecDeque<Instant>,
    day: Option<NaiveDate>,
    day_sent: u32,
    /// The broker asked for a pause until then (a throttle answer).
    cool_until: Option<Instant>,
}

type Today = Arc<dyn Fn() -> NaiveDate + Send + Sync>;

/// A send budget shared by every caller of one request category: rolling
/// windows (per second, per minute), an optional daily cap, and a pause the
/// broker can impose with a throttle answer (BRL-01, BRL-03).
///
/// A send is claimed only at the moment it may go, under one lock that
/// also checks: nothing is booked while a caller waits, so a caller that
/// is refused, gives up or is dropped costs nobody behind it (BRL-05).
/// Waiting is bounded by the caller's deadline; a send that could not go
/// before it is refused at once with the wait it would have needed.
pub struct Budget {
    limits: parking_lot::Mutex<Limits>,
    state: parking_lot::Mutex<BudgetState>,
    today: Today,
}

impl std::fmt::Debug for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budget").finish_non_exhaustive()
    }
}

/// Today in IST, the calendar the Indian brokers count their daily caps
/// in.
pub fn today_ist() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::seconds(crate::clock::IST_OFFSET_SECS)).date_naive()
}

impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits: parking_lot::Mutex::new(limits),
            state: parking_lot::Mutex::new(BudgetState::default()),
            today: Arc::new(today_ist),
        }
    }

    /// Read "today" from `f` (tests move the calendar).
    pub fn with_today(mut self, f: impl Fn() -> NaiveDate + Send + Sync + 'static) -> Self {
        self.today = Arc::new(f);
        self
    }

    /// Change the limits, keeping what was already sent (a plan change
    /// does not forget today's count).
    pub fn set_limits(&self, limits: Limits) {
        *self.limits.lock() = limits;
    }

    pub fn limits(&self) -> Limits {
        self.limits.lock().clone()
    }

    /// Hold every send of this budget until `until` (a broker's
    /// Retry-After). A later pause extends an earlier one, never shortens
    /// it.
    pub fn cool_down(&self, until: Instant) {
        let mut s = self.state.lock();
        if s.cool_until.is_none_or(|t| t < until) {
            s.cool_until = Some(until);
        }
    }

    /// Sends inside the longest window, as last pruned.
    pub fn recent(&self) -> usize {
        self.state.lock().sent.len()
    }

    /// Sends counted today.
    pub fn sent_today(&self) -> u32 {
        let s = self.state.lock();
        if s.day == Some((self.today)()) {
            s.day_sent
        } else {
            0
        }
    }

    /// The earliest instant at which every window, the daily cap and any
    /// pause allow a send, pruning instants older than the longest window.
    fn earliest(
        limits: &Limits,
        s: &mut BudgetState,
        now: Instant,
        today: NaiveDate,
    ) -> Result<Instant, Denied> {
        if s.day != Some(today) {
            s.day = Some(today);
            s.day_sent = 0;
        }
        if let Some(cap) = limits.per_day {
            if s.day_sent >= cap {
                return Err(Denied::Daily(cap));
            }
        }
        let longest = limits
            .windows
            .iter()
            .map(|w| w.span)
            .max()
            .unwrap_or(Duration::ZERO);
        while s
            .sent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= longest)
        {
            s.sent.pop_front();
        }
        let mut at = now;
        if let Some(c) = s.cool_until {
            if c > at {
                at = c;
            }
        }
        for w in &limits.windows {
            let max = w.max.max(1) as usize;
            let inside = s
                .sent
                .iter()
                .rev()
                .take_while(|t| now.saturating_duration_since(**t) < w.span)
                .count();
            if inside >= max {
                // The send that must leave the window first.
                let oldest = s.sent[s.sent.len() - inside + (inside - max)];
                let free = oldest + w.span;
                if free > at {
                    at = free;
                }
            }
        }
        Ok(at)
    }

    /// Claim one send, waiting while the windows or a pause hold it, but
    /// never past `deadline`.
    pub async fn acquire(&self, deadline: Instant) -> Result<(), Denied> {
        self.acquire_with(None, deadline).await
    }

    /// Claim one send from this budget and, in the same hold, from `outer`
    /// (a ceiling shared by several categories). Locks are always taken
    /// in that order.
    pub async fn acquire_with(
        &self,
        outer: Option<&Budget>,
        deadline: Instant,
    ) -> Result<(), Denied> {
        let limits = self.limits();
        let outer_limits = outer.map(Budget::limits);
        loop {
            let at = {
                let now = Instant::now();
                let today = (self.today)();
                let mut s = self.state.lock();
                let mut at = Self::earliest(&limits, &mut s, now, today)?;
                let mut o_state = match (outer, &outer_limits) {
                    (Some(o), Some(ol)) => {
                        let mut os = o.state.lock();
                        let oat = Self::earliest(ol, &mut os, now, (o.today)())?;
                        if oat > at {
                            at = oat;
                        }
                        Some(os)
                    }
                    _ => None,
                };
                if at <= now {
                    s.sent.push_back(now);
                    s.day_sent = s.day_sent.saturating_add(1);
                    if let Some(os) = o_state.as_mut() {
                        os.sent.push_back(now);
                        os.day_sent = os.day_sent.saturating_add(1);
                    }
                    return Ok(());
                }
                if at > deadline {
                    return Err(Denied::Wait(at - now));
                }
                at
            };
            tokio::time::sleep_until(at).await;
        }
    }
}

/// Exponential backoff delay for retry `attempt` (0-based), capped, with
/// up to 50% downward jitter so many clients do not retry in lockstep.
pub fn backoff_delay(attempt: u32, base: Duration, max: Duration) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.min(16));
    let capped = exp.min(max);
    let jitter = rand::random::<f64>() * 0.5;
    capped.mul_f64(1.0 - jitter)
}

/// Test support: enough callers queued on `pacer` that the next one's turn
/// is further than [`MAX_QUEUE_WAIT`] away. Keep the returned queue alive
/// while asserting; dropping it lets every caller go.
#[cfg(test)]
pub(crate) fn crowd(
    pacer: &Pacer,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
    let n = (MAX_QUEUE_WAIT.as_nanos() / pacer.interval().as_nanos().max(1)) as usize + 10;
    let mut queue: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> =
        Box::pin(async move {
            futures_util::future::join_all((0..n).map(|_| pacer.acquire())).await;
        });
    // One poll puts every caller in line.
    let waker = futures_util::task::noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let _ = std::future::Future::poll(queue.as_mut(), &mut cx);
    queue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spaces_out_a_burst() {
        let p = Pacer::per_second(10.0);
        let start = Instant::now();
        for _ in 0..5 {
            p.acquire().await.unwrap();
        }
        // First slot is immediate, then four intervals of 100 ms.
        assert_eq!(start.elapsed(), Duration::from_millis(400));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_pacer_does_not_delay() {
        let p = Pacer::per_second(2.0);
        p.acquire().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        let t = Instant::now();
        p.acquire().await.unwrap();
        assert_eq!(t.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_acquire_refuses_without_booking() {
        let p = Pacer::per_second(1.0);
        let max = Duration::from_secs(2);
        let start = Instant::now();
        // Slots at 0, 1 and 2 s are within the bound; the fourth is 3 s away.
        let r = futures_util::future::join_all((0..4).map(|_| p.acquire_within(max))).await;
        assert_eq!(r, vec![Ok(()), Ok(()), Ok(()), Err(Duration::from_secs(3))]);
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        // The refusal booked nothing: the next slot is at 3 s, not 4 s.
        p.acquire_within(max).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(3));
    }

    fn limits(windows: &[Rolling], per_day: Option<u32>) -> Limits {
        Limits {
            windows: windows.to_vec(),
            per_day,
        }
    }

    /// Every window holds: two a second and three a minute means the third
    /// send waits a second and the fourth waits for the minute, which a
    /// ten-second deadline refuses at once with the wait it would need.
    #[tokio::test(start_paused = true)]
    async fn budget_holds_every_window_and_refuses_past_the_deadline() {
        let b = Budget::new(limits(
            &[Rolling::per_second(2), Rolling::per_minute(3)],
            None,
        ));
        let start = Instant::now();
        let far = start + Duration::from_secs(600);
        b.acquire(far).await.unwrap();
        b.acquire(far).await.unwrap();
        assert_eq!(start.elapsed(), Duration::ZERO);
        b.acquire(far).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        let near = Instant::now() + Duration::from_secs(10);
        assert_eq!(
            b.acquire(near).await,
            Err(Denied::Wait(Duration::from_secs(59)))
        );
        // Refused without waiting and without booking: the minute frees at
        // 60 s exactly.
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        b.acquire(far).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }

    /// A caller dropped while it waits (a closed page) books nothing: the
    /// next caller gets the slot the dropped one was waiting for.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_waiter_books_nothing() {
        let b = Budget::new(limits(&[Rolling::per_second(1)], None));
        let start = Instant::now();
        let far = start + Duration::from_secs(60);
        b.acquire(far).await.unwrap();
        let gave_up = tokio::time::timeout(Duration::from_millis(100), b.acquire(far)).await;
        assert!(gave_up.is_err());
        b.acquire(far).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(b.state.lock().sent.len(), 1);
    }

    /// The daily cap refuses outright, without waiting, until the date
    /// changes.
    #[tokio::test(start_paused = true)]
    async fn daily_cap_refuses_until_the_next_day() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let day = Arc::new(AtomicU32::new(15));
        let d = day.clone();
        let b = Budget::new(limits(&[Rolling::per_second(100)], Some(2))).with_today(move || {
            NaiveDate::from_ymd_opt(2026, 10, d.load(Ordering::SeqCst)).unwrap()
        });
        let far = Instant::now() + Duration::from_secs(60);
        b.acquire(far).await.unwrap();
        b.acquire(far).await.unwrap();
        assert_eq!(b.acquire(far).await, Err(Denied::Daily(2)));
        assert_eq!(b.sent_today(), 2);
        day.store(16, Ordering::SeqCst);
        assert_eq!(b.sent_today(), 0);
        b.acquire(far).await.unwrap();
        assert_eq!(b.sent_today(), 1);
    }

    /// A broker's pause holds every caller of the budget; one that cannot
    /// wait that long is refused at once.
    #[tokio::test(start_paused = true)]
    async fn a_pause_holds_every_caller() {
        let b = Budget::new(limits(&[Rolling::per_second(10)], None));
        let start = Instant::now();
        b.cool_down(start + Duration::from_secs(5));
        // A shorter pause never shortens a longer one.
        b.cool_down(start + Duration::from_secs(1));
        assert_eq!(
            b.acquire(start + Duration::from_secs(2)).await,
            Err(Denied::Wait(Duration::from_secs(5)))
        );
        b.acquire(start + Duration::from_secs(10)).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(5));
    }

    /// A ceiling shared by several budgets is claimed in the same hold.
    #[tokio::test(start_paused = true)]
    async fn an_outer_ceiling_is_claimed_with_the_budget() {
        let outer = Budget::new(limits(&[Rolling::per_second(1)], None));
        let a = Budget::new(limits(&[Rolling::per_minute(5)], None));
        let b = Budget::new(limits(&[Rolling::per_minute(5)], None));
        let start = Instant::now();
        let far = start + Duration::from_secs(60);
        a.acquire_with(Some(&outer), far).await.unwrap();
        b.acquire_with(Some(&outer), far).await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        // A refusal by the ceiling claims nothing from the budget.
        let near = Instant::now() + Duration::from_millis(10);
        assert!(a.acquire_with(Some(&outer), near).await.is_err());
        assert_eq!(a.state.lock().sent.len(), 1);
    }

    /// BRL-05: a caller dropped while it waits for its turn books nothing:
    /// the next caller goes at the slot the dropped one was waiting for. It
    /// used to keep its booked slot, delaying everyone behind it.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_pacer_waiter_books_nothing() {
        let p = Pacer::per_second(1.0);
        let start = Instant::now();
        let _ = p.acquire().await;
        let gave_up = tokio::time::timeout(Duration::from_millis(100), p.acquire()).await;
        assert!(gave_up.is_err());
        let _ = p.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    /// BRL-05: turns go in arrival order, and once the queue holds more
    /// than ten seconds of sends a new request is refused at once, before
    /// sending, with the trader-facing sentence; it does not wait.
    #[tokio::test(start_paused = true)]
    async fn a_long_queue_refuses_new_requests_at_once() {
        let p = Arc::new(Pacer::per_second(1.0));
        let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for i in 0..11 {
            let (p, order) = (p.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                p.acquire().await.unwrap();
                order.lock().push(i);
            }));
            tokio::task::yield_now().await;
        }
        let start = Instant::now();
        let e = p.acquire().await.unwrap_err();
        assert_eq!(start.elapsed(), Duration::ZERO, "refused without waiting");
        assert_eq!(e.client_message(), BUSY_MESSAGE);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(*order.lock(), (0..11).collect::<Vec<_>>());
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }

    #[test]
    fn backoff_is_capped_and_jittered() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(30);
        for attempt in 0..40 {
            let d = backoff_delay(attempt, base, max);
            assert!(d <= max);
            let full = base.saturating_mul(1u32 << attempt.min(16)).min(max);
            assert!(d >= full / 2);
        }
        assert!(backoff_delay(0, base, max) <= base);
    }
}
