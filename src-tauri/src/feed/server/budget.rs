//! What one connection may send (RATE-01): a budget of frames and of bytes,
//! each a token bucket, charged for every frame before it is parsed. A
//! client that spends either is closed with 1008 (policy violation), so a
//! flood of messages costs it its connection instead of the app's CPU: a
//! 1 MiB frame is JSON-parsed in full, which on a Raspberry Pi serving a
//! LAN or a tunnel is the expensive part.
//!
//! The defaults leave every real client far inside: an SDK that subscribes
//! the most instruments one client may hold one message at a time, the
//! app's own pages, and a bulk subscribe of all of them in one message.
//! The web reads frames without a limit; this is a deliberate addition.

use tokio::time::Instant;

/// A connection's allowance. Each bucket starts full, holds at most its
/// burst and refills at its rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameBudget {
    pub burst_frames: u32,
    pub frames_per_second: u32,
    pub burst_bytes: usize,
    pub bytes_per_second: usize,
}

impl Default for FrameBudget {
    fn default() -> Self {
        Self {
            // Every one of the 3000 instruments a client may hold
            // unsubscribed and subscribed again, one message each, at once.
            burst_frames: 6000,
            frames_per_second: 1000,
            burst_bytes: 8 * 1024 * 1024,
            bytes_per_second: 2 * 1024 * 1024,
        }
    }
}

struct Bucket {
    level: f64,
    burst: f64,
    rate: f64,
    at: Instant,
}

impl Bucket {
    fn new(burst: f64, rate: f64, now: Instant) -> Self {
        Self {
            level: burst,
            burst,
            rate,
            at: now,
        }
    }

    /// Takes `n` (at most a full bucket) if it is there.
    fn take(&mut self, n: f64, now: Instant) -> bool {
        let refill = now.saturating_duration_since(self.at).as_secs_f64() * self.rate;
        self.level = (self.level + refill).min(self.burst);
        self.at = now;
        let n = n.min(self.burst);
        if self.level >= n {
            self.level -= n;
            true
        } else {
            false
        }
    }
}

/// One connection's meter.
pub(super) struct Meter {
    frames: Bucket,
    bytes: Bucket,
}

impl Meter {
    pub(super) fn new(b: FrameBudget, now: Instant) -> Self {
        Self {
            frames: Bucket::new(b.burst_frames as f64, b.frames_per_second as f64, now),
            bytes: Bucket::new(b.burst_bytes as f64, b.bytes_per_second as f64, now),
        }
    }

    /// Charge one frame of `len` bytes; false once either budget is spent.
    pub(super) fn admit(&mut self, len: usize, now: Instant) -> bool {
        // Both are charged, so a refused frame does not refill the other.
        let frame_ok = self.frames.take(1.0, now);
        let bytes_ok = self.bytes.take(len as f64, now);
        frame_ok && bytes_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn budget() -> FrameBudget {
        FrameBudget {
            burst_frames: 10,
            frames_per_second: 5,
            burst_bytes: 1000,
            bytes_per_second: 100,
        }
    }

    #[test]
    fn a_burst_then_the_rate() {
        let t0 = Instant::now();
        let mut m = Meter::new(budget(), t0);
        for _ in 0..10 {
            assert!(m.admit(1, t0));
        }
        assert!(!m.admit(1, t0), "the burst is spent");
        // One frame refills every 200 ms.
        assert!(m.admit(1, t0 + Duration::from_millis(210)));
        assert!(!m.admit(1, t0 + Duration::from_millis(220)));
    }

    #[test]
    fn bytes_are_budgeted_too() {
        let t0 = Instant::now();
        let mut m = Meter::new(budget(), t0);
        assert!(m.admit(900, t0));
        assert!(!m.admit(200, t0), "only 100 bytes left");
        // Two seconds refill 200 bytes on top of what was left.
        assert!(m.admit(200, t0 + Duration::from_secs(2)));
    }

    #[test]
    fn a_frame_larger_than_the_burst_needs_a_full_bucket() {
        let t0 = Instant::now();
        let mut m = Meter::new(budget(), t0);
        assert!(
            m.admit(5000, t0),
            "a full bucket admits one oversized frame"
        );
        assert!(!m.admit(5000, t0));
    }
}
