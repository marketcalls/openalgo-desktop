//! What one broker message brought, as one unit, and which session of
//! which connection it came from (audit R107-04, R107-17).
//!
//! A packet's events (Zerodha's full quote and its book) go out together,
//! so a reader never sees half a packet, nor another connection's events
//! in between: the bridge pairs a quote with its book inside the packet,
//! never by peeking at what happens to be queued. Each connection's
//! sessions are numbered across the manager, and a session that opens or
//! ends is announced, so state that came from the session it replaced can
//! be told apart from state of the manager's other connections (a pool's
//! new connection replaces nothing).
//!
//! The same events still go out one by one on the manager's tick channel
//! for readers that want single events (scalping, strategies, sandbox).

use crate::brokers::common::streaming::{MarketEvent, ReleaseRefusals};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// A connection task's sessions take the low bits of their numbers, the
/// task's place in the order of tasks started the high ones.
const TASK_SHIFT: u32 = 32;

/// One session of one of a manager's connections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionId {
    /// The manager's connection (0: the first).
    pub conn: usize,
    /// The session's number, unique across the manager's connections. A
    /// connection task's sessions grow as they open, and all of them lie
    /// above those of every task started before it: a task being stopped
    /// can still finish connecting after its replacement has opened, and
    /// must not outrank it.
    pub session: u64,
}

/// What a connection published.
#[derive(Debug)]
pub enum PacketKind {
    /// The session `from` opened: anything an earlier session of the same
    /// connection said is no longer current.
    Opened,
    /// The session `from` ended (dropped, refused or stopped): nothing it
    /// said is kept current any more, and the connection may stay down.
    Closed,
    /// The market events of one broker message, in order.
    Events(Vec<MarketEvent>),
    /// The session `from` was refused these instruments of its whole set,
    /// with why, and the manager has let go of them (review B2).
    Refused(ReleaseRefusals),
}

#[derive(Debug)]
pub struct Packet {
    pub from: SessionId,
    pub kind: PacketKind,
    /// How many market events the manager had published before this packet:
    /// a reader that fell behind compares it with what it has read to know
    /// the events it missed (`RecvError::Lagged` counts packets).
    pub events_before: u64,
}

impl Packet {
    /// How many market events the manager had published up to and
    /// including this packet.
    pub fn events_through(&self) -> u64 {
        match &self.kind {
            PacketKind::Events(events) => self.events_before + events.len() as u64,
            PacketKind::Opened | PacketKind::Closed | PacketKind::Refused(_) => self.events_before,
        }
    }
}

pub type MarketPacket = Arc<Packet>;

/// Where a manager's connections publish packets, and its session numbers.
#[derive(Clone)]
pub(super) struct PacketOut {
    tx: broadcast::Sender<MarketPacket>,
    /// Connection tasks started so far.
    tasks: Arc<AtomicU64>,
    /// Market events published so far.
    events: Arc<AtomicU64>,
}

impl Default for PacketOut {
    fn default() -> Self {
        Self::with_capacity(1)
    }
}

impl PacketOut {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            tx: broadcast::channel(capacity.max(1)).0,
            tasks: Arc::default(),
            events: Arc::default(),
        }
    }

    pub(super) fn subscribe(&self) -> broadcast::Receiver<MarketPacket> {
        self.tx.subscribe()
    }

    /// A task starts for connection `conn`: where its sessions' numbers
    /// begin.
    pub(super) fn task_started(&self, conn: usize) -> SessionId {
        let task = self.tasks.fetch_add(1, Ordering::Relaxed) + 1;
        SessionId {
            conn,
            session: task << TASK_SHIFT,
        }
    }

    /// The session after `last` on its connection task opened: number it
    /// and say so.
    pub(super) fn open(&self, last: SessionId) -> SessionId {
        let from = SessionId {
            session: last.session + 1,
            ..last
        };
        let before = self.events.load(Ordering::Relaxed);
        self.publish(from, PacketKind::Opened, before);
        from
    }

    /// The session `from` ended: say so.
    pub(super) fn close(&self, from: SessionId) {
        let before = self.events.load(Ordering::Relaxed);
        self.publish(from, PacketKind::Closed, before);
    }

    /// The session `from` was refused these instruments: say so.
    pub(super) fn refused(&self, from: SessionId, refused: ReleaseRefusals) {
        let before = self.events.load(Ordering::Relaxed);
        self.publish(from, PacketKind::Refused(refused), before);
    }

    /// A number no session opened so far exceeds and no later task's
    /// reaches (tests).
    #[cfg(test)]
    pub(super) fn latest(&self) -> u64 {
        ((self.tasks.load(Ordering::Relaxed) + 1) << TASK_SHIFT) - 1
    }

    /// One message's market events, together (none: nothing is sent; no
    /// receiver yet is normal and nothing is retained).
    pub(super) fn send(&self, from: SessionId, events: Vec<MarketEvent>) {
        if !events.is_empty() {
            let before = self
                .events
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            self.publish(from, PacketKind::Events(events), before);
        }
    }

    fn publish(&self, from: SessionId, kind: PacketKind, events_before: u64) {
        let _ = self.tx.send(Arc::new(Packet {
            from,
            kind,
            events_before,
        }));
    }
}
