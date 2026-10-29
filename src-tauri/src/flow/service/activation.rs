//! Activate, deactivate and the monitors' status (the web's
//! `activate_workflow`, `deactivate_workflow` and `get_monitor_status`,
//! `blueprints/flow.py` 565-708, 1204-1215).
//!
//! Activation persists the active flag first and registers the trigger
//! second, so a failed write leaves nothing running; a registration that
//! fails rolls the activation back. Deactivation gives back the
//! workflow's feed subscriptions first: while the feed has not confirmed
//! them it answers 503 and leaves every trigger in place, so a retry loses
//! nothing. Neither changes the webhook switch: an inactive workflow's
//! webhook answers 403 "Workflow is not active".

use super::triggers::rollback_activation;
use super::{api_key, execution_blocked, load, workflow_not_found};
use crate::flow::contract::triggers::TriggerError;
use crate::flow::store::workflows;
use crate::flow::triggers::store;
use crate::services::core::Reply;
use crate::state::AppState;
use serde_json::json;
use std::sync::Arc;

const STILL_RUNNING: &str = "This workflow is still running. Try again when it finishes.";
const FEED_CONNECTED: &str = "The workflow feed is still connected. Try again shortly.";

/// `POST /flow/api/workflows/<id>/activate`.
pub async fn activate(app: &Arc<AppState>, id: i64) -> Reply {
    let w = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => {
            tracing::error!("Failed to activate workflow {}: {}", id, e);
            return Reply::new(500, json!({"error": "Could not activate workflow"}));
        }
    };
    if w.is_active {
        return Reply::new(
            200,
            json!({"status": "already_active", "message": "Workflow is already active"}),
        );
    }
    if api_key(app).await.is_none() {
        return Reply::new(400, json!({"error": "API key not configured"}));
    }
    if let Some(mut blocked) = execution_blocked(&w) {
        blocked["error"] = json!("Workflow cannot be activated");
        return Reply::new(400, blocked);
    }
    let graph = w.graph().ok();
    let Some(trigger) = graph.as_ref().and_then(|g| g.trigger()) else {
        return Reply::new(400, json!({"error": "No trigger node found in workflow"}));
    };
    let now = app.now();
    let persisted = app
        .blocking(move |c| workflows::set_active(&*c.sqlite.conn()?, id, true, now))
        .await;
    if !matches!(persisted, Ok(true)) {
        tracing::error!("Failed to persist active state for workflow {}", id);
        return Reply::new(500, json!({"error": "Could not activate workflow"}));
    }
    match app
        .flow
        .triggers()
        .register(id, &trigger.node_type, &trigger.data)
        .await
    {
        Ok(()) => Reply::new(
            200,
            json!({"status": "success", "message": format!("Workflow activated with {} trigger", trigger.node_type)}),
        ),
        Err(TriggerError::Invalid(m)) => {
            rollback_activation(app, id).await;
            Reply::new(400, json!({"error": m}))
        }
        Err(TriggerError::Failed(m)) => {
            tracing::error!("Failed to activate workflow {}: {}", id, m);
            rollback_activation(app, id).await;
            Reply::new(500, json!({"error": m}))
        }
    }
}

/// Give back the workflow's feed subscriptions; `false` while the feed
/// still holds one.
async fn released(app: &Arc<AppState>, id: i64) -> bool {
    let hooks = app.flow.triggers();
    app.flow.subscriptions.release(id, hooks.as_ref()).await;
    !app.flow.subscriptions.has(id)
}

/// `POST /flow/api/workflows/<id>/deactivate`.
pub async fn deactivate(app: &Arc<AppState>, id: i64) -> Reply {
    let Some(_claim) = app.flow.try_claim(id) else {
        return Reply::new(409, json!({"error": STILL_RUNNING}));
    };
    let w = match load(app, id).await {
        Ok(Some(w)) => w,
        Ok(None) => return workflow_not_found(),
        Err(e) => {
            tracing::error!("Failed to deactivate workflow {}: {}", id, e);
            return Reply::new(500, json!({"error": "Could not deactivate workflow"}));
        }
    };
    if !released(app, id).await {
        return Reply::new(503, json!({"error": FEED_CONNECTED}));
    }
    if !w.is_active {
        return Reply::new(
            200,
            json!({"status": "already_inactive", "message": "Workflow is already inactive"}),
        );
    }
    if let Err(e) = app.flow.triggers().unregister(id).await {
        tracing::error!("Failed to deactivate workflow {}: {}", id, e);
        return Reply::new(500, json!({"error": e}));
    }
    let now = app.now();
    let done = app
        .blocking(move |c| store::deactivate(&*c.sqlite.conn()?, id, now))
        .await;
    if !matches!(done, Ok(true)) {
        tracing::error!("Failed to persist inactive state for workflow {}", id);
        return Reply::new(500, json!({"error": "Could not deactivate workflow"}));
    }
    Reply::new(
        200,
        json!({"status": "success", "message": "Workflow deactivated"}),
    )
}

/// `GET /flow/api/monitor/status`.
pub fn monitor_status(app: &AppState) -> Reply {
    Reply::new(200, app.flow.triggers().status())
}
