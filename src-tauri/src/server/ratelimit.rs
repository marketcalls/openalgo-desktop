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

#[derive(Default)]
pub struct RateLimiter {
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
