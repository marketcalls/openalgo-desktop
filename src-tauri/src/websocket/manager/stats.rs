//! A broker feed's counters, for health, Run diagnostics and tests:
//! connections, ends, stalls and why the last connection ended, the events
//! received and when the last one arrived, and what the feed's readers
//! skipped (`losses`).
//!
//! The connection counts are per broker: they run from OpenAlgo's start
//! until another broker's feed takes the manager over, and then start
//! again, so the Health page never charges one broker with another's
//! reconnects.

use super::losses::{self, Reader, SinceConnect};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;

/// Why a broker feed's connection ended, or an attempt to make one failed,
/// so the manager connected again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropCause {
    /// The connection could not be made: the network, the broker refused
    /// the handshake or did not answer in time.
    ConnectFailed,
    /// The broker or the network closed the connection.
    Closed,
    /// Nothing at all arrived for the stall timeout.
    Silent,
    /// The connection answered but market data stopped (`data_stalled`).
    DataStopped,
    /// The broker did not answer a heartbeat in time.
    HeartbeatUnanswered,
    /// The broker stopped taking frames (TIME-02).
    NotTakingFrames,
    /// The broker did not confirm the login in time (TIME-05).
    LoginNotConfirmed,
    /// The computer woke from sleep (ARCH-03).
    Woke,
}

impl DropCause {
    /// The name health reports it by.
    pub fn name(self) -> &'static str {
        match self {
            DropCause::ConnectFailed => "connect_failed",
            DropCause::Closed => "closed",
            DropCause::Silent => "silent",
            DropCause::DataStopped => "data_stopped",
            DropCause::HeartbeatUnanswered => "heartbeat_unanswered",
            DropCause::NotTakingFrames => "not_taking_frames",
            DropCause::LoginNotConfirmed => "login_not_confirmed",
            DropCause::Woke => "woke",
        }
    }

    /// The broker went quiet, as opposed to the connection failing.
    pub fn is_stall(self) -> bool {
        matches!(
            self,
            DropCause::Silent | DropCause::DataStopped | DropCause::HeartbeatUnanswered
        )
    }
}

/// The latest reconnect: why, and when it began.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LastDrop {
    pub cause: DropCause,
    pub at: DateTime<Utc>,
}

/// A feed's connection counts for the broker it streams, read together.
#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    /// The broker whose feed the counts are for; None before the first.
    pub broker: Option<&'static str>,
    pub connects: u64,
    pub disconnects: u64,
    pub stalls: u64,
    pub last_drop: Option<LastDrop>,
}

#[derive(Debug, Default)]
struct Current {
    broker: Option<&'static str>,
    last_drop: Option<LastDrop>,
}

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
    current: Mutex<Current>,
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
            current: Mutex::new(Current::default()),
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

    /// A feed for `broker` takes the manager over: the connection counts
    /// start again when it is another broker's.
    pub(super) fn begin(&self, broker: &'static str) {
        let mut cur = self.current.lock();
        if cur.broker.is_some_and(|b| b != broker) {
            for n in [&self.connects, &self.disconnects, &self.stalls] {
                n.store(0, Ordering::Relaxed);
            }
            cur.last_drop = None;
        }
        cur.broker = Some(broker);
    }

    pub(super) fn opened(&self) {
        self.connects.fetch_add(1, Ordering::Relaxed);
    }

    /// The connection ended (or could not be made) for `cause`, and the
    /// manager will connect again.
    pub(super) fn dropped(&self, cause: DropCause) {
        self.disconnects.fetch_add(1, Ordering::Relaxed);
        if cause.is_stall() {
            self.stalls.fetch_add(1, Ordering::Relaxed);
        }
        self.current.lock().last_drop = Some(LastDrop {
            cause,
            at: Utc::now(),
        });
    }

    /// The connection counts for the broker the feed streams.
    pub fn connection(&self) -> Connection {
        let cur = self.current.lock();
        Connection {
            broker: cur.broker,
            connects: self.connects.load(Ordering::Relaxed),
            disconnects: self.disconnects.load(Ordering::Relaxed),
            stalls: self.stalls.load(Ordering::Relaxed),
            last_drop: cur.last_drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Health page's reconnect counts: every drop is counted with its
    /// cause and time, a quiet broker as a stall too, and another broker's
    /// feed starts the counts again.
    #[test]
    fn drops_are_counted_with_their_cause_per_broker() {
        let s = FeedStats::default();
        assert_eq!(s.connection().broker, None);
        s.begin("kotak");
        s.opened();
        s.dropped(DropCause::Silent);
        s.opened();
        s.dropped(DropCause::Closed);
        s.opened();
        let c = s.connection();
        assert_eq!(c.broker, Some("kotak"));
        assert_eq!((c.connects, c.disconnects, c.stalls), (3, 2, 1));
        let last = c.last_drop.expect("the last drop");
        assert_eq!(last.cause, DropCause::Closed);
        assert!((Utc::now() - last.at).num_seconds() < 5, "{:?}", last.at);

        s.begin("kotak");
        assert_eq!(s.connection().connects, 3, "a new login keeps the counts");
        s.begin("indmoney");
        let c = s.connection();
        assert_eq!(c.broker, Some("indmoney"));
        assert_eq!((c.connects, c.disconnects, c.stalls), (0, 0, 0));
        assert_eq!(c.last_drop, None);
    }

    #[test]
    fn only_a_quiet_broker_is_a_stall() {
        let stalls: Vec<&str> = [
            DropCause::ConnectFailed,
            DropCause::Closed,
            DropCause::Silent,
            DropCause::DataStopped,
            DropCause::HeartbeatUnanswered,
            DropCause::NotTakingFrames,
            DropCause::LoginNotConfirmed,
            DropCause::Woke,
        ]
        .into_iter()
        .filter(|c| c.is_stall())
        .map(DropCause::name)
        .collect();
        assert_eq!(stalls, ["silent", "data_stopped", "heartbeat_unanswered"]);
    }
}
