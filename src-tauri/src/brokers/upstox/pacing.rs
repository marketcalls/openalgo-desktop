//! Upstox request pacing (web `api/rate_limiter.py`).
//!
//! Upstox publishes three simultaneous rolling windows per category (per
//! second, per minute, per 30 minutes). The shared `common::ratelimit::Pacer`
//! only spaces requests, which would let the standard category run at its
//! per-second rate for a whole minute and blow the per-minute budget, so
//! each category is a `common::ratelimit::Budget` with the three windows.
//! A send is claimed at the moment it goes (a caller dropped while it
//! waits books nothing), and a caller waits at most
//! `ratelimit::MAX_QUEUE_WAIT` before it is refused, before sending
//! (BRL-05). The web books slots ahead (`SlidingWindowLimiter.reserve`).
//! State is the sends of the last 30 minutes, bounded by that window's cap.

use crate::brokers::common::outcome;
use crate::brokers::common::ratelimit::{Budget, Limits, Rolling, BUSY_MESSAGE, MAX_QUEUE_WAIT};
use crate::error::Result;
use std::time::Duration;
use tokio::time::Instant;

const HORIZON: Duration = Duration::from_secs(1800);

/// One rolling-window limiter.
#[derive(Debug)]
pub struct WindowLimiter {
    budget: Budget,
}

impl WindowLimiter {
    pub fn new(per_second: usize, per_minute: usize, per_30min: usize) -> Self {
        let cap = |n: usize| u32::try_from(n.max(1)).unwrap_or(u32::MAX);
        Self {
            budget: Budget::new(Limits {
                windows: vec![
                    Rolling::per_second(cap(per_second)),
                    Rolling::per_minute(cap(per_minute)),
                    Rolling {
                        max: cap(per_30min),
                        span: HORIZON,
                    },
                ],
                per_day: None,
            }),
        }
    }

    /// Web `ORDER_LIMITER`: 8/s, 475/min, 1900/30min (published 10/500/2000).
    pub fn order() -> Self {
        Self::new(8, 475, 1900)
    }

    /// Web `STANDARD_LIMITER`: 45/s, 475/min, 1900/30min (published 50/500/2000).
    pub fn standard() -> Self {
        Self::new(45, 475, 1900)
    }

    /// Wait for a send slot, at most `MAX_QUEUE_WAIT`; refused before
    /// sending past it (a definite refusal inside an order placement).
    pub async fn acquire(&self) -> Result<()> {
        self.acquire_until(Instant::now() + MAX_QUEUE_WAIT).await
    }

    /// Wait for a send slot until `deadline`.
    pub async fn acquire_until(&self, deadline: Instant) -> Result<()> {
        self.budget.acquire(deadline).await.map_err(|d| {
            tracing::warn!("Upstox request refused before sending: {:?}", d);
            outcome::busy(BUSY_MESSAGE)
        })
    }

    #[cfg(test)]
    pub fn booked(&self) -> usize {
        self.budget.recent()
    }
}

/// Reactive retry budget for read endpoints (web `MAX_RETRIES`).
pub const MAX_RETRIES: u32 = 3;
/// Web `BASE_BACKOFF`: 1, 2, 4 s.
pub const BASE_BACKOFF: Duration = Duration::from_secs(1);
/// Upstox's rate-limit error code.
pub const RATE_LIMIT_CODE: &str = "UDAPI10005";

/// Delay before retry `attempt` (0-based): `Retry-After` when Upstox sends
/// it (floored at 50 ms), else 1, 2, 4 s.
pub fn retry_delay(retry_after: Option<&str>, attempt: u32) -> Duration {
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<f64>().ok()) {
        if secs.is_finite() && secs >= 0.0 {
            return Duration::from_secs_f64(secs.clamp(0.05, 60.0));
        }
    }
    BASE_BACKOFF.saturating_mul(1u32 << attempt.min(6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn per_second_window_spaces_a_burst() {
        let l = WindowLimiter::new(2, 100, 1000);
        let start = Instant::now();
        for _ in 0..5 {
            l.acquire().await.unwrap();
        }
        // Slots: 0, 0, 1, 1, 2 seconds.
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    /// The minute window binds after the second window; a send it would
    /// hold past the queue bound is refused at once (BRL-05), and one with
    /// a longer deadline waits for the minute.
    #[tokio::test(start_paused = true)]
    async fn per_minute_window_binds_after_the_second_window() {
        let l = WindowLimiter::new(100, 3, 1000);
        let start = Instant::now();
        for _ in 0..3 {
            l.acquire().await.unwrap();
        }
        let e = l.acquire().await.unwrap_err();
        assert_eq!(e.client_message(), BUSY_MESSAGE);
        assert_eq!(start.elapsed(), Duration::ZERO);
        l.acquire_until(start + Duration::from_secs(120))
            .await
            .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn old_slots_are_purged() {
        let l = WindowLimiter::new(10, 100, 1000);
        for _ in 0..5 {
            l.acquire().await.unwrap();
        }
        tokio::time::sleep(HORIZON + Duration::from_secs(1)).await;
        l.acquire().await.unwrap();
        assert_eq!(l.booked(), 1);
    }

    #[test]
    fn retry_delays() {
        assert_eq!(retry_delay(None, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(None, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(Some("0"), 0), Duration::from_millis(50));
        assert_eq!(retry_delay(Some("2.5"), 1), Duration::from_millis(2500));
        assert_eq!(retry_delay(Some("soon"), 1), Duration::from_secs(2));
    }
}
