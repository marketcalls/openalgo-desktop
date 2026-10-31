//! HTTP Request (the web's `execute_http_request`,
//! `flow_executor_service.py` 3080-3252): one request to a public address,
//! its answer stored in the node's output variable.
//!
//! Each field is interpolated exactly once, and the headers' JSON is parsed
//! before its values are, so a value a webhook substituted can neither be
//! expanded again nor add a header. The destination is checked first
//! ([`guard`]), the request goes out on Flow's own client ([`client`]) and
//! a redirect is never followed. Fixed web defects: a body given as an
//! object is sent as JSON instead of ending the run (N-httpRequest-11); a
//! non-2xx answer says `HTTP <code>` (N-httpRequest-13); an invalid port
//! fails the node, not the run (N-httpRequest-19); 100.64.0.0/10 is
//! refused (N-httpRequest-07); the answer is capped at 5 MB
//! (N-httpRequest-16).

mod client;
pub mod guard;
pub mod url;

pub use client::install;

use super::pyjson;
use crate::flow::contract::env::{HttpCall, HttpReply};
use crate::flow::contract::node::{FlowNode, NodeAbort, NodeCtx, NodeOutcome};
use crate::flow::contract::pyfmt;
use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// The timeout when none is set or it is not positive (milliseconds).
pub const HTTP_TIMEOUT_DEFAULT_MS: i64 = 30_000;
/// The longest timeout honoured (milliseconds).
pub const HTTP_TIMEOUT_MAX_MS: i64 = 60_000;

/// What a request cut short by the run stopping answers.
const STOPPED: &str = "The workflow was stopped while it was waiting.";

pub struct HttpRequest;

/// The headers as the node sends them: a JSON object (or its text), each
/// value interpolated once after parsing.
fn headers(ctx: &NodeCtx, raw: &Value) -> Result<Vec<(String, String)>, String> {
    let parsed = match raw {
        Value::String(s) => {
            let text = s.trim();
            if text.is_empty() {
                Value::Object(Map::new())
            } else {
                let v =
                    pyjson::loads(text).map_err(|e| format!("headers is not valid JSON: {}", e))?;
                // NaN and Infinity decode as Python floats, which the run
                // keeps as their text: named here as the web names them
                // (decisions.md 36).
                if v.is_string() && !text.starts_with('"') {
                    return Err("headers must be a JSON object, got float".into());
                }
                v
            }
        }
        other => other.clone(),
    };
    match parsed {
        Value::Object(map) => Ok(map
            .iter()
            .map(|(k, v)| (k.clone(), ctx.vars.interpolate(&pyfmt::str(v))))
            .collect()),
        other => Err(format!(
            "headers must be a JSON object, got {}",
            pyfmt::type_name(&other)
        )),
    }
}

/// `json.dumps(value, ensure_ascii=False, separators=(",", ":"))`, as
/// httpx writes a JSON body (keys in the order the run holds them).
fn compact_json(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{08}' => out.push_str("\\b"),
                    '\u{0c}' => out.push_str("\\f"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(compact_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!(
                    "{}:{}",
                    compact_json(&Value::String(k.clone())),
                    compact_json(v)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => pyfmt::json_dumps(other),
    }
}

/// The body of a POST, PUT or PATCH and whether it is JSON: text that
/// parses as JSON is re-sent as JSON, except JSON `null`, which is no body
/// at all (httpx's `json=None`, decisions.md 32); other text as it is, an
/// empty body as `{}`, and an object or array as JSON (the web raised on
/// those).
fn encode_body(body: &Value) -> (Vec<u8>, bool) {
    if !pyfmt::truthy(body) {
        return (b"{}".to_vec(), true);
    }
    match body {
        Value::String(s) => match pyjson::loads(s) {
            Ok(Value::Null) => (Vec::new(), false),
            Ok(v) => (compact_json(&v).into_bytes(), true),
            Err(_) => (s.clone().into_bytes(), false),
        },
        other => (compact_json(other).into_bytes(), true),
    }
}

/// `response.json()` when the body parses, else `response.text`.
fn response_data(reply: &HttpReply) -> Value {
    let bytes = reply
        .body
        .strip_prefix(b"\xef\xbb\xbf".as_slice())
        .unwrap_or(&reply.body);
    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Ok(v) = pyjson::loads(text) {
            return v;
        }
    }
    let charset = reply
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, v)| {
            v.split(';')
                .skip(1)
                .find_map(|p| p.trim().strip_prefix("charset="))
                .map(|c| c.trim_matches('"').to_ascii_lowercase())
        });
    let single_byte = matches!(
        charset.as_deref(),
        Some("iso-8859-1" | "latin-1" | "latin1" | "l1" | "iso8859-1" | "ascii" | "us-ascii")
    );
    if single_byte {
        return Value::String(reply.body.iter().map(|b| *b as char).collect());
    }
    Value::String(String::from_utf8_lossy(&reply.body).into_owned())
}

/// The URL without `user:password@` and the Basic credentials it carried,
/// which httpx sends as an `Authorization` header.
fn credentials(url: &str) -> (String, Option<String>) {
    let Some(info) = url::urlsplit(url)
        .ok()
        .and_then(|p| p.userinfo().map(str::to_string))
    else {
        return (url.to_string(), None);
    };
    let (user, pass) = match info.split_once(':') {
        Some((u, p)) => (url::unquote(u), url::unquote(p)),
        None => (url::unquote(&info), String::new()),
    };
    let stripped = match ::url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => url.to_string(),
    };
    if user.is_empty() && pass.is_empty() {
        return (stripped, None);
    }
    let token = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", user, pass));
    (stripped, Some(format!("Basic {}", token)))
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String, replace: bool) {
    let present = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if present && !replace {
        return;
    }
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    headers.push((name.to_string(), value));
}

/// Refuse with the web's words, logged as the web logs them.
fn aborted(ctx: &NodeCtx, message: String) -> NodeOutcome {
    ctx.error(format!("HTTP request aborted: {}", message));
    NodeOutcome::error(message)
}

impl HttpRequest {
    /// The timeout in milliseconds, defaulted and capped; a list, dict or
    /// infinity ends the run as the web's `int()` does (decisions.md 24).
    fn timeout(ctx: &NodeCtx, data: &Map<String, Value>) -> Result<Duration, NodeAbort> {
        let mut ms = ctx.try_get_int(data, "timeout", HTTP_TIMEOUT_DEFAULT_MS)?;
        if ms <= 0 {
            ms = HTTP_TIMEOUT_DEFAULT_MS;
        }
        Ok(Duration::from_millis(ms.min(HTTP_TIMEOUT_MAX_MS) as u64))
    }
}

#[async_trait]
impl FlowNode for HttpRequest {
    fn node_type(&self) -> &'static str {
        "httpRequest"
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let method = ctx.get_str(data, "method", "GET").to_uppercase();
        // Read raw and interpolated once, below (N-httpRequest-04).
        let raw_url = match data.get("url") {
            None => String::new(),
            Some(v) => pyfmt::str(v),
        };
        let timeout = match Self::timeout(ctx, data) {
            Ok(t) => t,
            Err(abort) => return abort.into(),
        };
        if raw_url.is_empty() {
            return NodeOutcome::error("No URL specified");
        }
        let mut headers = match headers(ctx, data.get("headers").unwrap_or(&json!({}))) {
            Ok(h) => h,
            Err(message) => return aborted(ctx, message),
        };
        let url = ctx.vars.interpolate(&raw_url);
        let body = match data.get("body").cloned().unwrap_or(json!("")) {
            Value::String(s) if !s.is_empty() => Value::String(ctx.vars.interpolate(&s)),
            other => other,
        };
        let vetted = match guard::check(ctx.env.http.as_ref(), &url).await {
            Ok(v) => v,
            Err(reason) => {
                return aborted(ctx, format!("refusing to send this request: {}", reason))
            }
        };
        ctx.info(format!("HTTP {} {}", method, url::redact(&url)));
        let body = match method.as_str() {
            "GET" | "DELETE" => None,
            "POST" | "PUT" | "PATCH" => {
                let (bytes, is_json) = encode_body(&body);
                if is_json {
                    set_header(
                        &mut headers,
                        "content-type",
                        "application/json".into(),
                        false,
                    );
                }
                Some(bytes)
            }
            _ => return NodeOutcome::error(format!("Unsupported method: {}", method)),
        };
        let (url, auth) = credentials(&url);
        if let Some(auth) = auth {
            set_header(&mut headers, "authorization", auth, true);
        }
        let call = HttpCall {
            method,
            url,
            headers,
            body,
            timeout,
            connect_to: vetted.connect_to,
        };
        let reply = tokio::select! {
            r = ctx.env.http.send(call) => r,
            _ = ctx.cancel.cancelled() => return NodeOutcome::error(STOPPED),
        };
        let reply = match reply {
            Ok(r) => r,
            // No node log line here, as on the web; never an empty reason.
            Err(e) if e.trim().is_empty() => return NodeOutcome::error("the request failed"),
            Err(e) => return NodeOutcome::error(e),
        };
        let ok = (200..=299).contains(&reply.status);
        let mut result = json!({
            "status": if ok { "success" } else { "error" },
            "statusCode": reply.status,
            "data": response_data(&reply),
        });
        if !ok {
            // The web's result had no message, so the run read "node failed"
            // (parity N-httpRequest-13).
            result["message"] = json!(format!("HTTP {}", reply.status));
        }
        ctx.store_output(data, result.clone());
        NodeOutcome::result(result)
    }
}

#[cfg(test)]
mod tests;
