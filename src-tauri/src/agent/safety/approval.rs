//! The approval hook (`contract::approval::ApprovalHook`): each human
//! decision on a paused order goes into the audit trail before the run
//! resumes (the web's `audit.record_decision` in `/chat/confirm`), so an
//! approval is on record even if the resumed run then fails.
//!
//! The paused run itself is core's (`ag_paused_run`): the confirm route
//! claims it in one statement before it calls [`AuditTrail::decided`], so
//! a second approval of the same run (two tabs, a double click, the panel
//! and the chat page together) finds it already answered and resumes
//! nothing (D-001), and a run left paused past ten minutes is refused and
//! reported here as abandoned (D-002). Approval is only a decision: the
//! risk guard still runs inside the order tool and can still refuse.
//!
//! Desktop fix (D-009): the decision row carries the arguments the trader
//! approved, redacted, beside the call id; the web stored the call id only.

use super::audit::{self, Entry, Phase};
use crate::agent::contract::approval::{ApprovalHook, Decision, DecisionScope};
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The tool name a decision row carries when the call named none.
pub const UNNAMED_TOOL: &str = "pending_tool";

/// The note a trader typed with a decision, cut as the web cuts it.
const MAX_NOTE_CHARS: usize = 500;

/// Records every decision, and every paused run that will never resume.
pub struct AuditTrail;

/// The `decision` row of one decision.
pub fn decision_entry(scope: &DecisionScope, d: &Decision) -> Entry {
    let name = if d.tool_name.trim().is_empty() {
        UNNAMED_TOOL
    } else {
        d.tool_name.as_str()
    };
    let mut entry = Entry::new(Phase::Decision, name);
    entry.conversation_id = scope.conversation_id;
    entry.run_id = Some(scope.run_id.clone());
    entry.args = Some(json!({
        "tool_call_id": d.tool_call_id,
        "arguments": Value::Object(d.args.clone()),
    }));
    entry.risk_verdict = Some(
        if d.approved {
            "decision:approved"
        } else {
            "decision:rejected"
        }
        .into(),
    );
    entry.ok = Some(d.approved);
    let mut response = Map::new();
    response.insert("approved".into(), json!(d.approved));
    if !scope.actor.is_empty() {
        response.insert("actor".into(), json!(scope.actor));
    }
    if let Some(note) = scope
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let note: String = note.chars().take(MAX_NOTE_CHARS).collect();
        response.insert("note".into(), json!(note));
    }
    entry.response = Some(Value::Object(response));
    entry
}

/// The row of a paused run that ended without a decision (it expired, was
/// cancelled, or its conversation was deleted): the record shows its
/// orders were never approved.
pub fn abandoned_entry(run_id: &str, conversation_id: Option<i64>) -> Entry {
    let mut entry = Entry::new(Phase::Decision, UNNAMED_TOOL);
    entry.conversation_id = conversation_id;
    entry.run_id = Some(run_id.to_string());
    entry.risk_verdict = Some("decision:abandoned".into());
    entry.ok = Some(false);
    entry.response = Some(json!({"approved": false, "abandoned": true}));
    entry
}

#[async_trait]
impl ApprovalHook for AuditTrail {
    async fn decided(&self, app: &Arc<AppState>, scope: &DecisionScope, decisions: &[Decision]) {
        for d in decisions {
            if audit::append(app, decision_entry(scope, d)).await.is_none() {
                tracing::warn!(
                    "The decision on {} in run {} is not in the audit trail",
                    d.tool_name,
                    scope.run_id
                );
            }
        }
    }

    async fn abandoned(&self, app: &Arc<AppState>, run_id: &str) {
        let run = run_id.to_string();
        let conversation = app
            .blocking(move |ctx| {
                Ok(ctx
                    .sqlite
                    .conn()?
                    .query_row(
                        "SELECT conversation_id FROM ag_paused_run WHERE run_id = ?1",
                        [&run],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok())
            })
            .await
            .unwrap_or(None);
        audit::append(app, abandoned_entry(run_id, conversation)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::conversations::store::{self, Claim, PausedRow};
    use crate::agent::safety::audit::{list, Filter, Listing};
    use crate::state::testing::{build, TestCtx};
    use chrono::{Duration, Utc};

    fn app() -> TestCtx {
        build(crate::brokers::BrokerRegistry::new(), Utc::now())
    }

    async fn rows(t: &TestCtx) -> Vec<Value> {
        let f = Filter {
            limit: 100,
            ..Filter::default()
        };
        match list(&t.ctx, "trader", f).await.unwrap() {
            Listing::Rows(r) => r,
            Listing::NotYours => panic!(),
        }
    }

    fn decision(approved: bool) -> Decision {
        let args = json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10,
            "product": "MIS", "api_key": "secret-value"});
        Decision {
            tool_call_id: "call-1".into(),
            tool_name: "place_order".into(),
            args: args.as_object().cloned().unwrap_or_default(),
            approved,
        }
    }

    fn scope(note: Option<&str>) -> DecisionScope {
        DecisionScope {
            conversation_id: Some(3),
            run_id: "run-1".into(),
            actor: "trader".into(),
            note: note.map(str::to_string),
        }
    }

    /// Each decision is one row, written before the run resumes, carrying
    /// what the trader approved (D-009) with every credential removed.
    #[tokio::test]
    async fn every_decision_is_recorded_with_what_was_approved() {
        let t = app();
        let long_note = "n".repeat(600);
        AuditTrail
            .decided(&t.ctx, &scope(Some(&long_note)), &[decision(true)])
            .await;
        AuditTrail
            .decided(
                &t.ctx,
                &scope(None),
                &[Decision {
                    tool_name: " ".into(),
                    ..decision(false)
                }],
            )
            .await;
        let rows = rows(&t).await;
        let (rejected, approved) = (&rows[0], &rows[1]);
        assert_eq!(approved["phase"], "decision");
        assert_eq!(approved["tool"], "place_order");
        assert_eq!(approved["risk_verdict"], "decision:approved");
        assert_eq!(approved["ok"], true);
        assert_eq!(approved["run_id"], "run-1");
        assert_eq!(approved["conversation_id"], 3);
        assert_eq!(approved["args"]["tool_call_id"], "call-1");
        assert_eq!(approved["args"]["arguments"]["symbol"], "SBIN");
        assert_eq!(approved["args"]["arguments"]["api_key"], "[redacted]");
        assert_eq!(approved["response"]["actor"], "trader");
        assert_eq!(approved["response"]["note"].as_str().unwrap().len(), 500);
        assert_eq!(rejected["tool"], UNNAMED_TOOL);
        assert_eq!(rejected["risk_verdict"], "decision:rejected");
        assert_eq!(rejected["ok"], false);
        assert_eq!(
            rejected["response"],
            json!({"approved": false, "actor": "trader"})
        );
        assert!(!serde_json::to_string(&rows)
            .unwrap()
            .contains("secret-value"));
    }

    fn pause(t: &TestCtx) -> String {
        let conn = t.ctx.sqlite.conn().unwrap();
        let now = t.ctx.now();
        let c = store::create_conversation(&conn, "trader", None, "chat", now).unwrap();
        store::save_paused(
            &conn,
            &PausedRow {
                run_id: "run-1".into(),
                conversation_id: c.id,
                session_id: "session-1".into(),
                user_id: "trader".into(),
                pending: "[]".into(),
                paused_at: store::stamp(now),
                expires_at: store::stamp(now + Duration::minutes(10)),
            },
        )
        .unwrap();
        "run-1".into()
    }

    /// The confirm route's order: claim the run, then record, then resume.
    async fn approve(app: Arc<AppState>, run: String, barrier: Arc<tokio::sync::Barrier>) -> Claim {
        barrier.wait().await;
        let r = run.clone();
        let claim = app
            .blocking(move |ctx| store::claim_paused(&*ctx.sqlite.conn()?, &r, "trader", ctx.now()))
            .await
            .unwrap();
        if matches!(claim, Claim::Claimed(_)) {
            let s = DecisionScope {
                run_id: run,
                ..scope(None)
            };
            AuditTrail.decided(&app, &s, &[decision(true)]).await;
        }
        claim
    }

    /// D-001 (Q-1035): approving one paused run from two tabs at once
    /// resumes it once and records one decision; the web resumed it twice,
    /// so an approved order could be placed twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_approvals_of_one_run_resume_it_once() {
        let t = app();
        for _ in 0..10 {
            t.ctx
                .sqlite
                .conn()
                .unwrap()
                .execute("DELETE FROM ag_paused_run", [])
                .unwrap();
            let run = pause(&t);
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let a = tokio::spawn(approve(t.ctx.clone(), run.clone(), barrier.clone()));
            let b = tokio::spawn(approve(t.ctx.clone(), run.clone(), barrier));
            let claims = [a.await.unwrap(), b.await.unwrap()];
            let won = claims
                .iter()
                .filter(|c| matches!(c, Claim::Claimed(_)))
                .count();
            let refused = claims
                .iter()
                .filter(|c| **c == Claim::AlreadyAnswered)
                .count();
            assert_eq!((won, refused), (1, 1), "{:?}", claims);
        }
        let decisions = rows(&t)
            .await
            .iter()
            .filter(|r| r["phase"] == "decision")
            .count();
        assert_eq!(decisions, 10, "one decision row per run");
    }

    /// D-002: an approval left open past ten minutes is refused, the run is
    /// cleared, and the trail shows it was never approved.
    #[tokio::test]
    async fn an_approval_left_past_ten_minutes_is_refused_and_recorded() {
        let t = app();
        let run = pause(&t);
        t.clock
            .advance(Duration::minutes(10) + Duration::seconds(1));
        let claim = store::claim_paused(&t.ctx.sqlite.conn().unwrap(), &run, "trader", t.ctx.now())
            .unwrap();
        assert_eq!(claim, Claim::Expired);
        AuditTrail.abandoned(&t.ctx, &run).await;
        let again = store::claim_paused(&t.ctx.sqlite.conn().unwrap(), &run, "trader", t.ctx.now())
            .unwrap();
        assert_eq!(again, Claim::Expired, "an expired run never resumes");
        let rows = rows(&t).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["risk_verdict"], "decision:abandoned");
        assert_eq!(rows[0]["ok"], false);
        assert_eq!(rows[0]["run_id"], "run-1");
        assert!(rows[0]["conversation_id"].as_i64().is_some(), "{}", rows[0]);
    }

    /// Before ten minutes the same approval goes through.
    #[tokio::test]
    async fn an_approval_inside_ten_minutes_resumes() {
        let t = app();
        let run = pause(&t);
        t.clock.advance(Duration::minutes(9));
        let claim = store::claim_paused(&t.ctx.sqlite.conn().unwrap(), &run, "trader", t.ctx.now())
            .unwrap();
        assert!(matches!(claim, Claim::Claimed(_)), "{:?}", claim);
    }

    /// The app's Agent records decisions without anything installing a
    /// hook first.
    #[tokio::test]
    async fn the_apps_hook_records_decisions() {
        let t = app();
        t.ctx
            .agent
            .approvals()
            .decided(&t.ctx, &scope(None), &[decision(true)])
            .await;
        assert_eq!(rows(&t).await.len(), 1);
    }
}
