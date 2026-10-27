//! Conversations, messages and the runs behind them (the web's
//! `database/agent_db.py` conversation part, agno's session store, and the
//! conversation routes of `blueprints/agent.py`).
//!
//! - [`store`]: the tables and their SQL (migration `097_agent_core`).
//! - The functions here are what the routes call: each reads its query or
//!   body, runs the store on the blocking pool and answers the web's shape.

pub mod store;

use crate::agent::reply::{Refusal, NOT_FOUND};
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Messages a conversation answers with, newest kept (the web's
/// `list_messages` limit).
pub const MAX_MESSAGES: i64 = 500;

/// The surfaces a conversation may belong to.
pub const SURFACES: [&str; 3] = ["chat", "chart", "voice"];

fn surface_error() -> Refusal {
    Refusal::bad_request("surface must be one of: chat, chart, voice")
}

fn failed(what: &'static str) -> impl Fn(crate::error::AppError) -> Refusal {
    move |e| {
        tracing::error!("Agent conversations: {} failed: {}", what, e);
        Refusal::new(500, what, None)
    }
}

/// `GET /agent/api/conversations?surface=chat,voice&limit=100`.
pub async fn list(
    app: &Arc<AppState>,
    user: &str,
    surface: Option<&str>,
    limit: Option<&str>,
) -> Result<Value, Refusal> {
    let surfaces: Vec<String> = surface
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if surfaces.iter().any(|s| !SURFACES.contains(&s.as_str())) {
        return Err(surface_error());
    }
    let limit = match limit {
        None => 100,
        Some(raw) => raw
            .trim()
            .parse::<i64>()
            .map_err(|_| Refusal::bad_request("limit must be a whole number"))?
            .clamp(1, 200),
    };
    let user = user.to_string();
    let rows = app
        .blocking(move |ctx| {
            store::list_conversations(&*ctx.sqlite.conn()?, &user, &surfaces, limit)
        })
        .await
        // The web answers an empty list when the read fails.
        .unwrap_or_else(|e| {
            tracing::error!("Could not list agent conversations: {}", e);
            Vec::new()
        });
    Ok(
        json!({"status": "success", "data": rows.iter().map(store::Conversation::to_json).collect::<Vec<_>>()}),
    )
}

/// `POST /agent/api/conversations` `{surface, title}` (201).
pub async fn create(
    app: &Arc<AppState>,
    user: &str,
    body: &Map<String, Value>,
) -> Result<Value, Refusal> {
    let surface = crate::agent::chat::request::text_of(body, "surface")
        .trim()
        .to_lowercase();
    let surface = if surface.is_empty() {
        "chat".to_string()
    } else {
        surface
    };
    if !SURFACES.contains(&surface.as_str()) {
        return Err(surface_error());
    }
    let title: String = crate::agent::chat::request::text_of(body, "title")
        .trim()
        .chars()
        .take(300)
        .collect();
    let (user, now) = (user.to_string(), app.now());
    let row = app
        .blocking(move |ctx| {
            let title = Some(title.as_str()).filter(|t| !t.is_empty());
            store::create_conversation(&*ctx.sqlite.conn()?, &user, title, &surface, now)
        })
        .await
        .map_err(failed("Could not create the conversation"))?;
    Ok(json!({"status": "success", "data": row.to_json()}))
}

/// `GET /agent/api/conversations/{id}`: the conversation and its messages,
/// oldest first.
pub async fn get(app: &Arc<AppState>, user: &str, id: i64) -> Result<Value, Refusal> {
    let user = user.to_string();
    let found = app
        .blocking(move |ctx| {
            let conn = ctx.sqlite.conn()?;
            match store::get_conversation(&conn, id, &user)? {
                None => Ok(None),
                Some(c) => Ok(Some((c, store::list_messages(&conn, id, MAX_MESSAGES)?))),
            }
        })
        .await
        .map_err(failed("Could not read the conversation"))?;
    let (conv, messages) = found.ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    Ok(json!({
        "status": "success",
        "data": conv.to_json(),
        "messages": messages.iter().map(store::Message::to_json).collect::<Vec<_>>(),
    }))
}

/// `DELETE /agent/api/conversations/{id}`.
pub async fn delete(app: &Arc<AppState>, user: &str, id: i64) -> Result<Value, Refusal> {
    let user = user.to_string();
    let removed = app
        .blocking(move |ctx| store::delete_conversation(&*ctx.sqlite.conn()?, id, &user))
        .await
        .map_err(failed("Could not delete the conversation"))?;
    if !removed {
        return Err(Refusal::new(404, NOT_FOUND, None));
    }
    Ok(json!({"status": "success", "message": "Conversation deleted"}))
}

/// `DELETE /agent/api/conversations/{id}/messages/{message_id}`: remove a
/// message and everything after it from both stores, the transcript and
/// the runs the model replays, so an edited question is not answered from
/// the history it replaced.
pub async fn truncate(
    app: &Arc<AppState>,
    user: &str,
    id: i64,
    message_id: i64,
) -> Result<Value, Refusal> {
    let user = user.to_string();
    let done = app
        .blocking(move |ctx| {
            let conn = ctx.sqlite.conn()?;
            if store::get_conversation(&conn, id, &user)?.is_none() {
                return Ok(None);
            }
            let removed = store::truncate_from(&conn, id, message_id)?;
            let runs: Vec<String> = removed
                .iter()
                .filter_map(|m| m.notices.as_array())
                .flatten()
                .filter(|e| e.get("type").and_then(Value::as_str) == Some("run"))
                .filter_map(|e| e.get("run_id").and_then(Value::as_str))
                .filter(|r| !r.is_empty())
                .map(str::to_string)
                .collect();
            // A run that will not go stays out of nothing worse than the
            // history window; the transcript is already right.
            let forgotten = store::delete_runs(&conn, &runs).unwrap_or_else(|e| {
                tracing::error!("Could not forget agent runs: {}", e);
                0
            });
            Ok(Some((removed.len(), forgotten)))
        })
        .await
        .map_err(failed("Could not truncate the conversation"))?;
    let (removed, forgotten) = done.ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    tracing::info!(
        "Truncated conversation {} from message {}: {} messages, {} runs forgotten",
        id,
        message_id,
        removed,
        forgotten
    );
    Ok(json!({"status": "success", "removed": removed, "runs_forgotten": forgotten}))
}
