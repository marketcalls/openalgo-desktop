//! The manager's references: for each instrument, how many of each mode its
//! consumers hold (feed clients through the bridge, the sandbox, strategies,
//! Flow), the connection that carries it, and the one subscription the
//! broker sees for it, at the highest mode held and, in Depth, the deepest
//! book held (audit Phase 3.2: a Quote owner's 5 levels followed by a Depth
//! owner's 20 streams 20, and goes back to 5 when the last 20-level owner
//! leaves). An instrument goes when its last reference does, so the
//! registry is bounded by what is held now.
//!
//! Instruments are placed on the broker's connections as the web's
//! `ConnectionPool` does (audit C07): on the first connection with room,
//! a new one once every open one is full, up to the broker's limit.

use super::PoolLimits;
use crate::brokers::common::streaming::{FeedMode, FeedSubscription};
use std::collections::{BTreeMap, HashMap};

/// Change to apply on the live socket.
#[derive(Debug, Clone)]
pub(super) enum Delta {
    Subscribe(FeedSubscription),
    Unsubscribe(FeedSubscription),
    ModeChange(FeedSubscription, FeedSubscription),
}

/// A change and the connection that carries its instrument.
pub(super) type Routed = (usize, Delta);

pub(super) struct Entry {
    pub(super) base: FeedSubscription,
    pub(super) modes: BTreeMap<FeedMode, u32>,
    /// Depth references by the book depth each asked for.
    pub(super) depths: BTreeMap<u8, u32>,
    /// The connection that carries the instrument.
    pub(super) conn: usize,
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

/// Count `key` up, or down (dropping it at zero), in a tally.
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

pub(super) struct Registry {
    pub(super) entries: HashMap<(String, String), Entry>,
    /// Instruments each connection carries.
    pub(super) per_conn: Vec<usize>,
    pub(super) limits: PoolLimits,
    /// Counts every change, so a session knows which changes the whole set
    /// it was sent already carries (`Apply::seq`).
    pub(super) changes: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_limits(PoolLimits::single(usize::MAX))
    }
}

impl Registry {
    pub(super) fn with_limits(limits: PoolLimits) -> Self {
        Self {
            entries: HashMap::new(),
            per_conn: vec![0; limits.connections.max(1)],
            limits,
            changes: 0,
        }
    }

    /// New limits (a broker session's feed): instruments already placed
    /// keep their connection.
    pub(super) fn set_limits(&mut self, limits: PoolLimits) {
        if self.per_conn.len() < limits.connections {
            self.per_conn.resize(limits.connections, 0);
        }
        self.limits = limits;
    }

    pub(super) fn clear(&mut self) {
        self.changes += 1;
        self.entries.clear();
        self.per_conn.iter_mut().for_each(|n| *n = 0);
    }

    pub(super) fn effective_all(&self) -> Vec<FeedSubscription> {
        self.entries.values().filter_map(Entry::effective).collect()
    }

    /// What connection `conn` must be subscribed to.
    pub(super) fn effective_on(&self, conn: usize) -> Vec<FeedSubscription> {
        self.entries
            .values()
            .filter(|e| e.conn == conn)
            .filter_map(Entry::effective)
            .collect()
    }

    /// Where `s` goes: its instrument's connection, else the first one
    /// `usable` says may take it that has room; `None` when none has.
    pub(super) fn place(
        &self,
        s: &FeedSubscription,
        usable: &dyn Fn(usize) -> bool,
    ) -> Option<usize> {
        if let Some(e) = self.entries.get(&s.instrument_key()) {
            return Some(e.conn);
        }
        (0..self.limits.connections).find(|c| {
            usable(*c) && self.per_conn.get(*c).copied().unwrap_or(0) < self.limits.per_connection
        })
    }

    /// Instruments among `subs` that no reference names yet.
    pub(super) fn new_instruments(&self, subs: &[FeedSubscription]) -> usize {
        subs.iter()
            .filter(|s| !self.entries.contains_key(&s.instrument_key()))
            .map(|s| s.instrument_key())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// Count one more reference, placing a new instrument on `conn`; the
    /// change its connection must see, if any.
    pub(super) fn add(&mut self, s: FeedSubscription, conn: usize) -> Option<Routed> {
        self.changes += 1;
        let key = s.instrument_key();
        if !self.entries.contains_key(&key) {
            if self.per_conn.len() <= conn {
                self.per_conn.resize(conn + 1, 0);
            }
            self.per_conn[conn] += 1;
        }
        let entry = self.entries.entry(key).or_insert_with(|| Entry {
            base: s.clone(),
            modes: BTreeMap::new(),
            depths: BTreeMap::new(),
            conn,
        });
        let before = entry.effective();
        tally(&mut entry.modes, s.mode, true);
        if s.mode == FeedMode::Depth {
            tally(&mut entry.depths, s.depth, true);
        }
        let conn = entry.conn;
        change(before, entry.effective()).map(|d| (conn, d))
    }

    /// Count one reference less (nothing when none is held); the change
    /// its connection must see, if any. The instrument goes with its last
    /// one.
    pub(super) fn remove(&mut self, s: &FeedSubscription) -> Option<Routed> {
        self.changes += 1;
        let key = s.instrument_key();
        let entry = self.entries.get_mut(&key)?;
        if !entry.modes.contains_key(&s.mode) {
            return None;
        }
        let before = entry.effective();
        tally(&mut entry.modes, s.mode, false);
        if s.mode == FeedMode::Depth {
            tally(&mut entry.depths, s.depth, false);
        }
        let after = entry.effective();
        let conn = entry.conn;
        if after.is_none() {
            self.entries.remove(&key);
            if let Some(n) = self.per_conn.get_mut(conn) {
                *n = n.saturating_sub(1);
            }
        }
        change(before, after).map(|d| (conn, d))
    }

    /// Forget `s`'s instrument and every reference to it: the broker will
    /// not stream it (a session's whole set came back with it refused).
    pub(super) fn forget_instrument(&mut self, s: &FeedSubscription) {
        self.changes += 1;
        if let Some(e) = self.entries.remove(&s.instrument_key()) {
            if let Some(n) = self.per_conn.get_mut(e.conn) {
                *n = n.saturating_sub(1);
            }
        }
    }

    /// The connection carrying `s`'s instrument, if it is held.
    pub(super) fn conn_of(&self, s: &FeedSubscription) -> Option<usize> {
        self.entries.get(&s.instrument_key()).map(|e| e.conn)
    }
}

/// Changes grouped by the connection that carries their instruments, in
/// first-seen order.
pub(super) fn by_conn(deltas: Vec<Routed>) -> Vec<(usize, Vec<Delta>)> {
    let mut out: Vec<(usize, Vec<Delta>)> = Vec::new();
    for (conn, d) in deltas {
        match out.iter_mut().find(|(c, _)| *c == conn) {
            Some((_, v)) => v.push(d),
            None => out.push((conn, vec![d])),
        }
    }
    out
}

/// One flush's changes as the web sends them (bk-b): each instrument's net
/// change, the unsubscribes first, then the mode changes, then the
/// subscribes, so a subscribe never reaches a broker at its cap before the
/// unsubscribe that frees its slot. An instrument subscribed and released
/// in the same flush sends nothing.
pub(super) fn in_web_order(deltas: Vec<Delta>) -> Vec<Delta> {
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    let mut net: Vec<(Option<FeedSubscription>, Option<FeedSubscription>)> = Vec::new();
    for d in deltas {
        let (key, before, after) = match d {
            Delta::Subscribe(s) => (s.instrument_key(), None, Some(s)),
            Delta::Unsubscribe(s) => (s.instrument_key(), Some(s), None),
            Delta::ModeChange(b, a) => (a.instrument_key(), Some(b), Some(a)),
        };
        match seen.get(&key) {
            Some(i) => net[*i].1 = after,
            None => {
                seen.insert(key, net.len());
                net.push((before, after));
            }
        }
    }
    let mut out: Vec<Delta> = net.into_iter().filter_map(|(b, a)| change(b, a)).collect();
    out.sort_by_key(|d| match d {
        Delta::Unsubscribe(_) => 0,
        Delta::ModeChange(..) => 1,
        Delta::Subscribe(_) => 2,
    });
    out
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

    fn of(symbol: &str, mode: FeedMode) -> FeedSubscription {
        FeedSubscription {
            symbol: symbol.into(),
            ..sub(mode, 5)
        }
    }

    /// bk-b: one flush goes out as the web sends it, unsubscribes, then mode
    /// changes, then subscribes, each instrument once with its net change.
    #[test]
    fn a_flush_releases_before_it_subscribes() {
        let flush = vec![
            Delta::Subscribe(of("A", FeedMode::Ltp)),
            Delta::Unsubscribe(of("B", FeedMode::Ltp)),
            Delta::ModeChange(of("C", FeedMode::Ltp), of("C", FeedMode::Quote)),
            // Taken and let go in the same flush: nothing.
            Delta::Subscribe(of("D", FeedMode::Ltp)),
            Delta::Unsubscribe(of("D", FeedMode::Ltp)),
            // Let go and taken again as it was: nothing.
            Delta::Unsubscribe(of("E", FeedMode::Quote)),
            Delta::Subscribe(of("E", FeedMode::Quote)),
        ];
        let named: Vec<String> = in_web_order(flush)
            .into_iter()
            .map(|d| match d {
                Delta::Subscribe(s) => format!("sub {}", s.symbol),
                Delta::Unsubscribe(s) => format!("unsub {}", s.symbol),
                Delta::ModeChange(_, a) => format!("mode {}", a.symbol),
            })
            .collect();
        assert_eq!(named, ["unsub B", "mode C", "sub A"]);
    }

    fn moved(d: Option<Routed>) -> Option<(FeedMode, u8, FeedMode, u8)> {
        match d {
            Some((_, Delta::ModeChange(b, a))) => Some((b.mode, b.depth, a.mode, a.depth)),
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
            r.add(sub(FeedMode::Quote, 5), 0).map(|(_, d)| d),
            Some(Delta::Subscribe(_))
        ));
        assert_eq!(
            moved(r.add(sub(FeedMode::Depth, 20), 0)),
            Some((FeedMode::Quote, 5, FeedMode::Depth, 20))
        );
        assert_eq!(r.effective_all()[0].depth, 20);
        assert_eq!(
            moved(r.add(sub(FeedMode::Depth, 5), 0)),
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
            r.remove(&sub(FeedMode::Quote, 5)).map(|(_, d)| d),
            Some(Delta::Unsubscribe(_))
        ));
        assert!(r.entries.is_empty());
    }

    /// A reference that is not held changes nothing, and does not take
    /// another mode's reference with it.
    #[test]
    fn releasing_what_is_not_held_changes_nothing() {
        let mut r = Registry::default();
        r.add(sub(FeedMode::Ltp, 5), 0);
        assert!(r.remove(&sub(FeedMode::Depth, 20)).is_none());
        assert_eq!(r.effective_all()[0].mode, FeedMode::Ltp);
    }

    fn at(symbol: &str) -> FeedSubscription {
        FeedSubscription {
            symbol: symbol.into(),
            ..sub(FeedMode::Ltp, 5)
        }
    }

    /// Place `symbol` as the manager does: its connection if held, else
    /// the first with room.
    fn place(r: &mut Registry, symbol: &str, usable: &dyn Fn(usize) -> bool) -> Option<usize> {
        let conn = r.place(&at(symbol), usable)?;
        r.add(at(symbol), conn);
        Some(conn)
    }

    /// Audit C07, web `ConnectionPool._get_adapter_with_capacity`: the first
    /// connection fills before the next; a held instrument keeps its
    /// connection; a place freed on an open one is taken before another;
    /// past every connection's room there is none, and a connection the
    /// broker refused takes nothing new.
    #[test]
    fn instruments_fill_one_connection_before_the_next() {
        let mut r = Registry::with_limits(PoolLimits {
            per_connection: 2,
            connections: 2,
        });
        let all = |_: usize| true;
        let placed: Vec<_> = ["A", "B", "C", "A", "D", "E"]
            .iter()
            .map(|s| place(&mut r, s, &all))
            .collect();
        assert_eq!(placed, [Some(0), Some(0), Some(1), Some(0), Some(1), None]);
        assert_eq!(r.per_conn, [2, 2]);
        r.remove(&at("B"));
        assert_eq!(place(&mut r, "E", &all), Some(0));
        assert_eq!(r.effective_on(1).len(), 2);
        r.remove(&at("C"));
        assert_eq!(place(&mut r, "F", &|c| c != 1), None);
        assert_eq!(r.conn_of(&at("D")), Some(1));
    }
}
