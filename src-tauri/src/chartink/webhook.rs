//! The public Chartink webhook, `POST /chartink/webhook/<webhook_id>` (web
//! `blueprints/chartink.py::webhook`), hardened like the strategy webhook.
//!
//! The URL's webhook id is the whole credential (Chartink cannot set a
//! header). Its first `LOCATOR_LEN` characters locate the row; the full id
//! is compared in constant time. Nothing here logs the id or the payload.
//!
//! Order of checks, each before the next costs anything:
//!
//! 1. per-address failure lockout and rate limit (the shared limiter), and
//!    a per-locator rate limit: refused with 429 before any lookup;
//! 2. the body size cap (413) before the body is read;
//! 3. the id: malformed or unknown answers the web's 404; a wrong id whose
//!    locator matches a webhook counts a failure against that webhook, and
//!    `LOCKOUT_FAILURES` of them inside `LOCKOUT_WINDOW`, from any number of
//!    addresses, lock it for `LOCK_DURATION` (403 even for the right id)
//!    until the trader turns the strategy off and on again;
//! 4. then the web's pipeline: inactive, payload, action keyword, intraday
//!    window, mappings, and the orders queued.

use super::store::{Strategy, LOCATOR_LEN};
use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Asia::Kolkata;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const MAX_PAYLOAD_BYTES: usize = 16384;
/// Web `WEBHOOK_RATE_LIMIT`: 100 per minute per locator.
pub const RATE_LIMIT: usize = 100;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Failed ids against one webhook that lock it.
pub const LOCKOUT_FAILURES: usize = 10;
pub const LOCKOUT_WINDOW: Duration = Duration::from_secs(600);
/// How long a locked webhook stays locked unless the trader unlocks it.
pub const LOCK_DURATION: Duration = Duration::from_secs(900);
/// Bound on every per-key map.
pub const MAX_TRACKED_KEYS: usize = 4096;

pub const INVALID_WEBHOOK: &str = "Invalid webhook ID";
pub const LOCKED_MESSAGE: &str = "This Chartink webhook is locked after repeated alerts with a wrong webhook address. Check who has the address, then turn the strategy off and on again to unlock it.";

/// Bounded in-memory windows: the per-locator rate, the per-webhook
/// failures and the locks.
pub struct Guard {
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    failures: Mutex<HashMap<i64, VecDeque<Instant>>>,
    locked: Mutex<HashMap<i64, Instant>>,
    throttle_logged: Mutex<Option<Instant>>,
    offset: Mutex<Duration>,
}

impl Default for Guard {
    fn default() -> Self {
        Self::new()
    }
}

impl Guard {
    pub fn new() -> Self {
        Self {
            rate: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            locked: Mutex::new(HashMap::new()),
            throttle_logged: Mutex::new(None),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn now(&self) -> Instant {
        Instant::now() + *self.offset.lock()
    }

    /// Move the windows' clock forward (tests).
    pub fn advance(&self, by: Duration) {
        *self.offset.lock() += by;
    }

    /// Keys held across every window (hygiene tests).
    pub fn tracked(&self) -> usize {
        self.rate.lock().len() + self.failures.lock().len() + self.locked.lock().len()
    }

    /// Count one hit for a locator; false when over the limit.
    pub fn rate_check(&self, locator: &str) -> bool {
        let now = self.now();
        let mut map = self.rate.lock();
        if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(locator) {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.saturating_duration_since(*t) < RATE_WINDOW)
            });
            if map.len() >= MAX_TRACKED_KEYS {
                map.clear();
            }
        }
        let q = map.entry(locator.to_string()).or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= RATE_WINDOW)
        {
            q.pop_front();
        }
        if q.len() >= RATE_LIMIT {
            return false;
        }
        q.push_back(now);
        true
    }

    /// Whether this webhook is locked now (an expired lock is dropped).
    pub fn is_locked(&self, strategy_id: i64) -> bool {
        let now = self.now();
        let mut locked = self.locked.lock();
        match locked.get(&strategy_id) {
            Some(at) if now.saturating_duration_since(*at) < LOCK_DURATION => true,
            Some(_) => {
                locked.remove(&strategy_id);
                false
            }
            None => false,
        }
    }

    /// Count one wrong id against a webhook; true when this one locked it.
    pub fn record_failure(&self, strategy_id: i64) -> bool {
        let now = self.now();
        let mut map = self.failures.lock();
        map.retain(|_, q| {
            q.back()
                .is_some_and(|t| now.saturating_duration_since(*t) < LOCKOUT_WINDOW)
        });
        if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(&strategy_id) {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, q)| q.back().copied())
                .map(|(k, _)| *k)
            {
                map.remove(&oldest);
            }
        }
        let q = map.entry(strategy_id).or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= LOCKOUT_WINDOW)
        {
            q.pop_front();
        }
        q.push_back(now);
        if q.len() >= LOCKOUT_FAILURES {
            map.remove(&strategy_id);
            drop(map);
            let mut locked = self.locked.lock();
            locked.retain(|_, at| now.saturating_duration_since(*at) < LOCK_DURATION);
            if locked.len() >= MAX_TRACKED_KEYS {
                locked.clear();
            }
            locked.insert(strategy_id, now);
            return true;
        }
        false
    }

    /// The trader unlocked the webhook (turned the strategy off and on).
    pub fn unlock(&self, strategy_id: i64) {
        self.failures.lock().remove(&strategy_id);
        self.locked.lock().remove(&strategy_id);
    }

    pub(crate) fn note_throttled(&self, why: &str) {
        let now = self.now();
        let mut last = self.throttle_logged.lock();
        if last.is_none_or(|t| now.saturating_duration_since(t) >= RATE_WINDOW) {
            *last = Some(now);
            tracing::warn!(
                "Chartink webhook requests are being refused: {} (logged once per minute)",
                why
            );
        }
    }
}

/// The locator of a webhook id, when the id is shaped like one (a UUID).
pub fn locator(webhook_id: &str) -> Option<&str> {
    let ok = webhook_id.len() == 36
        && webhook_id
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
    ok.then(|| &webhook_id[..LOCATOR_LEN])
}

/// The web's per-address limit (100 a minute) for this computer and devices
/// on the network, before any lookup. Tunnel callers share one identity,
/// so they are limited per webhook once the id checked out
/// ([`admit_webhook`]).
pub fn admit_address(
    guard: &Guard,
    limiter: &crate::server::ratelimit::RateLimiter,
    ip: IpAddr,
) -> bool {
    use crate::server::ratelimit::Bucket;
    if limiter
        .check(Bucket::ChartinkWebhook, ip, limiter.now())
        .is_err()
    {
        guard.note_throttled("one address is over the webhook rate limit");
        return false;
    }
    true
}

/// The limits of an id that checked out: behind a tunnel its own
/// per-address window (`tunnel_key`, `middleware::limiter_key`), then the
/// per-locator window. A valid id is never limited by other callers'
/// traffic or failures.
pub fn admit_webhook(
    guard: &Guard,
    limiter: &crate::server::ratelimit::RateLimiter,
    tunnel_key: Option<IpAddr>,
    webhook_id: &str,
) -> bool {
    use crate::server::ratelimit::Bucket;
    if let Some(key) = tunnel_key {
        if limiter
            .check(Bucket::ChartinkWebhook, key, limiter.now())
            .is_err()
        {
            guard.note_throttled("one webhook is over the webhook rate limit");
            return false;
        }
    }
    // Keyed on the locator only (never the full id).
    let key = locator(webhook_id).unwrap_or("malformed");
    if !guard.rate_check(key) {
        guard.note_throttled("one webhook is over its rate limit");
        return false;
    }
    true
}

/// Constant-time equality of two ids.
pub fn id_matches(given: &str, stored: &str) -> bool {
    use subtle::ConstantTimeEq;
    given.len() == stored.len() && bool::from(given.as_bytes().ct_eq(stored.as_bytes()))
}

/// What the scan name asks for (web: the first of BUY, SELL, SHORT, COVER
/// found in the upper-cased name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub action: &'static str,
    pub smart: bool,
    pub entry: bool,
}

pub fn signal_for(scan_name: &str) -> Option<Signal> {
    let s = scan_name.to_uppercase();
    if s.contains("BUY") {
        Some(Signal {
            action: "BUY",
            smart: false,
            entry: true,
        })
    } else if s.contains("SELL") {
        Some(Signal {
            action: "SELL",
            smart: true,
            entry: false,
        })
    } else if s.contains("SHORT") {
        Some(Signal {
            action: "SELL",
            smart: false,
            entry: true,
        })
    } else if s.contains("COVER") {
        Some(Signal {
            action: "BUY",
            smart: true,
            entry: false,
        })
    } else {
        None
    }
}

fn hhmm(s: Option<&str>) -> Option<NaiveTime> {
    s.and_then(|t| NaiveTime::parse_from_str(t.trim(), "%H:%M").ok())
}

/// The intraday window check: `Err(message)` when the alert is outside it.
pub fn window_refusal(s: &Strategy, signal: Signal, now: DateTime<Utc>) -> Option<&'static str> {
    if !s.is_intraday {
        return None;
    }
    let t = now.with_timezone(&Kolkata).time();
    let (Some(start), Some(end), Some(sq)) = (
        hhmm(s.start_time.as_deref()),
        hhmm(s.end_time.as_deref()),
        hhmm(s.squareoff_time.as_deref()),
    ) else {
        // The web raises on a missing time (500); refuse plainly instead.
        return Some("This intraday strategy has no trading window set");
    };
    if t < start {
        return Some("Cannot place orders before start time");
    }
    if t >= sq {
        return Some("Cannot place orders after squareoff time");
    }
    if signal.entry && t >= end {
        return Some("Cannot place entry orders after end time");
    }
    None
}

/// The order requests one alert produces (web payloads, without the API
/// key: the services are called directly).
pub fn orders_for(
    strategy: &Strategy,
    signal: Signal,
    stocks: &str,
    mappings: &[super::store::Mapping],
) -> (Vec<(bool, Value)>, Vec<String>) {
    let by_symbol: HashMap<&str, &super::store::Mapping> = mappings
        .iter()
        .map(|m| (m.chartink_symbol.as_str(), m))
        .collect();
    let mut orders = Vec::new();
    let mut processed = Vec::new();
    for raw in stocks.split(',') {
        let symbol = raw.trim();
        if symbol.is_empty() {
            continue;
        }
        let Some(m) = by_symbol.get(symbol) else {
            continue;
        };
        let mut o = Map::new();
        o.insert("strategy".into(), json!(strategy.name));
        o.insert("symbol".into(), json!(m.chartink_symbol));
        o.insert("exchange".into(), json!(m.exchange));
        o.insert("action".into(), json!(signal.action));
        o.insert("product".into(), json!(m.product_type));
        o.insert("pricetype".into(), json!("MARKET"));
        o.insert("price".into(), json!(0));
        o.insert("trigger_price".into(), json!(0));
        o.insert("disclosed_quantity".into(), json!(0));
        if signal.smart {
            o.insert("quantity".into(), json!(0));
            o.insert("position_size".into(), json!(0));
        } else {
            o.insert("quantity".into(), json!(m.quantity));
        }
        orders.push((signal.smart, Value::Object(o)));
        processed.push(symbol.to_string());
    }
    (orders, processed)
}
