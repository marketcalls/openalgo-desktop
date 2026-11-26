//! What the sandbox's feed worker holds of the market-data manager: each
//! reference with the broker session it was taken in, the bus's session
//! generation (CLAUDE.md "Feed parity with the web").
//!
//! `WebSocketManager::disconnect` forgets every reference when a broker
//! session ends, a new sign-in for the same broker included, and the
//! manager keeps one count per instrument and mode for all its consumers.
//! A reference taken in an ended session is therefore never given back: it
//! would take the one another holder took in the new session (a strategy, a
//! scalping stop, a Flow node, an 8765 client), whose prices would then
//! stop with no error. The worker takes such a reference again in the
//! session running now ([`Held::lapsed`]), as strategies and scalping stops
//! do (`strategy::tick_feed::FeedPrices`).

use crate::brokers::common::streaming::FeedSubscription;
use crate::sandbox::SymbolKey;
use std::collections::HashMap;

/// One reference: what was subscribed, and the session it was taken in.
struct Taken {
    sub: FeedSubscription,
    session: u64,
}

/// The worker's references, one per symbol.
#[derive(Default)]
pub(super) struct Held {
    refs: HashMap<SymbolKey, Taken>,
}

/// The symbol a subscription is for.
pub(super) fn key_of(sub: &FeedSubscription) -> SymbolKey {
    SymbolKey::new(sub.symbol.clone(), sub.exchange.clone())
}

impl Held {
    /// Note `sub` as taken in `session`.
    pub(super) fn insert(&mut self, sub: FeedSubscription, session: u64) {
        self.refs.insert(key_of(&sub), Taken { sub, session });
    }

    /// Whether `key` is held in `session`.
    pub(super) fn holds_in(&self, key: &SymbolKey, session: u64) -> bool {
        self.refs.get(key).is_some_and(|t| t.session == session)
    }

    /// Forget `key`, and say what goes back to the feed: the reference
    /// only when it was taken in `session`, the one running now.
    pub(super) fn release(&mut self, key: &SymbolKey, session: u64) -> Option<FeedSubscription> {
        self.refs
            .remove(key)
            .filter(|t| t.session == session)
            .map(|t| t.sub)
    }

    /// Forget `key` without giving anything back.
    pub(super) fn forget(&mut self, key: &SymbolKey) {
        self.refs.remove(key);
    }

    /// What was taken in a session before `session`.
    pub(super) fn lapsed(&self, session: u64) -> Vec<FeedSubscription> {
        self.refs
            .values()
            .filter(|t| t.session != session)
            .map(|t| t.sub.clone())
            .collect()
    }

    /// What goes back to the feed when the worker stops: only what was
    /// taken in `session`.
    pub(super) fn into_current(self, session: u64) -> Vec<FeedSubscription> {
        self.refs
            .into_values()
            .filter(|t| t.session == session)
            .map(|t| t.sub)
            .collect()
    }
}
