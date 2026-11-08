//! The manager's references: for each instrument, how many of each mode its
//! consumers hold (feed clients through the bridge, the sandbox, strategies,
//! Flow), and the one subscription the broker sees for it, at the highest
//! mode held and, in Depth, the deepest book held (audit Phase 3.2: a
//! Quote owner's 5 levels followed by a Depth owner's 20 streams 20, and
//! goes back to 5 when the last 20-level owner leaves). An instrument goes
//! when its last reference does, so the registry is bounded by what is
//! held now.

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
    /// Depth references by the book depth each asked for.
    pub(super) depths: BTreeMap<u8, u32>,
}

impl Entry {
    pub(super) fn effective(&self) -> Option<FeedSubscription> {
        let mode = *self.modes.keys().next_back()?;
        let mut sub = self.base.with_mode(mode);
        if mode == FeedMode::Depth {
            if let Some(deepest) = self.depths.keys().next_back() {
                sub.depth = *deepest;
            }
        }
        Some(sub)
    }
}

/// Count `n` up (or down, dropping it at zero) in a tally.
fn tally<K: Ord>(counts: &mut BTreeMap<K, u32>, key: K, up: bool) {
    if up {
        *counts.entry(key).or_insert(0) += 1;
    } else if let std::collections::btree_map::Entry::Occupied(mut e) = counts.entry(key) {
        *e.get_mut() -= 1;
        if *e.get() == 0 {
            e.remove();
        }
    }
}

/// The broker's change between two effective subscriptions of one
/// instrument, if it has to see one.
fn change(before: Option<FeedSubscription>, after: Option<FeedSubscription>) -> Option<Delta> {
    match (before, after) {
        (None, Some(a)) => Some(Delta::Subscribe(a)),
        (Some(b), None) => Some(Delta::Unsubscribe(b)),
        (Some(b), Some(a)) if b.mode != a.mode || b.depth != a.depth => {
            Some(Delta::ModeChange(b, a))
        }
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
                depths: BTreeMap::new(),
            });
        let before = entry.effective();
        tally(&mut entry.modes, s.mode, true);
        if s.mode == FeedMode::Depth {
            tally(&mut entry.depths, s.depth, true);
        }
        change(before, entry.effective())
    }

    /// Count one reference less (nothing when none is held); the change the
    /// broker must see, if any. The instrument goes with its last one.
    pub(super) fn remove(&mut self, s: &FeedSubscription) -> Option<Delta> {
        let key = s.instrument_key();
        let entry = self.entries.get_mut(&key)?;
        let before = entry.effective();
        if !entry.modes.contains_key(&s.mode) {
            return None;
        }
        tally(&mut entry.modes, s.mode, false);
        if s.mode == FeedMode::Depth {
            tally(&mut entry.depths, s.depth, false);
        }
        let after = entry.effective();
        if after.is_none() {
            self.entries.remove(&key);
        }
        change(before, after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(mode: FeedMode, depth: u8) -> FeedSubscription {
        FeedSubscription {
            symbol: "RELIANCE".into(),
            exchange: "NSE".into(),
            token: "2885".into(),
            brsymbol: "RELIANCE".into(),
            brexchange: "NSE".into(),
            mode,
            depth,
        }
    }

    fn moved(d: Option<Delta>) -> Option<(FeedMode, u8, FeedMode, u8)> {
        match d {
            Some(Delta::ModeChange(b, a)) => Some((b.mode, b.depth, a.mode, a.depth)),
            _ => None,
        }
    }

    /// Audit Phase 3.2: a Quote owner (5 levels) then a Depth owner asking
    /// for 20 on one socket streams 20, not the first reference's 5; when
    /// the last 20-level owner leaves it goes back to the deepest still
    /// held, then to Quote.
    #[test]
    fn the_deepest_book_held_is_streamed() {
        let mut r = Registry::default();
        assert!(matches!(
            r.add(sub(FeedMode::Quote, 5)),
            Some(Delta::Subscribe(_))
        ));
        assert_eq!(
            moved(r.add(sub(FeedMode::Depth, 20))),
            Some((FeedMode::Quote, 5, FeedMode::Depth, 20))
        );
        assert_eq!(r.effective_all()[0].depth, 20);
        assert_eq!(
            moved(r.add(sub(FeedMode::Depth, 5))),
            None,
            "a shallower book is inside the deeper one"
        );
        assert_eq!(
            moved(r.remove(&sub(FeedMode::Depth, 20))),
            Some((FeedMode::Depth, 20, FeedMode::Depth, 5))
        );
        assert_eq!(
            moved(r.remove(&sub(FeedMode::Depth, 5))),
            Some((FeedMode::Depth, 5, FeedMode::Quote, 5))
        );
        assert!(matches!(
            r.remove(&sub(FeedMode::Quote, 5)),
            Some(Delta::Unsubscribe(_))
        ));
        assert!(r.entries.is_empty());
    }

    /// A reference that is not held changes nothing, and does not take
    /// another mode's reference with it.
    #[test]
    fn releasing_what_is_not_held_changes_nothing() {
        let mut r = Registry::default();
        r.add(sub(FeedMode::Ltp, 5));
        assert!(r.remove(&sub(FeedMode::Depth, 20)).is_none());
        assert_eq!(r.effective_all()[0].mode, FeedMode::Ltp);
    }
}
