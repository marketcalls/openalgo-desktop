//! The Agent's audit trail (the web's `services/agent/safety/audit.py`, the
//! toolkit's `audit_attempt` / `audit_result` / `audit_decision`, and
//! `database/agent_db.py`'s `record_audit` and `list_audit`).
//!
//! Every mutating call writes an `attempt` row before anything is checked
//! or sent and a `result` row after; every human decision writes a
//! `decision` row before the run resumes; every finalised spoken line a
//! `transcript` row (the voice part). An attempt with no result is a call
//! that hung or was cut off mid-order.
//!
//! Two rules, as on the web: nothing secret is written, and a write that
//! fails never blocks or repeats a trade (it is logged and swallowed).
//!
//! Desktop fixes (each tested): every row is redacted, results and
//! exceptions included, where the web redacted arguments only (D-009); a
//! result row names its attempt row (`attempt_id`) and a decision row
//! carries the arguments the trader approved (D-009); one order-id
//! extractor instead of two (D-009); the phase vocabulary includes
//! `transcript` and is checked (D-041); the trail has a read route (D-006,
//! `server::routes::agent_audit`).

pub mod store;

use crate::agent::redact::{redact, REDACTED};
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;
pub use store::Filter;

/// The four kinds of row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Attempt,
    Result,
    Decision,
    Transcript,
}

impl Phase {
    pub const ALL: [Phase; 4] = [
        Phase::Attempt,
        Phase::Result,
        Phase::Decision,
        Phase::Transcript,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Attempt => "attempt",
            Phase::Result => "result",
            Phase::Decision => "decision",
            Phase::Transcript => "transcript",
        }
    }

    /// A stored phase name, or `None` for one outside the vocabulary.
    pub fn parse(name: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

/// Argument names whose value is never written (the web's
/// `REDACTED_ARGUMENT_NAMES`, matched exactly, case ignored).
pub const REDACTED_ARGUMENT_NAMES: [&str; 12] = [
    "api_key",
    "apikey",
    "auth_token",
    "authtoken",
    "feed_token",
    "feedtoken",
    "password",
    "secret",
    "token",
    "access_token",
    "refresh_token",
    "ciphertext",
];

/// Response keys that carry a broker order id: the web's two extractors'
/// keys together (D-009: one extractor).
const ORDER_ID_KEYS: [&str; 9] = [
    "orderid",
    "order_id",
    "orderids",
    "order_ids",
    "trigger_id",
    "triggerid",
    "norenordno",
    "nestordernumber",
    "broker_orderid",
];

/// The most order ids one row or result carries.
pub const MAX_ORDER_IDS: usize = 200;
const MAX_ID_DEPTH: usize = 16;

/// Column bounds (the web's `String(120)` and `String(60)`).
const MAX_TOOL_CHARS: usize = 120;
const MAX_RUN_ID_CHARS: usize = 120;
const MAX_VERDICT_CHARS: usize = 60;

/// The web's `redact_arguments`: the twelve credential names, at any depth.
pub fn redact_arguments(args: &Value) -> Value {
    match args {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if REDACTED_ARGUMENT_NAMES.contains(&k.trim().to_lowercase().as_str()) {
                        json!(REDACTED)
                    } else {
                        redact_arguments(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_arguments).collect()),
        other => other.clone(),
    }
}

/// What an argument mapping becomes on a row: the twelve names, then
/// every credential-shaped key and value, and the size bounds.
pub fn safe_args(args: &Value) -> Value {
    redact(&redact_arguments(args))
}

/// Every broker order id in a response, de-duplicated, in order, at most
/// [`MAX_ORDER_IDS`].
pub fn extract_order_ids(response: &Value) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    walk_ids(response, 0, &mut found);
    found
}

fn walk_ids(node: &Value, depth: usize, found: &mut Vec<String>) {
    if depth > MAX_ID_DEPTH || found.len() >= MAX_ORDER_IDS {
        return;
    }
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                if ORDER_ID_KEYS.contains(&key.trim().to_lowercase().as_str()) {
                    let items: Vec<&Value> = match value {
                        Value::Array(a) => a.iter().collect(),
                        v => vec![v],
                    };
                    for item in items {
                        let text = match item {
                            Value::String(s) => s.trim().to_string(),
                            Value::Number(n) => n.to_string(),
                            _ => continue,
                        };
                        if !text.is_empty() && !found.contains(&text) && found.len() < MAX_ORDER_IDS
                        {
                            found.push(text);
                        }
                    }
                } else {
                    walk_ids(value, depth + 1, found);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_ids(item, depth + 1, found);
            }
        }
        _ => {}
    }
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// One row to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub phase: Phase,
    pub tool: String,
    pub conversation_id: Option<i64>,
    pub run_id: Option<String>,
    pub attempt_id: Option<i64>,
    pub args: Option<Value>,
    pub risk_verdict: Option<String>,
    pub ok: Option<bool>,
    pub response: Option<Value>,
    pub order_ids: Option<Vec<String>>,
}

impl Entry {
    pub fn new(phase: Phase, tool: &str) -> Entry {
        Entry {
            phase,
            tool: tool.to_string(),
            conversation_id: None,
            run_id: None,
            attempt_id: None,
            args: None,
            risk_verdict: None,
            ok: None,
            response: None,
            order_ids: None,
        }
    }

    /// The row as stored: arguments and response redacted, columns bounded,
    /// order ids from the response when none were given.
    fn into_row(self, ts: String) -> store::NewRow {
        let order_ids = self.order_ids.unwrap_or_else(|| {
            self.response
                .as_ref()
                .map(extract_order_ids)
                .unwrap_or_default()
        });
        store::NewRow {
            ts,
            phase: self.phase.as_str(),
            tool: cut(&self.tool, MAX_TOOL_CHARS),
            conversation_id: self.conversation_id,
            run_id: self.run_id.as_deref().map(|r| cut(r, MAX_RUN_ID_CHARS)),
            attempt_id: self.attempt_id,
            args: self.args.as_ref().map(safe_args),
            risk_verdict: self
                .risk_verdict
                .as_deref()
                .map(|v| cut(v, MAX_VERDICT_CHARS)),
            ok: self.ok,
            response: self.response.as_ref().map(redact),
            order_ids: (!order_ids.is_empty()).then_some(order_ids),
        }
    }
}

/// Write one row; its id, or `None` when it could not be written (logged,
/// never raised: the trail is evidence, not a control).
pub fn write(app: &AppState, entry: Entry) -> Option<i64> {
    let (phase, tool) = (entry.phase.as_str(), entry.tool.clone());
    let row = entry.into_row(crate::agent::conversations::store::stamp(app.now()));
    match app.logs.conn().and_then(|c| store::insert(&c, &row)) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!(
                "Agent audit row could not be written: phase={} tool={}: {}",
                phase,
                tool,
                e
            );
            None
        }
    }
}

/// [`write`] from async code, on the blocking pool.
pub async fn append(app: &Arc<AppState>, entry: Entry) -> Option<i64> {
    match app.blocking(move |ctx| Ok(write(ctx, entry))).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Agent audit row could not be written: {}", e);
            None
        }
    }
}

/// One finalised spoken line (the web's `record_transcript`): tool
/// `voice:trader` for the microphone, `voice:agent` for what was spoken.
pub async fn record_transcript(
    app: &Arc<AppState>,
    role: &str,
    text: &str,
    conversation_id: Option<i64>,
    run_id: Option<&str>,
) -> Option<i64> {
    let role = if role == "agent" { "agent" } else { "trader" };
    let mut entry = Entry::new(Phase::Transcript, &format!("voice:{}", role));
    entry.conversation_id = conversation_id;
    entry.run_id = run_id.filter(|r| !r.is_empty()).map(str::to_string);
    entry.args = Some(json!({"text": text}));
    entry.ok = Some(true);
    append(app, entry).await
}

/// What a read of the trail found.
#[derive(Debug, Clone, PartialEq)]
pub enum Listing {
    Rows(Vec<Value>),
    /// The conversation named is another user's.
    NotYours,
}

/// The trail for the signed-in user, newest first. A conversation filter
/// that names another user's conversation is refused (404 in the route);
/// one that names a deleted conversation still reads its rows, which
/// outlive it. Rows are redacted again on the way out.
pub async fn list(
    app: &Arc<AppState>,
    user: &str,
    filter: Filter,
) -> crate::error::Result<Listing> {
    let user = user.to_string();
    app.blocking(move |ctx| {
        if let Some(id) = filter.conversation_id {
            let conn = ctx.sqlite.conn()?;
            let owner: Option<String> = conn
                .query_row(
                    "SELECT user_id FROM ag_conversation WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .ok();
            if owner.is_some_and(|o| o != user) {
                return Ok(Listing::NotYours);
            }
        }
        let rows = store::list(&*ctx.logs.conn()?, &filter)?;
        Ok(Listing::Rows(rows.iter().map(redact_row).collect()))
    })
    .await
}

fn redact_row(row: &Value) -> Value {
    let mut out: Map<String, Value> = row.as_object().cloned().unwrap_or_default();
    if let Some(v) = out.get("args").map(safe_args) {
        out.insert("args".into(), v);
    }
    if let Some(v) = out.get("response").map(redact) {
        out.insert("response".into(), v);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests;
