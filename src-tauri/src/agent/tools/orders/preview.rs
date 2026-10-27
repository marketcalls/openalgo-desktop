//! The approval card's view of a smart order (security review,
//! 2026-10-27): before the trader approves, what the order will send from
//! the position held now to the target, decided by the same read and the
//! same smart-order table the tool uses after the approval. Read only:
//! nothing is checked by the guard, claimed or sent here.

use super::plans::smart_order;
use super::{reads, smart};
use crate::agent::contract::tool::{Capabilities, Surface, ToolCtx};
use crate::agent::contract::viz::VizSink;
use crate::agent::safety::previews;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// `{held, target, action, quantity}` (and `message` when there is
/// nothing to place); `held` is null when the position cannot be read,
/// and `error` names an argument the tool would refuse. With the paused
/// run's id, the position shown is recorded, so the tool sends nothing if
/// the position it reads after the approval is another one.
pub async fn smart_order_preview(
    app: &Arc<AppState>,
    user: &str,
    run_id: Option<&str>,
    args: Map<String, Value>,
) -> Value {
    let (order, target, quantity) = match smart_order(&args) {
        Ok(checked) => checked,
        Err(refusal) => return json!({"held": null, "error": refusal.text()}),
    };
    let key = match app.blocking(ApiKeyService::current).await {
        Ok(Some(key)) => key,
        Ok(None) | Err(_) => return json!({"held": null, "target": target}),
    };
    let ctx = ToolCtx {
        app: app.clone(),
        surface: Surface::Chat,
        conversation_id: None,
        run_id: String::new(),
        session_id: String::new(),
        user_id: user.to_string(),
        api_key: key,
        caps: Capabilities::default(),
        analyzer_mode: false,
        operator_message: String::new(),
        chart_context: None,
        viz: VizSink::new(),
        cancel: CancellationToken::new(),
    };
    let text = |k: &str| {
        order
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let held =
        reads::open_position_quantity(&ctx, &text("symbol"), &text("exchange"), &text("product"))
            .await;
    let Some(held) = held else {
        return json!({"held": null, "target": target});
    };
    if let Some(run) = run_id.filter(|r| !r.trim().is_empty()) {
        let (user, run, key) = (
            user.to_string(),
            run.to_string(),
            smart::order_key(&order, target, quantity),
        );
        let recorded = app
            .blocking(move |c| {
                previews::record(&*c.sqlite.conn()?, &user, &run, &key, held, c.now())
            })
            .await;
        if let Err(e) = recorded {
            // The card then says nothing it can be held to: the tool reads
            // the position itself and checks what it reads.
            tracing::warn!(
                "The position a smart order's card showed could not be recorded: {}",
                e
            );
        }
    }
    smart::describe(held, target, quantity, &text("action"))
}
