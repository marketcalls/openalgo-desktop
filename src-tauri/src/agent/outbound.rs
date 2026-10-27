//! The Agent's requests outside a model turn: the ChatGPT plan's sign-in
//! (device code, poll, token exchange, renewal) and the web search
//! providers (DuckDuckGo, Tavily, Perplexity).
//!
//! One client for the process, built on first use with the Agent's own
//! client settings (`run::client::http_client`: rustls on ring, no
//! redirect, the link-local guard), never the broker one. Every request
//! sets its own timeout. A request's failure is described by its kind
//! only: a transport error's text can quote the URL (a search query) or
//! the body (a refresh token).

use parking_lot::Mutex;
use std::sync::LazyLock;

static CLIENT: LazyLock<Mutex<Option<reqwest_agent::Client>>> = LazyLock::new(|| Mutex::new(None));

/// The shared client (cheap to clone: one connection pool).
pub fn client() -> Result<reqwest_agent::Client, String> {
    let mut slot = CLIENT.lock();
    if let Some(c) = slot.as_ref() {
        return Ok(c.clone());
    }
    let c = crate::agent::run::client::http_client()?;
    *slot = Some(c.clone());
    Ok(c)
}

/// What kind of failure a request met, for a log line.
pub fn error_kind(e: &reqwest_agent::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connection"
    } else if e.is_redirect() {
        "redirect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else if e.is_request() {
        "request"
    } else {
        "transport"
    }
}

#[cfg(test)]
pub mod fake;
