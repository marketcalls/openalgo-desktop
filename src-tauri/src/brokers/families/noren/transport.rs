//! Noren REST transport: the two body dialects, the `stat`/`emsg` envelope,
//! session-error classification and per-category rolling-window pacing.

use super::{Dialect, NorenBroker, NorenConfig, Window};
use crate::brokers::common::http;
use crate::brokers::common::outcome;
use crate::brokers::common::ratelimit::{Budget, Limits, Rolling, BUSY_MESSAGE, MAX_QUEUE_WAIT};
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use serde_json::Value;
use tokio::time::Instant;

/// `emsg` fragments that mean the login is no longer valid (web
/// `broker/shoonya/api/data.py` `SESSION_ERROR_MARKERS`).
pub const SESSION_ERROR_MARKERS: &[&str] = &[
    "session expired",
    "invalid session",
    "session key",
    "not logged in",
    "invalid input : uid",
];

/// `emsg` fragments that mean an empty answer, not a failure.
pub const NO_DATA_MARKERS: &[&str] = &["no data"];

pub fn is_session_error(emsg: &str) -> bool {
    let t = emsg.to_ascii_lowercase();
    SESSION_ERROR_MARKERS.iter().any(|m| t.contains(m))
}

pub fn is_no_data(emsg: &str) -> bool {
    let t = emsg.to_ascii_lowercase();
    NO_DATA_MARKERS.iter().any(|m| t.contains(m))
}

/// A trader-facing error for a Noren `emsg`.
pub fn noren_error(name: &str, emsg: &str) -> AppError {
    let lower = emsg.to_ascii_lowercase();
    if is_session_error(emsg) {
        return session_expired(name);
    }
    if lower.contains("exceeds limit") || lower.contains("rate limit") {
        return AppError::Broker(format!(
            "{} is limiting requests right now. Wait a moment and try again.",
            name
        ));
    }
    let m = emsg.trim();
    if m.is_empty() {
        AppError::Broker(format!("{} refused the request.", name))
    } else {
        AppError::Broker(m.to_string())
    }
}

pub fn session_expired(name: &str) -> AppError {
    AppError::Auth(format!(
        "Your {} session has expired. Log in to {} again.",
        name, name
    ))
}

/// The stored session: trading user id and access token.
#[derive(Clone)]
pub struct Session {
    pub uid: String,
    pub token: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("uid", &self.uid)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Split the stored `uid:::token` (or a bare token plus the session's user
/// id).
pub fn session(cfg: &NorenConfig, auth: &AuthToken) -> Result<Session> {
    let raw = auth.raw();
    let (uid, token) = match raw.split_once(":::") {
        Some((u, t)) => (u.to_string(), t.to_string()),
        None => (
            auth.user_id().unwrap_or_default().to_string(),
            raw.to_string(),
        ),
    };
    if uid.trim().is_empty() || token.trim().is_empty() {
        return Err(session_expired(cfg.name));
    }
    Ok(Session { uid, token })
}

/// `jData` JSON with `&` escaped as `&`: Noren splits the body on `&`
/// and never URL-decodes it (web `_encode_jdata`).
pub fn encode_jdata(v: &Value) -> String {
    v.to_string().replace('&', "\\u0026")
}

/// Request body and content type for a dialect.
pub fn body(dialect: Dialect, jdata: &Value, token: &str) -> (String, &'static str) {
    match dialect {
        Dialect::BearerJData => (format!("jData={}", encode_jdata(jdata)), "text/plain"),
        Dialect::JKeyForm => (
            format!("jData={}&jKey={}", encode_jdata(jdata), token),
            "application/x-www-form-urlencoded",
        ),
    }
}

/// Pacing category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Order,
    Data,
    Quote,
}

/// Rolling-window limiter: at most `per_second` sends in any second and
/// `per_minute` in any minute, one `common::ratelimit::Budget`. A send is
/// claimed at the moment it may go, so a caller dropped while it waits
/// books nothing, and a caller waits at most `ratelimit::MAX_QUEUE_WAIT`:
/// past it the request is refused before sending, a definite refusal inside
/// an order placement (BRL-05; it used to wait as long as the minute window
/// needed). `None` is unpaced. State is the sends of the last minute,
/// bounded by `per_minute`.
#[derive(Debug)]
pub struct Limiter {
    budget: Option<Budget>,
}

impl Limiter {
    pub fn new(window: Option<Window>) -> Self {
        Self {
            budget: window.map(|w| {
                Budget::new(Limits {
                    windows: vec![
                        Rolling::per_second(w.per_second.max(1)),
                        Rolling::per_minute(w.per_minute.max(1)),
                    ],
                    per_day: None,
                })
            }),
        }
    }

    /// Wait for a send slot, at most `MAX_QUEUE_WAIT`, and claim it.
    pub async fn acquire(&self) -> Result<()> {
        self.acquire_until(Instant::now() + MAX_QUEUE_WAIT).await
    }

    /// Wait for a send slot until `deadline`, and claim it; refused before
    /// sending when the slot comes later.
    pub async fn acquire_until(&self, deadline: Instant) -> Result<()> {
        let Some(budget) = &self.budget else {
            return Ok(());
        };
        budget.acquire(deadline).await.map_err(|d| {
            tracing::warn!("A broker request was refused before sending: {:?}", d);
            outcome::busy(BUSY_MESSAGE)
        })
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.budget.as_ref().map_or(0, Budget::recent)
    }
}

impl NorenBroker {
    fn limiter(&self, c: Category) -> &Limiter {
        match c {
            Category::Order => &self.order_limiter,
            Category::Data => &self.data_limiter,
            Category::Quote => &self.quote_limiter,
        }
    }

    /// POST one endpoint (`/PlaceOrder`) and return the JSON answer,
    /// whatever its `stat`. `uid` is added to `jdata`.
    pub(crate) async fn post_raw(
        &self,
        endpoint: &str,
        mut jdata: Value,
        s: &Session,
        cat: Category,
    ) -> Result<Value> {
        if let Some(o) = jdata.as_object_mut() {
            o.entry("uid")
                .or_insert_with(|| Value::String(s.uid.clone()));
        }
        let dialect = match endpoint {
            "/TPSeries" | "/EODChartData" => self.cfg.chart_dialect.unwrap_or(self.cfg.dialect),
            _ => self.cfg.dialect,
        };
        let (payload, content_type) = body(dialect, &jdata, &s.token);
        self.limiter(cat).acquire().await?;
        let url = format!("{}{}", self.endpoints.rest, endpoint);
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", content_type)
            .body(payload);
        if dialect == Dialect::BearerJData {
            req = req.header("Authorization", format!("Bearer {}", s.token));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let (_, v): (_, Value) = http::read_json(self.cfg.id, resp).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(session_expired(self.cfg.name));
        }
        if status.is_server_error() {
            return Err(http::server_error(self.cfg.id, status));
        }
        Ok(v)
    }

    /// POST a book endpoint: a list on success, `[]` for `no data`.
    pub(crate) async fn post_list(
        &self,
        endpoint: &str,
        jdata: Value,
        s: &Session,
        cat: Category,
    ) -> Result<Vec<Value>> {
        let v = self.post_raw(endpoint, jdata, s, cat).await?;
        as_list(self.cfg.name, endpoint, v)
    }
}

/// `stat == Ok` or a trader-facing error.
pub fn check(name: &str, endpoint: &str, v: Value) -> Result<Value> {
    if stat_ok(&v) {
        return Ok(v);
    }
    let emsg = emsg(&v);
    tracing::warn!(broker = name, "{} refused {}: {}", name, endpoint, emsg);
    Err(noren_error(name, &emsg))
}

/// A list answer, `[]` for `no data` (Noren's empty book), an error
/// otherwise: a `null` answer is a book that was not read (BR-03).
pub fn as_list(name: &str, endpoint: &str, v: Value) -> Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a),
        Value::Null => {
            tracing::warn!(broker = name, "{} answered {} with null", name, endpoint);
            Err(AppError::Broker(format!(
                "OpenAlgo could not read the answer from {}. Try again shortly.",
                name
            )))
        }
        other => {
            let e = emsg(&other);
            if is_no_data(&e) {
                return Ok(Vec::new());
            }
            tracing::warn!(
                broker = name,
                "{} refused {}: {}",
                name,
                endpoint,
                crate::brokers::common::redact::url_safe_error(&e)
            );
            Err(noren_error(name, &e))
        }
    }
}

pub fn stat_ok(v: &Value) -> bool {
    v.get("stat").and_then(Value::as_str) == Some("Ok")
}

pub fn emsg(v: &Value) -> String {
    v.get("emsg")
        .or_else(|| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn dialect_bodies() {
        let j = json!({"uid":"U1","tsym":"M&M-EQ"});
        let (b, ct) = body(Dialect::BearerJData, &j, "tok");
        assert_eq!(ct, "text/plain");
        assert_eq!(b, "jData={\"tsym\":\"M\\u0026M-EQ\",\"uid\":\"U1\"}");
        let (b, ct) = body(Dialect::JKeyForm, &j, "tok");
        assert_eq!(ct, "application/x-www-form-urlencoded");
        assert!(b.ends_with("&jKey=tok"));
        assert_eq!(b.matches('&').count(), 1);
    }

    #[test]
    fn error_classification() {
        assert!(is_session_error("Session Expired :  Invalid Session Key"));
        assert!(is_session_error("Invalid Input : uid"));
        assert!(!is_session_error("Invalid Input : Trigger price invalid"));
        assert!(is_no_data("no data"));
        let e = noren_error("Shoonya", "Session Expired : Invalid Session Key");
        assert!(matches!(e, AppError::Auth(_)));
        let e = noren_error(
            "Flattrade",
            "Request exceeds Limit 9 for user per current second",
        );
        assert!(e.client_message().contains("limiting requests"));
        let e = noren_error("Zebu", " RMS:Margin Exceeds ");
        assert_eq!(e.client_message(), "RMS:Margin Exceeds");
    }

    #[test]
    fn lists_and_envelopes() {
        assert!(
            as_list("X", "/OrderBook", json!({"stat":"Not_Ok","emsg":"no data"}))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            as_list("X", "/OrderBook", json!([{"a":1}])).unwrap().len(),
            1
        );
        assert!(as_list(
            "X",
            "/OrderBook",
            json!({"stat":"Not_Ok","emsg":"Session Expired"})
        )
        .is_err());
        assert!(as_list("X", "/OrderBook", Value::Null).is_err());
        assert!(check("X", "/Limits", json!({"stat":"Ok"})).is_ok());
        let e = check("X", "/Limits", json!({"stat":"Not_Ok","emsg":"bad"})).unwrap_err();
        assert_eq!(e.client_message(), "bad");
    }

    #[test]
    fn session_from_composite_or_user_id() {
        let cfg = crate::brokers::shoonya::config();
        let s = session(cfg, &AuthToken::new("FA1:::tok")).unwrap();
        assert_eq!((s.uid.as_str(), s.token.as_str()), ("FA1", "tok"));
        let s = session(cfg, &AuthToken::new("sekrit").with_user_id("FA2")).unwrap();
        assert_eq!(s.uid, "FA2");
        assert!(session(cfg, &AuthToken::new("tok")).is_err());
        assert!(!format!("{:?}", s).contains("sekrit"));
    }

    /// BRL-06: Flattrade quotes count against the same budget as books,
    /// history and funds (web one DATA_LIMITER): nine sends a second in
    /// all, so the tenth, a quote after nine data calls, waits a second.
    /// They were two copies of the setting, so quotes added another nine.
    #[tokio::test(start_paused = true)]
    async fn flattrade_quotes_share_the_data_budget() {
        let b = crate::brokers::flattrade::broker(
            crate::brokers::common::symbols::SymbolResolver::new(),
        );
        let start = Instant::now();
        for _ in 0..9 {
            b.limiter(Category::Data).acquire().await.unwrap();
        }
        b.limiter(Category::Quote).acquire().await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        // Orders keep their own lane.
        let t = Instant::now();
        b.limiter(Category::Order).acquire().await.unwrap();
        assert_eq!(t.elapsed(), Duration::ZERO);
    }

    /// BRL-07: TradeSmart orders, history, books and funds share one
    /// non-quote budget (web); quotes keep their own.
    #[tokio::test(start_paused = true)]
    async fn tradesmart_orders_share_the_non_quote_budget() {
        let b = crate::brokers::tradesmart::broker(
            crate::brokers::common::symbols::SymbolResolver::new(),
        );
        let start = Instant::now();
        for _ in 0..8 {
            b.limiter(Category::Data).acquire().await.unwrap();
        }
        let q = Instant::now();
        b.limiter(Category::Quote).acquire().await.unwrap();
        assert_eq!(q.elapsed(), Duration::ZERO, "quotes have their own budget");
        b.limiter(Category::Order).acquire().await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert!(
            b.cfg.rate.order.is_none(),
            "a shared order budget names no own window"
        );
    }

    /// Both windows hold, and a send the minute window would hold past
    /// the queue bound is refused at once, before sending, as a definite
    /// refusal (BRL-05): it used to wait the 59 seconds. One that may wait
    /// longer goes when the minute frees.
    #[tokio::test(start_paused = true)]
    async fn limiter_respects_both_windows_and_refuses_a_long_wait() {
        let l = Limiter::new(Some(Window {
            per_second: 3,
            per_minute: 5,
        }));
        let start = Instant::now();
        for _ in 0..3 {
            l.acquire().await.unwrap();
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        l.acquire().await.unwrap(); // 4th waits for the second to roll
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        // The fifth fits the minute.
        l.acquire().await.unwrap();
        // The sixth is 59 s away: refused at once, and inside an order
        // placement as a definite refusal (never sent).
        let e = outcome::placing(l.acquire()).await.unwrap_err();
        assert!(matches!(e, AppError::Busy(_)), "{:?}", e);
        assert_eq!(e.client_message(), BUSY_MESSAGE);
        assert_eq!(start.elapsed(), Duration::from_secs(1), "refused at once");
        assert_eq!(l.held(), 5, "the refusal booked nothing");
        l.acquire_until(start + Duration::from_secs(120))
            .await
            .unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(60));
        assert!(l.held() <= 5);
        let free = Limiter::new(None);
        free.acquire().await.unwrap();
        assert_eq!(free.held(), 0);
    }

    /// A caller dropped while it waits for its slot books nothing: the next
    /// caller goes at the slot the dropped one was waiting for.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_waiter_books_nothing() {
        let l = Limiter::new(Some(Window {
            per_second: 1,
            per_minute: 100,
        }));
        let start = Instant::now();
        l.acquire().await.unwrap();
        let gave_up = tokio::time::timeout(Duration::from_millis(100), l.acquire()).await;
        assert!(gave_up.is_err());
        l.acquire().await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(l.held(), 2);
    }
}
