//! What an external data call can fail with (the web's `McpError` family in
//! `services/agent/mcp/client.py`).
//!
//! Every error carries a sentence a trader can read (the cause in plain
//! words and what to do next) and, apart from it, the technical detail for
//! the log. Status codes and protocol words never reach the sentence.
//!
//! The web's `McpBusy` (its own web server too busy to send the request)
//! has no counterpart: the desktop sends from the Agent's own task.

use super::registry::McpServerSpec;
use std::time::Duration;

/// What went wrong, as the toolkit's result names it.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Not reached, or a server-side error (`unavailable`).
    Unavailable,
    /// Paused by the circuit breaker (`paused`) for about this long.
    Paused(Duration),
    /// Connected, but no answer in time (`timed_out`).
    TimedOut,
    /// The server ran the tool and reported failure, or refused its name or
    /// arguments (`tool_error`); the server's own words, third-party text.
    Tool(String),
    /// An answer this client cannot use (`protocol_error`); the HTTP status
    /// when the server refused the request outright, else 0.
    Protocol(u16),
}

/// One failure: its kind, the trader's sentence and the log's detail.
#[derive(Debug, Clone, PartialEq)]
pub struct McpError {
    pub kind: Kind,
    pub message: String,
    pub detail: String,
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl McpError {
    /// The short code the toolkit puts in its result.
    pub fn code(&self) -> &'static str {
        match self.kind {
            Kind::Unavailable => "unavailable",
            Kind::Paused(_) => "paused",
            Kind::TimedOut => "timed_out",
            Kind::Tool(_) => "tool_error",
            Kind::Protocol(_) => "protocol_error",
        }
    }

    /// A 4xx other than 404 (a firewall's 403, say): every call would be
    /// answered the same way. A 404 is how a server says a session expired.
    pub fn is_refusal(&self) -> bool {
        matches!(self.kind, Kind::Protocol(s) if (400..500).contains(&s) && s != 404)
    }

    /// An outage, which counts toward the circuit breaker: the server did
    /// not answer, or refused at the door. A tool error or an unreadable
    /// answer still means the server is up.
    pub fn is_outage(&self) -> bool {
        matches!(
            self.kind,
            Kind::Unavailable | Kind::TimedOut | Kind::Paused(_)
        ) || self.is_refusal()
    }
}

/// The sentence that places an outage with whoever runs the server.
fn side(spec: &McpServerSpec) -> String {
    let owner = if spec.provider.is_empty() {
        "the service's".to_string()
    } else {
        format!("{}'s", spec.provider)
    };
    format!("The fault is on {} side, not in your setup.", owner)
}

pub fn unreachable(spec: &McpServerSpec, detail: &str) -> McpError {
    McpError {
        kind: Kind::Unavailable,
        message: format!(
            "{} is not answering right now. {} Try again in a minute or two.",
            spec.title,
            side(spec)
        ),
        detail: detail.into(),
    }
}

pub fn slow_down(spec: &McpServerSpec) -> McpError {
    McpError {
        kind: Kind::Unavailable,
        message: format!(
            "{} asked this platform to slow down. Wait a minute and try again.",
            spec.title
        ),
        detail: "HTTP 429".into(),
    }
}

pub fn timed_out(spec: &McpServerSpec, detail: &str) -> McpError {
    McpError {
        kind: Kind::TimedOut,
        message: format!(
            "{} took too long to answer. {} Try again in a minute or two.",
            spec.title,
            side(spec)
        ),
        detail: detail.into(),
    }
}

pub fn paused(spec: &McpServerSpec, retry_after: Duration) -> McpError {
    McpError {
        kind: Kind::Paused(retry_after),
        message: format!(
            "{} has not been answering, so it is paused for about {} seconds. {} Try again after that.",
            spec.title,
            retry_after.as_secs() + 1,
            side(spec)
        ),
        detail: "circuit open".into(),
    }
}

pub fn tool_error(message: String, text: String) -> McpError {
    McpError {
        kind: Kind::Tool(text),
        message,
        detail: "tool reported an error".into(),
    }
}

pub fn protocol(spec: &McpServerSpec, what: &str, detail: &str, status: u16) -> McpError {
    McpError {
        kind: Kind::Protocol(status),
        message: format!(
            "{} {}. The service may have changed; if this keeps happening, report it so the integration can be updated.",
            spec.title, what
        ),
        detail: if detail.is_empty() { what.into() } else { detail.into() },
    }
}
