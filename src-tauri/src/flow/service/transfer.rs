//! Export, import and replace (the web's `export_workflow`,
//! `import_workflow`, `replace_workflow`; `docs/prompt/flow-import-format.md`).
//!
//! The file format is the web's: `{name, description, nodes, edges,
//! version: "1.0", exported_at}`, the graph exactly as stored, so a
//! workflow exported from the web imports here unchanged, and back. Import
//! makes a new workflow (`<name> (imported)`, inactive, new webhook
//! credentials); replace swaps the graph of an existing one and keeps its
//! id, webhook address, secret, active state and history.

use super::{
    create_workflow, failed, invalid, load, no_data, unreadable, workflow_not_found, Body,
};
use crate::flow::store::workflows;
use crate::flow::validator::{self, ValidateOptions};
use crate::services::core::Reply;
use crate::state::AppState;
use chrono::Timelike;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The format version export writes (import reads none).
pub const FORMAT_VERSION: &str = "1.0";

/// `GET /flow/api/workflows/<id>/export`.
pub async fn export(app: &Arc<AppState>, id: i64) -> Reply {
    let w = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => return failed("export workflow", e),
    };
    // `datetime.utcnow().isoformat()`: naive UTC, microseconds when any.
    let now = app.now().naive_utc();
    let exported_at = if now.nanosecond() / 1000 == 0 {
        now.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    };
    Reply::new(
        200,
        json!({
            "name": w.name,
            "description": w.description,
            "nodes": w.nodes,
            "edges": w.edges,
            "version": FORMAT_VERSION,
            "exported_at": exported_at,
        }),
    )
}

/// Upgrade legacy node payloads in place, returning the notes.
fn migrate_nodes(data: &mut Map<String, Value>) -> Vec<String> {
    let Some(Value::Array(nodes)) = data.get("nodes") else {
        return Vec::new();
    };
    let (migrated, notes) = validator::migrate_legacy_node_data(nodes.clone());
    data.insert("nodes".into(), Value::Array(migrated));
    notes
}

/// `POST /flow/api/workflows/import`: validated as a finished workflow
/// before anything is stored.
pub async fn import(app: &Arc<AppState>, body: Body) -> Reply {
    let mut data = match body {
        Body::Nothing => return no_data(),
        Body::NonFinite => return unreadable("Invalid workflow format"),
        Body::Json(Value::Object(m)) => m,
        // The web fails a JSON array here with a 500 (parity IE-06).
        Body::Json(other) => {
            return invalid(
                "Invalid workflow format",
                &validator::validate_workflow(&other, ValidateOptions::COMPLETE),
            )
        }
    };
    let notes = migrate_nodes(&mut data);
    let errors =
        validator::validate_workflow(&Value::Object(data.clone()), ValidateOptions::COMPLETE);
    if !errors.is_empty() {
        return invalid("Invalid workflow format", &errors);
    }
    let name = format!(
        "{} (imported)",
        super::text_field(data.get("name")).unwrap_or_default()
    );
    let description = super::text_field(data.get("description"));
    let nodes = super::graph_field(&data, "nodes");
    let edges = super::graph_field(&data, "edges");
    match create_workflow(app, name, description, nodes, edges).await {
        Ok(w) => {
            let mut out = json!({"status": "success", "workflow_id": w.id});
            if !notes.is_empty() {
                out["migrations"] = json!(notes);
            }
            Reply::new(201, out)
        }
        Err(e) => failed("import workflow", e),
    }
}

/// `payload[key] or []`: what replace checks, and so what it stores.
fn graph_or_empty(payload: &Map<String, Value>, key: &str) -> Value {
    payload
        .get(key)
        .filter(|v| crate::flow::contract::pyfmt::truthy(v))
        .cloned()
        .unwrap_or_else(|| json!([]))
}

/// What replace writes: the graph, the name when one is given, the
/// description when present (an empty one clears it).
fn replace_edit(payload: &Map<String, Value>) -> workflows::Edit {
    workflows::Edit {
        name: payload
            .get("name")
            .filter(|v| crate::flow::contract::pyfmt::truthy(v))
            .and_then(|v| super::text_field(Some(v))),
        description: payload
            .get("description")
            .filter(|v| !v.is_null())
            .map(|v| super::text_field(Some(v))),
        nodes: Some(graph_or_empty(payload, "nodes")),
        edges: Some(graph_or_empty(payload, "edges")),
    }
}

/// `POST /flow/api/workflows/<id>/replace`: held to import's rules, the
/// name optional.
pub async fn replace(app: &Arc<AppState>, id: i64, body: Body) -> Reply {
    let before = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => return failed("replace workflow", e),
    };
    let mut payload = match body {
        Body::Nothing => return no_data(),
        Body::NonFinite => return unreadable("Invalid workflow structure"),
        Body::Json(Value::Object(m)) => m,
        Body::Json(_) => {
            return Reply::new(
                400,
                json!({"status": "error", "error": "Invalid workflow structure", "message": "Workflow must be a JSON object"}),
            )
        }
    };
    let notes = migrate_nodes(&mut payload);
    let truthy_or = |key: &str, default: Value| -> Value {
        payload
            .get(key)
            .filter(|v| crate::flow::contract::pyfmt::truthy(v))
            .cloned()
            .unwrap_or(default)
    };
    let check = json!({
        "name": truthy_or("name", json!(before.name)),
        "nodes": graph_or_empty(&payload, "nodes"),
        "edges": graph_or_empty(&payload, "edges"),
    });
    let errors = validator::validate_workflow(&check, ValidateOptions::COMPLETE);
    if let Some(first) = errors.first() {
        tracing::warn!(
            "Rejected replace of workflow {}: {} {} - {}",
            id,
            first.path,
            first.code,
            first.message
        );
        return invalid("Invalid workflow format", &errors);
    }
    let edit = replace_edit(&payload);
    let now = app.now();
    let updated = app
        .blocking(move |c| workflows::update(&*c.sqlite.conn()?, &c.security, id, &edit, now))
        .await;
    let after = match updated {
        Ok(Some(w)) => w,
        Ok(None) => return Reply::new(500, json!({"error": "Failed to replace workflow"})),
        Err(e) => return failed("replace workflow", e),
    };
    let needs_reactivate = super::triggers::follow_trigger_change(app, &before, &after).await;
    let count = |v: &Value| v.as_array().map(Vec::len).unwrap_or(0);
    tracing::info!(
        "Replaced workflow {} from JSON (nodes={} edges={} reactivate={})",
        id,
        count(&after.nodes),
        count(&after.edges),
        crate::flow::contract::pyfmt::str(&json!(needs_reactivate))
    );
    let message = if needs_reactivate {
        "Trigger changed and could not be re-registered, so the workflow was deactivated. Activate it again when the trigger is valid."
    } else {
        "Workflow replaced. Changes apply from the next run."
    };
    Reply::new(
        200,
        json!({
            "status": "success",
            "workflow_id": id,
            "migrations": notes,
            "needs_reactivate": needs_reactivate,
            "message": message,
        }),
    )
}
