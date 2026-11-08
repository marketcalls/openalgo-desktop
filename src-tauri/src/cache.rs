//! A bounded TTL cache whose fills cannot undo an invalidation: the Rust
//! form of the web's `utils/thread_safe_cache.LockedTTLCache`.
//!
//! Every cache in the app follows the web's rules (maintainer decision
//! 2026-10-13, `docs/port/no-keychain.md`):
//!
//! * it has a `maxsize` and a TTL; a full cache drops expired entries first,
//!   then the one closest to expiry;
//! * a reader reads [`TtlCache::generation`] *before* its database or broker
//!   query and stores the result with [`TtlCache::fill`], which stores
//!   nothing when the cache was cleared or invalidated in between. A read in
//!   flight across a logout, a key rotation or a broker switch is still
//!   answered to its own caller (it asked before the change) but is never
//!   cached;
//! * a writer calls [`TtlCache::invalidate`] or [`TtlCache::clear`] strictly
//!   *after* its commit. Clearing first leaves a window in which a reader
//!   loads the old row with the new generation.
//!
//! Time is passed in (`Instant`), so expiry tests need no sleeping.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

struct Inner<K, V> {
    map: HashMap<K, (V, Instant)>,
    generation: u64,
}

pub struct TtlCache<K, V> {
    inner: Mutex<Inner<K, V>>,
    maxsize: usize,
    ttl: Duration,
}

impl<K: Eq + Hash + Clone, V: Clone> TtlCache<K, V> {
    /// A cache of at most `maxsize` entries, each kept for `ttl` unless a
    /// fill gives its own (`fill_for`).
    pub fn new(maxsize: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                generation: 0,
            }),
            maxsize: maxsize.max(1),
            ttl,
        }
    }

    pub fn maxsize(&self) -> usize {
        self.maxsize
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The live value for `key` at `now`; an expired entry is dropped.
    pub fn get(&self, key: &K, now: Instant) -> Option<V> {
        let mut g = self.inner.lock();
        match g.map.get(key) {
            Some((v, exp)) if *exp > now => Some(v.clone()),
            Some(_) => {
                g.map.remove(key);
                None
            }
            None => None,
        }
    }

    /// Bumped by every `invalidate` and `clear`. Read it before a load.
    pub fn generation(&self) -> u64 {
        self.inner.lock().generation
    }

    /// The value and the generation, read together: what a reader needs
    /// before it decides to load.
    pub fn get_with_generation(&self, key: &K, now: Instant) -> (Option<V>, u64) {
        let mut g = self.inner.lock();
        let gen = g.generation;
        let v = match g.map.get(key) {
            Some((v, exp)) if *exp > now => Some(v.clone()),
            Some(_) => {
                g.map.remove(key);
                None
            }
            None => None,
        };
        (v, gen)
    }

    /// Store `value` (kept for the cache's TTL) only if nothing was
    /// invalidated since `generation`. Returns whether it was stored.
    pub fn fill(&self, key: K, value: V, generation: u64, now: Instant) -> bool {
        self.fill_for(key, value, generation, now, self.ttl)
    }

    /// `fill` with this entry's own TTL (a token kept until the next
    /// session boundary).
    pub fn fill_for(&self, key: K, value: V, generation: u64, now: Instant, ttl: Duration) -> bool {
        let mut g = self.inner.lock();
        if g.generation != generation {
            return false;
        }
        if !g.map.contains_key(&key) && g.map.len() >= self.maxsize {
            g.map.retain(|_, (_, exp)| *exp > now);
            while g.map.len() >= self.maxsize {
                let soonest = g
                    .map
                    .iter()
                    .min_by_key(|(_, (_, exp))| *exp)
                    .map(|(k, _)| k.clone());
                match soonest {
                    Some(k) => {
                        g.map.remove(&k);
                    }
                    None => break,
                }
            }
        }
        g.map.insert(key, (value, now + ttl));
        true
    }

    /// The cached value, else `load()` filled through the generation read
    /// before it ran. `should_cache` rejects results not worth keeping (an
    /// error, an empty answer); they are returned but not stored.
    pub fn get_or_load(
        &self,
        key: K,
        now: Instant,
        load: impl FnOnce() -> V,
        should_cache: impl FnOnce(&V) -> bool,
    ) -> V {
        let (hit, gen) = self.get_with_generation(&key, now);
        if let Some(v) = hit {
            return v;
        }
        let v = load();
        if should_cache(&v) {
            self.fill(key, v.clone(), gen, now);
        }
        v
    }

    /// Remove and return the live value for `key` when `belongs` accepts it,
    /// in one hold: an entry read once by its owner (a sign-in failure's
    /// sentence). Not an invalidation, so fills in flight still store; an
    /// entry `belongs` refuses stays for its owner.
    pub fn take_if(&self, key: &K, now: Instant, belongs: impl FnOnce(&V) -> bool) -> Option<V> {
        let mut g = self.inner.lock();
        match g.map.get(key) {
            Some((v, exp)) if *exp > now => {
                if !belongs(v) {
                    return None;
                }
                g.map.remove(key).map(|(v, _)| v)
            }
            Some(_) => {
                g.map.remove(key);
                None
            }
            None => None,
        }
    }

    /// Drop `key` and stop any read in flight from storing it again. Call
    /// after the writer's commit, never before.
    pub fn invalidate(&self, key: &K) {
        let mut g = self.inner.lock();
        g.map.remove(key);
        g.generation = g.generation.wrapping_add(1);
    }

    /// Drop every entry; counts as an invalidation of all of them.
    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.map.clear();
        g.generation = g.generation.wrapping_add(1);
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Seconds until the next daily session boundary (03:00 IST by default),
/// clamped like the web's `get_session_based_cache_ttl`: at least 5 minutes,
/// at most 24 hours. The TTL of cached broker auth and feed tokens.
pub fn until_session_boundary(
    now: chrono::DateTime<chrono::Utc>,
    expiry_hour: u32,
    expiry_minute: u32,
) -> Duration {
    let next = crate::session::boundary::next_boundary(now, expiry_hour, expiry_minute);
    let secs = (next - now).num_seconds();
    Duration::from_secs(secs.clamp(300, 24 * 3600) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_and_expiring() {
        let c: TtlCache<String, u32> = TtlCache::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..10 {
            let g = c.generation();
            c.fill(format!("k{}", i), i, g, t0 + Duration::from_secs(i as u64));
        }
        assert_eq!(c.len(), 3, "never more than maxsize");
        // The entries closest to expiry went first: the newest three stay.
        assert_eq!(c.get(&"k9".into(), t0), Some(9));
        assert_eq!(c.get(&"k0".into(), t0), None);
        assert_eq!(c.get(&"k9".into(), t0 + Duration::from_secs(70)), None);
        assert_eq!(c.len(), 2, "an expired entry is dropped when read");
    }

    /// A read that loaded before a writer's invalidation cannot store the
    /// value it read (the web's `fill` through the generation).
    #[test]
    fn a_fill_racing_an_invalidation_is_dropped() {
        let c: TtlCache<&str, &str> = TtlCache::new(8, Duration::from_secs(60));
        let now = Instant::now();
        let gen = c.generation(); // the reader starts its query
        c.invalidate(&"k"); // a writer commits and invalidates
        assert!(!c.fill("k", "stale", gen, now));
        assert_eq!(c.get(&"k", now), None);
        let gen = c.generation();
        c.clear(); // a logout clears everything
        assert!(!c.fill("k", "stale", gen, now));
        assert!(c.fill("k", "fresh", c.generation(), now));
        assert_eq!(c.get(&"k", now), Some("fresh"));
    }

    /// An entry is taken once, only by the reader it belongs to, never
    /// once expired; taking it is not an invalidation.
    #[test]
    fn take_if_hands_an_entry_out_once_to_its_owner() {
        let c: TtlCache<&str, &str> = TtlCache::new(8, Duration::from_secs(60));
        let now = Instant::now();
        assert!(c.fill("k", "mine", c.generation(), now));
        let gen = c.generation();
        assert_eq!(c.take_if(&"k", now, |v| *v == "yours"), None);
        assert_eq!(c.get(&"k", now), Some("mine"), "refused: it stays");
        assert_eq!(c.take_if(&"k", now, |v| *v == "mine"), Some("mine"));
        assert_eq!(c.take_if(&"k", now, |_| true), None, "once only");
        assert_eq!(c.generation(), gen, "a take invalidates nothing");
        assert!(c.fill("e", "old", gen, now));
        let later = now + Duration::from_secs(61);
        assert_eq!(c.take_if(&"e", later, |_| true), None, "expired");
        assert!(c.is_empty(), "the expired entry is dropped");
    }

    #[test]
    fn get_or_load_returns_but_does_not_store_a_stale_load() {
        let c: TtlCache<&str, u32> = TtlCache::new(8, Duration::from_secs(60));
        let now = Instant::now();
        let v = c.get_or_load(
            "k",
            now,
            || {
                c.clear(); // a writer finished while the load ran
                1
            },
            |_| true,
        );
        assert_eq!(v, 1, "the caller still gets what it read");
        assert_eq!(c.get(&"k", now), None, "but it is not cached");
        assert_eq!(c.get_or_load("k", now, || 2, |_| true), 2);
        assert_eq!(c.get_or_load("k", now, || 3, |_| true), 2, "cached");
        assert_eq!(c.get_or_load("x", now, || 0, |v| *v > 0), 0);
        assert_eq!(c.get(&"x", now), None, "rejected by should_cache");
    }

    #[test]
    fn per_entry_ttl() {
        let c: TtlCache<&str, u32> = TtlCache::new(8, Duration::from_secs(60));
        let now = Instant::now();
        c.fill_for("k", 1, c.generation(), now, Duration::from_secs(3600));
        assert_eq!(c.get(&"k", now + Duration::from_secs(600)), Some(1));
        assert_eq!(c.get(&"k", now + Duration::from_secs(3601)), None);
    }

    #[test]
    fn session_ttl_is_clamped_like_the_web() {
        use chrono::TimeZone;
        // 02:58 IST is 21:28 UTC the day before: two minutes to 03:00 IST,
        // clamped up to five.
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 10, 12, 21, 28, 0)
            .unwrap();
        assert_eq!(until_session_boundary(t, 3, 0), Duration::from_secs(300));
        // 09:15 IST (03:45 UTC): until 03:00 IST tomorrow, under 24 hours.
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 10, 13, 3, 45, 0)
            .unwrap();
        let ttl = until_session_boundary(t, 3, 0);
        assert_eq!(ttl, Duration::from_secs((17 * 60 + 45) * 60));
        assert!(ttl <= Duration::from_secs(24 * 3600));
    }
}
