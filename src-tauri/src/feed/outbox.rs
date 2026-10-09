//! Per-client outbound queue with bounded memory.
//!
//! Two lanes, drained control first:
//! * control frames (acks, errors, pongs, order updates) in a FIFO capped at
//!   `control_cap`. A client that lets this lane fill is not reading at all;
//!   the queue reports overflow and the connection is closed rather than
//!   buffering without limit.
//! * market data, one slot per `(instrument, mode)`. A newer frame for a key
//!   replaces the queued one (the slow client gets the latest price, never a
//!   backlog), so this lane can hold at most one frame per subscription.
//!   `market_cap` is a second fence: past it the oldest key is dropped.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify;

/// Market-data slot key: interned instrument id and mode number.
pub type MarketKey = (u64, u8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Text(String),
    Ping,
    Close(u16, String),
}

struct State {
    control: VecDeque<Outgoing>,
    market: HashMap<MarketKey, String>,
    order: VecDeque<MarketKey>,
    close: Option<(u16, String)>,
    /// A close to send once the control frames already queued are written.
    close_after: Option<(u16, String)>,
    finished: bool,
    overflowed: bool,
}

pub struct Outbox {
    state: Mutex<State>,
    notify: Notify,
    overflow: Notify,
    control_cap: usize,
    market_cap: usize,
    coalesced: AtomicU64,
    dropped: AtomicU64,
}

impl Outbox {
    pub fn new(control_cap: usize, market_cap: usize) -> Self {
        Self {
            state: Mutex::new(State {
                control: VecDeque::new(),
                market: HashMap::new(),
                order: VecDeque::new(),
                close: None,
                close_after: None,
                finished: false,
                overflowed: false,
            }),
            notify: Notify::new(),
            overflow: Notify::new(),
            control_cap: control_cap.max(1),
            market_cap: market_cap.max(1),
            coalesced: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Queue a control frame. Returns `false` (and flags overflow) when the
    /// client has stopped reading.
    pub fn push_control(&self, frame: String) -> bool {
        self.push(Outgoing::Text(frame))
    }

    pub fn push_ping(&self) -> bool {
        self.push(Outgoing::Ping)
    }

    fn push(&self, item: Outgoing) -> bool {
        {
            let mut s = self.state.lock();
            if s.close.is_some() || s.finished {
                return true;
            }
            if s.control.len() >= self.control_cap {
                if !s.overflowed {
                    s.overflowed = true;
                    drop(s);
                    self.overflow.notify_one();
                }
                return false;
            }
            s.control.push_back(item);
        }
        self.notify.notify_one();
        true
    }

    /// Queue market data for `key`, replacing an undelivered frame for the
    /// same key.
    pub fn push_market(&self, key: MarketKey, frame: String) {
        {
            let mut s = self.state.lock();
            if s.close.is_some() || s.finished {
                return;
            }
            if s.market.insert(key, frame).is_some() {
                self.coalesced.fetch_add(1, Ordering::Relaxed);
                return;
            }
            s.order.push_back(key);
            while s.order.len() > self.market_cap {
                if let Some(old) = s.order.pop_front() {
                    s.market.remove(&old);
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.notify.notify_one();
    }

    /// Ask the writer to send a close frame after nothing else. Pending
    /// frames are discarded.
    pub fn close(&self, code: u16, reason: &str) {
        {
            let mut s = self.state.lock();
            if s.close.is_some() || s.finished {
                return;
            }
            s.control.clear();
            s.market.clear();
            s.order.clear();
            s.close = Some((code, reason.to_string()));
        }
        self.notify.notify_one();
    }

    /// Ask the writer to send a close frame once the control frames already
    /// queued (an error frame) are written. Market frames are discarded.
    pub fn close_after_control(&self, code: u16, reason: &str) {
        {
            let mut s = self.state.lock();
            if s.close.is_some() || s.close_after.is_some() || s.finished {
                return;
            }
            s.market.clear();
            s.order.clear();
            s.close_after = Some((code, reason.to_string()));
        }
        self.notify.notify_one();
    }

    /// Stop the writer without a close frame (the socket is gone).
    pub fn finish(&self) {
        {
            let mut s = self.state.lock();
            s.finished = true;
            s.control.clear();
            s.market.clear();
            s.order.clear();
        }
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<Option<Outgoing>> {
        let mut s = self.state.lock();
        if s.finished {
            return Some(None);
        }
        if let Some((code, reason)) = s.close.take() {
            s.finished = true;
            return Some(Some(Outgoing::Close(code, reason)));
        }
        if let Some(item) = s.control.pop_front() {
            return Some(Some(item));
        }
        if let Some((code, reason)) = s.close_after.take() {
            s.finished = true;
            return Some(Some(Outgoing::Close(code, reason)));
        }
        while let Some(k) = s.order.pop_front() {
            if let Some(f) = s.market.remove(&k) {
                return Some(Some(Outgoing::Text(f)));
            }
        }
        None
    }

    /// Next frame to write; `None` once closed or finished.
    pub async fn next(&self) -> Option<Outgoing> {
        loop {
            if let Some(item) = self.pop() {
                return item;
            }
            self.notify.notified().await;
        }
    }

    /// Resolves once the control lane has overflowed.
    pub async fn overflowed(&self) {
        loop {
            if self.state.lock().overflowed {
                return;
            }
            self.overflow.notified().await;
        }
    }

    /// Frames waiting to be written (control plus market slots).
    pub fn len(&self) -> usize {
        let s = self.state.lock();
        s.control.len() + s.market.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Market frames replaced by a newer one before delivery.
    pub fn coalesced(&self) -> u64 {
        self.coalesced.load(Ordering::Relaxed)
    }

    /// Market frames dropped by the `market_cap` fence.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn market_lane_keeps_latest_per_key_and_control_goes_first() {
        let o = Outbox::new(8, 100);
        for i in 0..1000 {
            o.push_market((1, 1), format!("a{}", i));
            o.push_market((2, 1), format!("b{}", i));
        }
        o.push_control("ack".into());
        assert_eq!(o.len(), 3);
        assert_eq!(o.coalesced(), 1998);
        assert_eq!(o.next().await, Some(Outgoing::Text("ack".into())));
        assert_eq!(o.next().await, Some(Outgoing::Text("a999".into())));
        assert_eq!(o.next().await, Some(Outgoing::Text("b999".into())));
        assert!(o.is_empty());
    }

    #[tokio::test]
    async fn control_lane_overflow_is_reported_not_buffered() {
        let o = Outbox::new(2, 10);
        assert!(o.push_control("1".into()));
        assert!(o.push_control("2".into()));
        assert!(!o.push_control("3".into()));
        o.overflowed().await;
        assert_eq!(o.len(), 2);
    }

    #[tokio::test]
    async fn market_cap_drops_oldest_key() {
        let o = Outbox::new(2, 3);
        for k in 0..10u64 {
            o.push_market((k, 1), k.to_string());
        }
        assert_eq!(o.len(), 3);
        assert_eq!(o.dropped(), 7);
        assert_eq!(o.next().await, Some(Outgoing::Text("7".into())));
    }

    #[tokio::test]
    async fn close_discards_pending_and_ends_stream() {
        let o = Outbox::new(4, 4);
        o.push_control("x".into());
        o.close(4401, "auth timeout");
        assert_eq!(
            o.next().await,
            Some(Outgoing::Close(4401, "auth timeout".into()))
        );
        assert_eq!(o.next().await, None);
    }

    #[tokio::test]
    async fn a_close_after_control_frames_sends_them_first() {
        let o = Outbox::new(8, 8);
        o.push_control("error".into());
        o.close_after_control(4401, "Invalid API key");
        o.push_control("late ping".into());
        assert_eq!(o.next().await, Some(Outgoing::Text("error".into())));
        assert_eq!(o.next().await, Some(Outgoing::Text("late ping".into())));
        assert_eq!(
            o.next().await,
            Some(Outgoing::Close(4401, "Invalid API key".into()))
        );
        assert_eq!(o.next().await, None);
    }
}
