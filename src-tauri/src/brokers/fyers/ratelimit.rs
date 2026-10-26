//! Fyers request budgets per API category and plan (BRL-01, BRL-03,
//! BRL-04, BRL-05).
//!
//! Fyers publishes limits per API category, every window applying at once
//! (FYERS support, "What are the FYERS API rate limits for Standard and
//! Prime plans", checked 2026-10-12):
//!
//! | Category | Standard | Prime |
//! | --- | --- | --- |
//! | Data (quotes, depth, option chain, history) | 5/s, 50/min, 5,000/day | 10/s, 500/min, 500,000/day |
//! | Transaction (orders, positions, trades, GTT, margin) | 10/s, 200/min, 100,000/day | 10/s, 500/min, 500,000/day |
//! | User info (profile, funds, holdings) | 10/s, 200/min, 100,000/day | 10/s, 500/min, 500,000/day |
//!
//! Each category has its own [`Budget`], so a burst of data requests never
//! queues an order behind it, and every send also passes the earlier
//! app-wide ceiling of 10 a second (kept as a ceiling, it costs nothing
//! the categories allow). Without a verified plan the Standard limits
//! apply. Daily counts are saved per category and day
//! (`request_budget_service`), so a restart keeps the day's count; Fyers'
//! own throttle answers stay authoritative.
//!
//! The web (`api/rate_limiter.py`) paces one lane at 8 a second with no
//! category, minute or day accounting; this is a recorded deviation.

use crate::brokers::common::outcome;
use crate::brokers::common::ratelimit::{Budget, DailyCount, Denied, Limits, Rolling};
use crate::error::AppError;
use reqwest::Method;
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

/// The trader's Fyers API plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FyersPlan {
    #[default]
    Standard,
    Prime,
}

impl FyersPlan {
    pub fn label(self) -> &'static str {
        match self {
            FyersPlan::Standard => "Standard",
            FyersPlan::Prime => "Prime",
        }
    }

    /// `standard` / `prime`, as stored; anything else is Standard.
    pub fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("prime") {
            FyersPlan::Prime
        } else {
            FyersPlan::Standard
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            FyersPlan::Standard => "standard",
            FyersPlan::Prime => "prime",
        }
    }
}

/// The plans Broker Configuration offers, Standard (the default) first.
pub const PLANS: &[crate::brokers::RatePlan] = &[
    crate::brokers::RatePlan {
        id: "standard",
        label: "Standard",
        hint: "Market data: 5 requests a second, 50 a minute, 5,000 a day. Orders and account: 10 a second, 200 a minute, 100,000 a day.",
    },
    crate::brokers::RatePlan {
        id: "prime",
        label: "Prime",
        hint: "Market data: 10 requests a second, 500 a minute, 500,000 a day. Orders and account: 10 a second, 500 a minute, 500,000 a day. Choose it only if your Fyers API subscription is Prime.",
    },
];

/// Fyers API category of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Data,
    Transaction,
    UserInfo,
}

/// Every category, in the order the daily counts are saved.
const CATEGORIES: [Category; 3] = [Category::Data, Category::Transaction, Category::UserInfo];

impl Category {
    /// The name its daily count is saved under.
    fn id(self) -> &'static str {
        match self {
            Category::Data => "data",
            Category::Transaction => "transaction",
            Category::UserInfo => "user_info",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Category::Data => "market data",
            Category::Transaction => "order",
            Category::UserInfo => "account",
        }
    }

    /// The category of a REST call by its path: `/data/...` is market
    /// data; profile, funds, holdings, the login exchange and the socket
    /// address lookup are user info; every other `/api/v3` call (orders,
    /// positions, trades, GTT, margin) is a transaction.
    pub fn of(method: &Method, path_and_query: &str) -> Self {
        let path = path_and_query.split('?').next().unwrap_or("");
        if path.starts_with("/data/") {
            return Category::Data;
        }
        let user = ["/api/v3/profile", "/api/v3/funds", "/api/v3/holdings"];
        if path.starts_with("/indus/")
            || path == "/api/v3/validate-authcode"
            || (method == Method::GET && user.contains(&path))
        {
            return Category::UserInfo;
        }
        Category::Transaction
    }
}

/// The published limits of one category on one plan.
pub fn limits(plan: FyersPlan, category: Category) -> Limits {
    let (per_second, per_minute, per_day) = match (plan, category) {
        (FyersPlan::Standard, Category::Data) => (5, 50, 5_000),
        (FyersPlan::Standard, _) => (10, 200, 100_000),
        (FyersPlan::Prime, Category::Data) => (10, 500, 500_000),
        (FyersPlan::Prime, _) => (10, 500, 500_000),
    };
    Limits {
        windows: vec![
            Rolling::per_second(per_second),
            Rolling::per_minute(per_minute),
        ],
        per_day: Some(per_day),
    }
}

/// The app-wide ceiling every send also passes.
pub fn app_ceiling() -> Limits {
    Limits {
        windows: vec![Rolling::per_second(10)],
        per_day: None,
    }
}

/// Longest a single request waits for its turn before it is refused
/// (web `BROKER_MAX_QUEUE_WAIT_SECONDS`): a quote or an order sent long
/// after it was asked for is worse than a refusal the trader can retry.
pub const MAX_QUEUE_WAIT: Duration = Duration::from_secs(10);
/// Throttle answers retried within one logical request (web
/// `MAX_RETRIES`), shared by every send of that request.
pub const MAX_RETRIES: u32 = 3;

/// One logical request: a deadline for all its waiting and sending, and
/// the throttle retries it may still spend (BRL-04: one budget per
/// operation, however many sends or chunks it takes).
#[derive(Debug, Clone)]
pub struct Op {
    pub deadline: Instant,
    pub retries_left: u32,
}

impl Op {
    /// A single request: [`MAX_QUEUE_WAIT`] and [`MAX_RETRIES`].
    pub fn single() -> Self {
        Self::within(MAX_QUEUE_WAIT)
    }

    /// A request of several sends (a history in chunks) given `total` time.
    pub fn within(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
            retries_left: MAX_RETRIES,
        }
    }
}

/// The budgets of one Fyers app.
#[derive(Debug)]
pub struct FyersLimiter {
    plan: parking_lot::Mutex<FyersPlan>,
    data: Budget,
    transaction: Budget,
    user: Budget,
    app: Budget,
}

impl FyersLimiter {
    pub fn new(plan: FyersPlan) -> Self {
        Self {
            plan: parking_lot::Mutex::new(plan),
            data: Budget::new(limits(plan, Category::Data)),
            transaction: Budget::new(limits(plan, Category::Transaction)),
            user: Budget::new(limits(plan, Category::UserInfo)),
            app: Budget::new(app_ceiling()),
        }
    }

    /// Budgets with other limits (tests).
    pub fn with_limits(data: Budget, transaction: Budget, user: Budget, app: Budget) -> Self {
        Self {
            plan: parking_lot::Mutex::new(FyersPlan::Standard),
            data,
            transaction,
            user,
            app,
        }
    }

    pub fn plan(&self) -> FyersPlan {
        *self.plan.lock()
    }

    /// Switch plan, keeping what was sent today.
    pub fn set_plan(&self, plan: FyersPlan) {
        *self.plan.lock() = plan;
        for c in CATEGORIES {
            self.budget(c).set_limits(limits(plan, c));
        }
    }

    /// Today's count of each category with a daily cap.
    pub fn daily_counts(&self) -> Vec<DailyCount> {
        CATEGORIES
            .iter()
            .map(|c| {
                let (day, sent) = self.budget(*c).day_count();
                DailyCount {
                    category: c.id().to_string(),
                    day,
                    sent,
                }
            })
            .collect()
    }

    /// Count what was saved earlier today as already sent.
    pub fn restore_daily_counts(&self, counts: &[DailyCount]) {
        for saved in counts {
            if let Some(c) = CATEGORIES.iter().find(|c| c.id() == saved.category) {
                self.budget(*c).restore_day(saved.day, saved.sent);
            }
        }
    }

    pub fn budget(&self, c: Category) -> &Budget {
        match c {
            Category::Data => &self.data,
            Category::Transaction => &self.transaction,
            Category::UserInfo => &self.user,
        }
    }

    /// Claim a send of category `c` within `op`'s deadline, or refuse it
    /// before anything is sent (a refusal inside an order placement: the
    /// order was not sent).
    pub async fn acquire(&self, c: Category, op: &Op) -> Result<(), AppError> {
        match self
            .budget(c)
            .acquire_with(Some(&self.app), op.deadline)
            .await
        {
            Ok(()) => Ok(()),
            Err(d) => {
                let plan = self.plan();
                tracing::warn!(
                    "Fyers {:?} request refused before sending ({:?}, {} plan)",
                    c,
                    d,
                    plan.label()
                );
                let message = denied_message(plan, c, d);
                // A turn too far away is the web's busy refusal (429); the
                // daily cap, which the web does not have, stays a refusal.
                Err(match d {
                    Denied::Wait(_) => outcome::busy(message),
                    Denied::Daily(_) => outcome::refusal(message),
                })
            }
        }
    }

    /// Fyers answered with a throttle: hold every request of `c` until
    /// `until` (BRL-03), so other callers do not keep sending meanwhile.
    pub fn cool_down(&self, c: Category, until: Instant) {
        self.budget(c).cool_down(until);
    }
}

/// What the trader reads when a request is refused before sending.
pub fn denied_message(plan: FyersPlan, c: Category, d: Denied) -> String {
    match d {
        Denied::Daily(cap) => format!(
            "Fyers' daily limit of {} {} requests on the {} plan is used up for today, so OpenAlgo has stopped sending them. It resets at midnight.",
            group_thousands(cap),
            c.noun(),
            plan.label()
        ),
        Denied::Wait(w) => format!(
            "OpenAlgo is pacing {} requests to stay within Fyers' limits on the {} plan, and this one would have had to wait about {} seconds for its turn. Try again shortly.",
            c.noun(),
            plan.label(),
            w.as_secs().max(1)
        ),
    }
}

/// Fyers asked for a pause longer than the request can be held.
pub const SLOW_DOWN_MESSAGE: &str = "Fyers asked OpenAlgo to slow down for longer than this request can be held. It was not retried. Try again in a minute.";

fn group_thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// How long Fyers asked to wait before retry `attempt` (0-based):
/// `X-Retry-After-Ms`, else `Retry-After` in seconds or as an HTTP date,
/// else `base * 2^attempt`. Never shortened (BRL-03): the caller sleeps it
/// within its deadline or refuses. A value too large to hold is
/// `Duration::MAX`.
pub fn retry_delay(
    headers: &reqwest::header::HeaderMap,
    attempt: u32,
    base: Duration,
    now: SystemTime,
) -> Duration {
    let floor = Duration::from_millis(50);
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let secs = |v: f64| Duration::try_from_secs_f64(v.max(0.0)).unwrap_or(Duration::MAX);
    if let Some(ms) = header("x-retry-after-ms").and_then(|v| v.parse::<f64>().ok()) {
        if ms.is_finite() {
            return secs(ms / 1000.0).max(floor);
        }
    }
    if let Some(v) = header("retry-after") {
        if let Ok(s) = v.parse::<f64>() {
            if s.is_finite() {
                return secs(s).max(floor);
            }
        } else if let Ok(when) = chrono::DateTime::parse_from_rfc2822(v) {
            return match u64::try_from(when.timestamp_millis()) {
                Ok(ms) => (SystemTime::UNIX_EPOCH + Duration::from_millis(ms))
                    .duration_since(now)
                    .unwrap_or(Duration::ZERO)
                    .max(floor),
                Err(_) => floor,
            };
        }
    }
    base.saturating_mul(1u32 << attempt.min(8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn categories_by_path() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(
            Category::of(&get, "/data/quotes?symbols=NSE:SBIN-EQ"),
            Category::Data
        );
        assert_eq!(
            Category::of(&get, "/data/options-chain-v3?symbol=x"),
            Category::Data
        );
        assert_eq!(Category::of(&get, "/data/history?symbol=x"), Category::Data);
        assert_eq!(Category::of(&get, "/api/v3/funds"), Category::UserInfo);
        assert_eq!(Category::of(&get, "/api/v3/holdings"), Category::UserInfo);
        assert_eq!(Category::of(&get, "/api/v3/profile"), Category::UserInfo);
        assert_eq!(Category::of(&get, "/indus/home/tbtws"), Category::UserInfo);
        assert_eq!(
            Category::of(&post, "/api/v3/validate-authcode"),
            Category::UserInfo
        );
        assert_eq!(
            Category::of(&post, "/api/v3/orders/sync"),
            Category::Transaction
        );
        assert_eq!(Category::of(&get, "/api/v3/orders"), Category::Transaction);
        assert_eq!(
            Category::of(&get, "/api/v3/positions"),
            Category::Transaction
        );
        assert_eq!(
            Category::of(&get, "/api/v3/tradebook"),
            Category::Transaction
        );
        assert_eq!(
            Category::of(&post, "/api/v3/multiorder/margin"),
            Category::Transaction
        );
    }

    #[test]
    fn published_limits_per_plan() {
        let s = limits(FyersPlan::Standard, Category::Data);
        assert_eq!(
            s.windows,
            vec![Rolling::per_second(5), Rolling::per_minute(50)]
        );
        assert_eq!(s.per_day, Some(5_000));
        let p = limits(FyersPlan::Prime, Category::Data);
        assert_eq!(
            p.windows,
            vec![Rolling::per_second(10), Rolling::per_minute(500)]
        );
        assert_eq!(p.per_day, Some(500_000));
        let t = limits(FyersPlan::Standard, Category::Transaction);
        assert_eq!(t.per_day, Some(100_000));
        assert_eq!(FyersPlan::parse("PRIME"), FyersPlan::Prime);
        assert_eq!(FyersPlan::parse(""), FyersPlan::Standard);
        assert_eq!(FyersPlan::parse("gold"), FyersPlan::Standard);
    }

    #[test]
    fn messages_name_the_plan_and_the_limit() {
        assert_eq!(
            denied_message(FyersPlan::Standard, Category::Data, Denied::Daily(5_000)),
            "Fyers' daily limit of 5,000 market data requests on the Standard plan is used up for today, so OpenAlgo has stopped sending them. It resets at midnight."
        );
        assert_eq!(group_thousands(500_000), "500,000");
        assert_eq!(group_thousands(50), "50");
        assert!(denied_message(
            FyersPlan::Prime,
            Category::Transaction,
            Denied::Wait(Duration::from_secs(42))
        )
        .contains("about 42 seconds"));
    }

    /// A broker-requested wait is read in full, never capped (BRL-03).
    #[test]
    fn retry_delay_honours_every_header_form() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let base = Duration::from_secs(1);
        let h = |k: &'static str, v: &str| {
            let mut m = HeaderMap::new();
            m.insert(k, HeaderValue::from_str(v).unwrap());
            m
        };
        assert_eq!(
            retry_delay(&h("retry-after", "60"), 0, base, now),
            Duration::from_secs(60)
        );
        assert_eq!(
            retry_delay(&h("x-retry-after-ms", "1500"), 0, base, now),
            Duration::from_millis(1500)
        );
        assert_eq!(
            retry_delay(&h("x-retry-after-ms", "1"), 0, base, now),
            Duration::from_millis(50)
        );
        let date = chrono::DateTime::from_timestamp(1_760_000_030, 0)
            .unwrap()
            .to_rfc2822();
        assert_eq!(
            retry_delay(&h("retry-after", &date), 0, base, now),
            Duration::from_secs(30)
        );
        assert_eq!(
            retry_delay(&h("retry-after", "1e30"), 0, base, now),
            Duration::MAX
        );
        assert_eq!(
            retry_delay(&h("retry-after", "soon"), 2, base, now),
            Duration::from_secs(4)
        );
        assert_eq!(
            retry_delay(&HeaderMap::new(), 1, base, now),
            Duration::from_secs(2)
        );
    }
}
