//! The order guard as the order tools call it (the web's `get_guard` and
//! the `RiskGuard` methods): the trader's limits read fresh, the kill
//! switch file looked at, the session's counters loaded, the pure
//! [`risk`](super::risk) check run and its claim written, all in one step
//! on the blocking pool, in one write transaction.
//!
//! One book per Agent session (the web's
//! `get_guard(session_id or conversation_id or "default")`), so the cap
//! and the duplicate window span a whole conversation, not one message.

use super::risk::{self, Gate, Verdict};
use super::{counters, kill_switch, limits};
use crate::error::Result;
use crate::state::AppState;
use serde_json::{Map, Value};
use std::sync::Arc;

/// The book a run's checks share.
pub fn session_key(session_id: &str, conversation_id: Option<i64>) -> String {
    if !session_id.trim().is_empty() {
        session_id.to_string()
    } else if let Some(id) = conversation_id {
        id.to_string()
    } else {
        "default".into()
    }
}

fn gate(app: &AppState, analyzer_mode: Option<bool>) -> (limits::RiskLimits, Gate) {
    let limits = limits::read(app);
    let gate = Gate {
        kill_switch_file_present: kill_switch::present(&limits.kill_switch_path),
        analyzer_mode,
    };
    (limits, gate)
}

/// Every check the guard skipped for want of a price or funds goes to the
/// log, as the web's guard logs them.
fn log_warnings(verdict: &Verdict) {
    if let Some(Value::Array(warnings)) = verdict.details.get("warnings") {
        for w in warnings.iter().filter_map(Value::as_str) {
            tracing::warn!("Agent risk guard: {}", w);
        }
    }
}

/// Check one order and, when it passes, claim it for the session.
pub async fn check_order(
    app: &Arc<AppState>,
    session: &str,
    args: Map<String, Value>,
    analyzer_mode: Option<bool>,
) -> Result<Verdict> {
    let session = session.to_string();
    let verdict = app
        .blocking(move |ctx| {
            let (limits, gate) = gate(ctx, analyzer_mode);
            let now = ctx.now();
            let token = uuid::Uuid::new_v4().simple().to_string();
            let conn = ctx.sqlite.conn()?;
            counters::with_book(&conn, &session, now, |book| {
                risk::check_order(&limits, &gate, book, &args, now, Some(&token))
            })
        })
        .await?;
    log_warnings(&verdict);
    Ok(verdict)
}

/// Check a destructive operation that places nothing.
pub async fn check_destructive(
    app: &Arc<AppState>,
    operation: &'static str,
    analyzer_mode: Option<bool>,
    details: Map<String, Value>,
) -> Result<Verdict> {
    app.blocking(move |ctx| {
        let (limits, gate) = gate(ctx, analyzer_mode);
        Ok(risk::check_destructive(
            &limits,
            &gate,
            &Value::String(operation.into()),
            details,
        ))
    })
    .await
}

/// Check a modification against the order it changes (D-007).
pub async fn check_modify(
    app: &Arc<AppState>,
    args: Map<String, Value>,
    order: risk::ExistingOrder,
    analyzer_mode: Option<bool>,
    details: Map<String, Value>,
) -> Result<Verdict> {
    let verdict = app
        .blocking(move |ctx| {
            let (limits, gate) = gate(ctx, analyzer_mode);
            Ok(risk::check_modify(&limits, &gate, &args, &order, details))
        })
        .await?;
    log_warnings(&verdict);
    Ok(verdict)
}

/// Give back the claim of an order that was never sent.
pub async fn release(app: &Arc<AppState>, session: &str, verdict: &Verdict) -> Result<bool> {
    settle(app, session, verdict, risk::release).await
}

/// The claimed order was dispatched.
pub async fn commit(app: &Arc<AppState>, session: &str, verdict: &Verdict) -> Result<bool> {
    settle(app, session, verdict, risk::commit).await
}

async fn settle(
    app: &Arc<AppState>,
    session: &str,
    verdict: &Verdict,
    step: fn(&mut risk::book::Book, &Verdict) -> bool,
) -> Result<bool> {
    if verdict.claim_token().is_none() {
        return Ok(false);
    }
    let (session, verdict) = (session.to_string(), verdict.clone());
    app.blocking(move |ctx| {
        let conn = ctx.sqlite.conn()?;
        counters::with_book(&conn, &session, ctx.now(), |book| step(book, &verdict))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_without_a_session_shares_its_conversations_book() {
        assert_eq!(session_key("sess-1", Some(4)), "sess-1");
        assert_eq!(session_key(" ", Some(4)), "4");
        assert_eq!(session_key("", None), "default");
    }
}
