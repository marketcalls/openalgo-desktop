//! What the `/flow/api/...` routes do (the web's `blueprints/flow.py`),
//! answering the web's status codes and JSON shapes. The routes parse the
//! request and call one function here; nothing here knows HTTP beyond the
//! [`Reply`] it builds.
//!
//! | File | Routes |
//! |---|---|
//! | `mod.rs` | list, create, get, update, delete, run, execution history |
//! | `transfer` | export, import, replace |
//! | `triggers` | re-registering a changed trigger, rolling an activation back |
//! | `activation` | activate, deactivate, the monitors' status |
//! | `webhooks` | the webhook settings and the public webhook |
//! | `webhook_guard` | the public webhook's lockout after wrong secrets |

pub mod activation;
pub mod transfer;
pub mod triggers;
pub mod webhook_guard;
pub mod webhooks;

use crate::error::Result;
use crate::flow::contract::model::Workflow;
use crate::flow::contract::pyfmt;
use crate::flow::executor::{self, RunRequest};
use crate::flow::store::{executions, workflows};
use crate::flow::validator::{self, ValidateOptions, ValidationError};
use crate::security::Secret;
use crate::services::core::Reply;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// What a request body was.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// Empty, or JSON Python reads as false (`null`, `{}`, `[]`, `""`, `0`):
    /// the web's `if not data`.
    Nothing,
    /// Not JSON at all (bare `NaN` and `Infinity` included, which the web's
    /// parser takes and the desktop's does not).
    Unreadable,
    Json(Value),
}

impl Body {
    pub fn parse(bytes: &[u8]) -> Body {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Body::Nothing;
        }
        match serde_json::from_slice::<Value>(bytes) {
            Ok(v) if pyfmt::truthy(&v) => Body::Json(v),
            Ok(_) => Body::Nothing,
            Err(_) => Body::Unreadable,
        }
    }
}

/// The web's `{"error": "No data provided"}` (400).
pub fn no_data() -> Reply {
    Reply::new(400, json!({"error": "No data provided"}))
}

/// What a body that is not JSON answers, in the shape of the route's own
/// refusal (`error_title` is "Invalid workflow structure" or "Invalid
/// workflow format").
pub fn unreadable(error_title: &str) -> Reply {
    Reply::new(
        400,
        json!({
            "status": "error",
            "error": error_title,
            "message": "The workflow is not valid JSON (values such as NaN or Infinity are not allowed). Check it and try again.",
        }),
    )
}

/// A validator refusal: `{status, error, message: first message, errors}`.
pub fn invalid(error_title: &str, errors: &[ValidationError]) -> Reply {
    Reply::new(
        400,
        json!({
            "status": "error",
            "error": error_title,
            "message": errors.first().map(|e| e.message.clone()).unwrap_or_default(),
            "errors": errors,
        }),
    )
}

/// A body that is JSON but not an object (create, PUT): the validator's
/// one error, "Workflow must be a JSON object".
fn not_an_object(v: &Value) -> Reply {
    invalid(
        "Invalid workflow structure",
        &validator::validate_workflow(v, ValidateOptions::COMPLETE),
    )
}

fn workflow_not_found() -> Reply {
    Reply::new(404, json!({"error": "Workflow not found"}))
}

fn failed(what: &str, e: crate::error::AppError) -> Reply {
    tracing::error!("Flow could not {}: {}", what, e);
    Reply::new(500, json!({"error": format!("Failed to {}", what)}))
}

/// The workflow as the create and update routes answer it (no
/// `schedule_job_id`).
fn saved_json(w: &Workflow) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("id".into(), json!(w.id));
    m.insert("name".into(), json!(w.name));
    m.insert("description".into(), json!(w.description));
    m.insert("nodes".into(), w.nodes.clone());
    m.insert("edges".into(), w.edges.clone());
    m.insert("is_active".into(), json!(w.is_active));
    m.insert("webhook_token".into(), json!(w.webhook_token));
    m.insert(
        "webhook_secret".into(),
        json!(w.webhook_secret.as_ref().map(|s| s.expose().to_string())),
    );
    m.insert("webhook_enabled".into(), json!(w.webhook_enabled));
    m.insert("webhook_auth_type".into(), json!(w.webhook_auth_type));
    m.insert("created_at".into(), json!(w.created_at));
    m.insert("updated_at".into(), json!(w.updated_at));
    m
}

/// A stored workflow, `None` when there is none.
pub async fn load(app: &Arc<AppState>, id: i64) -> Result<Option<Workflow>> {
    app.blocking(move |c| workflows::get(&*c.sqlite.conn()?, &c.security, id))
        .await
}

/// The trader's API key, the one every run trades with.
pub async fn api_key(app: &Arc<AppState>) -> Option<Secret> {
    match app
        .blocking(crate::services::apikey_service::ApiKeyService::current)
        .await
    {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!("Flow could not read the API key: {}", e);
            None
        }
    }
}

/// `GET /flow/api/workflows`: every workflow, newest change first, with
/// its last run's status.
pub async fn list(app: &Arc<AppState>) -> Reply {
    match app.blocking(|c| workflows::list(&*c.sqlite.conn()?)).await {
        Ok(rows) => Reply::new(
            200,
            Value::Array(
                rows.into_iter()
                    .map(|w| {
                        json!({
                            "id": w.id, "name": w.name, "description": w.description,
                            "is_active": w.is_active, "webhook_enabled": w.webhook_enabled,
                            "created_at": w.created_at, "updated_at": w.updated_at,
                            "last_execution_status": w.last_execution_status,
                        })
                    })
                    .collect(),
            ),
        ),
        // A failed read is an error, never an empty list (SM-04).
        Err(e) => failed("list workflows", e),
    }
}

/// Make a workflow (with a new webhook token and secret); shared by
/// create, import and the Agent's `save_flow`.
pub async fn create_workflow(
    app: &Arc<AppState>,
    name: String,
    description: Option<String>,
    nodes: Value,
    edges: Value,
) -> Result<Workflow> {
    let now = app.now();
    app.blocking(move |c| {
        let w = workflows::NewWorkflow {
            name: &name,
            description: description.as_deref(),
            nodes: &nodes,
            edges: &edges,
        };
        workflows::create(&*c.sqlite.conn()?, &c.security, &w, now)
    })
    .await
}

/// A text field as the web stores it: text as is, `null` as absent, any
/// other value as Python's `str()` of it.
fn text_field(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(pyfmt::str(other)),
    }
}

/// `nodes` / `edges` as given, or `[]` when absent or empty in Python's
/// sense. The web validates `nodes or []` but stores the raw value, so a
/// `{}` or `""` passed the check and was stored as is (parity V-131); the
/// desktop stores what it checked.
fn graph_field(data: &Map<String, Value>, key: &str) -> Value {
    match data.get(key) {
        Some(v) if pyfmt::truthy(v) => v.clone(),
        _ => json!([]),
    }
}

/// `POST /flow/api/workflows`: structure is checked only when a graph was
/// sent (the editor creates with a name alone).
pub async fn create(app: &Arc<AppState>, body: Body) -> Reply {
    let data = match body {
        Body::Nothing => return no_data(),
        Body::Unreadable => return unreadable("Invalid workflow structure"),
        Body::Json(Value::Object(m)) => m,
        Body::Json(other) => return not_an_object(&other),
    };
    if data.contains_key("nodes") || data.contains_key("edges") {
        let payload = json!({
            "name": data.get("name").filter(|v| pyfmt::truthy(v)).cloned().unwrap_or(json!("")),
            "nodes": data.get("nodes").filter(|v| pyfmt::truthy(v)).cloned().unwrap_or(json!([])),
            "edges": data.get("edges").filter(|v| pyfmt::truthy(v)).cloned().unwrap_or(json!([])),
        });
        let errors = validator::validate_workflow(&payload, ValidateOptions::DRAFT);
        if let Some(first) = errors.first() {
            tracing::warn!(
                "Rejected workflow save: {} {} - {}",
                first.path,
                first.code,
                first.message
            );
            return invalid("Invalid workflow structure", &errors);
        }
    }
    // A present-but-null name is the default, never a failed insert (the
    // web answers 500 there: parity R-16).
    let name = text_field(data.get("name")).unwrap_or_else(|| "Untitled Workflow".into());
    let description = text_field(data.get("description"));
    match create_workflow(
        app,
        name,
        description,
        graph_field(&data, "nodes"),
        graph_field(&data, "edges"),
    )
    .await
    {
        Ok(w) => Reply::new(201, Value::Object(saved_json(&w))),
        Err(e) => failed("create workflow", e),
    }
}

/// `GET /flow/api/workflows/<id>`.
pub async fn get(app: &Arc<AppState>, id: i64) -> Reply {
    match load(app, id).await {
        Ok(Some(w)) => {
            let mut m = saved_json(&w);
            m.insert("schedule_job_id".into(), json!(w.schedule_job_id));
            Reply::new(200, Value::Object(m))
        }
        Ok(None) => workflow_not_found(),
        Err(e) => failed("read the workflow", e),
    }
}

/// The fields the editor may change; anything else is dropped and logged
/// (a PUT never flips the active flag or the webhook without the
/// registration that goes with it).
const EDITABLE: &[&str] = &["name", "description", "nodes", "edges"];

/// `PUT /flow/api/workflows/<id>`.
pub async fn update(app: &Arc<AppState>, id: i64, body: Body) -> Reply {
    let data = match body {
        Body::Nothing => return no_data(),
        Body::Unreadable => return unreadable("Invalid workflow structure"),
        Body::Json(Value::Object(m)) => m,
        Body::Json(other) => return not_an_object(&other),
    };
    let mut rejected: Vec<&String> = data
        .keys()
        .filter(|k| !EDITABLE.contains(&k.as_str()))
        .collect();
    rejected.sort();
    if !rejected.is_empty() {
        tracing::warn!(
            "Ignoring non-editable field(s) {:?} in PUT for workflow {}",
            rejected,
            id
        );
    }
    let existing = match load(app, id).await {
        Ok(w) => w,
        Err(e) => return failed("update workflow", e),
    };
    if let Some(r) = merged_graph_refusal(&data, existing.as_ref()) {
        return r;
    }
    let edit = workflows::Edit {
        name: data.get("name").and_then(|v| text_field(Some(v))),
        description: data
            .contains_key("description")
            .then(|| text_field(data.get("description"))),
        nodes: data.get("nodes").cloned(),
        edges: data.get("edges").cloned(),
    };
    let now = app.now();
    let updated = app
        .blocking(move |c| workflows::update(&*c.sqlite.conn()?, &c.security, id, &edit, now))
        .await;
    let w = match updated {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => return failed("update workflow", e),
    };
    let needs_reactivate = match existing {
        Some(before) => triggers::follow_trigger_change(app, &before, &w).await,
        None => false,
    };
    // A stood-down workflow answers as it now is (inactive), as the web's
    // shared row object does.
    let w = if needs_reactivate {
        load(app, id).await.ok().flatten().unwrap_or(w)
    } else {
        w
    };
    let mut m = saved_json(&w);
    m.insert("needs_reactivate".into(), json!(needs_reactivate));
    Reply::new(200, Value::Object(m))
}

/// A PUT that sends part of a graph is checked against the graph it makes
/// with the stored half.
fn merged_graph_refusal(data: &Map<String, Value>, existing: Option<&Workflow>) -> Option<Reply> {
    if !data.contains_key("nodes") && !data.contains_key("edges") {
        return None;
    }
    let stored = |key: &str| -> Value {
        existing
            .map(|w| {
                if key == "nodes" {
                    w.nodes.clone()
                } else {
                    w.edges.clone()
                }
            })
            .filter(pyfmt::truthy)
            .unwrap_or(json!([]))
    };
    let merged = json!({
        "name": data.get("name").filter(|v| pyfmt::truthy(v)).cloned()
            .unwrap_or_else(|| json!(existing.map(|w| w.name.clone()).unwrap_or_default())),
        "nodes": data.get("nodes").cloned().unwrap_or_else(|| stored("nodes")),
        "edges": data.get("edges").cloned().unwrap_or_else(|| stored("edges")),
    });
    let errors = validator::validate_workflow(&merged, ValidateOptions::DRAFT);
    (!errors.is_empty()).then(|| invalid("Invalid workflow structure", &errors))
}

/// `DELETE /flow/api/workflows/<id>`: refused while a run holds the
/// workflow; its feed subscriptions and trigger registrations go first.
pub async fn delete(app: &Arc<AppState>, id: i64) -> Reply {
    let Some(_claim) = app.flow.try_claim(id) else {
        return Reply::new(
            409,
            json!({"error": "This workflow is still running. Try again when it finishes."}),
        );
    };
    let w = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => return failed("delete workflow", e),
    };
    let hooks = app.flow.triggers();
    app.flow.subscriptions.release(id, hooks.as_ref()).await;
    if app.flow.subscriptions.has(id) {
        return Reply::new(
            503,
            json!({"error": "The workflow feed is still connected. Try again shortly."}),
        );
    }
    if w.is_active {
        if let Err(e) = hooks.unregister(id).await {
            tracing::warn!(
                "A trigger of workflow {} could not be removed before it was deleted: {}",
                id,
                e
            );
        }
    }
    match app
        .blocking(move |c| workflows::delete(&*c.sqlite.conn()?, id))
        .await
    {
        Ok(true) => Reply::new(
            200,
            json!({"status": "success", "message": "Workflow deleted"}),
        ),
        Ok(false) => Reply::new(500, json!({"error": "Failed to delete workflow"})),
        Err(e) => failed("delete workflow", e),
    }
}

/// The run's status code (`_execution_status_code`).
pub fn run_status(result: &Value) -> u16 {
    if result.get("already_running").is_some_and(pyfmt::truthy) {
        return 409;
    }
    if result.get("accepted").is_some_and(pyfmt::truthy) {
        return 202;
    }
    if result.get("busy").is_some_and(pyfmt::truthy) {
        return 429;
    }
    if result.get("status").and_then(Value::as_str) == Some("error") {
        return if result.get("errors").is_some_and(pyfmt::truthy) {
            502
        } else {
            500
        };
    }
    200
}

/// The strict refusal shared by Run Now, activation and the webhooks
/// (`_execution_blocked`): the reply body, or `None` when runnable.
pub fn execution_blocked(w: &Workflow) -> Option<Value> {
    let payload = json!({"name": w.name, "nodes": w.nodes, "edges": w.edges});
    let errors = validator::validate_workflow(&payload, ValidateOptions::COMPLETE);
    let first = errors.first()?;
    tracing::warn!(
        "Workflow {} ({}) blocked: {} {} - {}",
        w.id,
        w.name,
        first.path,
        first.code,
        first.message
    );
    Some(json!({
        "status": "error",
        "error": "Workflow cannot be executed",
        "message": first.message,
        "errors": errors,
    }))
}

/// `POST /flow/api/workflows/<id>/execute` (Run Now; an inactive workflow
/// runs too).
pub async fn run_now(app: &Arc<AppState>, id: i64) -> Reply {
    let w = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => return failed("run the workflow", e),
    };
    let Some(key) = api_key(app).await else {
        return Reply::new(400, json!({"error": "API key not configured"}));
    };
    if let Some(blocked) = execution_blocked(&w) {
        return Reply::new(400, blocked);
    }
    let result = executor::execute(
        app,
        id,
        RunRequest {
            api_key: Some(key),
            ..Default::default()
        },
    )
    .await;
    Reply::new(run_status(&result), result)
}

/// `GET /flow/api/workflows/<id>/executions?limit=`: newest first, the
/// limit read as the web reads it (missing, unreadable or 0 is 20; 1..200).
pub async fn history(app: &Arc<AppState>, id: i64, limit: Option<&str>) -> Reply {
    let requested = limit
        .and_then(|l| l.trim().parse::<i64>().ok())
        .filter(|n| *n != 0);
    let limit = requested.unwrap_or(20).clamp(1, executions::QUERY_MAX);
    match app
        .blocking(move |c| executions::list(&*c.sqlite.conn()?, id, limit))
        .await
    {
        Ok(rows) => Reply::new(
            200,
            Value::Array(
                rows.into_iter()
                    .map(|e| {
                        json!({
                            "id": e.id, "workflow_id": e.workflow_id, "status": e.status,
                            "started_at": e.started_at, "completed_at": e.completed_at,
                            "logs": e.logs, "error": e.error,
                        })
                    })
                    .collect(),
            ),
        ),
        Err(e) => failed("read the run history", e),
    }
}

#[cfg(test)]
mod tests;
