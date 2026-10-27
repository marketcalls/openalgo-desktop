//! The one path every order tool runs (the web's
//! `OrdersToolkit._run_mutation`), its steps fixed and written once:
//!
//! 1. the audit `attempt` row, before anything is checked or sent;
//! 2. the plan: the arguments checked, the guard's reads made (bounded by
//!    [`PREPARE_TIMEOUT`]) and the guard's verdict, its claim written;
//! 3. a refusal returned as the result, never raised, so the model reports
//!    it instead of retrying it as a passing fault;
//! 4. the dispatch through `/api/v1` (bounded by [`DISPATCH_TIMEOUT`]);
//! 5. the audit `result` row.
//!
//! A placement has three outcomes, never two (CLAUDE.md, "An order path
//! decides once, under the lock"): answered (placed, or refused, with the
//! claim given back), not sent (nothing reached the handlers, claim given
//! back), or unknown (it may have reached the broker: the claim is kept so
//! the duplicate window refuses an immediate identical resend, and the
//! model is told not to send it again and to have the trader check the
//! order book).

use super::plans::{Plan, Unplanned};
use crate::agent::contract::json::dumps_capped;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome, MAX_RESULT_CHARS};
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::redact::redact;
use crate::agent::safety::audit::{self, Entry, Phase};
use crate::agent::safety::guard;
use crate::agent::safety::risk::Verdict;
use crate::agent::tools::service::{extract_detail, unwrap, ServiceReply};
use crate::mcp::dispatch::Transport;
use serde_json::{json, Map, Value};
use std::time::Duration;

/// How long a tool waits for the reads before the guard (the web's
/// `AGENT_PREPARE_TIMEOUT_SECONDS`).
pub const PREPARE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a tool waits for its service call (the web's
/// `AGENT_DISPATCH_TIMEOUT_SECONDS`). A call still running then may yet
/// reach the broker, so its outcome is unknown.
pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Nothing reached the handlers (the web's `_HUB_BUSY_MESSAGE`).
pub const NOT_SENT_MESSAGE: &str = "The web server is too busy to take this call right now, so nothing was sent to the broker. Try again in a few seconds.";

/// The platform failed before sending anything.
pub const INTERNAL_MESSAGE: &str = "The platform hit an internal error while checking this request, before anything was sent to the broker, so nothing was placed or changed. Tell the operator it failed on the platform side and that the details are in the platform's error log. Do not call the tool again.";

/// The call may have reached the broker.
pub const UNKNOWN_MESSAGE: &str = "The platform failed while sending this to the broker. Whether the broker received it is not known, so do NOT send it again. Tell the operator to check the order book and positions before doing anything else.";

/// What the dispatch came back with.
pub(super) enum Sent {
    /// `/api/v1` answered definitely, placed or refused.
    Answered(ServiceReply),
    /// Nothing reached the handlers: nothing was sent.
    NotDispatched(String),
    /// It may have reached the broker; the detail is for the audit trail.
    Unknown(String),
    /// Refused at dispatch (the position moved since it was checked):
    /// nothing was sent.
    NotPlaced {
        stage: &'static str,
        message: String,
    },
    /// Already where the order would take it: nothing was sent.
    NothingPlaced(String),
    /// A square-off whose position read zero: no order was sent.
    NothingToClose {
        symbol: String,
        exchange: String,
        product: String,
    },
}

impl Sent {
    /// Sort a reply into the three outcomes.
    pub(super) fn of(reply: ServiceReply) -> Sent {
        match reply {
            ServiceReply::NoAnswer(Transport::NotSent) => Sent::NotDispatched(
                "NotSent: the request could not be handed to the handlers".into(),
            ),
            ServiceReply::NoAnswer(Transport::Budget) => {
                Sent::NotDispatched("Budget: the in-process request budget was spent".into())
            }
            ServiceReply::NoAnswer(Transport::Timeout) => Sent::Unknown(format!(
                "Timeout: no answer within {} seconds",
                DISPATCH_TIMEOUT.as_secs()
            )),
            ServiceReply::NoAnswer(Transport::NoReply) => {
                Sent::Unknown("NoReply: the handlers ran but no answer came back".into())
            }
            ServiceReply::Answered {
                outcome_unknown: true,
                body,
                ..
            } => Sent::Unknown(format!("Uncertain: {}", extract_detail(&body))),
            answered => Sent::Answered(answered),
        }
    }
}

/// The run's labels for one call.
pub(super) struct Call<'a> {
    pub ctx: &'a ToolCtx,
    pub tool: &'static str,
    /// Labels on the result's wrapper (symbol, exchange, order id).
    pub attributes: Vec<(&'static str, String)>,
    pub attempt_id: Option<i64>,
}

impl Call<'_> {
    fn entry(&self, phase: Phase) -> Entry {
        let mut e = Entry::new(phase, self.tool);
        e.conversation_id = self.ctx.conversation_id;
        e.run_id = Some(self.ctx.run_id.clone());
        e
    }

    async fn result_row(
        &self,
        ok: bool,
        response: Value,
        verdict: Option<&Verdict>,
        order_ids: Vec<String>,
    ) {
        let mut e = self.entry(Phase::Result);
        e.attempt_id = self.attempt_id;
        e.ok = Some(ok);
        e.response = Some(response);
        e.order_ids = Some(order_ids);
        e.risk_verdict = verdict.map(|v| v.code.as_str().to_string());
        audit::append(&self.ctx.app, e).await;
    }

    /// The result as the model reads it: capped JSON, labelled as data.
    fn emit(&self, payload: Value) -> ToolOutcome {
        let attrs: Vec<(&str, Option<&str>)> = self
            .attributes
            .iter()
            .map(|(k, v)| (*k, Some(v.as_str())))
            .collect();
        ToolOutcome::Ok(wrap_tool_result(
            self.tool,
            &dumps_capped(&payload, MAX_RESULT_CHARS),
            &attrs,
        ))
    }

    fn session(&self) -> String {
        guard::session_key(&self.ctx.session_id, self.ctx.conversation_id)
    }

    async fn release(&self, verdict: &Verdict) {
        if let Err(e) = guard::release(&self.ctx.app, &self.session(), verdict).await {
            tracing::error!(
                "The risk guard's claim for {} could not be given back: {}",
                self.tool,
                e
            );
        }
    }
}

/// Run one order tool through the five steps.
pub(super) async fn run_mutation(
    ctx: &ToolCtx,
    tool: &'static str,
    args: &Map<String, Value>,
    attributes: Vec<(&'static str, String)>,
    plan: impl std::future::Future<Output = Result<Plan, Unplanned>>,
) -> ToolOutcome {
    let mut attempt = Entry::new(Phase::Attempt, tool);
    attempt.conversation_id = ctx.conversation_id;
    attempt.run_id = Some(ctx.run_id.clone());
    attempt.args = Some(Value::Object(args.clone()));
    let attempt_id = audit::append(&ctx.app, attempt).await;
    let call = Call {
        ctx,
        tool,
        attributes,
        attempt_id,
    };
    let plan = match plan.await {
        Ok(plan) => plan,
        Err(refused) => return not_planned(&call, refused).await,
    };
    if !plan.verdict.allowed {
        return blocked(&call, &plan.verdict).await;
    }
    let sent = super::plans::dispatch(ctx, &plan.dispatch).await;
    settle(&call, &plan, sent).await
}

async fn not_planned(call: &Call<'_>, refused: Unplanned) -> ToolOutcome {
    match refused {
        Unplanned::Retry(message) => {
            let response = json!({"status": "error", "stage": "validation", "message": message});
            call.result_row(false, response, None, Vec::new()).await;
            ToolOutcome::Retry(message)
        }
        Unplanned::Internal(exception) => {
            tracing::error!(
                "Agent tool {} could not be prepared: {}",
                call.tool,
                exception
            );
            let response = json!({"status": "error", "stage": "validation",
                "message": INTERNAL_MESSAGE, "exception": exception});
            call.result_row(false, response, None, Vec::new()).await;
            call.emit(json!({"ok": false, "tool": call.tool, "status": "error",
                "message": INTERNAL_MESSAGE, "retry": false}))
        }
        Unplanned::Unverified { stage, message } => {
            tracing::warn!(
                "Agent tool {} refused unverified ({}): {}",
                call.tool,
                stage,
                message
            );
            let response = json!({"status": "error", "stage": stage, "message": message});
            call.result_row(false, response, None, Vec::new()).await;
            call.emit(json!({"ok": false, "tool": call.tool, "status": "error",
                "message": message, "retry": false}))
        }
        Unplanned::NothingToPlace { message, extra } => {
            let response = json!({"status": "success", "message": message});
            call.result_row(true, response.clone(), None, Vec::new())
                .await;
            let mut result = json!({"ok": true, "tool": call.tool, "status": "success",
                "order_ids": [], "message": message, "response": response});
            if let Value::Object(r) = &mut result {
                r.extend(extra);
            }
            call.emit(result)
        }
    }
}

async fn blocked(call: &Call<'_>, verdict: &Verdict) -> ToolOutcome {
    let risk = verdict.as_dict();
    call.result_row(
        false,
        json!({"status": "blocked", "risk": risk}),
        Some(verdict),
        Vec::new(),
    )
    .await;
    tracing::warn!(
        "Agent risk guard refused {}: {}",
        call.tool,
        verdict.as_message()
    );
    call.emit(json!({"ok": false, "tool": call.tool, "status": "blocked",
        "blocked_by": "risk_guard", "code": verdict.code.as_str(),
        "message": verdict.as_message(), "risk": risk, "retry": false}))
}

/// Steps 4 and 5 after the dispatch: give the claim back when nothing was
/// placed, keep it when the outcome is unknown, commit it when placed.
async fn settle(call: &Call<'_>, plan: &Plan, sent: Sent) -> ToolOutcome {
    let verdict = &plan.verdict;
    let risk = verdict.as_dict();
    let reply = match unsent(call, verdict, sent).await {
        Ok(reply) => reply,
        Err(outcome) => return outcome,
    };
    let body = match &reply {
        ServiceReply::Answered { body, .. } => body.clone(),
        ServiceReply::NoAnswer(_) => Value::Null,
    };
    match unwrap(plan.label, reply) {
        Err(_) => {
            call.release(verdict).await;
            let message = super::validate::refusal_message(&extract_detail(&body));
            call.result_row(
                false,
                json!({"status": "error", "message": message, "risk": risk}),
                Some(verdict),
                Vec::new(),
            )
            .await;
            call.emit(json!({"ok": false, "tool": call.tool, "status": "error",
                "message": message, "risk": risk, "retry": false}))
        }
        Ok(payload) => placed(call, plan, payload).await,
    }
}

/// The outcomes where no answer to a placement came back: nothing sent
/// (the claim goes back), or sent with no definite answer (the claim is
/// kept). `Ok` is an answer to settle.
async fn unsent(
    call: &Call<'_>,
    verdict: &Verdict,
    sent: Sent,
) -> Result<ServiceReply, ToolOutcome> {
    let risk = verdict.as_dict();
    match sent {
        Sent::NotDispatched(detail) => {
            call.release(verdict).await;
            tracing::warn!("Agent tool {} was not dispatched: {}", call.tool, detail);
            call.result_row(
                false,
                json!({"status": "error", "message": NOT_SENT_MESSAGE, "risk": risk}),
                Some(verdict),
                Vec::new(),
            )
            .await;
            Err(
                call.emit(json!({"ok": false, "tool": call.tool, "status": "error",
                "message": NOT_SENT_MESSAGE, "risk": risk, "retry": false})),
            )
        }
        Sent::Unknown(exception) => {
            tracing::error!(
                "Agent tool {}: the outcome at the broker is unknown: {}",
                call.tool,
                crate::agent::redact::redact_text(&exception)
            );
            call.result_row(
                false,
                json!({"status": "unknown", "message": UNKNOWN_MESSAGE,
                "exception": exception, "risk": risk}),
                Some(verdict),
                Vec::new(),
            )
            .await;
            Err(
                call.emit(json!({"ok": false, "tool": call.tool, "status": "unknown",
                "message": UNKNOWN_MESSAGE, "risk": risk, "retry": false})),
            )
        }
        Sent::NothingToClose {
            symbol,
            exchange,
            product,
        } => {
            call.release(verdict).await;
            let message = nothing_to_close(&symbol, &exchange, &product);
            call.result_row(
                false,
                json!({"status": "nothing_to_close", "message": message}),
                Some(verdict),
                Vec::new(),
            )
            .await;
            Err(call.emit(
                json!({"ok": false, "tool": call.tool, "status": "nothing_to_close",
                "message": message, "symbol": symbol, "exchange": exchange, "product": product,
                "quantity": 0, "order_ids": [], "retry": false}),
            ))
        }
        Sent::NotPlaced { stage, message } => {
            call.release(verdict).await;
            tracing::warn!(
                "Agent tool {} sent nothing ({}): {}",
                call.tool,
                stage,
                message
            );
            call.result_row(
                false,
                json!({"status": "error", "stage": stage, "message": message, "risk": risk}),
                Some(verdict),
                Vec::new(),
            )
            .await;
            Err(
                call.emit(json!({"ok": false, "tool": call.tool, "status": "error",
                "message": message, "risk": risk, "retry": false})),
            )
        }
        Sent::NothingPlaced(message) => {
            call.release(verdict).await;
            let response = json!({"status": "success", "message": message});
            call.result_row(true, response.clone(), Some(verdict), Vec::new())
                .await;
            Err(
                call.emit(json!({"ok": true, "tool": call.tool, "status": "success",
                "order_ids": [], "message": message, "response": response})),
            )
        }
        Sent::Answered(reply) => Ok(reply),
    }
}

async fn placed(call: &Call<'_>, plan: &Plan, payload: Value) -> ToolOutcome {
    let verdict = &plan.verdict;
    if let Err(e) = guard::commit(&call.ctx.app, &call.session(), verdict).await {
        tracing::error!(
            "The risk guard's claim for {} could not be committed: {}",
            call.tool,
            e
        );
    }
    let order_ids = audit::extract_order_ids(&payload);
    call.result_row(true, payload.clone(), Some(verdict), order_ids.clone())
        .await;
    let mut result = json!({"ok": true, "tool": call.tool, "status": "success",
        "order_ids": order_ids, "risk": verdict.as_dict(), "response": redact(&payload)});
    if let Value::Object(r) = &mut result {
        for (k, v) in &plan.extra {
            r.insert(k.clone(), v.clone());
        }
    }
    call.emit(result)
}

/// A square-off that read zero: neither a success nor a failure, because
/// zero is not proof the position is closed.
pub fn nothing_to_close(symbol: &str, exchange: &str, product: &str) -> String {
    format!(
        "The position service reported no open quantity for {} on {} under {}, so no order was sent. That does not prove the position is closed: some brokers report positions as flat when they are not, and a symbol, exchange or product that differs from the position book also reads as zero. Call get_positions and check this exact symbol, exchange and product before telling the operator anything is closed.",
        symbol, exchange, product
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_answer_is_unknown_and_an_unsent_one_is_not() {
        assert!(matches!(
            Sent::of(ServiceReply::NoAnswer(Transport::NotSent)),
            Sent::NotDispatched(_)
        ));
        assert!(matches!(
            Sent::of(ServiceReply::NoAnswer(Transport::Budget)),
            Sent::NotDispatched(_)
        ));
        assert!(matches!(
            Sent::of(ServiceReply::NoAnswer(Transport::Timeout)),
            Sent::Unknown(_)
        ));
        assert!(matches!(
            Sent::of(ServiceReply::NoAnswer(Transport::NoReply)),
            Sent::Unknown(_)
        ));
        let uncertain = ServiceReply::Answered {
            status: 500,
            body: json!({"status": "error", "message": "Check the order book."}),
            outcome_unknown: true,
        };
        let Sent::Unknown(detail) = Sent::of(uncertain) else {
            panic!()
        };
        assert_eq!(detail, "Uncertain: Check the order book.");
        let refused = ServiceReply::Answered {
            status: 400,
            body: json!({"status": "error", "message": "Invalid quantity"}),
            outcome_unknown: false,
        };
        assert!(matches!(Sent::of(refused), Sent::Answered(_)));
    }
}
