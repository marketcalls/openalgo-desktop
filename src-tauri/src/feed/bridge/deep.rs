//! The broker's deeper-book socket (Dhan 20, Fyers 50) and its fallback to
//! the main feed's levels, as the web's adapters do (lead decision
//! 2026-11-09; audit P-10):
//!
//! * the socket did not start for the session, or the broker refused it: a
//!   Depth subscription at its levels is taken on the main feed's levels,
//!   and acknowledged as the broker's web adapter acknowledges it (web
//!   Fyers: "TBT unavailable, falling back to 5-level depth",
//!   `fyers_websocket_adapter.py:373-381`; web Dhan gets there through its
//!   fallback monitor);
//! * a deep book that stays silent for the broker's timeout after its
//!   subscription and after its last book (Dhan: 30 s,
//!   `dhan_adapter.py:409-412, 972-1066`) falls back to the main feed's
//!   levels for as long as it is held; the web does not move it back, and
//!   neither does this.
//!
//! The acknowledgement names the depth the broker's web adapter names
//! (`dialect::subscribe_depth`); the frames carry the levels streamed.

use super::apply::Applied;
use super::{BrokerBridge, DesiredKey};
use crate::brokers::common::streaming::{FeedEvent, FeedSubscription};
use crate::feed::source::{InstrumentKey, DEFAULT_DEPTH};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// The deep books watched for silence, and those that fell back.
#[derive(Default)]
pub(super) struct DeepBooks {
    /// Silence after which a deep book falls back (`None`: never).
    timeout: Option<Duration>,
    /// Deep keys watched, since their subscription.
    watched: HashMap<DesiredKey, Instant>,
    /// When a deep book last came, per watched instrument.
    last_book: HashMap<InstrumentKey, Instant>,
    /// Keys served on the main feed's levels while held.
    pub(super) fell_back: HashSet<DesiredKey>,
}

impl DeepBooks {
    /// A new broker session: its own timeout, nothing watched.
    pub(super) fn reset(&mut self, timeout: Option<Duration>) {
        *self = DeepBooks {
            timeout,
            ..DeepBooks::default()
        };
    }

    /// `key`'s deep book was just taken.
    pub(super) fn watch(&mut self, key: &DesiredKey) {
        if self.timeout.is_some() {
            self.watched.insert(key.clone(), Instant::now());
        }
    }

    /// Forget keys no longer wanted, and books of instruments not watched.
    pub(super) fn retain(&mut self, wanted: impl Fn(&DesiredKey) -> bool) {
        self.watched.retain(|k, _| wanted(k));
        self.fell_back.retain(|k| wanted(k));
        let watched: HashSet<&InstrumentKey> = self.watched.keys().map(|k| &k.0).collect();
        self.last_book.retain(|i, _| watched.contains(i));
    }

    fn deadline(&self, key: &DesiredKey, since: Instant) -> Option<Instant> {
        let last = self
            .last_book
            .get(&key.0)
            .map_or(since, |at| (*at).max(since));
        Some(last + self.timeout?)
    }

    /// When the next watched book is due to fall back.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.watched
            .iter()
            .filter_map(|(k, since)| self.deadline(k, *since))
            .min()
    }

    /// The watched keys whose book has been silent too long, unwatched.
    fn take_silent(&mut self, now: Instant) -> Vec<DesiredKey> {
        let silent: Vec<DesiredKey> = self
            .watched
            .iter()
            .filter(|(k, since)| self.deadline(k, **since).is_some_and(|at| at <= now))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &silent {
            self.watched.remove(k);
        }
        silent
    }
}

/// The main feed's reference for a Depth key served at its own levels.
pub(super) fn on_main_levels(sub: FeedSubscription) -> FeedSubscription {
    FeedSubscription {
        depth: DEFAULT_DEPTH,
        ..sub
    }
}

impl BrokerBridge {
    /// A deep book came from the depth socket.
    pub(super) fn note_deep_book(&self, ev: &FeedEvent) {
        let FeedEvent::Depth(d) = ev else {
            return;
        };
        let key = InstrumentKey::new(d.symbol.clone(), d.exchange.clone());
        let mut st = self.state.lock();
        if st.deep_books.watched.keys().any(|k| k.0 == key) {
            st.deep_books.last_book.insert(key, Instant::now());
        }
    }

    /// Move the deep books silent too long to the main feed's levels: the
    /// main feed takes the instrument at Depth, then the deep book and its
    /// quote go. One the main feed cannot take is watched again.
    pub(super) async fn fall_back_silent(&self) {
        let mut any = false;
        let moving: Vec<(DesiredKey, FeedSubscription, FeedSubscription)> = {
            let mut st = self.state.lock();
            let silent = st.deep_books.take_silent(Instant::now());
            silent
                .into_iter()
                .filter_map(|k| match st.applied.remove(&k) {
                    Some(Applied::Deep { depth, quote }) => {
                        st.ops.start_moving(&k);
                        Some((k, depth, quote))
                    }
                    Some(other) => {
                        st.applied.insert(k, other);
                        None
                    }
                    None => None,
                })
                .collect()
        };
        for (k, depth, quote) in moving {
            let main = on_main_levels(depth.clone());
            let taken = self.manager.admit(vec![main.clone()]).await;
            let moved = taken.first().is_some_and(|r| r.is_ok());
            if moved {
                tracing::warn!(
                    "No {}-level book for {} on {} in {:?}; streaming the main feed's depth instead",
                    k.2,
                    k.0.symbol,
                    k.0.exchange,
                    self.state.lock().deep_books.timeout.unwrap_or_default()
                );
                let _ = self.depth_manager.unsubscribe(vec![depth]).await;
                let _ = self.manager.unsubscribe(vec![quote]).await;
            }
            any = true;
            let mut st = self.state.lock();
            st.ops.stop_moving(&k);
            if moved {
                st.applied.insert(k.clone(), Applied::Main(main));
                st.deep_books.fell_back.insert(k);
            } else {
                st.applied.insert(k.clone(), Applied::Deep { depth, quote });
                st.deep_books.watch(&k);
            }
        }
        if any {
            // A release asked for while it moved waits for the next pass.
            self.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests;
