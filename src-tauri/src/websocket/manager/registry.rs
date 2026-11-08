//! The manager's references: for each instrument, how many of each mode its
//! consumers hold (feed clients through the bridge, the sandbox, strategies,
//! Flow), and the one subscription the broker sees for it, at the highest
//! mode held. An instrument goes when its last reference does, so the
//! registry is bounded by what is held now.

use crate::brokers::common::streaming::{FeedMode, FeedSubscription};
use std::collections::{BTreeMap, HashMap};

/// Change to apply on the live socket.
#[derive(Debug, Clone)]
pub(super) enum Delta {
    Subscribe(FeedSubscription),
    Unsubscribe(FeedSubscription),
    ModeChange(FeedSubscription, FeedSubscription),
}

pub(super) struct Entry {
    pub(super) base: FeedSubscription,
    pub(super) modes: BTreeMap<FeedMode, u32>,
}

impl Entry {
    pub(super) fn effective(&self) -> Option<FeedSubscription> {
        self.modes
            .keys()
            .next_back()
            .map(|m| self.base.with_mode(*m))
    }
}

/// The broker's change between two effective subscriptions of one
/// instrument, if it has to see one.
fn change(before: Option<FeedSubscription>, after: Option<FeedSubscription>) -> Option<Delta> {
    match (before, after) {
        (None, Some(a)) => Some(Delta::Subscribe(a)),
        (Some(b), None) => Some(Delta::Unsubscribe(b)),
        (Some(b), Some(a)) if b.mode != a.mode => Some(Delta::ModeChange(b, a)),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Registry {
    pub(super) entries: HashMap<(String, String), Entry>,
}

impl Registry {
    pub(super) fn effective_all(&self) -> Vec<FeedSubscription> {
        self.entries.values().filter_map(Entry::effective).collect()
    }

    /// Instruments among `subs` that no reference names yet.
    pub(super) fn new_instruments(&self, subs: &[FeedSubscription]) -> usize {
        subs.iter()
            .filter(|s| !self.entries.contains_key(&s.instrument_key()))
            .map(|s| s.instrument_key())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// Count one more reference; the change the broker must see, if any.
    pub(super) fn add(&mut self, s: FeedSubscription) -> Option<Delta> {
        let entry = self
            .entries
            .entry(s.instrument_key())
            .or_insert_with(|| Entry {
                base: s.clone(),
                modes: BTreeMap::new(),
            });
        let before = entry.effective();
        *entry.modes.entry(s.mode).or_insert(0) += 1;
        change(before, entry.effective())
    }

    /// Count one reference less (nothing when none is held); the change the
    /// broker must see, if any. The instrument goes with its last one.
    pub(super) fn remove(&mut self, s: &FeedSubscription) -> Option<Delta> {
        let key = s.instrument_key();
        let entry = self.entries.get_mut(&key)?;
        let before = entry.effective();
        if let Some(n) = entry.modes.get_mut(&s.mode) {
            *n -= 1;
            if *n == 0 {
                entry.modes.remove(&s.mode);
            }
        }
        let after = entry.effective();
        if after.is_none() {
            self.entries.remove(&key);
        }
        change(before, after)
    }
}
