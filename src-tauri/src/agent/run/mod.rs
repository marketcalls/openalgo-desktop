//! One Agent turn, from the trader's message (or approval) to its last
//! frame (the web's `builder.build_agent` run, `stream.py`'s producer and
//! the blueprint's turn recorder).
//!
//! - [`client`]: the provider connection (genai over the Agent's own
//!   reqwest client).
//! - [`turn`]: the model loop; [`calls`]: a batch of tool calls and the
//!   trader's decisions on a paused one.
//! - [`history`]: earlier turns replayed; [`usage`]: tokens and cost;
//!   [`errors`]: a failure in the trader's words.
//! - [`output`]: frames to the client and the recorder; [`recorder`]: the
//!   stored answer; [`registry`]: the turns running now.
//!
//! [`drive`] runs one turn as a task the app owns. A turn ends one way:
//! `done` (stop, cancelled, incomplete), or a `confirm` with no `done` when
//! it pauses for approval. It is cut off after [`MAX_TURN`] (always on the
//! desktop, D-016), stopped by the trader, by the client going away, or by
//! a sign-out, lock, reset or quit.

pub mod calls;
pub mod client;
pub mod errors;
pub mod history;
pub mod output;
pub mod recorder;
pub mod registry;
pub mod turn;
pub mod usage;

use crate::agent::contract::frames::{DoneReason, ErrorKind, Frame, Requirement, WireId};
use crate::agent::contract::model::ResolvedModel;
use crate::agent::contract::tool::{AgentTool, ToolCtx};
use crate::agent::conversations::store;
use calls::{Calls, PendingCall};
use genai::chat::ChatMessage;
use output::FrameOut;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use turn::{LoopEnd, Turn};

/// Tool calls one run may make (the web's `DEFAULT_TOOL_CALL_LIMIT`).
pub const TOOL_CALL_LIMIT: usize = 25;

/// The longest one turn may run (the web's `MAX_TURN_SECONDS`).
pub const MAX_TURN: Duration = Duration::from_secs(900);

/// How long a paused run waits for the trader (maintainer, 2026-10-27).
pub const PAUSE_EXPIRY_MINUTES: i64 = 10;

/// What a turn cut off by [`MAX_TURN`] ends with.
pub const TURN_TOO_LONG_MESSAGE: &str = "This answer ran longer than OpenAlgo allows for one reply, so it was stopped. Send a follow-up to continue from here.";

/// What a turn cut off because the app is closing ends with.
pub const CLOSING_MESSAGE: &str =
    "OpenAlgo is closing, so this answer was stopped. Send it again after OpenAlgo starts.";

/// What a resume that matched no pending call ends with (kind input).
pub fn no_decision_matched(run_id: &str) -> String {
    format!(
        "No decision in the request matched a pending call on run {}",
        run_id
    )
}

/// The trader's answers to a paused run: call id to approved.
pub type Decisions = HashMap<String, bool>;

/// What a resumed turn settles first: the paused calls, the answers and
/// the trader's note.
type Answered = (Vec<PendingCall>, Decisions, Option<String>);

/// How a turn starts.
pub enum Start {
    /// A new message: its input as the model reads it.
    Fresh(ChatMessage),
    /// The trader answered a paused run.
    Resume(Resume),
}

/// A paused run being answered.
pub struct Resume {
    pub decisions: Decisions,
    pub note: Option<String>,
    pub paused: PausedState,
    /// The run's own messages up to the pause.
    pub messages: Vec<ChatMessage>,
}

/// What a paused run keeps (the `pending` column of `ag_paused_run`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PausedState {
    pub calls: Vec<PendingCall>,
    pub calls_made: usize,
    /// The question that opened the run, for the resumed start frame
    /// (D-036: the web sent `""`).
    pub user_message_id: WireId,
}

/// Everything one turn needs, built by the chat service before the stream
/// opens (every failure that can answer as an HTTP error happens there).
pub struct RunSpec {
    pub ctx: ToolCtx,
    pub model: ResolvedModel,
    pub effort: Option<&'static str>,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub history: Vec<ChatMessage>,
    pub user_message_id: WireId,
    pub start: Start,
}

/// How the turn ended, for the run's stored status.
enum Ending {
    Done(DoneReason),
    /// Paused: the confirm frame goes out once the pause is stored.
    Paused(PausedState, Vec<Requirement>),
    /// Still paused after a resume that matched nothing (already told).
    StillPaused(PausedState),
}

/// Run one turn to its end, sending frames into `out`, then store the run
/// and the answer.
pub async fn drive(spec: RunSpec, client: Arc<client::ModelClient>, mut out: FrameOut) {
    let app = spec.ctx.app.clone();
    let turn_limit = app.agent.turn_limit();
    if matches!(spec.start, Start::Fresh(_)) {
        open_run(&spec.ctx).await;
    }
    out.send(Frame::Start {
        run_id: spec.ctx.run_id.clone(),
        session_id: spec.ctx.session_id.clone(),
        conversation_id: spec
            .ctx
            .conversation_id
            .map(WireId::Number)
            .unwrap_or_default(),
        user_message_id: spec.user_message_id.clone(),
    })
    .await;
    let (start_messages, resume, calls_made) = match spec.start {
        Start::Fresh(input) => (vec![input], None, 0),
        Start::Resume(r) => {
            let made = r.paused.calls_made;
            (
                r.messages,
                Some((r.paused.calls, r.decisions, r.note)),
                made,
            )
        }
    };
    let mut turn = Turn {
        client: &client,
        model: &spec.model,
        effort: spec.effort,
        system: &spec.system_prompt,
        history: &spec.history,
        declared: turn::declare(&spec.tools),
        calls: Calls {
            ctx: &spec.ctx,
            tools: &spec.tools,
            made: calls_made,
        },
        meter: usage::UsageMeter::new(&spec.model),
        messages: start_messages,
    };
    let cancel = spec.ctx.cancel.clone();
    let gone = out.gone();
    let outcome = tokio::select! {
        end = go(&mut turn, resume, &mut out) => Some(end),
        _ = cancel.cancelled() => None,
        _ = gone => {
            tracing::info!("Agent stream client disconnected; cancelling run {}", spec.ctx.run_id);
            cancel.cancel();
            None
        }
        _ = tokio::time::sleep(turn_limit) => {
            cancel.cancel();
            Some(LoopEnd::Failed(Frame::Error { message: TURN_TOO_LONG_MESSAGE.into(), kind: ErrorKind::Internal }))
        }
    };
    let calls_made = turn.calls.made;
    let mut messages = std::mem::take(&mut turn.messages);
    drop(turn);
    let ending = finish(
        outcome,
        &spec.ctx,
        calls_made,
        &spec.user_message_id,
        &mut out,
    )
    .await;
    if let Some(answer) = turn::unanswered(&messages) {
        if matches!(ending, Ending::Done(_)) {
            messages.push(answer);
        }
    }
    persist(&spec.ctx, &messages, ending, out).await;
}

async fn go(turn: &mut Turn<'_>, resume: Option<Answered>, out: &mut FrameOut) -> LoopEnd {
    match resume {
        None => turn.run(out).await,
        Some((pending, decisions, note)) => {
            turn.resume(pending, &decisions, note.as_deref(), out).await
        }
    }
}

/// Record a new run before it starts (`running`), so a crash leaves a run
/// that is closed at the next start and never replayed.
async fn open_run(ctx: &ToolCtx) {
    let Some(conversation_id) = ctx.conversation_id else {
        return;
    };
    let (run_id, session_id, now) = (ctx.run_id.clone(), ctx.session_id.clone(), ctx.app.now());
    if let Err(e) = ctx
        .app
        .blocking(move |c| {
            store::insert_run(
                &*c.sqlite.conn()?,
                &run_id,
                conversation_id,
                &session_id,
                now,
            )
        })
        .await
    {
        tracing::error!("The Agent run could not be recorded: {}", e);
    }
}

/// Send the turn's last frames and say how it ended.
async fn finish(
    outcome: Option<LoopEnd>,
    ctx: &ToolCtx,
    calls_made: usize,
    user_message_id: &WireId,
    out: &mut FrameOut,
) -> Ending {
    let paused = |calls| PausedState {
        calls,
        calls_made,
        user_message_id: user_message_id.clone(),
    };
    match outcome {
        Some(LoopEnd::Completed) => {
            out.send(Frame::Done {
                reason: DoneReason::Stop,
            })
            .await;
            Ending::Done(DoneReason::Stop)
        }
        Some(LoopEnd::Paused(calls)) => {
            let requirements: Vec<Requirement> =
                calls.iter().map(|c| c.requirement.clone()).collect();
            Ending::Paused(paused(calls), requirements)
        }
        Some(LoopEnd::Unmatched(calls)) => {
            out.send(Frame::Error {
                message: no_decision_matched(&ctx.run_id),
                kind: ErrorKind::Input,
            })
            .await;
            out.send(Frame::Done {
                reason: DoneReason::Incomplete,
            })
            .await;
            Ending::StillPaused(paused(calls))
        }
        Some(LoopEnd::Failed(frame)) => {
            out.send(frame).await;
            out.send(Frame::Done {
                reason: DoneReason::Incomplete,
            })
            .await;
            Ending::Done(DoneReason::Incomplete)
        }
        None if ctx.app.shutdown.is_cancelled() => {
            out.send(Frame::Error {
                message: CLOSING_MESSAGE.into(),
                kind: ErrorKind::Internal,
            })
            .await;
            out.send(Frame::Done {
                reason: DoneReason::Incomplete,
            })
            .await;
            Ending::Done(DoneReason::Cancelled)
        }
        None => {
            tracing::info!("Agent run was cancelled");
            out.send(Frame::Done {
                reason: DoneReason::Cancelled,
            })
            .await;
            Ending::Done(DoneReason::Cancelled)
        }
    }
}

/// Store the run's messages and status, a pause, and the answer; never
/// fails the turn (a lost transcript is logged).
async fn persist(ctx: &ToolCtx, messages: &[ChatMessage], ending: Ending, mut out: FrameOut) {
    let app = ctx.app.clone();
    let now = app.now();
    let run_id = ctx.run_id.clone();
    let session_id = ctx.session_id.clone();
    let conversation = ctx.conversation_id;
    let user = ctx.user_id.clone();
    let json = serde_json::to_string(messages).unwrap_or_else(|_| "[]".into());
    let paused = match &ending {
        Ending::Paused(state, requirements) => Some((state.clone(), requirements.clone())),
        Ending::StillPaused(state) => Some((state.clone(), Vec::new())),
        Ending::Done(_) => None,
    };
    let confirm = matches!(ending, Ending::Paused(..));
    let status = match &ending {
        Ending::Paused(..) | Ending::StillPaused(_) => "paused",
        Ending::Done(DoneReason::Stop) => "completed",
        Ending::Done(DoneReason::Cancelled) => "cancelled",
        Ending::Done(DoneReason::Incomplete) => "error",
    };
    // The confirm frame goes out only once the pause is stored, so an
    // approval can never find it missing.
    let saved = save(
        &app,
        SaveRun {
            conversation,
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            user: user.clone(),
            status,
            messages: json,
            paused: paused.clone().map(|(state, _)| state),
            now,
        },
    )
    .await;
    if let Some((_, requirements)) = paused.filter(|_| confirm) {
        if saved {
            out.send(Frame::Confirm {
                run_id: run_id.clone(),
                session_id: session_id.clone(),
                requirements,
            })
            .await;
        } else {
            out.send(Frame::Error {
                message:
                    "The approval could not be saved, so this order was not placed. Ask again."
                        .into(),
                kind: ErrorKind::Internal,
            })
            .await;
            out.send(Frame::Done {
                reason: DoneReason::Incomplete,
            })
            .await;
        }
    }
    store_answer(&app, conversation, &user, out.recorder).await;
}

/// One run's stored state.
struct SaveRun {
    conversation: Option<i64>,
    run_id: String,
    session_id: String,
    user: String,
    status: &'static str,
    messages: String,
    paused: Option<PausedState>,
    now: chrono::DateTime<chrono::Utc>,
}

/// Store the run and, when it paused, its pending calls with their expiry
/// (else release a resume's claim). False when nothing could be stored.
async fn save(app: &Arc<crate::state::AppState>, run: SaveRun) -> bool {
    let Some(conversation_id) = run.conversation else {
        return run.paused.is_none();
    };
    let r = app
        .blocking(move |ctx| {
            let conn = ctx.sqlite.conn()?;
            store::save_run(&conn, &run.run_id, run.status, &run.messages, run.now)?;
            match run.paused {
                Some(state) => store::save_paused(
                    &conn,
                    &store::PausedRow {
                        run_id: run.run_id.clone(),
                        conversation_id,
                        session_id: run.session_id,
                        user_id: run.user,
                        pending: serde_json::to_string(&state)?,
                        paused_at: store::stamp(run.now),
                        expires_at: store::stamp(
                            run.now + chrono::Duration::minutes(PAUSE_EXPIRY_MINUTES),
                        ),
                    },
                ),
                None => store::release_paused(&conn, &run.run_id),
            }
        })
        .await;
    if let Err(e) = &r {
        tracing::error!("The Agent run could not be saved: {}", e);
    }
    r.is_ok()
}

/// The web's `_persist_turn`: bind the conversation to its session and
/// store the answer when there is anything to store.
async fn store_answer(
    app: &Arc<crate::state::AppState>,
    conversation: Option<i64>,
    user: &str,
    recorder: recorder::TurnRecorder,
) {
    let Some(conversation_id) = conversation else {
        return;
    };
    let user = user.to_string();
    let now = app.now();
    let r = app
        .blocking(move |ctx| {
            let conn = ctx.sqlite.conn()?;
            if !recorder.session_id.is_empty() {
                store::update_conversation(
                    &conn,
                    conversation_id,
                    &user,
                    None,
                    Some(&recorder.session_id),
                    now,
                )?;
            }
            if recorder.has_content() {
                store::add_message(
                    &conn,
                    conversation_id,
                    "assistant",
                    recorder.content(),
                    recorder.tools().as_ref(),
                    recorder.sidecar().as_ref(),
                    now,
                )?;
            }
            Ok(())
        })
        .await;
    if let Err(e) = r {
        tracing::error!(
            "Could not persist the agent turn for conversation {}: {}",
            conversation_id,
            e
        );
    }
}

#[cfg(test)]
mod tests;
