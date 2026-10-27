//! Every finalised spoken line is kept (the web's `POST
//! /agent/api/voice/transcript` and `audit.record_transcript`).
//!
//! A speech model handles much of an exchange itself (acknowledgements,
//! asking which expiry was meant) and none of that becomes an agent turn,
//! so a record built only from turns would miss it. Each line goes to the
//! audit trail as a `transcript` row: it is evidence of what was said out
//! loud, kept apart from the messages the agent wrote. The first line of a
//! session with no thread open opens one (surface `voice`), so a spoken
//! session always has somewhere to be found again. Recording never fails
//! the caller: a line that cannot be written is logged.
//!
//! The trail is the orders lane's (`agent::safety::audit`, in `logs.db`);
//! it is reached through [`TranscriptAudit`], installed at startup as
//!
//! ```text
//! app.agent.voice().set_transcript_audit(transcript::audit_with(|app, l| async move {
//!     agent::safety::audit::record_transcript(
//!         &app, l.role, &l.text, l.conversation_id, l.run_id.as_deref()).await;
//! }));
//! ```
//!
//! Until then a line is not written anywhere, and its words are never
//! logged.

use crate::agent::chat::build::whole_number;
use crate::agent::contract::tool::Surface;
use crate::agent::conversations::store;
use crate::agent::models::{success, text_field};
use crate::state::AppState;
use futures_util::future::BoxFuture;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// How much of the first line titles the thread it opens.
pub const TITLE_CHARS: usize = 60;

/// One finalised line, as the audit trail records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenLine {
    /// `trader` (the microphone) or `agent` (what was spoken back).
    pub role: &'static str,
    pub text: String,
    pub conversation_id: Option<i64>,
    pub run_id: Option<String>,
}

/// Writes one spoken line to the audit trail.
pub type TranscriptAudit =
    Arc<dyn Fn(Arc<AppState>, SpokenLine) -> BoxFuture<'static, ()> + Send + Sync>;

/// A [`TranscriptAudit`] from an async function of the app and the line.
pub fn audit_with<F, Fut>(write: F) -> TranscriptAudit
where
    F: Fn(Arc<AppState>, SpokenLine) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    Arc::new(
        move |app: Arc<AppState>, line: SpokenLine| -> BoxFuture<'static, ()> {
            Box::pin(write(app, line))
        },
    )
}

/// Before the audit trail is installed: nothing is written, and the line's
/// words stay out of the log.
pub fn not_installed() -> TranscriptAudit {
    audit_with(|_, line| async move {
        tracing::debug!(
            "A spoken line ({}, {} characters) was not recorded: the audit trail is not installed",
            line.role,
            line.text.chars().count()
        );
    })
}

/// The line a request carries: `role` is `agent` exactly, else `trader`;
/// the text trimmed; a conversation id that is not a whole number is none.
pub fn line_of(body: &Map<String, Value>) -> SpokenLine {
    let role = match body.get("role") {
        Some(Value::String(r)) if r == "agent" => "agent",
        _ => "trader",
    };
    SpokenLine {
        role,
        text: text_field(body, "text"),
        conversation_id: body
            .get("conversation_id")
            .filter(|v| !v.is_null())
            .and_then(|v| whole_number(Some(v))),
        run_id: Some(text_field(body, "run_id")).filter(|r| !r.is_empty()),
    }
}

/// `POST /agent/api/voice/transcript`: record one line; never fails.
pub async fn record(app: &Arc<AppState>, user: &str, body: &Map<String, Value>) -> Value {
    let mut line = line_of(body);
    if line.text.is_empty() {
        return success(json!({"data": {"recorded": false}}));
    }
    if line.conversation_id.is_none() {
        line.conversation_id = open_thread(app, user, &line.text).await;
    }
    let conversation_id = line.conversation_id;
    let audit = app.agent.voice().transcript_audit();
    audit(app.clone(), line).await;
    success(json!({"data": {"recorded": true, "conversation_id": conversation_id}}))
}

/// The thread a spoken session's first line opens, titled by the line.
async fn open_thread(app: &Arc<AppState>, user: &str, text: &str) -> Option<i64> {
    let title: String = text.chars().take(TITLE_CHARS).collect();
    let (user, now) = (user.to_string(), app.now());
    app.blocking(move |c| {
        store::create_conversation(
            &*c.sqlite.conn()?,
            &user,
            Some(&title),
            Surface::Voice.as_str(),
            now,
        )
    })
    .await
    .map(|conv| conv.id)
    .inspect_err(|e| tracing::warn!("Could not open a conversation for a spoken line: {}", e))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_read_as_the_web_reads_it() {
        let body = |v: Value| v.as_object().cloned().unwrap();
        let l = line_of(&body(
            json!({"role": "agent", "text": "  hi  ", "conversation_id": "7"}),
        ));
        assert_eq!(
            (l.role, l.text.as_str(), l.conversation_id),
            ("agent", "hi", Some(7))
        );
        let l = line_of(&body(
            json!({"role": " agent", "text": "x", "conversation_id": "abc"}),
        ));
        assert_eq!(
            (l.role, l.conversation_id, l.run_id),
            ("trader", None, None)
        );
        let l = line_of(&body(
            json!({"text": null, "conversation_id": null, "run_id": "r1"}),
        ));
        assert_eq!((l.text.as_str(), l.run_id.as_deref()), ("", Some("r1")));
    }
}
