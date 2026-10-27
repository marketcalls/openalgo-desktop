//! One batch of tool calls the model asked for (agno's
//! `run_function_calls`), and the trader's decisions on a paused batch
//! (agno's `continue_run`).
//!
//! In order, each call is counted against the run's limit, found among
//! the run's tools, its arguments read, and then either run (a tool that
//! needs no approval) or set aside for approval. A call over the limit, to
//! a tool the run was not given, or with arguments that are not an object
//! gets agno's own answer and no frames. A run that set calls aside pauses
//! once the batch is done.

use super::output::FrameOut;
use super::TOOL_CALL_LIMIT;
use crate::agent::contract::frames::{Frame, Requirement};
use crate::agent::contract::tool::{AgentTool, ToolCtx, ToolOutcome};
use crate::agent::redact::{redact, redact_map};
use crate::agent::tool_guard::{self, read_arguments};
use futures_util::FutureExt;
use genai::chat::{ToolCall, ToolResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// A call set aside for the trader's approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCall {
    pub call: ToolCall,
    pub args: Map<String, Value>,
    pub requirement: Requirement,
}

/// What a batch left behind.
#[derive(Debug, Default)]
pub struct BatchOutcome {
    pub responses: Vec<ToolResponse>,
    pub pending: Vec<PendingCall>,
}

/// The rest of the run's tool-calling state.
pub struct Calls<'a> {
    pub ctx: &'a ToolCtx,
    pub tools: &'a [Arc<dyn AgentTool>],
    /// Calls counted so far in this run (paused ones included).
    pub made: usize,
}

fn limit_reached(name: &str) -> String {
    format!(
        "Tool call limit reached. Tool call {} not executed. Don't try to execute it again.",
        name
    )
}

impl Calls<'_> {
    fn find(&self, name: &str) -> Option<&Arc<dyn AgentTool>> {
        self.tools.iter().find(|t| t.spec().name == name)
    }

    /// Run or set aside every call of one model turn.
    pub async fn batch(&mut self, calls: Vec<ToolCall>, out: &mut FrameOut) -> BatchOutcome {
        let mut outcome = BatchOutcome::default();
        for (i, mut call) in calls.into_iter().enumerate() {
            if call.call_id.is_empty() {
                call.call_id = format!("{}#{}", call.fn_name, self.made + i + 1);
            }
            self.made += 1;
            if self.made > TOOL_CALL_LIMIT {
                outcome.responses.push(ToolResponse::new(
                    &call.call_id,
                    limit_reached(&call.fn_name),
                ));
                continue;
            }
            let Some(tool) = self.find(&call.fn_name).cloned() else {
                outcome
                    .responses
                    .push(ToolResponse::new(&call.call_id, tool_guard::UNKNOWN_TOOL));
                continue;
            };
            let args = match read_arguments(&call.fn_arguments) {
                Ok(a) => a,
                Err(message) => {
                    outcome
                        .responses
                        .push(ToolResponse::new(&call.call_id, message));
                    continue;
                }
            };
            if tool.spec().needs_approval {
                outcome.pending.push(pending(&call, &tool, args));
                continue;
            }
            let text = self.execute(&tool, &call.call_id, args, out).await;
            outcome
                .responses
                .push(ToolResponse::new(&call.call_id, text));
        }
        outcome
    }

    /// Run one tool with its frames: the arguments checked first, the
    /// result redacted for the screen, the run's renderings after a
    /// success. Answers what the model reads.
    pub async fn execute(
        &self,
        tool: &Arc<dyn AgentTool>,
        call_id: &str,
        args: Map<String, Value>,
        out: &mut FrameOut,
    ) -> String {
        let spec = tool.spec();
        out.send(Frame::ToolStart {
            id: call_id.into(),
            name: spec.name.into(),
            args: redact_map(&args),
        })
        .await;
        let started = Instant::now();
        let outcome = match tool_guard::validate(&spec.parameters, &args) {
            Ok(checked) => {
                match std::panic::AssertUnwindSafe(tool.call(self.ctx, checked))
                    .catch_unwind()
                    .await
                {
                    Ok(o) => o,
                    Err(_) => {
                        tracing::error!("Agent tool {} failed unexpectedly", spec.name);
                        ToolOutcome::Retry(format!(
                            "{} failed inside OpenAlgo. This is a platform failure: report it to the user rather than calling the tool again.",
                            spec.name
                        ))
                    }
                }
            }
            Err(refusals) => {
                tracing::warn!(
                    "Agent tool {} refused its arguments ({} errors, {} argument keys)",
                    spec.name,
                    refusals.len(),
                    args.len()
                );
                ToolOutcome::Retry(tool_guard::correction(spec.name, &refusals, &args))
            }
        };
        out.send(Frame::ToolEnd {
            id: call_id.into(),
            name: spec.name.into(),
            ok: outcome.is_ok(),
            result: redact(&Value::String(outcome.text().to_string())),
            duration: Some(started.elapsed().as_secs_f64()),
        })
        .await;
        if outcome.is_ok() {
            for frame in self.ctx.viz.drain() {
                out.send(frame).await;
            }
        }
        match outcome {
            ToolOutcome::Ok(t) | ToolOutcome::Retry(t) => t,
        }
    }

    /// Apply the trader's decisions to a paused batch (keyed by tool call
    /// id or requirement id): approved calls run, rejected ones answer the
    /// trader's note, undecided ones stay pending. When no decision matched
    /// any pending call the batch comes back untouched (it stays paused).
    pub async fn decide(
        &mut self,
        pending: Vec<PendingCall>,
        decisions: &HashMap<String, bool>,
        note: Option<&str>,
        out: &mut FrameOut,
    ) -> Result<BatchOutcome, Vec<PendingCall>> {
        let decision = |p: &PendingCall| {
            decisions
                .get(&p.call.call_id)
                .or_else(|| decisions.get(&p.requirement.id))
                .copied()
        };
        if !pending.iter().any(|p| decision(p).is_some()) {
            return Err(pending);
        }
        let mut outcome = BatchOutcome::default();
        for p in pending {
            match decision(&p) {
                None => outcome.pending.push(p),
                Some(false) => outcome.responses.push(ToolResponse::new(
                    &p.call.call_id,
                    note.filter(|n| !n.is_empty())
                        .unwrap_or("Function call was rejected by the user"),
                )),
                Some(true) => {
                    // The run's tools are rebuilt for the resume: a tool the
                    // run no longer has (trading switched off meanwhile) is
                    // not run.
                    let text = match self.find(&p.call.fn_name).cloned() {
                        Some(tool) => self.execute(&tool, &p.call.call_id, p.args, out).await,
                        None => tool_guard::UNKNOWN_TOOL.to_string(),
                    };
                    outcome
                        .responses
                        .push(ToolResponse::new(&p.call.call_id, text));
                }
            }
        }
        Ok(outcome)
    }
}

fn pending(call: &ToolCall, tool: &Arc<dyn AgentTool>, args: Map<String, Value>) -> PendingCall {
    let spec = tool.spec();
    PendingCall {
        requirement: Requirement {
            id: format!("req-{}", uuid::Uuid::new_v4()),
            tool_call_id: call.call_id.clone(),
            tool_name: spec.name.into(),
            args: redact_map(&args),
            missing: tool_guard::missing_arguments(&spec.parameters, &args),
            kind: "confirmation".into(),
        },
        call: call.clone(),
        args,
    }
}
