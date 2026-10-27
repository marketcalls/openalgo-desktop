//! What the model routes do (the web's `list_models`, `create_model`,
//! `update_model`, `delete_model` and `set_default_model` routes): read the
//! request as the web reads it, keep the registry's rules, store a key where
//! the trader asked, and answer with the web's body and status.
//!
//! A key is written before the row when it is the provider's shared key (a
//! failed registration cannot leave a model without its credential) and
//! after it when it is the model's own (it needs the row's id). No answer
//! carries a key, only whether one is stored and its fingerprint.

use super::secrets::{self, model_secret_name, provider_secret_name, SecretInfo};
use super::store::{self, ModelChanges, ModelRow, NewModel, NOT_FOUND};
use super::{success, text_field, truthy};
use crate::agent::catalog::{self, Catalog};
use crate::agent::providers::base_url::validate_base_url;
use crate::agent::providers::{capable, catalog_id, Probe, PROVIDER_KINDS};
use crate::agent::reply::Refusal;
use crate::agent::settings::schema::REASONING_EFFORTS;
use crate::state::AppState;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Where a pasted key is stored: `provider` (the default; one paste serves
/// every model of the kind) or `model` (an override that wins).
const SCOPES: &[&str] = &["provider", "model"];

/// The flags a request may set on a row (an allowlist: the default and the
/// test result have their own paths).
const FLAGS: &[&str] = &[
    "enabled",
    "supports_reasoning",
    "supports_vision",
    "tools_unreliable",
];

/// Desktop bounds on what one row stores (the web's columns are
/// `String(200)` and `String(500)`, which SQLite does not enforce).
const MAX_NAME_CHARS: usize = 200;
const MAX_BASE_URL_CHARS: usize = 500;

fn internal(e: impl std::fmt::Display) -> Refusal {
    tracing::error!("Agent model registry failed: {}", e);
    Refusal::new(500, "Could not read the models", None)
}

/// One row as the routes answer it, with `supports_reasoning` resolved
/// against the catalogue (the web's `_with_resolved_capabilities`): the
/// catalogue decides for a model it knows, the stored box for one it does
/// not. The column keeps the trader's choice.
fn row_json(catalog: &Catalog, row: &ModelRow, secrets: &HashMap<String, SecretInfo>) -> Value {
    let mut out = row.to_json(secrets);
    if let Ok(id) = catalog_id(&row.provider_kind, &row.model_name) {
        out["supports_reasoning"] = Value::Bool(capable(
            catalog,
            &id,
            row.supports_reasoning,
            Probe::Reasoning,
        ));
    }
    out
}

/// One row as the routes answer it.
pub fn model_json(conn: &Connection, row: &ModelRow) -> Result<Value, Refusal> {
    let secrets = secrets::describe(conn, &row.secret_names()).map_err(internal)?;
    Ok(row_json(&catalog::current(), row, &secrets))
}

fn reload(conn: &Connection, id: i64) -> Result<Value, Refusal> {
    match store::get(conn, id).map_err(internal)? {
        Some(row) => model_json(conn, &row),
        None => Err(Refusal::new(404, NOT_FOUND, None)),
    }
}

/// `GET /agent/api/models`: every model, the default first.
pub fn list(app: &AppState) -> Result<Value, Refusal> {
    let conn = app.sqlite.conn().map_err(internal)?;
    let rows = store::list(&conn, false).map_err(internal)?;
    let mut names: Vec<String> = Vec::new();
    for r in &rows {
        names.extend(r.secret_names());
    }
    let secrets = secrets::describe(&conn, &names).map_err(internal)?;
    let catalog = catalog::current();
    let data: Vec<Value> = rows
        .iter()
        .map(|r| row_json(&catalog, r, &secrets))
        .collect();
    Ok(success(json!({ "data": data })))
}

/// The key a request carries and where it goes.
struct PastedKey {
    value: String,
    scope: &'static str,
}

fn pasted_key(body: &Map<String, Value>) -> Result<PastedKey, Refusal> {
    let asked = text_field(body, "api_key_scope").to_lowercase();
    let asked = if asked.is_empty() {
        "provider".to_string()
    } else {
        asked
    };
    let Some(scope) = SCOPES.iter().find(|s| **s == asked) else {
        return Err(Refusal::bad_request(format!(
            "api_key_scope must be one of: {}",
            SCOPES.join(", ")
        )));
    };
    Ok(PastedKey {
        value: text_field(body, "api_key"),
        scope,
    })
}

fn checked_effort(raw: String) -> Result<String, Refusal> {
    let effort = raw.to_lowercase();
    if REASONING_EFFORTS.contains(&effort.as_str()) {
        Ok(effort)
    } else {
        Err(Refusal::bad_request(format!(
            "default_reasoning_effort must be one of: {}",
            REASONING_EFFORTS.join(", ")
        )))
    }
}

fn checked_length(value: &str, max: usize, what: &str) -> Result<(), Refusal> {
    if value.chars().count() > max {
        return Err(Refusal::bad_request(format!(
            "{} can be at most {} characters",
            what, max
        )));
    }
    Ok(())
}

/// The base URL a request names (a non-text value is none, as the web
/// reads it), through the guard.
fn checked_base_url(body: &Map<String, Value>, kind: &str) -> Result<Option<String>, Refusal> {
    let raw = body.get("base_url").and_then(Value::as_str);
    let url = validate_base_url(raw, kind).map_err(Refusal::bad_request)?;
    if let Some(u) = &url {
        checked_length(u, MAX_BASE_URL_CHARS, "A base URL")?;
    }
    Ok(url)
}

/// The registration as the web reads it, checked in its order.
fn new_model(body: &Map<String, Value>) -> Result<NewModel, Refusal> {
    let kind = text_field(body, "provider_kind");
    if !PROVIDER_KINDS.contains(&kind.as_str()) {
        return Err(Refusal::bad_request(format!(
            "provider_kind must be one of: {}",
            PROVIDER_KINDS.join(", ")
        )));
    }
    let model_name = text_field(body, "model_name");
    if model_name.is_empty() {
        return Err(Refusal::bad_request("A model name is required"));
    }
    checked_length(&model_name, MAX_NAME_CHARS, "A model name")?;
    let display = text_field(body, "display_name");
    let display_name = if display.is_empty() {
        model_name.clone()
    } else {
        display
    };
    checked_length(&display_name, MAX_NAME_CHARS, "A display name")?;
    let base_url = checked_base_url(body, &kind)?;
    let effort = text_field(body, "default_reasoning_effort");
    let effort = checked_effort(if effort.is_empty() {
        "off".into()
    } else {
        effort
    })?;
    let flag = |k: &str| body.get(k).map(truthy);
    Ok(NewModel {
        provider_kind: kind,
        model_name,
        display_name,
        base_url,
        default_reasoning_effort: effort,
        enabled: flag(FLAGS[0]),
        supports_reasoning: flag(FLAGS[1]),
        supports_vision: flag(FLAGS[2]),
        tools_unreliable: flag(FLAGS[3]),
    })
}

/// `POST /agent/api/models`: register a model (never the default: it has
/// not been tested), storing the key where the trader asked. 201.
pub fn create(app: &AppState, body: &Map<String, Value>) -> Result<Value, Refusal> {
    let model = new_model(body)?;
    let key = pasted_key(body)?;
    let conn = app.sqlite.conn().map_err(internal)?;
    if !key.value.is_empty() && key.scope == "provider" {
        secrets::set(
            &conn,
            &app.security,
            &provider_secret_name(&model.provider_kind),
            &key.value,
        )
        .map_err(|m| Refusal::new(500, m, None))?;
    }
    let created = store::create(&conn, &model, app.now()).map_err(|m| {
        let status = if m.contains("already registered") {
            409
        } else {
            400
        };
        Refusal::new(status, m, None)
    })?;
    if !key.value.is_empty() && key.scope == "model" {
        if let Err(m) = secrets::set(
            &conn,
            &app.security,
            &model_secret_name(created.id),
            &key.value,
        ) {
            // The row stands: the key can be set by an edit, and saying so
            // beats deleting a model the trader asked for.
            let data = model_json(&conn, &created)?;
            return Err(Refusal::new(
                500,
                format!(
                    "{} was registered but its API key could not be stored: {}",
                    model.display_name, m
                ),
                Some(json!({ "data": data })),
            ));
        }
    }
    tracing::info!(
        "Agent model registered: {} ({})",
        model.display_name,
        model.provider_kind
    );
    Ok(success(json!({ "data": reload(&conn, created.id)? })))
}

/// The edit a request asks for, as the web reads it.
fn changes_of(body: &Map<String, Value>, row: &ModelRow) -> Result<ModelChanges, Refusal> {
    let mut c = ModelChanges::default();
    if body.contains_key("display_name") {
        let name = text_field(body, "display_name");
        checked_length(&name, MAX_NAME_CHARS, "A display name")?;
        c.display_name = Some(name);
    }
    if body.contains_key("base_url") {
        c.base_url = Some(checked_base_url(body, &row.provider_kind)?);
    }
    if body.contains_key("default_reasoning_effort") {
        c.default_reasoning_effort = Some(checked_effort(text_field(
            body,
            "default_reasoning_effort",
        ))?);
    }
    let flag = |k: &str| body.get(k).map(truthy);
    c.enabled = flag("enabled");
    c.supports_reasoning = flag("supports_reasoning");
    c.supports_vision = flag("supports_vision");
    c.tools_unreliable = flag("tools_unreliable");
    // Forwarded so the store refuses them by name rather than a 200 for a
    // change that did not happen.
    c.is_default = body.contains_key("is_default");
    c.provider_kind = body.get("provider_kind").cloned();
    c.model_name = body.get("model_name").cloned();
    Ok(c)
}

/// `PATCH /agent/api/models/<id>`: edit a model; a blank key keeps the
/// stored one.
pub fn update(app: &AppState, id: i64, body: &Map<String, Value>) -> Result<Value, Refusal> {
    let conn = app.sqlite.conn().map_err(internal)?;
    let row = store::get(&conn, id)
        .map_err(internal)?
        .ok_or_else(|| Refusal::new(404, NOT_FOUND, None))?;
    let changes = changes_of(body, &row)?;
    let key = pasted_key(body)?;
    if changes.is_empty() && key.value.is_empty() {
        return Err(Refusal::bad_request("Nothing to update"));
    }
    if !changes.is_empty() {
        store::update(&conn, id, &changes, app.now()).map_err(|m| {
            let status = if m.contains("already registered") {
                409
            } else {
                400
            };
            Refusal::new(status, m, None)
        })?;
    }
    if !key.value.is_empty() {
        let name = if key.scope == "model" {
            model_secret_name(id)
        } else {
            provider_secret_name(&row.provider_kind)
        };
        secrets::set(&conn, &app.security, &name, &key.value)
            .map_err(|m| Refusal::new(500, m, None))?;
    }
    Ok(success(json!({ "data": reload(&conn, id)? })))
}

/// `DELETE /agent/api/models/<id>`.
pub fn delete(app: &AppState, id: i64) -> Result<Value, Refusal> {
    let conn = app.sqlite.conn().map_err(internal)?;
    store::delete(&conn, id).map_err(|m| {
        let status = if m == NOT_FOUND { 404 } else { 500 };
        Refusal::new(status, m, None)
    })?;
    tracing::info!("Agent model {} deleted", id);
    Ok(success(json!({ "message": "Model deleted" })))
}

/// `POST /agent/api/models/<id>/default`.
pub fn set_default(app: &AppState, id: i64) -> Result<Value, Refusal> {
    let conn = app.sqlite.conn().map_err(internal)?;
    store::set_default(&conn, id).map_err(|m| {
        let status = if m == NOT_FOUND { 404 } else { 409 };
        Refusal::new(status, m, None)
    })?;
    Ok(success(json!({ "data": reload(&conn, id)? })))
}
