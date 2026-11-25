//! The feed calls the sandbox has decided and its worker has not made yet
//! (web `sandbox/websocket_execution_engine.py` `_feed_calls`,
//! marketcalls/openalgo#2192 5cf708a8a and 398f614cc).
//!
//! The engine decides what to stream on its own task and never waits for
//! the broker: [`FeedTicks`](super::FeedTicks) queues each decision here in
//! the same hold of its watch list as the change that made it, and one
//! worker makes the calls, oldest first. A subscribe can take the broker
//! up to 55 s to answer (its step before a new instrument, then its
//! admission), so while it does not answer, the order path goes on and
//! the calls wait. A call that has become unnecessary is cancelled before
//! it starts, so the queue does not grow with every order meanwhile and a
//! new symbol's subscribe does not wait behind a backlog of them:
//!
//! * a release cancels its symbol's subscribe that has not started:
//!   neither reaches the broker, and the symbol ends unstreamed, as it
//!   would have (a resubscribe it cancels may have something held, so the
//!   release takes its place);
//! * a release followed by a subscribe is kept: the release lets go of
//!   what is held, and the subscribe is then the one that streams it;
//! * a call joins one of its kind already waiting for its symbol;
//! * nothing waiting for another symbol is touched.
//!
//! So no symbol has more than two calls waiting. Subscribes are capped
//! besides ([`CAPACITY`], reached only by that many symbols at once): one
//! turned away is counted, and the adapter keeps its symbol on the watch
//! list, so the engine's quiet-feed backoff asks for it again (SB-01) while
//! its orders are checked by polling. Releases may use the room above the
//! cap, up to twice it, since leaving one out keeps a stream nobody needs.

use crate::brokers::common::streaming::FeedSubscription;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Notify;

/// Most subscribes and resubscribes waiting at once.
pub(super) const CAPACITY: usize = 1024;

/// Most calls waiting at once: releases may use the room above
/// [`CAPACITY`].
const RELEASE_CAPACITY: usize = 2 * CAPACITY;

/// One feed call for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum WatchCmd {
    Watch(FeedSubscription),
    Unwatch(FeedSubscription),
    /// Drop the subscription (if one was made) and make it again: the
    /// engine saw no tick for the symbol, or its subscribe failed or was
    /// turned away.
    Resubscribe(FeedSubscription),
}

impl WatchCmd {
    fn subscription(&self) -> &FeedSubscription {
        match self {
            Self::Watch(s) | Self::Unwatch(s) | Self::Resubscribe(s) => s,
        }
    }

    fn is_for(&self, s: &FeedSubscription) -> bool {
        let own = self.subscription();
        own.symbol == s.symbol && own.exchange == s.exchange
    }

    fn subscribes(&self) -> bool {
        !matches!(self, Self::Unwatch(_))
    }
}

/// Why a call was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refused {
    /// The queue is full; the call is counted in
    /// [`FeedCalls::turned_away`].
    Full,
    /// The adapter has stopped.
    Closed,
}

#[derive(Default)]
struct Waiting {
    calls: VecDeque<WatchCmd>,
    closed: bool,
}

/// The queue between the adapter and its worker. Its lock is held only
/// for in-memory list work, never across a call to the feed.
#[derive(Default)]
pub(super) struct FeedCalls {
    waiting: Mutex<Waiting>,
    ready: Notify,
    turned_away: AtomicU64,
}

impl FeedCalls {
    /// Queue a call, after cancelling or joining what it makes unnecessary
    /// for its symbol (see the module doc).
    pub(super) fn push(&self, call: WatchCmd) -> Result<(), Refused> {
        let mut waiting = self.waiting.lock();
        if waiting.closed {
            return Err(Refused::Closed);
        }
        let last = waiting
            .calls
            .iter()
            .rposition(|c| c.is_for(call.subscription()));
        if let Some(i) = last {
            match (waiting.calls[i].subscribes(), call.subscribes()) {
                (true, true) | (false, false) => return Ok(()),
                (true, false) => {
                    let cancelled = waiting.calls.remove(i);
                    // What waits before a subscribe for its symbol can only
                    // be a release, which already lets go of what is held.
                    let released = waiting.calls.iter().any(|c| c.is_for(call.subscription()));
                    if matches!(cancelled, Some(WatchCmd::Resubscribe(_))) && !released {
                        waiting.calls.insert(i, call);
                    }
                    return Ok(());
                }
                (false, true) => {}
            }
        }
        let room = if call.subscribes() {
            CAPACITY
        } else {
            RELEASE_CAPACITY
        };
        if waiting.calls.len() >= room {
            drop(waiting);
            self.turned_away.fetch_add(1, Ordering::Relaxed);
            return Err(Refused::Full);
        }
        waiting.calls.push_back(call);
        drop(waiting);
        self.ready.notify_one();
        Ok(())
    }

    /// The oldest call, once there is one; `None` once the queue is
    /// closed.
    pub(super) async fn next(&self) -> Option<WatchCmd> {
        loop {
            let ready = self.ready.notified();
            {
                let mut waiting = self.waiting.lock();
                if waiting.closed {
                    return None;
                }
                if let Some(call) = waiting.calls.pop_front() {
                    return Some(call);
                }
            }
            ready.await;
        }
    }

    /// Hand out nothing more and refuse new calls: the adapter or its
    /// worker has stopped, and the worker lets go of everything it holds.
    pub(super) fn close(&self) {
        let mut waiting = self.waiting.lock();
        waiting.closed = true;
        waiting.calls.clear();
        drop(waiting);
        self.ready.notify_one();
    }

    /// Calls a full queue turned away since the adapter started.
    pub(super) fn turned_away(&self) -> u64 {
        self.turned_away.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests;
