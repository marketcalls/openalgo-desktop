//! What the core asks of the trigger side (the triggers lane: the
//! scheduler, the price and order-update monitors, the feed).
//!
//! A saved edit, a replace and a delete change or end a workflow whose
//! trigger is registered, so the core routes call these hooks as the web's
//! blueprint calls `_register_trigger`, `_unregister_trigger` and
//! `release_workflow_subscriptions`. Until the triggers lane installs its
//! hooks ([`super::super::FlowModule::set_triggers`]) nothing is registered,
//! and [`NoTriggers`] answers that truthfully.

use super::subscriptions::FeedRelease;
use async_trait::async_trait;
use serde_json::{Map, Value};

/// Why a trigger could not be registered.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerError {
    /// The trigger node is misconfigured (the web's `ValueError`: no Order
    /// ID or symbol, a `{{variable}}` Order ID, an unknown status): 400.
    Invalid(String),
    /// The registration could not be recorded (the web's `RuntimeError`): 500.
    Failed(String),
}

impl TriggerError {
    pub fn message(&self) -> &str {
        match self {
            TriggerError::Invalid(m) | TriggerError::Failed(m) => m,
        }
    }
}

/// The trigger side's hooks.
#[async_trait]
pub trait TriggerHooks: FeedRelease {
    /// Register the scheduler job, price alert or order watch for a trigger
    /// node of type `trigger_type` with its `data` (`_register_trigger`).
    async fn register(
        &self,
        workflow_id: i64,
        trigger_type: &str,
        data: &Map<String, Value>,
    ) -> Result<(), TriggerError>;

    /// Remove every registration of the workflow, of every kind; an error
    /// when one could not be removed, so the workflow is never marked
    /// inactive while a job is still set (`_unregister_trigger`,
    /// `remove_workflow_job(strict=True)`).
    async fn unregister(&self, workflow_id: i64) -> Result<(), String>;
}

/// The hooks before the trigger side is installed: nothing is ever
/// registered, so there is nothing to remove or give back.
pub struct NoTriggers;

#[async_trait]
impl FeedRelease for NoTriggers {
    async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
}

#[async_trait]
impl TriggerHooks for NoTriggers {
    async fn register(&self, _: i64, _: &str, _: &Map<String, Value>) -> Result<(), TriggerError> {
        Ok(())
    }

    async fn unregister(&self, _: i64) -> Result<(), String> {
        Ok(())
    }
}
