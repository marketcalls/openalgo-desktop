//! Where the orders lane hangs the trader's approval decisions.
//!
//! The run pauses before a tool marked `needs_approval` runs and the stream
//! ends with a `confirm` frame. When the trader answers, the chat route
//! hands the decisions to [`ApprovalHook::decided`] before the run resumes
//! (the web records each one in the audit trail there, so an approval is in
//! the trail even if the resumed run then fails), and a paused run that is
//! abandoned (expired, cancelled, its conversation deleted) is reported to
//! [`ApprovalHook::abandoned`]. The risk guard itself runs inside each order
//! tool's body, after approval and before the service call, and reads no
//! prompt.

use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{Map, Value};
use std::sync::Arc;

/// One decision on one paused tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub tool_call_id: String,
    pub tool_name: String,
    /// The arguments the trader saw on the card.
    pub args: Map<String, Value>,
    pub approved: bool,
}

/// The run the decisions belong to.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionScope {
    pub conversation_id: Option<i64>,
    pub run_id: String,
    /// The signed-in user who decided.
    pub actor: String,
    /// The trader's reason, shown to the model on a rejection.
    pub note: Option<String>,
}

#[async_trait]
pub trait ApprovalHook: Send + Sync {
    /// The trader decided; called once per resume, before the run goes on.
    /// Must not fail the resume: a hook that cannot record logs and returns.
    async fn decided(&self, app: &Arc<AppState>, scope: &DecisionScope, decisions: &[Decision]);

    /// A paused run will never resume (expired, cancelled, deleted).
    async fn abandoned(&self, app: &Arc<AppState>, run_id: &str);
}

/// The hook before the orders lane installs its own: nothing is recorded.
pub struct NoApprovalRecord;

#[async_trait]
impl ApprovalHook for NoApprovalRecord {
    async fn decided(&self, _app: &Arc<AppState>, _scope: &DecisionScope, _decisions: &[Decision]) {
    }

    async fn abandoned(&self, _app: &Arc<AppState>, _run_id: &str) {}
}
