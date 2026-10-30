//! Starting a run for a trigger (a schedule, a price alert, an order
//! update, a webhook), and the skip when it cannot run.
//!
//! A run trades with the trader's current API key (lead decision 3; the
//! web stored the key at activation). While OpenAlgo is signed out (its
//! keys locked: from start-up until sign-in, and after logout) a trigger
//! that fires does not run: the key cannot be read. The skip is one failed
//! run in the workflow's history with the reason, and the triggers run
//! again after sign-in (lead decision 16). With OpenAlgo signed in and no
//! broker logged in (or the broker session ended at its daily boundary)
//! the workflow runs, as the strategy and Chartink webhooks and the web do:
//! each broker call fails at its node with the no-broker-session sentence
//! and stops its branch, analyzer-mode calls go to the sandbox, and nodes
//! that need no broker run.

use super::TriggerSide;
use crate::flow::contract::node::{RunTrigger, INFO};
use crate::flow::contract::vars::iso_ist;
use crate::flow::executor::{self, RunRequest};
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::Arc;

/// How a trigger's run went.
#[derive(Debug, Clone, PartialEq)]
pub enum Fired {
    /// The workflow ran (whatever the broker made of it): the result.
    Ran(Value),
    /// Another run of it was going: this event never reached the graph.
    AlreadyRunning,
    /// It could not run (signed out, no API key, the app closing); the
    /// reason was recorded.
    Skipped,
}

impl Fired {
    /// Whether the run reached the graph (a one-shot trigger is spent only
    /// then).
    pub fn executed(&self) -> bool {
        matches!(self, Fired::Ran(_))
    }

    /// The run's status, for the server log line.
    pub fn status(&self) -> String {
        match self {
            Fired::Ran(v) => v["status"].as_str().unwrap_or("success").to_string(),
            Fired::AlreadyRunning => "error".into(),
            Fired::Skipped => "skipped".into(),
        }
    }
}

/// What a trigger is called in a skip sentence.
fn noun(trigger: RunTrigger) -> &'static str {
    match trigger {
        RunTrigger::Schedule => "schedule",
        RunTrigger::PriceAlert => "price alert",
        RunTrigger::OrderUpdate => "order update",
        RunTrigger::Webhook => "webhook",
        RunTrigger::Manual => "trigger",
    }
}

/// Why a trigger cannot run now: OpenAlgo is signed out.
pub fn paused(app: &AppState, trigger: RunTrigger) -> Option<String> {
    (!app.security.is_unlocked()).then(|| {
        format!(
            "Skipped: OpenAlgo was signed out when this {} fired, so the workflow did not run. Its triggers run again after you sign in.",
            noun(trigger)
        )
    })
}

/// Write the skip into the workflow's run history.
pub async fn record_skip(app: &Arc<AppState>, workflow_id: i64, reason: String) {
    let now = app.now();
    let log = json!({"time": iso_ist(now), "message": reason, "level": INFO});
    let written = app
        .blocking(move |c| super::store::record_skip(&*c.sqlite.conn()?, workflow_id, log, now))
        .await;
    if let Err(e) = written {
        tracing::warn!(
            "Could not record a skipped run of workflow {}: {}",
            workflow_id,
            e
        );
    }
}

impl TriggerSide {
    /// Run the workflow for a trigger: the skip when it cannot run, else
    /// the executor's run with the current key and the trigger's payload
    /// (stored as `{{webhook...}}`).
    pub async fn fire(
        &self,
        workflow_id: i64,
        trigger: RunTrigger,
        payload: Option<Value>,
    ) -> Fired {
        let Some(app) = self.app() else {
            return Fired::Skipped;
        };
        if let Some(reason) = paused(&app, trigger) {
            tracing::info!("Workflow {}: {}", workflow_id, reason);
            record_skip(&app, workflow_id, reason).await;
            return Fired::Skipped;
        }
        let Some(key) = crate::flow::service::api_key(&app).await else {
            tracing::error!("No API key available for workflow {}", workflow_id);
            record_skip(
                &app,
                workflow_id,
                format!(
                    "Skipped: there is no API key, so the workflow did not run when this {} fired. Generate an API key in OpenAlgo.",
                    noun(trigger)
                ),
            )
            .await;
            return Fired::Skipped;
        };
        let result = executor::execute(
            &app,
            workflow_id,
            RunRequest {
                webhook_data: payload,
                api_key: Some(key),
                trigger,
            },
        )
        .await;
        if result
            .get("already_running")
            .is_some_and(crate::flow::contract::pyfmt::truthy)
        {
            return Fired::AlreadyRunning;
        }
        Fired::Ran(result)
    }

    /// The trigger time a monitor's payload carries (`datetime.now()
    /// .isoformat()`, in IST).
    pub fn triggered_at(&self) -> String {
        iso_ist(self.now())
    }

    /// Whether the workflow is still active; a read that fails counts as
    /// not (`_workflow_is_active`, fail closed).
    pub async fn still_active(&self, workflow_id: i64) -> bool {
        let Some(app) = self.app() else {
            return false;
        };
        match app
            .blocking(move |c| super::store::is_active(&*c.sqlite.conn()?, workflow_id))
            .await
        {
            Ok(active) => active.unwrap_or(false),
            Err(e) => {
                tracing::error!(
                    "Could not confirm workflow {} is active: {}",
                    workflow_id,
                    e
                );
                false
            }
        }
    }
}
