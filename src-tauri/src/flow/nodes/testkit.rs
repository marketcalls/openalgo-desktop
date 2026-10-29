//! What the logic family's tests share: a run context at a market time,
//! node data from JSON, and the run log as `(level, message)` pairs.

use crate::clock::ManualClock;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::NodeEnv;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeCtxParts, NodeOutcome};
use crate::flow::contract::subscriptions::Subscriptions;
use chrono::{NaiveDateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{Map, Value};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// Node data from a JSON object.
pub fn data(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// The instant of an IST wall time `YYYY-MM-DDTHH:MM:SS`.
pub fn ist(wall: &str) -> chrono::DateTime<Utc> {
    let naive = NaiveDateTime::parse_from_str(wall, "%Y-%m-%dT%H:%M:%S").unwrap();
    Kolkata
        .from_local_datetime(&naive)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

/// A context over `client` at the IST wall time `wall`, with `env`.
pub fn ctx_at(client: Arc<FakeFlowClient>, wall: &str, env: NodeEnv) -> NodeCtx {
    NodeCtx::new(NodeCtxParts {
        client,
        clock: ManualClock::new(ist(wall)),
        cancel: CancellationToken::new(),
        workflow_id: Some(1),
        default_strategy: "Test Flow".into(),
        subscriptions: Arc::new(Subscriptions::default()),
        session_boundary: (3, 0),
        env,
        app: Weak::new(),
    })
}

/// A context over a fresh fake client at 10:15 IST on a Thursday.
pub fn ctx() -> (Arc<FakeFlowClient>, NodeCtx) {
    let client = Arc::new(FakeFlowClient::new());
    let ctx = ctx_at(client.clone(), "2026-10-29T10:15:00", NodeEnv::default());
    (client, ctx)
}

/// The run log as `(level, message)`.
pub fn logs(ctx: &NodeCtx) -> Vec<(String, String)> {
    ctx.logs()
        .iter()
        .map(|e| {
            (
                e["level"].as_str().unwrap_or("").to_string(),
                e["message"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// The log messages alone.
pub fn messages(ctx: &NodeCtx) -> Vec<String> {
    logs(ctx).into_iter().map(|(_, m)| m).collect()
}

/// Run `node` on `d` in `ctx`.
pub async fn run(node: &dyn FlowNode, ctx: &NodeCtx, d: Value) -> NodeOutcome {
    node.run(ctx, &data(d)).await
}

/// The node's answer dict (panics on none or an abort).
pub fn answer(out: &NodeOutcome) -> Value {
    out.value().cloned().unwrap_or(Value::Null)
}
