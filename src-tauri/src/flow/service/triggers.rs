//! Keeping an active workflow's registration in step with its graph (the
//! web's `_reregister_trigger` and `_rollback_activation`).
//!
//! A schedule, price alert or order watch is registered from a snapshot of
//! the trigger node at activation. A save or replace that changes any
//! registered field of an active workflow re-registers it; if that fails,
//! the workflow is stood down rather than left running a stale
//! configuration, and the editor is told to activate it again.

use crate::flow::contract::model::{Graph, Workflow};
use crate::flow::contract::triggers::TriggerError;
use crate::flow::store::workflows;
use crate::flow::validator::trigger_config;
use crate::state::AppState;
use std::sync::Arc;

/// A stored `nodes` value as the list the validator reads (anything else
/// as no nodes).
fn graph_nodes(nodes: &serde_json::Value) -> &[serde_json::Value] {
    nodes.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// After a save: re-register when the workflow was active and its trigger
/// configuration changed. `true` when it had to be deactivated instead
/// (`needs_reactivate`).
pub async fn follow_trigger_change(
    app: &Arc<AppState>,
    before: &Workflow,
    after: &Workflow,
) -> bool {
    if !before.is_active
        || trigger_config(graph_nodes(&before.nodes)) == trigger_config(graph_nodes(&after.nodes))
    {
        return false;
    }
    tracing::info!(
        "Workflow {} trigger configuration changed while active; re-registering it",
        after.id
    );
    match reregister(app, after).await {
        Ok(()) => false,
        Err(e) => {
            tracing::error!(
                "Could not re-register the trigger for workflow {}; deactivating it so it cannot run a stale configuration: {}",
                after.id,
                e.message()
            );
            rollback_activation(app, after.id).await;
            true
        }
    }
}

/// Swap a live workflow's registration for its current graph: every kind
/// is removed first (the trigger's type may have changed), then the
/// current trigger node is registered.
pub async fn reregister(app: &Arc<AppState>, w: &Workflow) -> Result<(), TriggerError> {
    let graph = Graph::parse(&w.nodes, &w.edges).map_err(TriggerError::Invalid)?;
    let trigger = graph
        .trigger()
        .ok_or_else(|| TriggerError::Invalid("Workflow has no trigger node".into()))?;
    if super::api_key(app).await.is_none() {
        return Err(TriggerError::Failed("API key not configured".into()));
    }
    let hooks = app.flow.triggers();
    hooks.unregister(w.id).await.map_err(TriggerError::Failed)?;
    hooks
        .register(w.id, &trigger.node_type, &trigger.data)
        .await
}

/// Undo an activation so nothing is left registered: every registration
/// and the active flag, each attempted even when another fails.
pub async fn rollback_activation(app: &Arc<AppState>, workflow_id: i64) {
    if let Err(e) = app.flow.triggers().unregister(workflow_id).await {
        tracing::error!(
            "Could not roll back the scheduler job for workflow {} after a failed activation; it may need to be deactivated manually: {}",
            workflow_id,
            e
        );
    }
    let now = app.now();
    let cleared = app
        .blocking(move |c| workflows::set_active(&*c.sqlite.conn()?, workflow_id, false, now))
        .await;
    if let Err(e) = cleared {
        tracing::error!(
            "Could not roll back the active flag for workflow {} after a failed activation; it may need to be deactivated manually: {}",
            workflow_id,
            e
        );
    }
}
