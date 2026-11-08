//! Whether market data still flows on a Noren socket that still answers
//! heartbeats (audit FB-03; web `<member>_websocket.py`
//! `_update_last_data_time` and `_check_connection_health`, issue #2075,
//! the same in Shoonya, Flattrade, Zebu and TradeSmart).
//!
//! A Noren session can keep acknowledging `{"t":"h"}` while it delivers no
//! touchline or depth frame, so the manager's frame watchdog never fires
//! and nothing that runs on ticks (stops, sandbox triggers, Flow) does
//! either. The watchdog arms only once data has been arriving over time:
//! frames in [`ARM_BUCKETS`] distinct [`ARM_BUCKET_MS`] buckets, all within
//! [`ARM_WINDOW_MS`]. An after-hours snapshot then never arms it (the
//! socket would be recycled every [`SILENCE_MS`] until the market opened),
//! while a live session that falls silent for [`SILENCE_MS`] is reconnected.
//! Arming lasts for one connection.

use std::collections::VecDeque;

/// Silence after which an armed watchdog reports a stall (web
/// `DATA_SILENCE_TIMEOUT = 180`).
pub const SILENCE_MS: i64 = 180_000;
/// Width of one arming bucket (web `DATA_ARM_BUCKET = 30`).
pub const ARM_BUCKET_MS: i64 = 30_000;
/// Distinct buckets that must see data (web `DATA_ARM_BUCKETS = 3`).
pub const ARM_BUCKETS: usize = 3;
/// Span the buckets must fall in (web `DATA_ARM_WINDOW = 300`).
pub const ARM_WINDOW_MS: i64 = 300_000;

/// The market data liveness of one connection.
#[derive(Debug, Default)]
pub struct Liveness {
    last_data_ms: Option<i64>,
    armed: bool,
    /// Start of each bucket data arrived in, at most [`ARM_BUCKETS`].
    buckets: VecDeque<i64>,
}

impl Liveness {
    /// A market data frame arrived at `now_ms`.
    pub fn note_data(&mut self, now_ms: i64) {
        self.last_data_ms = Some(now_ms);
        if self.armed {
            return;
        }
        let bucket = now_ms - now_ms.rem_euclid(ARM_BUCKET_MS);
        if self.buckets.back() != Some(&bucket) {
            if self.buckets.len() == ARM_BUCKETS {
                self.buckets.pop_front();
            }
            self.buckets.push_back(bucket);
        }
        let spread = self
            .buckets
            .front()
            .is_some_and(|first| now_ms - first <= ARM_WINDOW_MS);
        if self.buckets.len() == ARM_BUCKETS && spread {
            self.armed = true;
            tracing::info!("Market data is flowing; watching for tick silence from here on");
        }
    }

    /// A new connection: nothing has arrived on it yet.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether data has stopped as of `now_ms`: armed, and silent for more
    /// than [`SILENCE_MS`].
    pub fn stalled_at(&self, now_ms: i64) -> bool {
        self.armed
            && self
                .last_data_ms
                .is_some_and(|last| now_ms - last > SILENCE_MS)
    }
}

#[cfg(test)]
mod tests;
