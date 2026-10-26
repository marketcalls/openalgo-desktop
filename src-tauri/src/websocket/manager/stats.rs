//! A broker feed's counters, for health, Run diagnostics and tests:
//! connections, ends, stalls, the events received and when the last one
//! arrived, and what the feed's readers skipped (`losses`).

use super::losses::{self, Reader, SinceConnect};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// Counters for monitoring and tests.
#[derive(Debug)]
pub struct FeedStats {
    pub connects: AtomicU64,
    pub disconnects: AtomicU64,
    pub stalls: AtomicU64,
    pub events: AtomicU64,
    /// When the last tick, depth or order update arrived: milliseconds
    /// after `epoch`, plus one; 0 before the first (DIA-01 diagnostics).
    last_event: AtomicU64,
    epoch: Instant,
    /// What the feed's readers skipped, and the counts at the current
    /// connection's start (NEW-REL-05).
    losses: losses::Losses,
}

impl Default for FeedStats {
    fn default() -> Self {
        Self {
            connects: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            stalls: AtomicU64::new(0),
            events: AtomicU64::new(0),
            last_event: AtomicU64::new(0),
            epoch: Instant::now(),
            losses: losses::Losses::default(),
        }
    }
}

impl FeedStats {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX - 1)
    }

    pub(super) fn mark_event(&self) {
        self.events.fetch_add(1, Ordering::Relaxed);
        self.last_event
            .store(self.now_ms().saturating_add(1), Ordering::Relaxed);
    }

    /// A reader of this feed fell behind and skipped `n` events.
    pub fn note_skipped(&self, reader: Reader, n: u64) {
        self.losses.note(reader, n);
    }

    pub(super) fn mark_connected(&self) {
        self.losses
            .connected(self.events.load(Ordering::Relaxed), self.now_ms());
    }

    /// Events received and skipped by each reader since the current (or
    /// last) connection began.
    pub fn since_connect(&self) -> SinceConnect {
        self.losses
            .since_connect(self.events.load(Ordering::Relaxed), self.now_ms())
    }

    /// What each reader skipped since OpenAlgo started, every reader in
    /// `Reader::ALL` order; a reconnect does not reset it.
    pub(crate) fn skipped_since_start(&self) -> Vec<(Reader, u64)> {
        self.losses.since_start()
    }

    /// How long ago the last tick, depth or order update arrived; None
    /// before the first.
    pub fn last_event_age(&self) -> Option<Duration> {
        let at = self.last_event.load(Ordering::Relaxed).checked_sub(1)?;
        Some(
            self.epoch
                .elapsed()
                .saturating_sub(Duration::from_millis(at)),
        )
    }
}
