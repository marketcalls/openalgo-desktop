//! How a tool reaches OpenAlgo: the `/api/v1` handler stack, in process,
//! with the trader's API key (the web's `OpenAlgoToolkit.service_call`).
//!
//! The web's tools call `services/*` functions with the run's key; the
//! desktop's run the same `/api/v1` handlers MCP does
//! ([`crate::mcp::dispatch::run_as`]): same validation, same analyzer
//! routing, same Semi-Auto queue, same response shapes, and never a socket.
//! Calls come from [`AGENT_CALLER`], so the Agent has its own share of the
//! `/api/v1` rate limit and never spends an SDK client's or MCP's.
//!
//! [`unwrap`] turns a reply into the payload or the web's correction text:
//! the call, the reason, the arguments that look wrong, and whether a retry
//! is worth it. Write tools (the orders lane) read [`ServiceReply`]
//! themselves: a placement whose answer was lost may have been placed.

use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::mcp::dispatch::{self, Raw, Transport};
use regex::Regex;
use serde_json::{Map, Value};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::LazyLock;
use std::time::Duration;

/// The address the Agent's in-process calls carry: its own identity in the
/// app's internal block `100::/64` (discard-only, RFC 6666), beside MCP's
/// `100::1` and the shared tunnel identity `100::2`, so it never shares a
/// rate limit, a failure budget or a ban with anyone else, and is never
/// banned (security review, 2026-10-27: it was `100::2`, the tunnel's).
pub const AGENT_CALLER: IpAddr = IpAddr::V6(Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 3));

/// How long a tool waits for a read (the web's
/// `AGENT_SERVICE_TIMEOUT_SECONDS`).
pub const SERVICE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the model is told when the call could not run in time (the web's
/// `SERVICE_BUSY_MESSAGE`).
pub fn busy_message(label: &str) -> String {
    format!(
        "{} could not run because the web server was too busy to take it. Nothing was changed. Try the tool again in a moment.",
        label
    )
}

/// What `/api/v1` answered, or why nothing came back.
#[derive(Debug, Clone)]
pub enum ServiceReply {
    Answered {
        status: u16,
        body: Value,
        outcome_unknown: bool,
    },
    NoAnswer(Transport),
}

impl ServiceReply {
    fn from_raw(r: Result<Raw, Transport>) -> ServiceReply {
        match r {
            Ok(raw) => ServiceReply::Answered {
                status: raw.status,
                body: serde_json::from_str(&raw.text).unwrap_or(Value::String(raw.text)),
                outcome_unknown: raw.outcome_unknown,
            },
            Err(t) => ServiceReply::NoAnswer(t),
        }
    }
}

impl ToolCtx {
    /// POST `/api/v1/<endpoint>` with `payload` and the run's key.
    pub async fn api_post(
        &self,
        endpoint: &str,
        payload: Map<String, Value>,
        timeout: Duration,
    ) -> ServiceReply {
        let req = match dispatch::post_request(self.api_key.expose(), endpoint, payload) {
            Ok(r) => r,
            Err(t) => return ServiceReply::NoAnswer(t),
        };
        ServiceReply::from_raw(dispatch::run_as(&self.app, AGENT_CALLER, req, timeout).await)
    }

    /// GET `/api/v1/<endpoint>?<query>` with the run's key.
    pub async fn api_get(
        &self,
        endpoint: &str,
        query: &[(&str, &str)],
        timeout: Duration,
    ) -> ServiceReply {
        let req = match dispatch::get_request(self.api_key.expose(), endpoint, query) {
            Ok(r) => r,
            Err(t) => return ServiceReply::NoAnswer(t),
        };
        ServiceReply::from_raw(dispatch::run_as(&self.app, AGENT_CALLER, req, timeout).await)
    }

    /// A read: POST, then [`unwrap`] with the web's `label` for the service
    /// function (`quotes_service.get_quotes`).
    pub async fn service_read(
        &self,
        label: &str,
        endpoint: &str,
        payload: Map<String, Value>,
    ) -> Result<Value, ToolOutcome> {
        unwrap(
            label,
            self.api_post(endpoint, payload, SERVICE_TIMEOUT).await,
        )
    }
}

/// The payload of a successful reply, or the correction the model reads
/// (the web's `unwrap_service_result` and `_failure_message`).
pub fn unwrap(label: &str, reply: ServiceReply) -> Result<Value, ToolOutcome> {
    match reply {
        ServiceReply::NoAnswer(_) => Err(ToolOutcome::Retry(busy_message(label))),
        ServiceReply::Answered { status, body, .. } => {
            let failed = !(200..300).contains(&status)
                || body
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .map(str::to_lowercase)
                    == Some("error".into())
                || body.get("success") == Some(&Value::Bool(false));
            if failed {
                Err(ToolOutcome::Retry(failure_message(
                    label,
                    &body,
                    Some(status),
                )))
            } else {
                Ok(body)
            }
        }
    }
}

/// What the model should do about a status (the web's `_STATUS_GUIDANCE`).
fn guidance(status: Option<u16>) -> &'static str {
    const INVALID: &str = "The request was rejected as invalid. Correct the named argument and call the tool again with a valid value.";
    const AUTH: &str = "Authentication was rejected. This is a configuration problem only the user can fix. Do not retry; report it.";
    const UPSTREAM: &str = "The upstream broker or service failed. Retry at most once; if it fails again, report the failure to the user instead of looping.";
    match status.unwrap_or(0) {
        400 | 422 => INVALID,
        401 | 403 => AUTH,
        404 => "The target was not found. Check the spelling, and resolve the exact OpenAlgo symbol and exchange with the symbol search tool before calling again.",
        409 => "The request conflicts with the current state. Re-read the current state before retrying.",
        429 => "Rate limited. Wait before calling again, and prefer a multi-symbol variant of the tool over many single-symbol calls.",
        500 | 502 => UPSTREAM,
        501 => "The connected broker does not support this capability. Do not retry; tell the user their broker cannot do this.",
        503 => "The upstream broker or service is unavailable. Retry at most once; if it fails again, report the failure to the user instead of looping.",
        504 => "The upstream broker or service timed out. Retry at most once; if it fails again, report the failure to the user instead of looping.",
        _ => "Correct the arguments and call the tool again, or report the failure to the user if it cannot be corrected.",
    }
}

/// The web's `_failure_message`: the call, its reason, the arguments the
/// reason names, and what to do next.
pub fn failure_message(label: &str, payload: &Value, status: Option<u16>) -> String {
    let detail = extract_detail(payload);
    let mut first = format!("{} failed", label);
    if let Some(s) = status {
        first.push_str(&format!(" (HTTP {})", s));
    }
    if detail.is_empty() {
        first.push('.');
    } else {
        first.push_str(&format!(": {}", detail));
    }
    let mut parts = vec![first];
    let fields = guess_fields(&detail);
    if !fields.is_empty() {
        let named: Vec<String> = fields.iter().map(|f| format!("'{}'", f)).collect();
        parts.push(format!(
            "The problem is with the argument(s) {}.",
            named.join(", ")
        ));
    }
    parts.push(guidance(status).into());
    parts.join(" ")
}

/// The reason in a service error (the web's `extract_detail`).
pub fn extract_detail(payload: &Value) -> String {
    match payload {
        Value::Null => return String::new(),
        Value::String(s) => return s.trim().to_string(),
        Value::Object(map) => {
            for key in ["message", "error", "detail", "reason", "error_message"] {
                if let Some(Value::String(s)) = map.get(key) {
                    if !s.trim().is_empty() {
                        return s.trim().to_string();
                    }
                }
            }
            match map.get("errors") {
                Some(Value::Array(errors)) if !errors.is_empty() => {
                    let lines: Vec<String> = errors
                        .iter()
                        .take(5)
                        .map(|item| match item {
                            Value::Object(m) => {
                                let pick = |a: &str, b: &str| {
                                    m.get(a)
                                        .filter(|v| truthy(v))
                                        .or_else(|| m.get(b))
                                        .map(py_str)
                                        .unwrap_or_default()
                                };
                                let line = format!(
                                    "{}: {}",
                                    pick("path", "field"),
                                    pick("message", "code")
                                );
                                line.trim_matches(|c| c == ':' || c == ' ')
                                    .trim()
                                    .to_string()
                            }
                            other => py_str(other),
                        })
                        .filter(|l| !l.is_empty())
                        .collect();
                    if !lines.is_empty() {
                        return lines.join("; ");
                    }
                }
                Some(Value::String(s)) if !s.trim().is_empty() => return s.trim().to_string(),
                _ => {}
            }
        }
        _ => {}
    }
    py_repr(payload).trim().chars().take(500).collect()
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

/// Python's `str()` of a decoded JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr()` of a decoded JSON value: what the web's messages show
/// when a payload has no message of its own (a dict of field errors).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_quote(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", py_quote(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_quote(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Argument names a service message may name (the web's
/// `_KNOWN_ARGUMENT_NAMES`), longest first in the pattern.
const KNOWN_ARGUMENT_NAMES: &[&str] = &[
    "disclosed_quantity",
    "trigger_price",
    "instrumenttype",
    "option_type",
    "start_date",
    "underlying",
    "price_type",
    "pricetype",
    "expiry_date",
    "end_date",
    "position_size",
    "trigger_id",
    "exchange",
    "quantity",
    "strategy",
    "interval",
    "symbols",
    "orderid",
    "product",
    "symbol",
    "action",
    "expiry",
    "strike",
    "price",
    "mode",
];

static ARGUMENT_NAME: LazyLock<Option<Regex>> = LazyLock::new(|| {
    let mut names = KNOWN_ARGUMENT_NAMES.to_vec();
    // Python's sorted(key=len, reverse=True) is stable: equal lengths keep
    // the list's order.
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    Regex::new(&format!(r"(?i)\b({})\b", names.join("|"))).ok()
});

static MISSING_FIELDS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)missing\s+(?:mandatory\s+|required\s+)?field\(?s?\)?\s*:?\s*(.+)").ok()
});

/// Which tool arguments a message complains about (the web's
/// `_guess_fields`), at most six, in order of appearance.
pub fn guess_fields(message: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if message.is_empty() {
        return names;
    }
    if let Some(caps) = MISSING_FIELDS.as_ref().and_then(|re| re.captures(message)) {
        if let Some(rest) = caps.get(1) {
            for raw in rest.as_str().split(|c: char| c == ',' || c.is_whitespace()) {
                let cleaned = raw.trim_matches(|c| " .'\"()[]".contains(c)).to_lowercase();
                if !cleaned.is_empty() && !names.contains(&cleaned) {
                    names.push(cleaned);
                }
            }
        }
    }
    if let Some(re) = ARGUMENT_NAME.as_ref() {
        for m in re.captures_iter(message) {
            if let Some(g) = m.get(1) {
                let cleaned = g.as_str().to_lowercase();
                if !names.contains(&cleaned) {
                    names.push(cleaned);
                }
            }
        }
    }
    names.truncate(6);
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A tool's read goes through the real `/api/v1` handlers in process,
    /// with the run's key: an unknown key is refused as for any client.
    #[tokio::test]
    async fn a_read_reaches_the_api_in_process() {
        let t =
            crate::state::testing::build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now());
        let ctx = crate::agent::tools::testing::ctx(
            t.ctx.clone(),
            crate::agent::contract::tool::Surface::Chat,
            Default::default(),
        );
        let mut payload = Map::new();
        payload.insert("symbol".into(), json!("INFY"));
        payload.insert("exchange".into(), json!("NSE"));
        let refused = ctx
            .service_read("quotes_service.get_quotes", "quotes", payload)
            .await;
        let Err(ToolOutcome::Retry(text)) = refused else {
            panic!("{:?}", refused)
        };
        assert!(
            text.starts_with("quotes_service.get_quotes failed (HTTP "),
            "{}",
            text
        );
        let reply = ctx.api_get("ping", &[], SERVICE_TIMEOUT).await;
        assert!(
            matches!(reply, ServiceReply::Answered { .. }),
            "{:?}",
            reply
        );
    }

    /// The Agent is its own caller: not MCP, not the tunnel's shared
    /// identity, not the limiter's overflow or per-credential keys, and
    /// internal (never banned, its own budget key, no aggregate).
    #[test]
    fn the_agent_is_its_own_internal_caller() {
        use crate::server::ratelimit::OVERFLOW;
        use crate::server::source::PROXIED_CALLER;
        let ids = [AGENT_CALLER, dispatch::MCP_CALLER, PROXIED_CALLER, OVERFLOW];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
        let IpAddr::V6(v) = AGENT_CALLER else {
            panic!()
        };
        assert_eq!(
            v.segments()[..4],
            [0x100, 0, 0, 0],
            "inside 100::/64, outside the minted and overflow prefixes"
        );
        assert!(crate::db::sqlite::monitor::never_banned(
            &AGENT_CALLER.to_string()
        ));
        assert_eq!(
            crate::server::addr::budget_key_with(AGENT_CALLER, &[]),
            AGENT_CALLER
        );
        assert_eq!(crate::server::addr::aggregate_key(AGENT_CALLER), None);
    }

    #[test]
    fn a_refusal_reads_as_the_web_words_it() {
        let reply = ServiceReply::Answered {
            status: 400,
            body: json!({"status": "error", "message": "Missing mandatory field(s): symbol, exchange"}),
            outcome_unknown: false,
        };
        assert_eq!(
            unwrap("quotes_service.get_quotes", reply),
            Err(ToolOutcome::Retry(
                "quotes_service.get_quotes failed (HTTP 400): Missing mandatory field(s): symbol, exchange The problem is with the argument(s) 'symbol', 'exchange'. The request was rejected as invalid. Correct the named argument and call the tool again with a valid value."
                    .into()
            ))
        );
    }

    #[test]
    fn success_is_the_payload_and_a_lost_answer_is_busy() {
        let ok = ServiceReply::Answered {
            status: 200,
            body: json!({"status": "success", "data": {"ltp": 1.5}}),
            outcome_unknown: false,
        };
        assert_eq!(unwrap("x.y", ok).unwrap()["data"]["ltp"], 1.5);
        assert_eq!(
            unwrap("x.y", ServiceReply::NoAnswer(Transport::Timeout)),
            Err(ToolOutcome::Retry(busy_message("x.y")))
        );
    }

    /// Keys come in the order the reply was read in: sorted, as the app's
    /// `/api/v1` writes its maps (the web shows its dict's own order).
    #[test]
    fn a_payload_without_a_message_is_shown_as_python_would() {
        let p =
            json!({"status": "error", "message": {"symbol": ["Missing data for required field."]}});
        assert_eq!(
            extract_detail(&p),
            "{'message': {'symbol': ['Missing data for required field.']}, 'status': 'error'}"
        );
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_repr(&json!([true, null, 1.5])), "[True, None, 1.5]");
    }

    #[test]
    fn fields_are_guessed_in_order_and_capped() {
        assert_eq!(
            guess_fields("Invalid symbol or exchange; price must be positive"),
            vec!["symbol", "exchange", "price"]
        );
        assert_eq!(
            guess_fields("Missing required field: interval"),
            vec!["interval"]
        );
    }
}
