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

/// Where hits from new addresses are counted while the table is full of
/// live entries: one shared bucket, so a full table fails closed instead of
/// forgetting anyone's count. In `100:0:0:2::/64` of the discard-only
/// `100::/64` range: an address nothing is sent to or banned.
pub const OVERFLOW: IpAddr = IpAddr::V6(std::net::Ipv6Addr::new(0x100, 0, 0, 2, 0, 0, 0, 0));

/// A per-credential key (`middleware::limiter_key`, `100:0:0:1::/64`): the
/// window of a valid credential behind a tunnel, recreated on its next
/// request. Callers cannot mint these with invented credentials (their
/// failures count per caller), but they are the first to go when the
/// table is full.
fn minted(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v) if v.segments()[..4] == [0x100, 0, 0, 1])
}

/// A request window: it holds no failure and no penalty, so it may be
/// dropped to make room (that caller's window starts afresh). Failure
/// budgets and sign-in limits are never dropped while they count.
fn request_window(bucket: Bucket, ip: &IpAddr) -> bool {
    // The shared overflow window and a network's aggregate are never
    // dropped: a fresh one would not limit anyone.
    *ip != OVERFLOW
        && !crate::server::addr::is_aggregate(*ip)
        && (minted(ip)
            || matches!(
                bucket,
                Bucket::Api
                    | Bucket::Order
                    | Bucket::SmartOrder
                    | Bucket::Guard
                    | Bucket::StrategyWebhook
                    | Bucket::ChartinkWebhook
            ))
}

/// How many times one device's limit an aggregate allows: every device of
/// one of this machine's own IPv6 networks together
/// (`addr::aggregate_key`), well above a household's legitimate use.
pub const AGGREGATE_FACTOR: usize = 5;

/// Keys that always get their own entry, so a full table never puts this
/// computer, the shared tunnel and MCP identities (`100::/64`) or a
/// network's aggregate in the overflow bucket. A handful of keys.
fn reserved(ip: &IpAddr) -> bool {
    if crate::server::addr::is_aggregate(*ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => {
            v.is_loopback()
                || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback())
                || v.segments()[..4] == [0x100, 0, 0, 0]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    Api,
    Order,
    SmartOrder,
    LoginMinute,
    LoginHour,
    Reset,
    /// Failed API keys, MCP tokens and feed keys by caller, one budget for
    /// all three (`middleware::failures_exhausted`). Spent, it refuses
    /// further invalid attempts early; never a valid credential.
    ApiKeyFail,
    /// Every request by caller: a resource guard well above legitimate use
    /// (`middleware::peer_layer`, the feed's `authenticate`).
    Guard,
    /// `/strategy/webhook/<token>` by caller address (web
    /// `WEBHOOK_RATE_LIMIT`, 100 per minute).
    StrategyWebhook,
    /// `/chartink/webhook/<id>` by caller address (web
    /// `WEBHOOK_RATE_LIMIT`, 100 per minute).
    ChartinkWebhook,
    /// Failed strategy and Chartink webhook authentications (unknown
    /// address, caller outside the allowlist) by caller. Spent, it refuses
    /// further invalid attempts early; never a valid webhook address.
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
            Bucket::Guard => (1000, Duration::from_secs(1)),
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

/// How a request window answered ([`RateLimiter::admit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Within the window.
    Allowed,
    /// Over the caller's own window.
    Over(Duration),
    /// Over a window this caller shares with others: the overflow window a
    /// new caller is counted in while the table is full, or its network's
    /// aggregate (`addr::aggregate_key`). Others can fill these, so refuse
    /// the caller only if it does not present a valid credential.
    OverflowOver(Duration),
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
    /// recorded, so a client that backs off recovers). A request window
    /// that should not refuse a caller with a valid credential uses
    /// [`RateLimiter::admit`] instead.
    pub fn check(&self, bucket: Bucket, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        match self.admit(bucket, ip, now) {
            Admission::Allowed => Ok(()),
            Admission::Over(wait) | Admission::OverflowOver(wait) => Err(wait),
        }
    }

    /// Count a hit, and say whether the refusal (if any) came from the
    /// caller's own window or from the shared [`OVERFLOW`] window a new
    /// caller is counted in while the table is full of live failure counts.
    /// Two tiers: a per-address IPv6 key also counts against its network's
    /// aggregate (`addr::aggregate_key`, [`AGGREGATE_FACTOR`] times the
    /// limit), in every bucket, so no surface charges one tier only. The hit
    /// is recorded in both or neither.
    pub fn admit(&self, bucket: Bucket, ip: IpAddr, now: Instant) -> Admission {
        let (limit, window) = bucket.limit();
        let mut map = self.map.lock();
        let own = Self::slot(&mut map, bucket, ip, now);
        let aggregate = crate::server::addr::aggregate_key(ip);
        let mut tiers = vec![(own, limit)];
        if let Some(net) = aggregate {
            tiers.push((net, limit * AGGREGATE_FACTOR));
        }
        for (tier, (key, limit)) in tiers.iter().enumerate() {
            let q = map.entry((bucket, *key)).or_default();
            while q
                .front()
                .map(|t| now.saturating_duration_since(*t) >= window)
                .unwrap_or(false)
            {
                q.pop_front();
            }
            if q.len() >= *limit {
                let oldest = q.front().copied().unwrap_or(now);
                let wait = window.saturating_sub(now.saturating_duration_since(oldest));
                // The caller's own window, or one it shares with others.
                let shared = tier > 0 || (*key == OVERFLOW && ip != OVERFLOW);
                return if shared {
                    Admission::OverflowOver(wait)
                } else {
                    Admission::Over(wait)
                };
            }
        }
        for (key, _) in &tiers {
            map.entry((bucket, *key)).or_default().push_back(now);
        }
        Admission::Allowed
    }

    /// Whether `bucket` is exhausted for `ip` (its own window, or its
    /// network's aggregate), without counting a hit.
    pub fn is_exhausted(&self, bucket: Bucket, ip: IpAddr, now: Instant) -> bool {
        let (limit, window) = bucket.limit();
        let map = self.map.lock();
        let full = |key: IpAddr, limit: usize| {
            map.get(&(bucket, key))
                .map(|q| {
                    q.iter()
                        .filter(|t| now.saturating_duration_since(**t) < window)
                        .count()
                        >= limit
                })
                .unwrap_or(false)
        };
        // A caller without its own entry while the table is full is counted
        // in the overflow bucket.
        let own = if !map.contains_key(&(bucket, ip))
            && map.len() >= MAX_ENTRIES
            && !reserved(&ip)
            && !minted(&ip)
        {
            OVERFLOW
        } else {
            ip
        };
        full(own, limit)
            || crate::server::addr::aggregate_key(ip)
                .is_some_and(|net| full(net, limit * AGGREGATE_FACTOR))
    }

    /// Hits inside the window of `bucket` for `key` (tests).
    #[cfg(test)]
    pub fn hits(&self, bucket: Bucket, key: IpAddr, now: Instant) -> usize {
        let (_, window) = bucket.limit();
        self.map
            .lock()
            .get(&(bucket, key))
            .map(|q| {
                q.iter()
                    .filter(|t| now.saturating_duration_since(**t) < window)
                    .count()
            })
            .unwrap_or(0)
    }

    /// The key a hit for `ip` is recorded under. A failure count still
    /// inside its window is never dropped to make room, or a caller could
    /// flush every lockout by filling the table: expired entries go first,
    /// then request windows (oldest first; they hold no failure, and that
    /// caller's window starts afresh). Only when every entry is live does a
    /// new caller go to the shared, bounded [`OVERFLOW`] bucket, which still
    /// limits it; the callers of request windows refuse an overflow only
    /// for a caller without a valid credential ([`Admission::OverflowOver`]).
    /// This computer, the shared identities and valid credentials' windows
    /// always get their own entry.
    fn slot(
        map: &mut HashMap<(Bucket, IpAddr), VecDeque<Instant>>,
        bucket: Bucket,
        ip: IpAddr,
        now: Instant,
    ) -> IpAddr {
        if map.len() < MAX_ENTRIES || map.contains_key(&(bucket, ip)) {
            return ip;
        }
        Self::sweep(map, now);
        if map.len() >= MAX_ENTRIES {
            // Down to seven eighths, so a stream of new callers does not
            // sort the table on every hit.
            let mut windows: Vec<((Bucket, IpAddr), Instant)> = map
                .iter()
                .filter(|((b, k), _)| request_window(*b, k))
                .map(|(k, q)| (*k, q.back().copied().unwrap_or(now)))
                .collect();
            windows.sort_by_key(|(_, last)| *last);
            let target = MAX_ENTRIES - MAX_ENTRIES / 8;
            for (k, _) in windows {
                if map.len() <= target {
                    break;
                }
                map.remove(&k);
            }
        }
        if map.len() < MAX_ENTRIES || reserved(&ip) || minted(&ip) {
            ip
        } else {
            OVERFLOW
        }
    }

    /// Drop the entries whose window has passed.
    fn sweep(map: &mut HashMap<(Bucket, IpAddr), VecDeque<Instant>>, now: Instant) {
        map.retain(|(b, _), q| {
            let (_, w) = b.limit();
            q.back()
                .map(|t| now.saturating_duration_since(*t) < w)
                .unwrap_or(false)
        });
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
        assert!(rl.len() <= MAX_ENTRIES + 1);
    }

    fn credential(n: u32) -> IpAddr {
        let [a, b, c, d] = n.to_be_bytes();
        IpAddr::V6(std::net::Ipv6Addr::new(
            0x100,
            0,
            0,
            1,
            u16::from_be_bytes([a, b]),
            u16::from_be_bytes([c, d]),
            0,
            0,
        ))
    }

    /// Filling the table with per-credential windows never flushes a live
    /// lockout of an address.
    #[test]
    fn per_credential_windows_never_flush_a_lockout() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        let lan = ip(7);
        for _ in 0..10 {
            rl.check(Bucket::ApiKeyFail, lan, t0).unwrap();
            rl.check(Bucket::WebhookFail, lan, t0).unwrap();
        }
        for n in 0..(3 * MAX_ENTRIES as u32) {
            let _ = rl.check(Bucket::ApiKeyFail, credential(n), t0);
            let _ = rl.check(Bucket::WebhookFail, credential(n), t0);
        }
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, lan, t0));
        assert!(rl.is_exhausted(Bucket::WebhookFail, lan, t0));
        assert!(rl.check(Bucket::ApiKeyFail, lan, t0).is_err());
        assert!(rl.len() <= MAX_ENTRIES);
        // A new credential is still recorded.
        let last = credential(3 * MAX_ENTRIES as u32 - 1);
        assert!(rl.check(Bucket::ApiKeyFail, last, t0).is_ok());
    }

    /// Flooding with new callers' request windows and failures never drops
    /// a failure count still inside its window; request windows make room.
    #[test]
    fn active_failures_are_never_evicted_by_flooding() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        let lan = ip(9);
        for _ in 0..10 {
            rl.check(Bucket::ApiKeyFail, lan, t0).unwrap();
        }
        let addr = |n: u32| IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + n));
        for n in 0..(3 * MAX_ENTRIES as u32) {
            let _ = rl.check(Bucket::Guard, addr(n), t0);
            let _ = rl.check(Bucket::Api, addr(n), t0);
            if n % 3 == 0 {
                let _ = rl.check(Bucket::WebhookFail, addr(n), t0);
            }
        }
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, lan, t0));
        assert!(rl.check(Bucket::ApiKeyFail, lan, t0).is_err());
        // One shared overflow entry per kind of bucket at most.
        assert!(rl.len() <= MAX_ENTRIES + 8, "{}", rl.len());
    }

    /// A table full of live failure counts still limits new callers: they
    /// share one bounded overflow window, reported as such so a caller
    /// with a valid credential is not refused by it. This computer keeps
    /// its own window.
    #[test]
    fn a_full_table_still_limits_new_callers() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        let addr = |n: u32| IpAddr::V4(Ipv4Addr::from(0x0c00_0000 + n));
        for n in 0..MAX_ENTRIES as u32 {
            rl.check(Bucket::ApiKeyFail, addr(n), t0).unwrap();
        }
        for n in 0..100 {
            let fresh = addr(MAX_ENTRIES as u32 + n);
            assert_eq!(rl.admit(Bucket::Api, fresh, t0), Admission::Allowed);
        }
        let next = addr(MAX_ENTRIES as u32 + 500);
        assert!(matches!(
            rl.admit(Bucket::Api, next, t0),
            Admission::OverflowOver(_)
        ));
        assert!(rl.check(Bucket::Api, next, t0).is_err());
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(rl.admit(Bucket::Api, local, t0), Admission::Allowed);
        // Every live failure count is still there.
        assert!(rl.check(Bucket::ApiKeyFail, addr(0), t0).is_ok());
        assert!(rl.len() <= MAX_ENTRIES + 4);
    }

    /// Ten thousand addresses of one IPv6 /64 are one caller: one entry.
    #[test]
    fn an_ipv6_device_is_one_caller_across_its_64() {
        use crate::server::source::Source;
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        for n in 0..10_000u128 {
            let a = IpAddr::V6(std::net::Ipv6Addr::from(
                0x2001_0db8_0001_0002_0000_0000_0000_0000_u128 | (n * 7919),
            ));
            let _ = rl.check(Bucket::ApiKeyFail, Source::Lan(a).ip(), t0);
        }
        assert_eq!(rl.len(), 1);
        let one: IpAddr = "2001:db8:1:2::".parse().unwrap();
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, one, t0));
        // IPv4 stays per address.
        let v4 = |n: u8| Source::Lan(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))).ip();
        assert_ne!(v4(1), v4(2));
    }

    /// A table full of live addresses fails closed: new addresses share
    /// the overflow bucket, nobody's count is dropped, and this computer
    /// still gets its own entry.
    #[test]
    fn a_full_table_fails_closed_into_one_bucket() {
        let rl = RateLimiter::new();
        let t0 = Instant::now();
        // Distinct devices (IPv4: no network aggregate).
        let addr = |n: u32| IpAddr::V4(Ipv4Addr::from(0x0d00_0000 + n));
        for n in 0..MAX_ENTRIES as u32 {
            rl.check(Bucket::ApiKeyFail, addr(n), t0).unwrap();
        }
        for _ in 0..9 {
            rl.check(Bucket::ApiKeyFail, addr(0), t0).unwrap();
        }
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, addr(0), t0));
        // Ten new addresses fail once each: the shared bucket is used up,
        // and the next new address is refused too.
        for n in 0..10 {
            let fresh = addr(MAX_ENTRIES as u32 + n);
            assert!(!rl.is_exhausted(Bucket::ApiKeyFail, fresh, t0));
            rl.check(Bucket::ApiKeyFail, fresh, t0).unwrap();
        }
        let next = addr(MAX_ENTRIES as u32 + 99);
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, next, t0));
        assert!(rl.check(Bucket::ApiKeyFail, next, t0).is_err());
        assert!(rl.is_exhausted(Bucket::ApiKeyFail, addr(0), t0));
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!rl.is_exhausted(Bucket::ApiKeyFail, local, t0));
        assert!(rl.check(Bucket::ApiKeyFail, local, t0).is_ok());
        assert!(rl.len() <= MAX_ENTRIES + 2);
        // Once the windows pass, the table frees up.
        let later = t0 + Duration::from_secs(61);
        assert!(rl.check(Bucket::ApiKeyFail, next, later).is_ok());
        assert!(rl.len() < 10);
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
