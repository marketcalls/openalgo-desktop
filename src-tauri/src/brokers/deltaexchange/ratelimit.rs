//! Delta's weighted request quota (web `api/rate_limiter.py`).
//!
//! Delta does not throttle by requests per second. Every REST endpoint has a
//! weight, drawn from a quota of 10000 units per fixed 5-minute window; past
//! it the exchange answers 429 with `X-RATE-LIMIT-RESET` (milliseconds until
//! the window resets, no `Retry-After`). Two buckets, because Delta throttles
//! unauthenticated traffic by IP and authenticated traffic by user: public
//! market data can never spend the allowance order placement needs.
//!
//! The local budget is 90% of the quota. When it is spent a caller waits for
//! the window to reset, but never longer than `MAX_WAIT`; past that the call
//! fails cleanly. Waiting happens before a request is signed, because Delta
//! rejects signatures older than 5 seconds.
//!
//! State is two small fixed structs: nothing grows with traffic.

use parking_lot::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// Delta resets the quota every fixed 5 minutes.
pub const WINDOW: Duration = Duration::from_secs(300);
pub const FULL_QUOTA: u32 = 10_000;
/// Spend at most 90% of the documented quota (headroom for the drift between
/// Delta's fixed window and the floating one tracked here).
pub const BUDGET: u32 = FULL_QUOTA / 10 * 9;
/// Longest a caller is parked waiting for the window to reset.
pub const MAX_WAIT: Duration = Duration::from_secs(30);
/// 429 retries for signed calls.
pub const MAX_RETRIES: u32 = 3;
/// 429 retries for public market data (and transient network errors).
pub const PUBLIC_RETRIES: u32 = 2;
/// Fallback backoff when a 429 names no reset: 1, 2, 4 s.
pub const BASE_BACKOFF: Duration = Duration::from_secs(1);
/// Sleeps allowed inside one `consume` before giving up.
pub const MAX_SLEEPS: u32 = 2;
/// Unlisted endpoints cost one unit.
pub const DEFAULT_WEIGHT: u32 = 1;
/// Longest an order placement, change or cancel waits in all for the rate
/// limit (quota waits and 429 retries together) before it is refused: a
/// MARKET order sent a minute late is worse than one refused (12-U2).
pub const ORDER_WAIT_CAP: Duration = Duration::from_secs(10);

/// Whether a call writes orders (and so waits at most `ORDER_WAIT_CAP`).
pub fn is_order_write(method: &str, path: &str) -> bool {
    !method.eq_ignore_ascii_case("GET") && path.starts_with("/v2/orders")
}

/// Which allowance a call draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Unauthenticated market data, throttled by IP.
    Public,
    /// Signed calls, throttled by user.
    Private,
}

const WEIGHTS_BY_METHOD: &[(&str, &str, u32)] = &[
    ("POST", "/v2/orders/batch", 25),
    ("PUT", "/v2/orders/batch", 25),
    ("DELETE", "/v2/orders/batch", 25),
    ("POST", "/v2/orders", 5),
    ("PUT", "/v2/orders", 5),
    ("DELETE", "/v2/orders", 5),
];

const WEIGHTS: &[(&str, u32)] = &[
    ("/v2/orders/history", 10),
    ("/v2/fills", 10),
    ("/v2/wallet/transactions", 10),
    ("/v2/positions/change_margin", 5),
    ("/v2/products", 3),
    ("/v2/tickers", 3),
    ("/v2/l2orderbook", 3),
    ("/v2/history/candles", 3),
    ("/v2/history/sparklines", 3),
    ("/v2/orders", 3),
    ("/v2/positions", 3),
    ("/v2/wallet/balances", 3),
    ("/v2/rate_limits/quota", 3),
];

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Quota cost of one call (web `endpoint_weight`): method-specific entries
/// first, then the longest matching prefix, else 1.
pub fn endpoint_weight(path: &str, method: &str) -> u32 {
    let path = path.split('?').next().unwrap_or("").trim_end_matches('/');
    let method = method.to_ascii_uppercase();
    for (m, prefix, w) in WEIGHTS_BY_METHOD {
        if *m == method && under(path, prefix) {
            return *w;
        }
    }
    WEIGHTS
        .iter()
        .filter(|(prefix, _)| under(path, prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, w)| *w)
        .unwrap_or(DEFAULT_WEIGHT)
}

#[derive(Debug, Default)]
struct Window {
    used: u32,
    start: Option<Instant>,
}

impl Window {
    fn roll(&mut self, now: Instant, window: Duration) {
        match self.start {
            Some(s) if now.saturating_duration_since(s) < window => {}
            _ => {
                self.used = 0;
                self.start = Some(now);
            }
        }
    }

    fn resets_in(&self, now: Instant, window: Duration) -> Duration {
        self.start
            .map(|s| (s + window).saturating_duration_since(now))
            .unwrap_or(Duration::ZERO)
    }
}

/// Local accounting of both buckets.
#[derive(Debug)]
pub struct Quota {
    public: Mutex<Window>,
    private: Mutex<Window>,
    budget: u32,
    window: Duration,
}

impl Default for Quota {
    fn default() -> Self {
        Self::new(BUDGET, WINDOW)
    }
}

impl Quota {
    pub fn new(budget: u32, window: Duration) -> Self {
        Self {
            public: Mutex::new(Window::default()),
            private: Mutex::new(Window::default()),
            budget,
            window,
        }
    }

    fn bucket(&self, b: Bucket) -> &Mutex<Window> {
        match b {
            Bucket::Public => &self.public,
            Bucket::Private => &self.private,
        }
    }

    /// Reserve `weight` units now, or say how long until the window resets.
    /// The check and the reservation happen under one lock, so concurrent
    /// callers cannot all see room for one.
    pub fn try_take(&self, bucket: Bucket, weight: u32) -> Result<(), Duration> {
        let now = Instant::now();
        let mut w = self.bucket(bucket).lock();
        w.roll(now, self.window);
        if w.used.saturating_add(weight) <= self.budget {
            w.used += weight;
            Ok(())
        } else {
            Err(w.resets_in(now, self.window))
        }
    }

    /// Adopt the exchange's own view of the window (`/v2/rate_limits/quota`).
    pub fn set_server(&self, bucket: Bucket, used: u32, resets_in: Duration) {
        let now = Instant::now();
        let mut w = self.bucket(bucket).lock();
        w.used = used;
        let elapsed = self.window.saturating_sub(resets_in);
        w.start = Some(now.checked_sub(elapsed).unwrap_or(now));
    }

    /// The exchange refused a call: spend the local budget and, when Delta
    /// named the reset, pin the local window to it.
    pub fn note_429(&self, bucket: Bucket, reset: Option<Duration>) {
        let now = Instant::now();
        let mut w = self.bucket(bucket).lock();
        w.roll(now, self.window);
        w.used = self.budget;
        if let Some(r) = reset {
            let elapsed = self.window.saturating_sub(r);
            w.start = Some(now.checked_sub(elapsed).unwrap_or(now));
        }
    }

    /// `(used, resets_in)` of one bucket, for logs and tests.
    pub fn snapshot(&self, bucket: Bucket) -> (u32, Duration) {
        let now = Instant::now();
        let mut w = self.bucket(bucket).lock();
        w.roll(now, self.window);
        (w.used, w.resets_in(now, self.window))
    }
}

/// Seconds until the quota resets, from `X-RATE-LIMIT-RESET` (milliseconds).
pub fn quota_reset(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get("x-rate-limit-reset")?.to_str().ok()?;
    let ms = raw.trim().parse::<f64>().ok()?;
    (ms.is_finite()).then(|| Duration::from_secs_f64((ms / 1000.0).max(0.0)))
}

/// The wait the server asked for, uncapped: `X-RATE-LIMIT-RESET`, else
/// `Retry-After` (seconds; a proxy in front of Delta may send it).
pub fn server_requested_delay(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    quota_reset(headers).or_else(|| {
        let raw = headers.get("retry-after")?.to_str().ok()?;
        let s = raw.trim().parse::<f64>().ok()?;
        (s.is_finite()).then(|| Duration::from_secs_f64(s.max(0.0)))
    })
}

/// How long to sleep before retrying a 429, never more than `MAX_WAIT`.
pub fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: u32) -> Duration {
    match server_requested_delay(headers) {
        Some(d) => d.clamp(Duration::from_millis(50), MAX_WAIT),
        None => BASE_BACKOFF
            .saturating_mul(1u32 << attempt.min(8))
            .min(MAX_WAIT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn weights_match_the_web_table() {
        assert_eq!(endpoint_weight("/v2/orders", "POST"), 5);
        assert_eq!(endpoint_weight("/v2/orders", "PUT"), 5);
        assert_eq!(endpoint_weight("/v2/orders", "DELETE"), 5);
        assert_eq!(endpoint_weight("/v2/orders/all", "DELETE"), 5);
        assert_eq!(endpoint_weight("/v2/orders/batch", "POST"), 25);
        assert_eq!(endpoint_weight("/v2/orders", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/orders?state=open", "get"), 3);
        assert_eq!(endpoint_weight("/v2/orders/history", "GET"), 10);
        assert_eq!(endpoint_weight("/v2/fills", "GET"), 10);
        assert_eq!(endpoint_weight("/v2/tickers/BTCUSD", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/l2orderbook/27", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/history/candles", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/positions/margined", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/positions/change_margin", "POST"), 5);
        assert_eq!(endpoint_weight("/v2/wallet/balances", "GET"), 3);
        assert_eq!(endpoint_weight("/v2/products", "GET"), 3);
        assert_eq!(
            endpoint_weight("/v2/products/27/orders/leverage", "POST"),
            3
        );
        assert_eq!(endpoint_weight("/v2/profile", "GET"), 1);
        assert_eq!(endpoint_weight("/v2/ordersx", "GET"), 1);
        assert_eq!(BUDGET, 9000);
    }

    #[tokio::test(start_paused = true)]
    async fn budget_is_per_bucket_and_resets_with_the_window() {
        let q = Quota::new(10, Duration::from_secs(300));
        for _ in 0..3 {
            q.try_take(Bucket::Private, 3).unwrap();
        }
        // 9 used; 3 more would exceed 10.
        let wait = q.try_take(Bucket::Private, 3).unwrap_err();
        assert_eq!(wait, Duration::from_secs(300));
        // The public bucket is untouched.
        q.try_take(Bucket::Public, 10).unwrap();
        tokio::time::advance(Duration::from_secs(120)).await;
        assert_eq!(
            q.try_take(Bucket::Private, 3).unwrap_err(),
            Duration::from_secs(180)
        );
        tokio::time::advance(Duration::from_secs(180)).await;
        q.try_take(Bucket::Private, 3).unwrap();
        assert_eq!(q.snapshot(Bucket::Private).0, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn server_view_and_429_pin_the_window() {
        let q = Quota::new(100, Duration::from_secs(300));
        q.set_server(Bucket::Public, 40, Duration::from_secs(10));
        assert_eq!(q.snapshot(Bucket::Public), (40, Duration::from_secs(10)));
        q.note_429(Bucket::Private, Some(Duration::from_millis(2500)));
        let (used, left) = q.snapshot(Bucket::Private);
        assert_eq!((used, left), (100, Duration::from_millis(2500)));
        assert!(q.try_take(Bucket::Private, 1).is_err());
        tokio::time::advance(Duration::from_millis(2500)).await;
        q.try_take(Bucket::Private, 1).unwrap();
    }

    #[test]
    fn retry_delay_reads_the_reset_header_in_ms_and_caps_it() {
        let mut h = HeaderMap::new();
        assert_eq!(retry_delay(&h, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(&h, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(&h, 10), MAX_WAIT);
        h.insert("x-rate-limit-reset", HeaderValue::from_static("1500"));
        assert_eq!(quota_reset(&h), Some(Duration::from_millis(1500)));
        assert_eq!(retry_delay(&h, 0), Duration::from_millis(1500));
        h.insert("x-rate-limit-reset", HeaderValue::from_static("600000"));
        assert_eq!(retry_delay(&h, 0), MAX_WAIT);
        assert_eq!(server_requested_delay(&h), Some(Duration::from_secs(600)));
        let mut r = HeaderMap::new();
        r.insert("retry-after", HeaderValue::from_static("2"));
        assert_eq!(server_requested_delay(&r), Some(Duration::from_secs(2)));
        r.insert("x-rate-limit-reset", HeaderValue::from_static("junk"));
        assert_eq!(quota_reset(&r), None);
    }
}
