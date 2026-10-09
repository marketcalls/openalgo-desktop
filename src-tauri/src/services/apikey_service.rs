//! OpenAlgo API key: verification for `/api/v1`, regeneration and order mode.
//!
//! Verification: a keyed digest of the presented key (HMAC-SHA256 under a
//! random per-process key, the same construction as the stored lookup index)
//! indexes a bounded cache (positive and negative results, 5 minutes). On a miss the HMAC index finds
//! the one candidate row and Argon2 verifies it. Regenerating the key clears
//! the cache.

use crate::db::sqlite::api_keys;
use crate::error::Result;
use crate::security::hashing::lookup_hmac;
use crate::security::Secret;
use crate::state::AppState;
use parking_lot::Mutex;
use rand::RngCore;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const CACHE_CAP: usize = 1024;
pub const CACHE_TTL: Duration = Duration::from_secs(300);

/// Key digest -> (valid, expiry).
type Entries = HashMap<String, (bool, Instant)>;

/// Verified-key cache. Entries are keyed by an HMAC of the presented key
/// under a random key that lives only in this process, so the map never
/// holds the API key or an unsalted hash of it.
pub struct ApiKeyCache {
    cache_key: [u8; 32],
    /// The map, and the generation `clear` bumps: a check that started
    /// before a key regeneration cannot store its stale answer after it
    /// (security review S-12).
    map: Mutex<(Entries, u64)>,
}

impl Default for ApiKeyCache {
    fn default() -> Self {
        let mut cache_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut cache_key);
        Self {
            cache_key,
            map: Mutex::new((HashMap::new(), 0)),
        }
    }
}

impl ApiKeyCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn digest(&self, key: &str) -> String {
        lookup_hmac(&self.cache_key, key)
    }

    /// The generation to pass to [`Self::put_at`], read before checking.
    pub fn generation(&self) -> u64 {
        self.map.lock().1
    }

    pub fn get(&self, key: &str, now: Instant) -> Option<bool> {
        let mut guard = self.map.lock();
        let map = &mut guard.0;
        let d = self.digest(key);
        match map.get(&d) {
            Some((valid, exp)) if *exp > now => Some(*valid),
            Some(_) => {
                map.remove(&d);
                None
            }
            None => None,
        }
    }

    pub fn put(&self, key: &str, valid: bool, now: Instant) {
        let gen = self.generation();
        self.put_at(key, valid, now, gen);
    }

    /// Store an answer computed while `gen` was current; dropped when the
    /// cache was cleared in between.
    pub fn put_at(&self, key: &str, valid: bool, now: Instant, gen: u64) {
        let mut guard = self.map.lock();
        if guard.1 != gen {
            return;
        }
        let map = &mut guard.0;
        if map.len() >= CACHE_CAP {
            map.retain(|_, (_, exp)| *exp > now);
            while map.len() >= CACHE_CAP {
                let oldest = map
                    .iter()
                    .min_by_key(|(_, (_, e))| *e)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => {
                        map.remove(&k);
                    }
                    None => break,
                }
            }
        }
        let d = self.digest(key);
        map.insert(d, (valid, now + CACHE_TTL));
    }

    pub fn clear(&self) {
        let mut guard = self.map.lock();
        guard.0.clear();
        guard.1 = guard.1.wrapping_add(1);
    }

    pub fn len(&self) -> usize {
        self.map.lock().0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct ApiKeyService;

impl ApiKeyService {
    /// Whether `api_key` is the stored OpenAlgo key.
    pub fn is_valid(state: &AppState, api_key: &str) -> bool {
        if api_key.is_empty() {
            return false;
        }
        let now = Instant::now();
        if let Some(v) = state.api_keys.get(api_key, now) {
            return v;
        }
        if !state.security.is_unlocked() {
            return false;
        }
        let gen = state.api_keys.generation();
        let valid = match Self::verify_uncached(state, api_key) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("API key check failed: {}", e);
                return false;
            }
        };
        state.api_keys.put_at(api_key, valid, now, gen);
        valid
    }

    fn verify_uncached(state: &AppState, api_key: &str) -> Result<bool> {
        let lookup = state.security.api_key_lookup(api_key)?;
        let row = {
            let conn = state.sqlite.conn()?;
            api_keys::find_by_lookup(&conn, &lookup)?
        };
        let Some(row) = row else {
            return Ok(false);
        };
        let ok = state.security.verify_password(api_key, &row.key_hash)?;
        if ok {
            if let Ok(conn) = state.sqlite.conn() {
                let _ = api_keys::touch(&conn, row.id);
            }
        }
        Ok(ok)
    }

    /// Generate and store a new key for `username`; returns it once.
    pub fn regenerate(state: &AppState, username: &str) -> Result<Secret> {
        let key = api_keys::generate_api_key();
        {
            let conn = state.sqlite.conn()?;
            api_keys::upsert_for_user(&conn, &state.security, username, &key)?;
        }
        state.api_keys.clear();
        Ok(Secret::new(key))
    }

    /// The key for the signed-in session (API key page, session status).
    pub fn current(state: &AppState) -> Result<Option<Secret>> {
        let conn = state.sqlite.conn()?;
        api_keys::get_plaintext(&conn, &state.security)
    }

    pub fn order_mode(state: &AppState) -> Result<String> {
        let conn = state.sqlite.conn()?;
        Ok(api_keys::get_order_mode(&conn)?.unwrap_or_else(|| "auto".into()))
    }

    pub fn set_order_mode(state: &AppState, mode: &str) -> Result<bool> {
        let conn = state.sqlite.conn()?;
        api_keys::set_order_mode(&conn, mode)
    }
}

/// Used by the older services that re-check the key.
pub fn require_valid(state: &AppState, api_key: &str) -> Result<()> {
    if ApiKeyService::is_valid(state, api_key) {
        Ok(())
    } else {
        Err(crate::error::AppError::Auth(
            "Invalid openalgo apikey".into(),
        ))
    }
}

#[cfg(test)]
mod tests {

    /// S-12: an answer computed before a regeneration is not cached after it.
    #[test]
    fn a_check_racing_a_regeneration_cannot_cache_the_old_key() {
        let c = ApiKeyCache::new();
        let now = Instant::now();
        let gen = c.generation();
        c.clear(); // the key was regenerated while the check ran
        c.put_at("old-key", true, now, gen);
        assert_eq!(c.get("old-key", now), None);
        c.put_at("new-key", true, now, c.generation());
        assert_eq!(c.get("new-key", now), Some(true));
    }

    use super::*;

    #[test]
    fn cache_expires_and_stays_bounded() {
        let c = ApiKeyCache::new();
        let t0 = Instant::now();
        c.put("k", true, t0);
        assert_eq!(c.get("k", t0), Some(true));
        assert_eq!(c.get("k", t0 + CACHE_TTL + Duration::from_secs(1)), None);
        for i in 0..(CACHE_CAP * 2) {
            c.put(&format!("k{}", i), false, t0);
        }
        assert!(c.len() <= CACHE_CAP);
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn cache_key_is_keyed_per_process_not_a_plain_hash() {
        use sha2::{Digest, Sha256};
        let a = ApiKeyCache::new();
        let b = ApiKeyCache::new();
        let key = "presented-api-key";
        let plain = hex::encode(Sha256::digest(key.as_bytes()));
        let da = a.digest(key);
        assert_eq!(da, a.digest(key), "same cache, same key: stable");
        assert_ne!(da, b.digest(key), "independent caches use independent keys");
        assert_ne!(da, plain, "not an unsalted SHA-256 of the key");
        assert!(!da.contains(key));
        a.put(key, true, Instant::now());
        assert!(a.map.lock().0.keys().all(|k| k != key && *k != plain));
        assert_eq!(a.get(key, Instant::now()), Some(true));
        assert_eq!(b.get(key, Instant::now()), None);
    }
}
