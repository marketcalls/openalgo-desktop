//! Outbound pacing for broker APIs.
//!
//! Brokers publish per-category limits (Kite: 10 orders/s, 1 quote/s for
//! the batch endpoint, 3 historical calls/s). A `Pacer` hands out send slots
//! no closer together than `1 / rate`, so a burst from one caller queues
//! instead of earning a 429. State is one `Instant` per category: nothing
//! grows with traffic.

use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Minimum-interval limiter for one request category.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    next: Mutex<Option<Instant>>,
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
            next: Mutex::new(None),
        }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Wait for the next send slot. The slot is claimed before sleeping, so
    /// concurrent callers are spaced out rather than released together.
    pub async fn acquire(&self) {
        let wait_until = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let slot = match *next {
                Some(t) if t > now => t,
                _ => now,
            };
            *next = Some(slot + self.interval);
            slot
        };
        tokio::time::sleep_until(wait_until).await;
    }

    /// Like [`acquire`](Self::acquire), but bounded: when the next free
    /// slot is more than `max_wait` away the call is refused at once with
    /// that wait, and books nothing, so a refused caller delays nobody
    /// behind it (web `utils/broker_backpressure.py` `check_queue_wait`).
    pub async fn acquire_within(&self, max_wait: Duration) -> Result<(), Duration> {
        let wait_until = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let slot = match *next {
                Some(t) if t > now => t,
                _ => now,
            };
            let wait = slot.saturating_duration_since(now);
            if wait > max_wait {
                return Err(wait);
            }
            *next = Some(slot + self.interval);
            slot
        };
        tokio::time::sleep_until(wait_until).await;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spaces_out_a_burst() {
        let p = Pacer::per_second(10.0);
        let start = Instant::now();
        for _ in 0..5 {
            p.acquire().await;
        }
        // First slot is immediate, then four intervals of 100 ms.
        assert_eq!(start.elapsed(), Duration::from_millis(400));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_pacer_does_not_delay() {
        let p = Pacer::per_second(2.0);
        p.acquire().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let t = Instant::now();
        p.acquire().await;
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
