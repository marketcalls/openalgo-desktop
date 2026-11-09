//! What one broker message brought, as one unit, and which session of
//! which connection it came from (audit R107-04, R107-17).
//!
//! A packet's events (Zerodha's full quote and its book) go out together,
//! so a reader never sees half a packet, nor another connection's events
//! in between: the bridge pairs a quote with its book inside the packet,
//! never by peeking at what happens to be queued. Each connection's
//! sessions are numbered across the manager, and a session that opens is
//! announced, so state that came from the session it replaced can be told
//! apart from state of the manager's other connections (a pool's new
//! connection replaces nothing).
//!
//! The same events still go out one by one on the manager's tick channel
//! for readers that want single events (scalping, strategies, sandbox).

use crate::brokers::common::streaming::MarketEvent;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// One session of one of a manager's connections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SessionId {
    /// The manager's connection (0: the first).
    pub conn: usize,
    /// The session's number, unique across the manager's connections and
    /// growing with each session opened.
    pub session: u64,
}

/// What a connection published.
#[derive(Debug)]
pub enum PacketKind {
    /// The session `from` opened: anything an earlier session of the same
    /// connection said is no longer current.
    Opened,
    /// The market events of one broker message, in order.
    Events(Vec<MarketEvent>),
}

#[derive(Debug)]
pub struct Packet {
    pub from: SessionId,
    pub kind: PacketKind,
}

pub type MarketPacket = Arc<Packet>;

/// Where a manager's connections publish packets, and its session numbers.
#[derive(Clone)]
pub(super) struct PacketOut {
    tx: broadcast::Sender<MarketPacket>,
    sessions: Arc<AtomicU64>,
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
            sessions: Arc::default(),
        }
    }

    pub(super) fn subscribe(&self) -> broadcast::Receiver<MarketPacket> {
        self.tx.subscribe()
    }

    /// A new session of connection `conn` opened: number it and say so.
    pub(super) fn open(&self, conn: usize) -> SessionId {
        let session = self.sessions.fetch_add(1, Ordering::Relaxed) + 1;
        let from = SessionId { conn, session };
        let _ = self.tx.send(Arc::new(Packet {
            from,
            kind: PacketKind::Opened,
        }));
        from
    }

    /// The latest session number given out (tests).
    #[cfg(test)]
    pub(super) fn latest(&self) -> u64 {
        self.sessions.load(Ordering::Relaxed)
    }

    /// One message's market events, together (none: nothing is sent; no
    /// receiver yet is normal and nothing is retained).
    pub(super) fn send(&self, from: SessionId, events: Vec<MarketEvent>) {
        if !events.is_empty() {
            let _ = self.tx.send(Arc::new(Packet {
                from,
                kind: PacketKind::Events(events),
            }));
        }
    }
}
