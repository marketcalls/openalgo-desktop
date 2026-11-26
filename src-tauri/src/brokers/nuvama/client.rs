//! The one request path for authenticated API Connect calls (web
//! `api/baseurl.py` `nuvama_request` and `NuvamaResponse`).
//!
//! Nuvama answers a refusal or an empty book with HTTP 222 and an `error`
//! object (`{"error": {"actCd", "errCd", "errMsg"}}`), so the status alone
//! does not say a call worked: [`Answer::ok`] needs a 200 whose body carries
//! no error. A refused session (`EGN0011` "Session Expired", or a 401/403)
//! is an [`AppError::Auth`] the trader answers by logging in again.

use super::session::NuvamaSession;
use super::NuvamaBroker;
use crate::brokers::common::{http, python, redact};
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;

/// Nuvama's "nothing in this book" codes (web `_EMPTY_BOOK_CODES`):
/// ETRD0007 orders, ETRD0002 trades, ETRD0004 positions. As on the web,
/// any of them reads as empty on any book. Without ETRD0007 a flat
/// account's order book was a refusal (web openalgo#2479).
pub(crate) const EMPTY_BOOK_CODES: &[&str] = &["ETRD0002", "ETRD0004", "ETRD0007"];
/// Nuvama's code for a session it no longer accepts.
const SESSION_EXPIRED_CODES: &[&str] = &["EGN0011"];
/// The status Nuvama answers refusals and empty books with.
pub(crate) const NUVAMA_REFUSAL_STATUS: u16 = 222;

/// Which pacing lane a call takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    /// Quotes, depth and history (web `_pace`, 8 a second).
    Data,
    /// Place, modify and cancel (RATE-03, `ORDERS_PER_SECOND`).
    Orders,
    /// Books and funds: unpaced, as on the web.
    Account,
}

/// What Nuvama answered: the HTTP status and the JSON body.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub status: u16,
    pub body: Value,
}

impl Answer {
    /// A 200 whose body is not an error envelope (web `NuvamaResponse.ok`).
    pub fn ok(&self) -> bool {
        self.status == 200 && self.body.is_object() && !has_error(&self.body)
    }

    /// The `errCd` of an error envelope.
    pub fn error_code(&self) -> Option<&str> {
        self.body
            .get("error")
            .and_then(|e| e.get("errCd"))
            .and_then(Value::as_str)
    }

    /// Nuvama's answer for an empty book: its "nothing here" code on a
    /// status that is not a server error. The web takes the code on any
    /// status; a 5xx says nothing about the book.
    pub fn empty_book(&self) -> bool {
        self.status < 500
            && self
                .error_code()
                .is_some_and(|c| EMPTY_BOOK_CODES.contains(&c))
    }

    /// Nuvama's text for a refusal (web `error_message`), or `default`.
    pub fn message(&self, default: &str) -> String {
        let b = &self.body;
        if let Some(m) = b
            .get("error")
            .and_then(|e| e.get("errMsg"))
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
        {
            return m.trim().to_string();
        }
        if !self.ok() {
            if let Some(m) = b
                .get("data")
                .and_then(|d| d.get("msg"))
                .and_then(Value::as_str)
                .filter(|m| !m.trim().is_empty())
            {
                return m.trim().to_string();
            }
        }
        ["message", "msg"]
            .iter()
            .find_map(|k| b.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(default)
            .to_string()
    }

    /// The body's `data` object of a successful answer.
    pub fn data(&self) -> Option<&Value> {
        self.body.get("data").filter(|d| d.is_object())
    }

    /// The refusal of a read (`what`: what was asked, for the log): Nuvama's
    /// own words, or a server error's.
    pub fn refused(&self, what: &str) -> AppError {
        let msg = self.message("");
        tracing::warn!(
            status = self.status,
            code = self.error_code().unwrap_or(""),
            "Nuvama refused {}: {}",
            what,
            msg
        );
        if self.status >= 500 {
            return AppError::Broker(http::SERVER_ERROR.into());
        }
        AppError::Broker(if msg.is_empty() {
            "Nuvama did not answer this request. Try again shortly.".into()
        } else {
            msg
        })
    }
}

/// An `error` key that holds something (web `not self.data.get("error")`
/// and the login's `body.get("error")`: Python's truth, so `""`, `0`, `{}`
/// and `[]` are no error).
pub(super) fn has_error(body: &Value) -> bool {
    python::truthy(body.get("error"))
}

pub(crate) fn session_expired() -> AppError {
    AppError::Auth("Your Nuvama session has expired. Log in to Nuvama again.".into())
}

/// One request: method, full URL, JSON body.
pub(crate) struct Call<'a> {
    pub method: Method,
    pub url: String,
    pub body: Option<&'a Value>,
    pub lane: Lane,
    pub timeout: Duration,
}

impl<'a> Call<'a> {
    pub fn get(url: String, lane: Lane) -> Self {
        Self {
            method: Method::GET,
            url,
            body: None,
            lane,
            timeout: http::REQUEST_TIMEOUT,
        }
    }

    pub fn send(method: Method, url: String, body: &'a Value, lane: Lane) -> Self {
        Self {
            method,
            url,
            body: Some(body),
            lane,
            timeout: http::REQUEST_TIMEOUT,
        }
    }
}

impl NuvamaBroker {
    async fn pace(&self, lane: Lane) -> Result<()> {
        match lane {
            Lane::Data => self.pacers.data.acquire().await,
            Lane::Orders => self.pacers.orders.acquire().await,
            Lane::Account => Ok(()),
        }
    }

    /// Send `call` with the session headers and read the answer. A refused
    /// session is an error; every other answer comes back for the caller
    /// to judge. The URL never reaches a log: it carries the account id.
    pub(crate) async fn call(&self, session: &NuvamaSession, call: Call<'_>) -> Result<Answer> {
        self.pace(call.lane).await?;
        let mut req = session.apply(
            self.http
                .request(call.method.clone(), &call.url)
                .timeout(call.timeout),
        );
        if let Some(body) = call.body {
            req = req.body(body.to_string());
        }
        let resp = req.send().await.map_err(redact::http)?;
        let status = resp.status().as_u16();
        if status == 401 || status == 403 {
            tracing::warn!(status, "Nuvama refused the session");
            return Err(session_expired());
        }
        let (_, body) = http::read_json::<Value>("nuvama", resp)
            .await
            .map_err(redact::redact)?;
        let answer = Answer { status, body };
        if answer
            .error_code()
            .is_some_and(|c| SESSION_EXPIRED_CODES.contains(&c))
        {
            tracing::warn!(status, "Nuvama said the session expired");
            return Err(session_expired());
        }
        Ok(answer)
    }

    /// The session of a stored token.
    pub(crate) fn session(&self, auth: &AuthToken) -> Result<NuvamaSession> {
        NuvamaSession::parse(auth.raw())
    }
}
