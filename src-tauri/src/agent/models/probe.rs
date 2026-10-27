//! A model's credential test (`POST /agent/api/models/<id>/test`, the web's
//! `test_model`): the cheapest real request the provider allows, streamed
//! because a run only ever streams (a test on a path the product never takes
//! can fail on a defect no trader meets, and pass over one they would), at
//! most one token, no retries, 30 seconds, every chunk read (an unread
//! stream passes a key the provider then rejects).
//!
//! A failure keeps the provider's own words, because "invalid API key" and
//! "model not found" need different fixes; only the key itself is taken out
//! of them, and they are capped at 2,000 characters. The outcome is recorded
//! on the row: a first pass makes an enabled model the default when none is,
//! a failing default hands the default on.

use super::secrets;
use super::service::model_json;
use super::store::{self, NOT_FOUND};
use super::success;
use crate::agent::contract::model::ResolvedModel;
use crate::agent::providers::validate_provider_config;
use crate::agent::reply::Refusal;
use crate::agent::resolver::resolve_row;
use crate::agent::run::client::StreamOpenError;
use crate::state::AppState;
use futures_util::StreamExt;
use genai::chat::ChatRequest;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a test waits for the provider: long enough for a cold local
/// Ollama to load a model.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most of a provider's message that is stored and shown.
pub const MAX_TEST_ERROR_CHARS: usize = 2000;

/// What a passing test says.
pub const PASSED: &str = "The provider accepted the request";

fn internal(e: impl std::fmt::Display) -> Refusal {
    tracing::error!("An Agent model test could not run: {}", e);
    Refusal::new(500, "The test could not be run. Try again.", None)
}

/// Before anything is sent: the answer when the row cannot be sent at all,
/// or the model to send to.
enum Prepared {
    Answered(Value),
    Ready(Box<ResolvedModel>),
}

/// The web's order: a configuration the gate refuses is a failed test with
/// the row; one that cannot be turned into a request is a failed test
/// without it (the web's `litellm_kwargs` path).
fn prepare(app: &Arc<AppState>, c: &AppState, id: i64) -> Result<Prepared, Refusal> {
    let conn = c.sqlite.conn().map_err(internal)?;
    let row = store::get(&conn, id)
        .map_err(internal)?
        .ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    let found = secrets::describe(&conn, &row.secret_names()).map_err(internal)?;
    let has_key = found.values().any(|s| s.has_value);
    if let Some(problem) = validate_provider_config(
        &row.provider_kind,
        &row.model_name,
        row.base_url.as_deref(),
        has_key,
    ) {
        record(&conn, id, false, Some(problem.as_str()), c);
        let data = model_json(&conn, &row_after(&conn, id)?)?;
        return Ok(Prepared::Answered(answer(false, &problem, 0, Some(data))));
    }
    drop(conn);
    match resolve_row(app, &row) {
        Ok(model) => Ok(Prepared::Ready(Box::new(model))),
        Err(e) => {
            let conn = c.sqlite.conn().map_err(internal)?;
            record(&conn, id, false, Some(e.message()), c);
            Ok(Prepared::Answered(answer(false, e.message(), 0, None)))
        }
    }
}

fn row_after(conn: &rusqlite::Connection, id: i64) -> Result<store::ModelRow, Refusal> {
    store::get(conn, id)
        .map_err(internal)?
        .ok_or_else(|| Refusal::new(404, NOT_FOUND, None))
}

fn record(conn: &rusqlite::Connection, id: i64, ok: bool, error: Option<&str>, c: &AppState) {
    if let Err(e) = store::record_test(conn, id, ok, error, c.now()) {
        tracing::warn!("An Agent model test result could not be recorded: {}", e);
    }
}

fn answer(ok: bool, message: &str, latency_ms: u64, data: Option<Value>) -> Value {
    let mut body = json!({ "ok": ok, "message": message, "latency_ms": latency_ms });
    if let Some(d) = data {
        body["data"] = d;
    }
    success(body)
}

/// `POST /agent/api/models/<id>/test`. Always 200 once the model exists:
/// `ok` says whether the provider accepted the request.
pub async fn test(app: &Arc<AppState>, id: i64) -> Result<Value, Refusal> {
    test_within(app, id, TEST_TIMEOUT).await
}

/// [`test`] with its own time limit (a test of the limit).
pub async fn test_within(app: &Arc<AppState>, id: i64, limit: Duration) -> Result<Value, Refusal> {
    let ctx = app.clone();
    let prepared = app
        .blocking(move |c| Ok(prepare(&ctx, c, id)))
        .await
        .map_err(internal)??;
    let model = match prepared {
        Prepared::Answered(body) => return Ok(body),
        Prepared::Ready(model) => model,
    };
    let started = Instant::now();
    let outcome = send(app, &model, limit).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let (ok, message) = match outcome {
        Ok(()) => (true, PASSED.to_string()),
        Err(text) => (false, clean(&model, text).await),
    };
    if !ok {
        tracing::warn!(
            "Agent model {} failed its credential test: {}",
            model.display_name,
            message
        );
    }
    let stored = message.clone();
    let data = app
        .blocking(move |c| {
            let conn = c.sqlite.conn()?;
            record(&conn, id, ok, (!ok).then_some(stored.as_str()), c);
            Ok(row_after(&conn, id).and_then(|row| model_json(&conn, &row)))
        })
        .await
        .map_err(internal)??;
    Ok(answer(ok, &message, latency_ms, Some(data)))
}

/// One request, capped at one token, every chunk read, within the limit.
async fn send(app: &Arc<AppState>, model: &ResolvedModel, limit: Duration) -> Result<(), String> {
    let client = app.agent.client()?;
    let exchange = async {
        let opened = client
            .stream_with(model, ChatRequest::from_user("ping"), None, |o| {
                o.with_max_tokens(1)
            })
            .await
            .map_err(|e| match e {
                StreamOpenError::Credentials(e) => e.message().to_string(),
                StreamOpenError::Provider(e) => provider_text(&e),
            })?;
        let mut stream = opened.stream;
        while let Some(event) = stream.next().await {
            event.map_err(|e| provider_text(&e))?;
        }
        Ok(())
    };
    match tokio::time::timeout(limit, exchange).await {
        Ok(result) => result,
        Err(_) => {
            let secs = limit.as_secs().max(1);
            Err(format!(
                "The provider did not answer within {} second{}",
                secs,
                if secs == 1 { "" } else { "s" }
            ))
        }
    }
}

/// The provider's own words: the `error.message` of a JSON error body when
/// there is one, else the body, else the error as genai reports it.
fn provider_text(e: &genai::Error) -> String {
    // A streamed request's refusal arrives wrapped in the stream's error.
    if let genai::Error::WebStream { error, .. } = e {
        if let Some(inner) = error.downcast_ref::<genai::Error>() {
            return provider_text(inner);
        }
    }
    let failure = crate::agent::run::errors::read_genai_error(e);
    let parsed: Option<Value> = serde_json::from_str(failure.text.trim()).ok();
    let from_json = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.pointer("/message"))
            .or_else(|| v.pointer("/error"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    from_json
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            if failure.text.trim().is_empty() {
                e.to_string()
            } else {
                failure.text
            }
        })
}

/// The key taken out of the provider's words (it may quote the one it
/// refused), secret-shaped text too, and the length capped.
async fn clean(model: &ResolvedModel, text: String) -> String {
    let mut text = text;
    if let Ok(auth) = model.credentials.fetch().await {
        if let Some(key) = auth.key.as_ref().filter(|k| k.len() >= 8) {
            text = text.replace(key.expose(), "[redacted]");
        }
    }
    let text = crate::agent::redact::redact_text(&text);
    text.chars().take(MAX_TEST_ERROR_CHARS).collect()
}
