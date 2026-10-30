//! Running a workflow (the web's `execute_workflow`,
//! `_execute_workflow_locked` and `_run_or_start`,
//! `flow_executor_service.py` 4675-5069).
//!
//! - One run per workflow at a time: [`execute`] claims the workflow or
//!   answers `already_running`; delete and deactivate take the same claim.
//! - Every run is a task Flow owns, so a caller that goes away (a webhook
//!   sender timing out) never stops a run between two order legs; the
//!   caller awaits it. A run that can wait more than ten seconds is
//!   answered `accepted` at once and finishes on its own (`waits`).
//! - Every trigger converges here, so the strict validator runs before any
//!   record exists: an incomplete graph never reaches the broker.
//! - The record is written twice: `running` when the run starts, and its
//!   end with the whole node trace.
//!
//! `walk` is the graph walk; `waits` the wait estimate.

pub mod waits;
pub mod walk;

use crate::flow::contract::client::app::AppFlowClient;
use crate::flow::contract::model::{RunStatus, Workflow};
use crate::flow::contract::node::{NodeCtx, NodeCtxParts, RunTrigger, ERROR};
use crate::flow::contract::pyfmt;
use crate::flow::contract::time::ist_now;
use crate::flow::store::{executions, workflows};
use crate::flow::validator::{self, ValidateOptions};
use crate::security::Secret;
use crate::state::AppState;
use futures_util::FutureExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// What starts a run: the trigger's payload and the key it trades with.
#[derive(Default)]
pub struct RunRequest {
    /// The webhook (or monitor) payload, stored as `{{webhook...}}`.
    pub webhook_data: Option<Value>,
    /// The trader's API key at the moment of the trigger.
    pub api_key: Option<Secret>,
    /// What started the run, set by the code that started it (never read
    /// from the payload): the price monitor's runs are the only ones the
    /// priceAlert node trusts without a quote (parity N-priceAlert-04).
    pub trigger: RunTrigger,
}

/// What a run that the app stopped answers its caller.
pub const STOPPED_BEFORE_ANSWER: &str =
    "The workflow was stopped before it finished, because OpenAlgo was signed out or is closing. Check the order book for anything it placed.";

/// The answer to a trigger while the workflow is running
/// (`_already_running`).
pub fn already_running(workflow_id: i64) -> Value {
    tracing::warn!("Workflow {} is already running", workflow_id);
    json!({"status": "error", "message": "Workflow is already running", "already_running": true})
}

/// Run a workflow for any trigger (Run Now, webhook, schedule, monitors,
/// the Agent): claim it, start the run as an owned task, and answer with
/// its result, or `accepted` when it can wait more than ten seconds.
pub async fn execute(app: &Arc<AppState>, workflow_id: i64, req: RunRequest) -> Value {
    let waits_long = {
        let stored = app
            .blocking(move |c| workflows::get(&*c.sqlite.conn()?, &c.security, workflow_id))
            .await;
        let now = ist_now(app.clock.as_ref());
        matches!(stored, Ok(Some(w)) if waits::runs_in_background(&w.nodes, now))
    };
    let Some(claim) = app.flow.try_claim(workflow_id) else {
        return already_running(workflow_id);
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let owner = app.clone();
    let stop = app.flow.stop_token();
    app.flow.spawn(async move {
        let _claim = claim;
        let out = run_claimed(&owner, workflow_id, req, stop).await;
        if let Some(e) = out.get("message").filter(|_| out["status"] != "success") {
            tracing::info!("Workflow {} ended: {}", workflow_id, pyfmt::str(e));
        }
        let _ = tx.send(out);
    });
    if waits_long {
        tracing::info!(
            "Workflow {} started in the background (it waits)",
            workflow_id
        );
        return json!({"status": "accepted", "message": waits::STARTED_IN_BACKGROUND, "accepted": true});
    }
    rx.await
        .unwrap_or_else(|_| json!({"status": "error", "message": STOPPED_BEFORE_ANSWER}))
}

/// The run itself; the caller holds the workflow's claim and took the stop
/// token when it claimed, so a stop that comes before the run builds its
/// context still reaches it.
async fn run_claimed(
    app: &Arc<AppState>,
    workflow_id: i64,
    req: RunRequest,
    stop: CancellationToken,
) -> Value {
    let workflow = match app
        .blocking(move |c| workflows::get(&*c.sqlite.conn()?, &c.security, workflow_id))
        .await
    {
        Ok(Some(w)) => w,
        Ok(None) => return json!({"status": "error", "message": "Workflow not found"}),
        Err(e) => {
            tracing::error!(
                "Workflow {} could not be read for a run: {}",
                workflow_id,
                e
            );
            return json!({"status": "error", "message": "The workflow could not be read. Try again."});
        }
    };
    if let Some(blocked) = not_runnable(&workflow) {
        return blocked;
    }
    let now = app.now();
    let execution_id = match app
        .blocking(move |c| executions::create(&*c.sqlite.conn()?, workflow_id, now))
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Workflow {} run could not be recorded: {}", workflow_id, e);
            return json!({"status": "error", "message": "Failed to create execution record"});
        }
    };
    let ctx = context(app, &workflow, req.api_key.clone(), stop).with_trigger(req.trigger);
    if let Some(data) = req.webhook_data.filter(pyfmt::truthy) {
        ctx.vars.set("webhook", data);
        tracing::info!("Webhook data injected into workflow {}", workflow_id);
    }
    let walked = std::panic::AssertUnwindSafe(walk_from_trigger(
        app,
        &workflow,
        &ctx,
        req.api_key.is_some(),
    ))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| {
        tracing::error!("Workflow {} stopped on an internal error", workflow_id);
        Err("The workflow stopped on an unexpected error.".into())
    });
    finish(app, execution_id, &ctx, walked).await
}

/// The strict validator's refusal, before any record exists.
fn not_runnable(w: &Workflow) -> Option<Value> {
    let payload = json!({"name": w.name, "nodes": w.nodes, "edges": w.edges});
    let errors = validator::validate_workflow(&payload, ValidateOptions::COMPLETE);
    let first = errors.first()?;
    tracing::error!(
        "Workflow {} ({}) is not runnable: {}",
        w.id,
        w.name,
        first.message
    );
    Some(json!({
        "status": "error",
        "message": format!("Workflow cannot be executed: {}", first.message),
        "errors": errors,
    }))
}

/// The run's context: the in-app client with the run's key, IST clock,
/// stop token, the workflow's name as the order tag.
fn context(
    app: &Arc<AppState>,
    w: &Workflow,
    api_key: Option<Secret>,
    stop: CancellationToken,
) -> NodeCtx {
    let config = app.server_config();
    NodeCtx::new(NodeCtxParts {
        client: Arc::new(AppFlowClient::new(
            Arc::downgrade(app),
            api_key.unwrap_or_else(|| Secret::new("")),
        )),
        clock: app.clock.clone(),
        cancel: stop,
        workflow_id: Some(w.id),
        default_strategy: w.name.clone(),
        subscriptions: app.flow.subscriptions.clone(),
        session_boundary: (config.session_expiry_hour, config.session_expiry_minute),
        env: app.flow.env(),
        app: Arc::downgrade(app),
    })
}

/// From the trigger node to the end of the walk: the failed nodes, or why
/// the whole run stopped.
async fn walk_from_trigger(
    app: &Arc<AppState>,
    w: &Workflow,
    ctx: &NodeCtx,
    has_key: bool,
) -> Result<Vec<Value>, String> {
    if !has_key {
        return Err("API key required for workflow execution".into());
    }
    tracing::info!("Starting workflow: {}", w.name);
    ctx.info(format!("Starting workflow: {}", w.name));
    let graph = w.graph()?;
    let trigger = graph
        .trigger()
        .ok_or_else(|| "No trigger node found".to_string())?;
    let mut walk = walk::Walk::new(&graph, &app.flow.nodes, ctx);
    walk.visit(&trigger.id, 0).await?;
    Ok(walk.errors)
}

/// Write the run's end and answer as the web does.
async fn finish(
    app: &Arc<AppState>,
    execution_id: i64,
    ctx: &NodeCtx,
    walked: Result<Vec<Value>, String>,
) -> Value {
    let (status, error, mut out) = match walked {
        Ok(errors) if errors.is_empty() => (
            RunStatus::Completed,
            None,
            json!({"status": "success", "message": "Workflow executed successfully"}),
        ),
        Ok(errors) => {
            let summary = errors
                .iter()
                .map(|e| format!("{}: {}", pyfmt::str(&e["type"]), pyfmt::str(&e["message"])))
                .collect::<Vec<_>>()
                .join("; ");
            let message = format!("{} node(s) failed: {}", errors.len(), summary);
            (
                RunStatus::Failed,
                Some(summary),
                json!({"status": "error", "message": message, "errors": errors}),
            )
        }
        Err(e) => {
            ctx.log(format!("Error: {}", e), ERROR);
            (
                RunStatus::Failed,
                Some(e.clone()),
                json!({"status": "error", "message": e}),
            )
        }
    };
    let logs = ctx.logs();
    if let Some(m) = out.as_object_mut() {
        m.insert("execution_id".into(), json!(execution_id));
        m.insert("logs".into(), Value::Array(logs.clone()));
    }
    let now = app.now();
    let written = app
        .blocking(move |c| {
            executions::finish(
                &*c.sqlite.conn()?,
                execution_id,
                status,
                error.as_deref(),
                &logs,
                now,
            )
        })
        .await;
    if let Err(e) = written {
        tracing::error!(
            "The end of run {} could not be recorded: {}",
            execution_id,
            e
        );
    }
    out
}

#[cfg(test)]
mod tests;
