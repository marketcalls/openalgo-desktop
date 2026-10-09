//! Per-IP moving-window rate limits, matching the web's Flask-Limiter setup.
//!
//! | Bucket       | Limit           | Applies to                                        |
//! |--------------|-----------------|---------------------------------------------------|
//! | Api          | 100 per second  | every other `/api/v1` route                       |
//! | Order        | 10 per second   | place/modify/cancel, options orders, GTT writes   |
//! | SmartOrder   | 10 per second   | placesmartorder                                   |
//! | LoginMinute  | 5 per minute    | `/auth/login`, `/auth/login/totp`, broker logins  |
//! | LoginHour    | 25 per hour     | same                                              |
//! | Reset        | 15 per hour     | `/auth/reset-password`                            |
//! | ApiKeyFail   | 10 per minute   | failed API-key checks; over it the key is not even tried |
//! | StrategyWebhook | 100 per minute | `/strategy/webhook/<token>` per caller address |
//! | WebhookFail  | 10 per minute   | failed strategy-webhook authentications per caller |
//!
//! Sign-in to OpenAlgo itself also has a failure budget per source
//! ([`LoginBackoff`]), whatever name is typed. Callers behind a tunnel share
//! one address; for a stable credential checked against a stored secret
//! (webhook address, strategy token, API key) their failures are counted per
//! credential (`middleware::limiter_key`), never for sign-in.
//!
//! No rate-limit headers are sent (web contract). Memory is bounded: windows
//! are trimmed on every check and the table is swept when it grows.

use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const MAX_ENTRIES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    Api,
    Order,
    SmartOrder,
    LoginMinute,
    LoginHour,
    Reset,
    ApiKeyFail,
    /// `/strategy/webhook/<token>` by caller address (web
    /// `WEBHOOK_RATE_LIMIT`, 100 per minute).
    StrategyWebhook,
    /// `/chartink/webhook/<id>` by caller address (web
    /// `WEBHOOK_RATE_LIMIT`, 100 per minute).
    ChartinkWebhook,
    /// Failed strategy-webhook authentications (unknown token, address
    /// outside the allowlist) by caller address; over it the address is
    /// refused before any lookup.
    WebhookFail,
}

impl Bucket {
    pub fn limit(&self) -> (usize, Duration) {
        match self {
            Bucket::Api => (100, Duration::from_secs(1)),
            Bucket::Order | Bucket::SmartOrder => (10, Duration::from_secs(1)),
            Bucket::LoginMinute => (5, Duration::from_secs(60)),
            Bucket::LoginHour => (25, Duration::from_secs(3600)),
            Bucket::Reset => (15, Duration::from_secs(3600)),
            Bucket::ApiKeyFail => (10, Duration::from_secs(60)),
            Bucket::StrategyWebhook | Bucket::ChartinkWebhook => (100, Duration::from_secs(60)),
            Bucket::WebhookFail => (10, Duration::from_secs(60)),
        }
    }

    /// Flask-Limiter's description, which is the web's 429 body for /api/v1.
    pub fn describe(&self) -> String {
        let (n, w) = self.limit();
        match w.as_secs() {
            1 => format!("{} per 1 second", n),
            60 => format!("{} per 1 minute", n),
            _ => format!("{} per 1 hour", n),
        }
    }

    /// Which bucket an `/api/v1` path counts against.
    pub fn for_api_path(path: &str) -> Bucket {
        let p = path.trim_start_matches("/api/v1/").trim_end_matches('/');
        match p {
            "placesmartorder" => Bucket::SmartOrder,
            "placeorder" | "modifyorder" | "cancelorder" | "optionsorder" | "optionsmultiorder"
            | "placegttorder" | "modifygttorder" | "cancelgttorder" => Bucket::Order,
            // Web `API_RATE_LIMIT` (10 per second) on the strategy routes.
            p if p.starts_with("strategy/") => Bucket::Order,
            _ => Bucket::Api,
        }
    }
}

/// Failed sign-ins tolerated before a wait is imposed.
pub const BACKOFF_FREE_FAILURES: u32 = 5;
/// The first wait; it doubles with each further failure.
pub const BACKOFF_FIRST: Duration = Duration::from_secs(30);
/// The longest wait: a delay, never a long lockout of the account.
pub const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
/// Failures are forgotten after this long without a new one.
pub const BACKOFF_FORGET: Duration = Duration::from_secs(60 * 60);

/// Network sources tracked one by one; past this, new ones share
/// [`SignInSource::Overflow`]. This computer and the tunnel never count
/// toward it and are never dropped.
pub const BACKOFF_NETWORK_SOURCES: usize = 1024;

/// Where a sign-in attempt comes from. Each source has its own failure
/// budget for the one account on this computer, so a caller from one source
/// can never delay a sign-in from another: above all, nothing remote can
/// delay the trader at the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignInSource {
    /// A program on this computer (a loopback peer that did not come through
    /// a tunnel or proxy). A malicious local process already owns the OS
    /// account, so only it can delay this budget.
    Local,
    /// Every caller behind a tunnel or proxy on this computer, together:
    /// the app cannot see their addresses.
    Tunnel,
    /// A device on the network. IPv6 addresses are grouped by their /64,
    /// which one device can rotate through freely.
    Network(IpAddr),
    /// Network sources beyond [`BACKOFF_NETWORK_SOURCES`], together.
    Overflow,
}

/// One claimed sign-in attempt ([`LoginBackoff::claim`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claim {
    /// The budget it was counted in.
    pub key: SignInSource,
    /// The failure count right after it was claimed.
    count: u32,
}

#[derive(Debug, Clone, Copy)]
struct BackoffState {
    /// Attempts counted as failures (including ones still being checked).
    failures: u32,
    /// No attempt is checked before this instant.
    until: Option<Instant>,
    last_attempt: Instant,
}

/// The wait `failures` failures impose.
fn backoff_wait(failures: u32) -> Duration {
    match failures.saturating_sub(BACKOFF_FREE_FAILURES) {
        0 => Duration::ZERO,
        over => BACKOFF_FIRST
            .saturating_mul(1u32 << (over - 1).min(16))
            .min(BACKOFF_MAX),
    }
}

/// The sign-in failure budget of the one account on this computer, per
/// [`SignInSource`]: wrong passwords, wrong authenticator codes, and
/// sign-ins with a name that is not the account's all spend the budget of
/// the source they came from. After [`BACKOFF_FREE_FAILURES`] failures each
/// further failure imposes a wait that doubles from [`BACKOFF_FIRST`] up to
/// [`BACKOFF_MAX`]; while it runs no attempt from that source is checked,
/// the right password included. It is a delay, never a lockout: once the
/// wait has passed the right password works. A complete sign-in clears the
/// source's budget; failures are forgotten after [`BACKOFF_FORGET`] without
/// one.
///
/// An attempt is claimed ([`Self::claim`]) before the password or code is
/// checked, and counts as a failure from that moment, under the same lock
/// that checks the wait: requests sent in parallel cannot all pass before
/// the first failure is recorded. The outcome then clears the budget
/// ([`Self::succeed`]), keeps the failure, or gives the attempt back
/// ([`Self::release`]) when nothing was checked or the password was right
/// but a code is still needed.
///
/// Keyed by source, never by the name typed, so a flood of made-up names
/// spends only its own source's budget and cannot push another source's
/// count out of the table. Cross-site requests are refused before they get
/// here, so a web page cannot spend the budget of this computer.
#[derive(Default)]
pub struct LoginBackoff {
    map: Mutex<HashMap<SignInSource, BackoffState>>,
}

impl LoginBackoff {
    /// The budget `source` is counted in: itself, or [`SignInSource::Overflow`]
    /// for a new network source when the table is full of recent ones.
    fn slot(
        map: &mut HashMap<SignInSource, BackoffState>,
        source: SignInSource,
        now: Instant,
    ) -> SignInSource {
        if !matches!(source, SignInSource::Network(_)) || map.contains_key(&source) {
            return source;
        }
        let networks = |m: &HashMap<SignInSource, BackoffState>| {
            m.keys()
                .filter(|k| matches!(k, SignInSource::Network(_)))
                .count()
        };
        if networks(map) < BACKOFF_NETWORK_SOURCES {
            return source;
        }
        // Drop only budgets that are spent and forgotten: never one still
        // waiting or still counting.
        map.retain(|k, s| {
            !matches!(k, SignInSource::Network(_))
                || now.saturating_duration_since(s.last_attempt) < BACKOFF_FORGET
        });
        if networks(map) < BACKOFF_NETWORK_SOURCES {
            source
        } else {
            SignInSource::Overflow
        }
    }

    /// Claim an attempt from `source` before checking anything: `Err` with
    /// the remaining wait while one runs; otherwise the attempt is counted
    /// as a failure now, and the claim is returned for [`Self::succeed`] or
    /// [`Self::release`].
    pub fn claim(&self, source: SignInSource, now: Instant) -> Result<Claim, Duration> {
        let mut map = self.map.lock();
        let key = Self::slot(&mut map, source, now);
        let s = map.entry(key).or_insert(BackoffState {
            failures: 0,
            until: None,
            last_attempt: now,
        });
        if now.saturating_duration_since(s.last_attempt) >= BACKOFF_FORGET {
            *s = BackoffState {
                failures: 0,
                until: None,
                last_attempt: now,
            };
        }
        if let Some(wait) = s
            .until
            .map(|u| u.saturating_duration_since(now))
            .filter(|d| !d.is_zero())
        {
            return Err(wait);
        }
        s.failures = s.failures.saturating_add(1);
        s.last_attempt = now;
        let wait = backoff_wait(s.failures);
        s.until = (!wait.is_zero()).then(|| now + wait);
        Ok(Claim {
            key,
            count: s.failures,
        })
    }

    /// The claimed attempt succeeded completely: the budget starts over.
    pub fn succeed(&self, claim: &Claim) {
        self.clear(claim.key);
    }

    /// Clear `key`'s budget (a complete sign-in proven another way).
    pub fn clear(&self, key: SignInSource) {
        self.map.lock().remove(&key);
    }

    /// Give a claimed attempt back: nothing was checked, or the password was
    /// right and a code is still to come. Earlier failures stay counted.
    /// No wait was running when it was claimed, so when no other attempt
    /// was claimed since, none runs after it either; otherwise the waits
    /// the later claims set are kept.
    pub fn release(&self, claim: &Claim) {
        let mut map = self.map.lock();
        if let Some(s) = map.get_mut(&claim.key) {
            if s.failures == claim.count {
                s.until = None;
            }
            s.failures = s.failures.saturating_sub(1);
            if s.failures == 0 {
                map.remove(&claim.key);
            }
        }
    }

    /// How long `source` must still wait, if at all.
    pub fn wait(&self, source: SignInSource, now: Instant) -> Option<Duration> {
        self.map
            .lock()
            .get(&source)
            .and_then(|s| s.until)
            .map(|u| u.saturating_duration_since(now))
            .filter(|d| !d.is_zero())
    }

    /// Failures counted for `source` so far (and not yet forgotten).
    pub fn failures(&self, source: SignInSource) -> u32 {
        self.map.lock().get(&source).map_or(0, |s| s.failures)
    }

    /// Budgets held (bounded by [`BACKOFF_NETWORK_SOURCES`] plus the three
    /// fixed sources).
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Default)]
pub struct RateLimiter {
    /// Per-account sign-in failure budget.
    pub backoff: LoginBackoff,
    map: Mutex<HashMap<(Bucket, IpAddr), VecDeque<Instant>>>,
    /// Tests pin time so a window cannot slide while a slow runner (Windows,
    /// coverage instrumentation) is still sending the requests that fill it.
    frozen: Mutex<Option<Instant>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The instant callers should count a hit at.
    pub fn now(&self) -> Instant {
        self.frozen.lock().unwrap_or_else(Instant::now)
    }

    /// Pin the limiter's clock (tests only); `None` releases it.
    #[cfg(test)]
    pub fn freeze(&self, at: Option<Instant>) {
        *self.frozen.lock() = at;
    }

    /// Count a hit. `Err(retry_after)` when over the limit (the hit is not
    /// recorded, so a client that backs off recovers).
    pub fn check(&self, bucket: Bucket, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let (limit, window) = bucket.limit();
        let mut map = self.map.lock();
        if map.len() >= MAX_ENTRIES {
            Self::sweep(&mut map, now);
        }
        let q = map.entry((bucket, ip)).or_default();
        while q
            .front()
            .map(|t| now.saturating_duration_since(*t) >= window)
            .unwrap_or(false)
        {
            q.pop_front();
        }
        if q.len() >= limit {
            let oldest = q.front().copied().unwrap_or(now);
            return Err(window.saturating_sub(now.saturating_duration_since(oldest)));
        }
        q.push_back(now);
        Ok(())
    }

    /// Whether `bucket` is exhausted for `ip`, without counting a hit.
    pub fn is_exhausted(&self, bucket: Bucket, ip: IpAddr, now: Instant) -> bool {
        let (limit, window) = bucket.limit();
        let map = self.map.lock();
        map.get(&(bucket, ip))
            .map(|q| {
                q.iter()
                    .filter(|t| now.saturating_duration_since(**t) < window)
                    .count()
                    >= limit
            })
            .unwrap_or(false)
    }

    fn sweep(map: &mut HashMap<(Bucket, IpAddr), VecDeque<Instant>>, now: Instant) {
        map.retain(|(b, _), q| {
            let (_, w) = b.limit();
            q.back()
                .map(|t| now.saturating_duration_since(*t) < w)
                .unwrap_or(false)
        });
        if map.len() >= MAX_ENTRIES {
            // Still full: drop the short-window buckets, keep login history.
            map.retain(|(b, _), _| {
                matches!(b, Bucket::LoginMinute | Bucket::LoginHour | Bucket::Reset)
            });
        }
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {

    use SignInSource::{Local, Network, Overflow, Tunnel};

    /// Fail `n` claimed attempts from `src` at `t`, each after the wait.
    fn fail_n(b: &LoginBackoff, src: SignInSource, n: u32, mut t: Instant) -> Instant {
        for _ in 0..n {
            if let Some(w) = b.wait(src, t) {
                t += w;
            }
            b.claim(src, t).unwrap();
        }
        t
    }

    #[test]
    fn sign_in_backoff_doubles_and_is_capped_then_clears() {
        let b = LoginBackoff::default();
        let t0 = Instant::now();
        for _ in 0..BACKOFF_FREE_FAILURES {
            assert!(b.claim(Local, t0).is_ok());
        }
        assert!(b.wait(Local, t0).is_none());
        assert!(b.claim(Local, t0).is_ok());
        assert_eq!(b.claim(Local, t0), Err(BACKOFF_FIRST));
        let t1 = t0 + BACKOFF_FIRST;
        assert!(b.claim(Local, t1).is_ok());
        assert_eq!(b.wait(Local, t1), Some(BACKOFF_FIRST * 2));
        let t2 = fail_n(&b, Local, 20, t1);
        assert_eq!(b.wait(Local, t2), Some(BACKOFF_MAX), "capped at 5 minutes");
        assert!(b.wait(Local, t2 + BACKOFF_MAX).is_none());
        b.clear(Local);
        assert_eq!(b.failures(Local), 0);
        assert!(b.claim(Local, t2).is_ok());
    }

    #[test]
    fn a_released_attempt_is_not_a_failure() {
        let b = LoginBackoff::default();
        let t0 = Instant::now();
        let t = fail_n(&b, Tunnel, BACKOFF_FREE_FAILURES, t0);
        // The sixth attempt is claimed (and would start the wait) ...
        let key = b.claim(Tunnel, t).unwrap();
        assert_eq!(b.wait(Tunnel, t), Some(BACKOFF_FIRST));
        // ... but the password was right and a code is still due.
        b.release(&key);
        assert_eq!(b.failures(Tunnel), BACKOFF_FREE_FAILURES);
        assert!(b.wait(Tunnel, t).is_none());
        // A wait already served is not started again by a released attempt.
        b.claim(Tunnel, t).unwrap();
        let t1 = t + BACKOFF_FIRST;
        let key = b.claim(Tunnel, t1).unwrap();
        b.release(&key);
        assert!(b.wait(Tunnel, t1).is_none());
        assert_eq!(b.failures(Tunnel), BACKOFF_FREE_FAILURES + 1);
        // With another attempt claimed in between, its wait is kept.
        let first = b.claim(Tunnel, t1).unwrap();
        assert!(b.claim(Tunnel, t1).is_err(), "the wait is running");
        let t2 = t1 + BACKOFF_FIRST * 2;
        let _second = b.claim(Tunnel, t2).unwrap();
        b.release(&first);
        assert!(b.wait(Tunnel, t2).is_some());
    }

    #[test]
    fn sources_have_their_own_budgets() {
        let b = LoginBackoff::default();
        let t0 = Instant::now();
        let lan: IpAddr = "192.168.1.50".parse().unwrap();
        fail_n(&b, Tunnel, BACKOFF_FREE_FAILURES + 1, t0);
        fail_n(&b, Network(lan), BACKOFF_FREE_FAILURES + 1, t0);
        assert!(b.wait(Tunnel, t0).is_some());
        assert!(b.wait(Network(lan), t0).is_some());
        assert!(b.wait(Local, t0).is_none());
        assert!(
            b.claim(Local, t0).is_ok(),
            "remote failures never delay this computer"
        );
    }

    #[test]
    fn many_network_sources_never_evict_another_budget() {
        let b = LoginBackoff::default();
        let t0 = Instant::now();
        let first: IpAddr = "10.9.0.1".parse().unwrap();
        fail_n(&b, Local, BACKOFF_FREE_FAILURES + 1, t0);
        fail_n(&b, Tunnel, BACKOFF_FREE_FAILURES + 1, t0);
        fail_n(&b, Network(first), BACKOFF_FREE_FAILURES + 1, t0);
        for i in 0..10_000u32 {
            let ip = IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i));
            let _ = b.claim(Network(ip), t0);
        }
        assert!(b.len() <= BACKOFF_NETWORK_SOURCES + 3);
        for src in [Local, Tunnel, Network(first)] {
            assert_eq!(b.failures(src), BACKOFF_FREE_FAILURES + 1, "{:?}", src);
            assert!(b.wait(src, t0).is_some(), "{:?}", src);
        }
        // The flood's extra sources share one budget, which is throttled.
        assert!(b.wait(Overflow, t0).is_some());
        // Spent budgets are dropped once forgotten, making room again.
        let later = t0 + BACKOFF_FORGET + Duration::from_secs(1);
        let fresh = IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1));
        assert_eq!(b.claim(Network(fresh), later).unwrap().key, Network(fresh));
    }

    #[test]
    fn sign_in_failures_are_forgotten_after_an_hour_without_one() {
        let b = LoginBackoff::default();
        let t0 = Instant::now();
        let t = fail_n(&b, Local, BACKOFF_FREE_FAILURES + 1, t0);
        // Within the hour the count carries on.
        let t1 = t + BACKOFF_FORGET - Duration::from_secs(1);
        b.claim(Local, t1).unwrap();
        assert_eq!(b.wait(Local, t1), Some(BACKOFF_FIRST * 2));
        // An hour after the last attempt it starts over.
        let t2 = t1 + BACKOFF_FORGET;
        b.claim(Local, t2).unwrap();
        assert_eq!(b.failures(Local), 1);
    }

    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }

    #[test]
    fn api_allows_100_per_second_then_429() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..100 {
            assert!(rl.check(Bucket::Api, ip(1), t0).is_ok());
        }
        assert!(rl.check(Bucket::Api, ip(1), t0).is_err());
        // Another IP is unaffected.
        assert!(rl.check(Bucket::Api, ip(2), t0).is_ok());
        // Moving window: after a second the oldest hits age out.
        assert!(rl
            .check(Bucket::Api, ip(1), t0 + Duration::from_millis(1001))
            .is_ok());
    }

    #[test]
    fn moving_window_is_not_a_fixed_window() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        for i in 0..5 {
            rl.check(Bucket::LoginMinute, ip(1), t0 + Duration::from_secs(i * 10))
                .unwrap();
        }
        // 59 s after the first hit: still 5 in the window.
        assert!(rl
            .check(Bucket::LoginMinute, ip(1), t0 + Duration::from_secs(59))
            .is_err());
        // 61 s: the first hit left the window.
        assert!(rl
            .check(Bucket::LoginMinute, ip(1), t0 + Duration::from_secs(61))
            .is_ok());
    }

    #[test]
    fn order_bucket_and_description() {
        assert_eq!(Bucket::for_api_path("/api/v1/placeorder"), Bucket::Order);
        assert_eq!(Bucket::for_api_path("/api/v1/placeorder/"), Bucket::Order);
        assert_eq!(
            Bucket::for_api_path("/api/v1/placesmartorder"),
            Bucket::SmartOrder
        );
        assert_eq!(Bucket::for_api_path("/api/v1/funds"), Bucket::Api);
        assert_eq!(Bucket::Order.describe(), "10 per 1 second");
        assert_eq!(Bucket::Api.describe(), "100 per 1 second");
    }

    #[test]
    fn table_stays_bounded() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        for i in 0..(MAX_ENTRIES as u32 + 500) {
            let a = IpAddr::V4(Ipv4Addr::from(i));
            let _ = rl.check(
                Bucket::Api,
                a,
                t0 + Duration::from_secs(2 * (i as u64 / 1000)),
            );
        }
        assert!(rl.len() <= MAX_ENTRIES);
    }

    #[test]
    fn exhausted_does_not_count() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..10 {
            rl.check(Bucket::ApiKeyFail, ip(3), t0).unwrap();
        }
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, ip(3), t0));
        assert!(!rl.is_exhausted(Bucket::ApiKeyFail, ip(4), t0));
    }
}
