//! Browser sessions for the UI (the Flask session equivalent).
//!
//! The cookie carries only a random 256-bit id (HttpOnly, SameSite=Lax,
//! Path=/). Everything else stays in this in-memory store, which is bounded:
//! unauthenticated sessions expire after 30 idle minutes, the store never
//! holds more than `MAX_SESSIONS`, and every session is dropped at the daily
//! boundary like the web's permanent-session expiry.

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rand::RngCore;
use std::collections::HashMap;

pub const COOKIE_NAME: &str = "session";
pub const MAX_SESSIONS: usize = 256;
pub const ANON_IDLE: i64 = 30 * 60;
pub const PENDING_TOTP_SECS: i64 = 300;

#[derive(Clone, Default)]
pub struct WebSession {
    pub id: String,
    pub user: Option<String>,
    pub csrf_token: String,
    pub created_at: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    pub authenticated_at: Option<DateTime<Utc>>,
    pub pending_totp_user: Option<String>,
    pub pending_totp_started: Option<DateTime<Utc>>,
    pub totp_verified_at: Option<DateTime<Utc>>,
    /// SHA-256 of the password-reset token (the raw token is never stored).
    pub reset_token_hash: Option<String>,
    pub reset_email: Option<String>,
}

impl std::fmt::Debug for WebSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSession")
            .field("user", &self.user)
            .field("authenticated_at", &self.authenticated_at)
            .finish_non_exhaustive()
    }
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

pub struct WebSessionStore {
    map: Mutex<HashMap<String, WebSession>>,
    /// Bumped after any session ends (sign-out, password change or reset,
    /// account reset, the daily boundary, id rotation, eviction), so live
    /// update connections of ended sessions are closed (security review
    /// S-09).
    ended: tokio::sync::watch::Sender<u64>,
}

impl Default for WebSessionStore {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            ended: tokio::sync::watch::channel(0).0,
        }
    }
}

impl WebSessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Changes after any session ended.
    pub fn subscribe_ended(&self) -> tokio::sync::watch::Receiver<u64> {
        self.ended.subscribe()
    }

    fn signal_ended(&self) {
        self.ended.send_modify(|g| *g = g.wrapping_add(1));
    }

    fn expired(s: &WebSession, now: DateTime<Utc>) -> bool {
        s.user.is_none()
            && s.last_seen
                .map(|t| now - t > Duration::seconds(ANON_IDLE))
                .unwrap_or(true)
    }

    pub fn create(&self, now: DateTime<Utc>) -> WebSession {
        let mut map = self.map.lock();
        if map.len() >= MAX_SESSIONS {
            map.retain(|_, s| !Self::expired(s, now));
        }
        let mut evicted_signed_in = false;
        while map.len() >= MAX_SESSIONS {
            // Evict the least recently seen, preferring anonymous sessions.
            let victim = map
                .values()
                .min_by_key(|s| (s.user.is_some(), s.last_seen))
                .map(|s| (s.id.clone(), s.user.is_some()));
            match victim {
                Some((v, signed_in)) => {
                    map.remove(&v);
                    evicted_signed_in |= signed_in;
                }
                None => break,
            }
        }
        let s = WebSession {
            id: random_token(),
            csrf_token: random_token(),
            created_at: Some(now),
            last_seen: Some(now),
            ..Default::default()
        };
        map.insert(s.id.clone(), s.clone());
        drop(map);
        if evicted_signed_in {
            self.signal_ended();
        }
        s
    }

    pub fn get(&self, id: &str, now: DateTime<Utc>) -> Option<WebSession> {
        let mut map = self.map.lock();
        let expired = map.get(id).map(|s| Self::expired(s, now))?;
        if expired {
            map.remove(id);
            return None;
        }
        let s = map.get_mut(id)?;
        s.last_seen = Some(now);
        Some(s.clone())
    }

    pub fn update<F: FnOnce(&mut WebSession)>(&self, id: &str, f: F) -> bool {
        match self.map.lock().get_mut(id) {
            Some(s) => {
                f(s);
                true
            }
            None => false,
        }
    }

    /// Replace the session id (fixation hygiene on sign-in) keeping nothing
    /// from the old session but the CSRF token.
    pub fn rotate(&self, old_id: &str, now: DateTime<Utc>) -> WebSession {
        let old = self.map.lock().remove(old_id);
        if old.is_some() {
            self.signal_ended();
        }
        let csrf = old.map(|s| s.csrf_token);
        let mut s = self.create(now);
        if let Some(c) = csrf {
            s.csrf_token = c;
            self.update(&s.id, |x| x.csrf_token = s.csrf_token.clone());
        }
        s
    }

    pub fn remove(&self, id: &str) {
        let removed = self.map.lock().remove(id).is_some();
        if removed {
            self.signal_ended();
        }
    }

    /// Drop every session (logout means all devices; daily boundary;
    /// password change).
    pub fn clear(&self) {
        self.map.lock().clear();
        self.signal_ended();
    }

    /// End the sessions signed in (or, anonymous, created) before
    /// `boundary` (the daily boundary, SES-02); a session from after it
    /// stays. Returns how many ended.
    pub fn end_before(&self, boundary: DateTime<Utc>) -> usize {
        let n = {
            let mut map = self.map.lock();
            let before = map.len();
            map.retain(|_, s| {
                s.authenticated_at
                    .or(s.created_at)
                    .is_some_and(|t| t >= boundary)
            });
            before - map.len()
        };
        if n > 0 {
            self.signal_ended();
        }
        n
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The signed-in user, if any session has one (single-user app).
    pub fn signed_in_user(&self) -> Option<String> {
        self.map.lock().values().find_map(|s| s.user.clone())
    }

    /// Signed-in sessions (security dashboard), most recently seen first.
    pub fn authenticated_sessions(&self) -> Vec<WebSession> {
        let mut v: Vec<WebSession> = self
            .map
            .lock()
            .values()
            .filter(|s| s.user.is_some())
            .cloned()
            .collect();
        v.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));
        v
    }

    pub fn authenticated_count(&self) -> usize {
        self.map
            .lock()
            .values()
            .filter(|s| s.user.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_get_update_remove() {
        let st = WebSessionStore::new();
        let now = Utc::now();
        let s = st.create(now);
        assert_eq!(s.id.len(), 43);
        assert!(st.get(&s.id, now).is_some());
        st.update(&s.id, |x| x.user = Some("u".into()));
        assert_eq!(st.signed_in_user().as_deref(), Some("u"));
        st.remove(&s.id);
        assert!(st.get(&s.id, now).is_none());
    }

    #[test]
    fn anonymous_sessions_expire_and_store_is_bounded() {
        let st = WebSessionStore::new();
        let now = Utc::now();
        let a = st.create(now);
        assert!(st
            .get(&a.id, now + Duration::seconds(ANON_IDLE + 1))
            .is_none());
        let signed = st.create(now);
        st.update(&signed.id, |x| x.user = Some("u".into()));
        for i in 0..(MAX_SESSIONS * 3) {
            st.create(now + Duration::seconds(i as i64));
        }
        assert!(st.len() <= MAX_SESSIONS);
        assert!(st.get(&signed.id, now).is_some(), "signed-in session kept");
    }

    #[test]
    fn rotate_changes_id_and_keeps_csrf() {
        let st = WebSessionStore::new();
        let now = Utc::now();
        let a = st.create(now);
        let b = st.rotate(&a.id, now);
        assert_ne!(a.id, b.id);
        assert_eq!(a.csrf_token, st.get(&b.id, now).unwrap().csrf_token);
        assert!(st.get(&a.id, now).is_none());
    }
}
