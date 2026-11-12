//! Error log behind the Diagnostics page (the web's `log/errors.jsonl`).
//!
//! Two sources land in `logs.db` `error_logs`:
//! - server warnings and errors, captured by [`CaptureLayer`] (a `tracing`
//!   layer) into a bounded in-memory queue that the monitor writer drains;
//! - browser error reports posted to `/admin/api/errors/client`, sanitized
//!   like the web (control characters stripped, sensitive URL query values
//!   redacted, every field length-capped).
//!
//! Log messages never carry secrets (CLAUDE.md), so the captured text is the
//! formatted message only, not the span fields.

use crate::db::sqlite::monitor::{self, ErrorEntry};
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Captured events waiting for the writer; oldest dropped beyond this.
pub const QUEUE_CAP: usize = 1_000;

static QUEUE: Mutex<VecDeque<ErrorEntry>> = parking_lot::const_mutex(VecDeque::new());

/// `tracing` layer that queues WARN and ERROR events for the error log.
#[derive(Debug, Default, Clone, Copy)]
pub struct CaptureLayer;

#[derive(Default)]
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{:?}", value);
        }
    }
}

/// The level and message the error log keeps of `event`: WARN and ERROR
/// events with a message, nothing else.
fn captured(event: &Event<'_>) -> Option<(&'static str, String)> {
    let level = match *event.metadata().level() {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        _ => return None,
    };
    let mut v = MessageVisitor::default();
    event.record(&mut v);
    (!v.0.is_empty()).then_some((level, v.0))
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let Some((level, message)) = captured(event) else {
            return;
        };
        let v = MessageVisitor(message);
        push(ErrorEntry {
            ts: monitor::ts(Utc::now()),
            level: level.into(),
            logger: Some(meta.target().to_string()),
            module: meta
                .module_path()
                .map(|m| m.rsplit("::").next().unwrap_or(m).to_string()),
            file: meta
                .file()
                .map(|f| format!("{}:{}", f, meta.line().unwrap_or(0))),
            // A Slack or Discord webhook URL a library put in its error
            // text never reaches the page (the web's log filter).
            message: truncate(
                &crate::messaging::channels::webhook::redact_urls(&v.0),
                20_000,
            ),
            exception: None,
            request: None,
        });
    }
}

fn push(e: ErrorEntry) {
    let mut q = QUEUE.lock();
    while q.len() >= QUEUE_CAP {
        q.pop_front();
    }
    q.push_back(e);
}

/// Test helper: run `f` on this thread and return, with its result, every
/// message the error log (and so the Diagnostics page) would keep from it.
#[cfg(test)]
pub fn kept_while<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;
    #[derive(Clone, Default)]
    struct Keep(std::sync::Arc<Mutex<Vec<String>>>);
    impl<S: Subscriber> Layer<S> for Keep {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            if let Some((_, m)) = captured(event) {
                self.0.lock().push(m);
            }
        }
    }
    let keep = Keep::default();
    let subscriber = tracing_subscriber::registry().with(keep.clone());
    let out = tracing::subscriber::with_default(subscriber, f);
    let kept = keep.0.lock().clone();
    (out, kept)
}

/// Take everything queued (the writer persists it).
pub fn take_queued() -> Vec<ErrorEntry> {
    QUEUE.lock().drain(..).collect()
}

pub fn queued_len() -> usize {
    QUEUE.lock().len()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

// ------------------------------------------------------------ client reports

const SENSITIVE_QUERY_NAMES: &[&str] = &[
    "token",
    "code",
    "requesttoken",
    "accesstoken",
    "authtoken",
    "refreshtoken",
    "resettoken",
    "apikey",
    "email",
    "state",
    "password",
    "otp",
    "secret",
    "clientsecret",
    "idtoken",
    "jwt",
];

/// Keep printable characters, newlines and tabs (web `_scrub_control_chars`).
pub fn scrub(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

/// Redact sensitive query values and drop the fragment (web
/// `_sanitize_client_error_url`).
pub fn sanitize_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(mut u) if matches!(u.scheme(), "http" | "https") => {
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .map(|(k, v)| {
                    let norm = k.to_ascii_lowercase().replace(['_', '-'], "");
                    if SENSITIVE_QUERY_NAMES.contains(&norm.as_str()) {
                        (k.into_owned(), "[redacted]".to_string())
                    } else {
                        (k.into_owned(), v.into_owned())
                    }
                })
                .collect();
            u.set_fragment(None);
            if pairs.is_empty() {
                u.set_query(None);
            } else {
                u.query_pairs_mut().clear().extend_pairs(pairs);
            }
            u.to_string()
        }
        Ok(u) => format!("{}:", u.scheme()),
        // Relative or unparsable: keep only the path.
        Err(_) => raw.split(['?', '#']).next().unwrap_or_default().to_string(),
    }
}

/// Record a browser error report. `Err(msg)` is a trader-facing 400.
pub fn record_client_report(
    ctx: &AppState,
    data: &Map<String, Value>,
    now: DateTime<Utc>,
) -> std::result::Result<Result<()>, &'static str> {
    let field = |k: &str, cap: usize| -> String {
        let raw = match data.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        truncate(&scrub(&raw), cap)
    };
    let level = match field("level", 16).trim().to_ascii_uppercase().as_str() {
        "WARN" => "WARNING",
        _ => "ERROR",
    };
    let message = field("message", 2_000);
    if message.is_empty() {
        return Err("Missing message");
    }
    let stack = field("stack", 20_000);
    let url = truncate(&sanitize_url(&field("url", 4_000)), 2_000);
    let component_stack = field("component_stack", 5_000);
    let user_agent = field("user_agent", 500);
    let mut details = Vec::new();
    if !url.is_empty() {
        details.push(format!("URL: {}", url));
    }
    if !user_agent.is_empty() {
        details.push(format!("UA: {}", user_agent));
    }
    if !component_stack.is_empty() {
        details.push(format!("Component stack:\n{}", component_stack));
    }
    if !stack.is_empty() {
        details.push(format!("Stack:\n{}", stack));
    }
    let mut text = format!("[CLIENT] {}", message);
    if !details.is_empty() {
        text.push('\n');
        text.push_str(&details.join("\n\n"));
    }
    let entry = ErrorEntry {
        ts: monitor::ts(now),
        level: level.into(),
        logger: Some("client.browser".into()),
        module: Some("browser".into()),
        file: None,
        message: text,
        exception: None,
        request: Some(json!({"method": "POST", "path": "/admin/api/errors/client"})),
    };
    Ok(ctx
        .logs
        .conn()
        .and_then(|c| monitor::insert_error(&c, &entry)))
}

/// Persist queued server events.
pub fn flush_queue(ctx: &AppState) -> Result<usize> {
    let items = take_queued();
    if items.is_empty() {
        return Ok(0);
    }
    let conn = ctx.logs.conn()?;
    for e in &items {
        monitor::insert_error(&conn, e)?;
    }
    Ok(items.len())
}

// ------------------------------------------------------------- read models

/// Stable group key (web `_fingerprint_entry`).
pub fn fingerprint(e: &ErrorEntry) -> String {
    use sha2::{Digest, Sha256};
    let sig_src = match &e.exception {
        Some(Value::Array(a)) if !a.is_empty() => {
            let last = a
                .last()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default();
            last.split(':').next().unwrap_or_default().to_string()
        }
        Some(Value::String(s)) if !s.is_empty() => s.chars().take(200).collect(),
        _ => e.message.chars().take(200).collect(),
    };
    let parts = [
        e.level.clone(),
        e.logger.clone().unwrap_or_default(),
        e.module.clone().unwrap_or_default(),
        normalize_signature(&sig_src),
    ];
    let h = Sha256::digest(parts.join("|").as_bytes());
    hex::encode(h)[..12].to_string()
}

/// Collapse hex addresses, timestamps and numbers (web `_normalize_signature`).
pub fn normalize_signature(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '0' && i + 1 < b.len() && b[i + 1] == 'x' {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 {
                out.push_str("0x?");
                i = j;
                continue;
            }
        }
        if c.is_ascii_digit() && (i == 0 || !b[i - 1].is_alphanumeric()) {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit()) {
                j += 1;
            }
            if j == b.len() || !b[j].is_alphanumeric() {
                out.push_str("<n>");
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_redacted_like_the_web() {
        assert_eq!(
            sanitize_url("http://127.0.0.1:5000/x?apikey=abc&page=2&request_token=t#frag"),
            "http://127.0.0.1:5000/x?apikey=%5Bredacted%5D&page=2&request_token=%5Bredacted%5D"
        );
        assert_eq!(sanitize_url("/broker?code=1"), "/broker");
        assert_eq!(sanitize_url("javascript:alert(1)"), "javascript:");
    }

    #[test]
    fn signatures_ignore_numbers_and_addresses() {
        assert_eq!(
            normalize_signature("Order 123 failed at 0x7f00ab"),
            "Order <n> failed at 0x?"
        );
        let a = ErrorEntry {
            level: "ERROR".into(),
            message: "Timeout after 31 ms".into(),
            ..Default::default()
        };
        let b = ErrorEntry {
            level: "ERROR".into(),
            message: "Timeout after 52 ms".into(),
            ..Default::default()
        };
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn queue_is_bounded() {
        for i in 0..(QUEUE_CAP + 50) {
            push(ErrorEntry {
                message: format!("e{i}"),
                ..Default::default()
            });
        }
        assert!(queued_len() <= QUEUE_CAP);
        take_queued();
    }
}
