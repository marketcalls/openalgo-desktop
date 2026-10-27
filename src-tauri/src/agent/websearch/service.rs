//! What the `/agent/api/websearch*` routes answer (the web search part of
//! the web's `blueprints/agent.py`), each in the web's envelope. Keys go in
//! through their own routes, never through the settings payload, and come
//! back as a flag and a fingerprint only.

use super::settings::{self, WriteError};
use super::{probe, provider_spec};
use crate::agent::models::success;
use crate::agent::reply::Refusal;
use crate::security::Secret;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const READ_FAILED: &str = "Could not read the web search configuration";
const SAVE_FAILED: &str = "Could not save the web search configuration";
const TEST_FAILED: &str = "Could not run the web search test";

/// The longest test message returned (the web's `MAX_TEST_ERROR_CHARS`).
const MAX_TEST_MESSAGE_CHARS: usize = 2000;

fn config(app: &AppState) -> Result<Value, Refusal> {
    settings::config(app).map_err(|e| {
        tracing::error!("{}: {}", READ_FAILED, e);
        Refusal::new(500, READ_FAILED, None)
    })
}

/// `GET /agent/api/websearch`.
pub fn get(app: &AppState) -> Result<Value, Refusal> {
    Ok(success(
        json!({"data": config(app)?, "defaults": settings::defaults()}),
    ))
}

/// `PUT /agent/api/websearch`: a partial update, all or nothing.
pub fn put(app: &AppState, body: &Map<String, Value>) -> Result<Value, Refusal> {
    if body.is_empty() {
        return Err(Refusal::bad_request("Nothing to update"));
    }
    settings::update(app, body).map_err(|e| match e {
        WriteError::Invalid(m) => Refusal::bad_request(m),
        WriteError::Store(_) => Refusal::new(500, SAVE_FAILED, None),
    })?;
    Ok(success(json!({"data": config(app)?})))
}

/// A provider in a path, or the 400 naming the three.
pub fn checked_provider(raw: &str) -> Result<&'static super::ProviderSpec, Refusal> {
    provider_spec(raw).map_err(Refusal::bad_request)
}

fn write_refusal(e: WriteError) -> Refusal {
    match e {
        WriteError::Invalid(m) => Refusal::bad_request(m),
        WriteError::Store(m) => Refusal::new(500, m, None),
    }
}

/// `PUT /agent/api/websearch/providers/<provider>/key`.
pub fn put_key(
    app: &AppState,
    provider: &str,
    body: &Map<String, Value>,
) -> Result<Value, Refusal> {
    let spec = checked_provider(provider)?;
    settings::set_key(app, spec.id, body.get("api_key")).map_err(write_refusal)?;
    Ok(success(json!({
        "data": config(app)?,
        "message": format!("{} key stored", spec.label),
    })))
}

/// `DELETE /agent/api/websearch/providers/<provider>/key` (idempotent).
pub fn delete_key(app: &AppState, provider: &str) -> Result<Value, Refusal> {
    let spec = checked_provider(provider)?;
    settings::clear_key(app, spec.id).map_err(write_refusal)?;
    Ok(success(json!({
        "data": config(app)?,
        "message": format!("{} key cleared", spec.label),
    })))
}

/// The typed key a test body may carry: absent, or a string.
pub fn typed_key(body: Option<&Value>) -> Result<Option<Secret>, Refusal> {
    match body {
        None => Ok(None),
        Some(Value::Object(m)) => match m.get("api_key") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(Secret::new(s.as_str()))),
            Some(_) => Err(Refusal::bad_request("api_key must be a string")),
        },
        Some(_) => Err(Refusal::bad_request(
            "The request body must be a JSON object",
        )),
    }
}

/// A typed key out of a message that might quote it, then capped.
fn cleaned(message: &str, typed: Option<&Secret>) -> String {
    let text = match typed.map(|k| k.expose()).filter(|k| k.chars().count() >= 8) {
        Some(k) => message.replace(k, "[redacted]"),
        None => message.to_string(),
    };
    text.chars().take(MAX_TEST_MESSAGE_CHARS).collect()
}

/// `POST /agent/api/websearch/providers/<provider>/test`: one real query.
/// A failing provider is a 200 with `ok` false.
pub async fn test(
    app: &Arc<AppState>,
    provider: &str,
    typed: Option<Secret>,
) -> Result<Value, Refusal> {
    let spec = checked_provider(provider)?;
    let shown = typed.clone();
    let p = probe::probe(app, spec.id, typed)
        .await
        .map_err(Refusal::bad_request)?;
    let message = cleaned(&p.message, shown.as_ref());
    if p.ok {
        tracing::info!(
            "Web search provider {} passed its test in {} ms",
            p.provider,
            p.latency_ms
        );
    } else {
        tracing::error!(
            "Web search provider {} failed its test: {}",
            p.provider,
            message
        );
    }
    let data = app
        .blocking(|c| Ok(config(c)))
        .await
        .map_err(|_| Refusal::new(500, TEST_FAILED, None))??;
    Ok(success(json!({
        "ok": p.ok,
        "provider": p.provider,
        "message": message,
        "latency_ms": p.latency_ms,
        "result_count": p.result_count,
        "data": data,
    })))
}
