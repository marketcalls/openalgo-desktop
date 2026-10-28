//! The chat service behind `/agent/api/chat/*` (the web's
//! `blueprints/agent.py` chat routes, which the desktop keeps thin).
//!
//! Everything that can fail as an HTTP error happens before the stream
//! opens: the stream slot, the setup gate, the request's fields, the
//! conversation, the model. Then the turn runs as a task the app owns and
//! the route streams its frames.
//!
//! - [`request`]: the request's fields; [`build`]: one run's tools, prompt
//!   and history; [`start`] and [`resume`]: a new turn and an approval.

pub mod build;
pub mod request;

use crate::agent::attachments;
use crate::agent::contract::frames::{Frame, WireId};
use crate::agent::conversations::store;
use crate::agent::run::output::{FrameOut, QUEUE_CAPACITY};
use crate::agent::run::{self, PausedState, Resume, RunSpec, Start};
use crate::agent::stream::{Slot, STREAMS_BUSY_MESSAGE};
use crate::state::AppState;
use build::{RunParts, Turnkey};
use genai::chat::{Binary, ChatMessage, ContentPart, MessageContent};
use serde_json::{json, Map, Value};

pub use crate::agent::reply::{Refusal, NOT_FOUND};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Who is asking: the signed-in user and the browser session.
#[derive(Debug, Clone)]
pub struct Caller {
    pub user: String,
    pub session: String,
}

/// A turn that started: its frames and the stream slot it holds.
pub struct Streaming {
    pub frames: mpsc::Receiver<Frame>,
    pub slot: Slot,
}

fn busy(app: &AppState) -> Result<Slot, Refusal> {
    app.agent
        .slots()
        .admit()
        .ok_or_else(|| Refusal::new(429, STREAMS_BUSY_MESSAGE, Some(json!({"kind": "busy"}))))
}

/// `POST /agent/api/chat/stream`: one turn of a conversation.
pub async fn start(
    app: &Arc<AppState>,
    caller: &Caller,
    body: Map<String, Value>,
) -> Result<Streaming, Refusal> {
    let slot = busy(app)?;
    let api_key = build::preconditions(app).await?;
    let message = request::message_of(&body)?;
    let surface = request::surface_of(&body)?;
    let effort = request::effort_of(&body)?;
    let files = attachments::parse_attachments(body.get("attachments"))
        .map_err(|e| Refusal::new(400, e.0, Some(json!({"kind": "input"}))))?;
    let web_search = request::web_search_of(&body);
    let model_id = build::model_id_of(&body)?;
    let (conversation, opened_here) =
        build::conversation_for(app, caller, &body, &message, surface).await?;
    let lines = request::runtime_lines(body.get("chart_context"));
    let key = Turnkey {
        caller: caller.clone(),
        conversation: conversation.clone(),
        surface,
        api_key,
        operator_message: message.clone(),
        trading_asked: request::trading_asked(&body),
        web_search,
        chart_context: body
            .get("chart_context")
            .and_then(Value::as_object)
            .cloned(),
        runtime_lines: lines.clone(),
        model_id,
        effort: effort.clone(),
        require_vision: attachments::has_image(&files),
        resuming: false,
    };
    let parts = match build::run_parts(app, key).await {
        Ok(p) => p,
        Err(r) => {
            if opened_here {
                build::discard_conversation(app, conversation.id, &caller.user).await;
            }
            return Err(r);
        }
    };
    let notices = request::run_options_notice(
        effort.as_deref(),
        &lines,
        web_search,
        attachments::stored_metadata(&files),
    );
    let user_row = build::store_user_message(app, conversation.id, &message, notices).await;
    let input = model_input(&message, &files);
    Ok(launch(
        app,
        parts,
        user_row.map(WireId::Number).unwrap_or_default(),
        Start::Fresh(input),
        slot,
    ))
}

/// What the model reads for the turn: the message, the text files wrapped
/// as attachments after it, and the images beside it.
fn model_input(message: &str, files: &[attachments::Attachment]) -> ChatMessage {
    let block = attachments::prompt_block(files);
    let text = if block.is_empty() {
        message.to_string()
    } else {
        format!("{}\n\n{}", message, block)
    };
    let mut parts = vec![ContentPart::Text(text)];
    for f in files.iter().filter(|f| f.kind == attachments::Kind::Image) {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&f.data);
        parts.push(ContentPart::Binary(Binary::from_base64(
            f.mime.clone(),
            b64,
            Some(f.name.clone()),
        )));
    }
    ChatMessage::user(MessageContent::from_parts(parts))
}

/// Spawn the turn as a task the app owns; the route streams its frames.
fn launch(
    app: &Arc<AppState>,
    parts: RunParts,
    user_message_id: WireId,
    start: Start,
    slot: Slot,
) -> Streaming {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let RunParts {
        ctx,
        model,
        effort,
        system_prompt,
        tools,
        history,
        client,
        session,
    } = parts;
    let guard = app
        .agent
        .runs()
        .register(&ctx.run_id, &session, ctx.cancel.clone());
    let out = FrameOut::new(tx, ctx.cancel.clone());
    let spec = RunSpec {
        ctx,
        model,
        effort,
        system_prompt,
        tools,
        history,
        user_message_id,
        start,
    };
    app.spawn(async move {
        let _registered = guard;
        run::drive(spec, client, out).await;
    });
    Streaming { frames: rx, slot }
}

/// `POST /agent/api/chat/confirm`: resume a paused run with the trader's
/// decisions.
pub async fn resume(
    app: &Arc<AppState>,
    caller: &Caller,
    body: Map<String, Value>,
) -> Result<Streaming, Refusal> {
    let slot = busy(app)?;
    let api_key = build::preconditions(app).await?;
    let run_id = request::text_of(&body, "run_id").trim().to_string();
    if run_id.is_empty() {
        return Err(Refusal::bad_request("A run_id is required"));
    }
    let conversation_id = build::whole_number(body.get("conversation_id"))
        .ok_or_else(|| Refusal::bad_request("conversation_id must be a whole number"))?;
    let conversation = build::owned_conversation(app, conversation_id, &caller.user)
        .await?
        .ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    let session_id = Some(request::text_of(&body, "session_id").trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| conversation.session_id.clone())
        .ok_or_else(|| Refusal::bad_request("A session_id is required to resume a paused run"))?;
    let (decisions, names) = request::decisions_of(body.get("decisions"))?;
    if decisions.is_empty() {
        return Err(Refusal::bad_request("At least one decision is required"));
    }
    let note: Option<String> = Some(
        request::text_of(&body, "note")
            .trim()
            .chars()
            .take(500)
            .collect::<String>(),
    )
    .filter(|n| !n.is_empty());
    let surface = request::surface_of(&body)?;
    let effort = request::effort_of(&body)?;
    let last_user = build::last_user_row(app, conversation.id).await;
    let (stored_effort, stored_lines, stored_web) = request::run_options_of(&last_user.notices);
    let lines = Some(request::runtime_lines(body.get("chart_context")))
        .filter(|l| !l.is_empty())
        .unwrap_or(stored_lines);
    let mut conversation = conversation;
    conversation.session_id = Some(session_id);
    let key = Turnkey {
        caller: caller.clone(),
        conversation,
        surface,
        api_key,
        operator_message: last_user.content.clone(),
        trading_asked: request::trading_asked(&body),
        // The narrower of the stored switch and this request's.
        web_search: stored_web && request::web_search_of(&body),
        chart_context: body
            .get("chart_context")
            .and_then(Value::as_object)
            .cloned(),
        runtime_lines: lines,
        model_id: build::model_id_of(&body)?,
        effort: effort.or(stored_effort),
        require_vision: false,
        resuming: true,
    };
    let mut parts = build::run_parts(app, key).await?;
    let (paused, messages) = build::claim(app, &run_id, &caller.user, conversation_id).await?;
    parts.ctx.run_id = run_id.clone();
    build::record_decisions(
        app,
        caller,
        &parts.ctx,
        &paused,
        &decisions,
        &names,
        note.as_deref(),
    )
    .await;
    let user_message_id = paused.user_message_id.clone();
    let start = Start::Resume(Resume {
        decisions,
        note,
        paused,
        messages,
    });
    Ok(launch(app, parts, user_message_id, start, slot))
}

/// `POST /agent/api/chat/{run_id}/cancel`: stop a running turn. Best
/// effort; a run that already ended or is not this session's answers the
/// same (D-015: only the session that started it stops it).
pub fn cancel(app: &AppState, caller: &Caller, run_id: &str) -> Result<Value, Refusal> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err(Refusal::bad_request("A run_id is required"));
    }
    app.agent.runs().cancel(run_id, &caller.session);
    tracing::info!("Agent run {} cancelled by {}", run_id, caller.user);
    Ok(json!({"status": "success", "message": "Cancellation requested", "run_id": run_id}))
}

/// The paused state a resume claimed, ready for the run.
pub(crate) fn paused_state(row: &store::PausedRow) -> Option<PausedState> {
    serde_json::from_str(&row.pending).ok()
}

#[cfg(test)]
pub(crate) mod tests;
