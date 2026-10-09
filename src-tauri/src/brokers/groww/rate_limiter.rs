//! Groww pacing per API type and HTTP 429 retries (web
//! `api/rate_limiter.py`, #2194).
//!
//! Groww limits requests by API type, and APIs of one type share the limit
//! (Groww API docs, "Rate limits"):
//!
//! | Type | Calls | Limit |
//! | --- | --- | --- |
//! | Orders | create, modify, cancel | 250/min |
//! | Live Data | quote, LTP, OHLC | 300/min |
//! | Non Trading | order list/trades, positions, holdings, margin, funds | 500/min |
//! | Authentication | access token | 30/min |
//!
//! History is not in that table; bursts of about ten were refused with HTTP
//! 429, so it is paced at one request a second. Calls of one type are spaced
//! by the per-minute figure (which also keeps them under the per-second
//! one); types never wait for each other. The pacers are owned by the
//! broker, one `Instant` each, so nothing grows with traffic and no task is
//! spawned.
//!
//! Waiting is bounded (web `utils/broker_backpressure.py`): a call whose
//! turn is more than `MAX_QUEUE_WAIT` away is refused at once, before it
//! books a slot, so a burst cannot hold requests for minutes and a refused
//! call delays nobody behind it; an order sent long after the trader
//! decided is worse than a refusal they can retry.
//!
//! A 429 is retried after Groww's `Retry-After` (seconds or an HTTP date),
//! else after 1, 2 and 4 seconds, at most `MAX_RETRIES` times. Retrying a
//! refused order is safe: a 429 means the order was not taken, and Groww
//! rejects a repeated `order_reference_id`. A wait longer than
//! `MAX_RETRY_WAIT` is not slept: the call is refused with a message that
//! says it was not retried.

use crate::brokers::common::ratelimit::Pacer;
use crate::error::{AppError, Result};
use std::future::Future;
use std::time::{Duration, SystemTime};

/// Retries after the first 429 (web `MAX_RETRIES`).
pub const MAX_RETRIES: u32 = 3;
/// Backoff when Groww names no wait: 1, 2, 4 s (web `BASE_BACKOFF`).
pub const BASE_BACKOFF: Duration = Duration::from_secs(1);
/// Longest a call may wait for its turn (web
/// `BROKER_MAX_QUEUE_WAIT_SECONDS` and `BROKER_MAX_ORDER_QUEUE_WAIT_SECONDS`).
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(10);
/// Longest Groww-requested wait that is slept before a retry (web
/// `cap_server_delay`, the same ceiling).
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(10);

/// A call refused because its turn would come too late (web
/// `BROKER_BUSY_MESSAGE`).
pub const BUSY_MESSAGE: &str = "OpenAlgo is pacing requests to stay within Groww's rate limit, and this one would have had to wait too long for its turn. Try again in a few seconds.";
/// Groww asked for a wait longer than a request is held (web
/// `retry_delay`'s refusal).
pub const SLOW_DOWN_MESSAGE: &str = "Groww asked OpenAlgo to slow down for longer than a request can be held. This request was not retried. Try again in a minute.";

/// Groww API type of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiType {
    Order,
    Live,
    NonTrading,
    Auth,
    History,
}

impl ApiType {
    /// Seconds between two calls of this type: 60 / per-minute limit.
    pub fn min_interval(self) -> Duration {
        match self {
            ApiType::Order => Duration::from_secs_f64(60.0 / 250.0),
            ApiType::Live => Duration::from_secs_f64(60.0 / 300.0),
            ApiType::NonTrading => Duration::from_secs_f64(60.0 / 500.0),
            ApiType::Auth => Duration::from_secs_f64(60.0 / 30.0),
            ApiType::History => Duration::from_secs(1),
        }
    }
}

/// One pacer per API type.
#[derive(Debug)]
pub struct GrowwLimiter {
    order: Pacer,
    live: Pacer,
    non_trading: Pacer,
    auth: Pacer,
    history: Pacer,
}

impl Default for GrowwLimiter {
    fn default() -> Self {
        let p = |t: ApiType| Pacer::with_interval(t.min_interval());
        Self {
            order: p(ApiType::Order),
            live: p(ApiType::Live),
            non_trading: p(ApiType::NonTrading),
            auth: p(ApiType::Auth),
            history: p(ApiType::History),
        }
    }
}

impl GrowwLimiter {
    fn pacer(&self, t: ApiType) -> &Pacer {
        match t {
            ApiType::Order => &self.order,
            ApiType::Live => &self.live,
            ApiType::NonTrading => &self.non_trading,
            ApiType::Auth => &self.auth,
            ApiType::History => &self.history,
        }
    }

    /// Wait for the next slot of `t` (web `apply_rate_limit`), or refuse
    /// at once, booking nothing, when that slot is more than
    /// `MAX_QUEUE_WAIT` away (web `check_queue_wait`).
    pub async fn acquire(&self, t: ApiType) -> Result<()> {
        self.pacer(t)
            .acquire_within(MAX_QUEUE_WAIT)
            .await
            .map_err(|wait| {
                tracing::warn!(
                    "Groww {:?} call refused before sending: its turn is {:?} away",
                    t,
                    wait
                );
                AppError::Broker(BUSY_MESSAGE.into())
            })
    }
}

/// How long to wait before retry `attempt` (0-based) of a 429: Groww's
/// `Retry-After` in seconds or as an HTTP date (at least 50 ms), else 1, 2,
/// 4 seconds (web `retry_delay`). A value too large to hold is
/// `Duration::MAX`, which the caller refuses rather than sleeps.
pub fn retry_delay(retry_after: Option<&str>, attempt: u32, now: SystemTime) -> Duration {
    let fallback = BASE_BACKOFF.saturating_mul(1u32 << attempt.min(16));
    let Some(v) = retry_after.map(str::trim).filter(|v| !v.is_empty()) else {
        return fallback;
    };
    let floor = Duration::from_millis(50);
    if let Ok(secs) = v.parse::<f64>() {
        if secs.is_nan() {
            return fallback;
        }
        return Duration::try_from_secs_f64(secs.max(0.0))
            .unwrap_or(Duration::MAX)
            .max(floor);
    }
    match chrono::DateTime::parse_from_rfc2822(v) {
        Ok(when) => {
            let Ok(ms) = u64::try_from(when.timestamp_millis()) else {
                return floor;
            };
            let when = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
            when.duration_since(now)
                .unwrap_or(Duration::ZERO)
                .max(floor)
        }
        Err(_) => fallback,
    }
}

/// What one attempt of a call returned: its HTTP status, Groww's
/// `Retry-After` and the value handed back to the caller.
pub struct Attempt<R> {
    pub status: u16,
    pub retry_after: Option<String>,
    pub value: R,
}

/// One paced Groww call, retried while Groww answers 429 (web
/// `groww_request`). Returns the last answer, a 429 only when every retry
/// was refused. Refused with a trader-facing error, before anything is
/// sent, when the call's turn is too far away, and without a retry when
/// Groww asks for a wait longer than `MAX_RETRY_WAIT`.
pub async fn paced<R, F, Fut>(limiter: &GrowwLimiter, t: ApiType, mut call: F) -> Result<R>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Attempt<R>>>,
{
    let mut attempt = 0u32;
    loop {
        limiter.acquire(t).await?;
        let a = call().await?;
        if a.status != 429 || attempt >= MAX_RETRIES {
            return Ok(a.value);
        }
        let wait = retry_delay(a.retry_after.as_deref(), attempt, SystemTime::now());
        if wait > MAX_RETRY_WAIT {
            tracing::warn!(
                "Groww asked to wait {:?} before retrying a {:?} call; not retried",
                wait,
                t
            );
            return Err(AppError::Broker(SLOW_DOWN_MESSAGE.into()));
        }
        tracing::warn!(
            "Groww is limiting {:?} requests; retry {} in {:?}",
            t,
            attempt + 1,
            wait
        );
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}
