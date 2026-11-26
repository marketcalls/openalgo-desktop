//! The one way every Pocketful REST call is sent (web `api/baseurl.py`).
//!
//! * Host: `https://algo.pocketful.in` (the docs' base URL and the SDK's
//!   `_root_url`); tests point it at a local fake.
//! * Headers: the doc pages disagree (`Authorization: Bearer` on one,
//!   `x-authorization-token` with `x-device-type` on another), so both are
//!   sent, as the web does. An endpoint's documented extras (the charts and
//!   margin calculator's `clientid`) are added per request.
//! * Session: the access token is `AuthToken::raw()`; the trading client id
//!   every call carries is the session's user id (stored at sign-in). The
//!   web packs `<client_id>:::<access_token>` because its session row has
//!   no account; the desktop's session always carries it.
//! * Answers: `{"status": "success" | "error", ...}` and, on the newer
//!   endpoints (profile, margin), `{"status": true | false, ...}`.
//!   [`Answer::ok`] accepts either success form. A body that is not JSON
//!   keeps `body: None`, so a caller can tell "unreadable" from "refused".
//! * Lanes: data and account reads share one pacer at the web's 8 a second
//!   (`_MIN_INTERVAL` 0.12 s); places, modifies and cancels have their own
//!   (RATE-02). Market data repeats an HTTP 429 twice with the web's
//!   backoff (1 s, 2 s) and then refuses as busy; nothing else is ever sent
//!   twice.

use super::PocketfulBroker;
use crate::brokers::common::outcome;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use regex::Regex;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;

/// web `REQUEST_TIMEOUT`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// web `_HISTORY_TIMEOUT`.
pub const HISTORY_TIMEOUT: Duration = Duration::from_secs(30);
/// web `_MIN_INTERVAL`: about 8 data requests a second against the
/// documented 10.
pub const DATA_INTERVAL: Duration = Duration::from_millis(120);
/// web `_RATE_LIMIT_RETRIES`.
pub const RATE_LIMIT_RETRIES: u32 = 2;
/// web `_RATE_LIMIT_BACKOFF`, doubled per retry.
pub const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(1);

/// web `_RATE_LIMITED`.
pub const RATE_LIMITED: &str = "Pocketful is refusing more market data requests because its request limit was reached (10 a second, 5,000 an hour, 10,000 a day). Wait a few minutes and try again.";

/// Which pacer a request waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Market data and account reads.
    Data,
    /// Places, modifies and cancels (orders and GTT rules).
    Orders,
}

/// One request, relative to the REST host.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub query: Vec<(&'static str, String)>,
    pub json: Option<Value>,
    pub headers: Vec<(&'static str, String)>,
    pub timeout: Duration,
}

impl Request {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            json: None,
            headers: Vec::new(),
            timeout: REQUEST_TIMEOUT,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn query(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.query.push((key, value.into()));
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.json = Some(body);
        self
    }

    pub fn header(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((key, value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// What Pocketful answered (web `PocketfulResponse`).
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: StatusCode,
    /// The parsed body; `None` when it was not JSON.
    pub body: Option<Value>,
}

impl Answer {
    /// A 2xx whose body reports success in either envelope.
    pub fn ok(&self) -> bool {
        self.status.is_success()
            && self
                .body
                .as_ref()
                .and_then(|b| b.get("status"))
                .is_some_and(|s| s.as_str() == Some("success") || s.as_bool() == Some(true))
    }

    /// The `data` member, when the body is an object that has one.
    pub fn payload(&self) -> Option<&Value> {
        self.body.as_ref()?.get("data")
    }

    /// Pocketful's own message (for the log, and a refusal's reason).
    pub fn message(&self) -> String {
        match self.body.as_ref().and_then(|b| b.get("message")) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    /// The body carries Pocketful's explicit failure marker (`status:
    /// error` or `false`): a readable refusal, never a lost answer.
    pub fn failed(&self) -> bool {
        self.body
            .as_ref()
            .and_then(|b| b.get("status"))
            .is_some_and(|s| {
                s.as_bool() == Some(false)
                    || s.as_str().is_some_and(|t| {
                        matches!(
                            t.to_ascii_lowercase().as_str(),
                            "error" | "failure" | "failed"
                        )
                    })
            })
    }

    /// The annexure's "log in again" answers.
    pub fn session_refused(&self) -> bool {
        matches!(
            self.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        )
    }
}

/// "Error from backend: (500)-event id not found" -> "event id not found"
/// (web `gtt_data._BACKEND_PREFIX`).
static BACKEND_PREFIX: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*error from backend:\s*(\(\s*\d+\s*\)\s*-?\s*)?").ok());

/// Pocketful's own message ([`Answer::message`]) as a trader reads it:
/// without its backend code prefix, its first letter capitalised, or ""
/// (web `gtt_data.broker_reason`, which the web's GTT and margin calls
/// both use).
pub(super) fn broker_reason(message: &str) -> String {
    let stripped = match BACKEND_PREFIX.as_ref() {
        Some(re) => re.replace(message, "").into_owned(),
        None => message.to_string(),
    };
    let mut chars = stripped.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl PocketfulBroker {
    /// Send one request with the session headers and read the answer.
    /// A transport error comes back as a redacted [`AppError::Http`], so a
    /// placement can tell a request that never left from one whose answer
    /// was lost (`outcome::after_send`).
    pub async fn send(&self, lane: Lane, auth: &AuthToken, req: Request) -> Result<Answer> {
        let token = Self::access_token(auth)?;
        let pacer = match lane {
            Lane::Data => &self.pacer,
            Lane::Orders => &self.order_pacer,
        };
        pacer.acquire().await?;
        let mut builder = self
            .http
            .request(req.method, format!("{}{}", self.urls.rest, req.path))
            .timeout(req.timeout)
            .bearer_auth(token)
            .header("x-authorization-token", token)
            .header("x-device-type", "WEB")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        for (k, v) in &req.headers {
            builder = builder.header(*k, v);
        }
        if !req.query.is_empty() {
            builder = builder.query(&req.query);
        }
        if let Some(body) = &req.json {
            builder = builder.json(body);
        }
        let resp = builder.send().await.map_err(|e| super::redact(e.into()))?;
        read(resp).await
    }

    /// [`Self::send`] for market data: an HTTP 429 is repeated twice with
    /// the web's backoff, then refused as busy (web `_data_request`).
    pub async fn send_data(&self, auth: &AuthToken, req: Request) -> Result<Answer> {
        let mut attempt = 0u32;
        loop {
            let answer = self.send(Lane::Data, auth, req.clone()).await?;
            if answer.status != StatusCode::TOO_MANY_REQUESTS {
                return Ok(answer);
            }
            if attempt >= RATE_LIMIT_RETRIES {
                tracing::warn!(
                    "Pocketful {} still rate limited after {} retries",
                    req.path,
                    attempt
                );
                return Err(AppError::Busy(RATE_LIMITED.into()));
            }
            let delay = self.rate_limit_backoff * 2u32.pow(attempt);
            attempt += 1;
            tracing::warn!(
                "Pocketful {} rate limited; retry {} in {:?}",
                req.path,
                attempt,
                delay
            );
            tokio::time::sleep(delay).await;
        }
    }
}

/// The status and the body (parsed when it is JSON). A server error is
/// noted for the placement in progress (LOG-08).
async fn read(resp: reqwest::Response) -> Result<Answer> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    if status.is_server_error() {
        outcome::note_server_error();
    }
    let body = serde_json::from_slice::<Value>(&bytes).ok();
    if body.is_none() {
        tracing::warn!(
            status = status.as_u16(),
            "Pocketful answered with a body that is not JSON"
        );
    }
    Ok(Answer { status, body })
}
