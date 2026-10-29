//! Contexts, fakes and runners shared by the data family's tests.

use crate::clock::ManualClock;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::env::{NodeEnv, StrategyBook, TradingCalendar};
use crate::flow::contract::node::{NodeCtx, NodeCtxParts, NodeOutcome};
use crate::flow::contract::subscriptions::Subscriptions;
use crate::flow::contract::vars::DEFAULT_BOUNDARY;
use crate::flow::nodes::{data, NodeRegistry};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// IST wall time `YYYY-MM-DDTHH:MM:SS` as a UTC instant.
pub fn ist(text: &str) -> DateTime<Utc> {
    let wall = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .unwrap_or_else(|e| panic!("bad time {}: {}", text, e));
    Kolkata
        .from_local_datetime(&wall)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| panic!("ambiguous time {}", text))
}

/// A run's context at IST wall time `at`, for a workflow named `workflow`.
pub fn ctx_at(client: Arc<FakeFlowClient>, at: &str, workflow: &str, env: NodeEnv) -> NodeCtx {
    NodeCtx::new(NodeCtxParts {
        client,
        clock: ManualClock::new(ist(at)),
        cancel: CancellationToken::new(),
        workflow_id: None,
        default_strategy: workflow.to_string(),
        subscriptions: Arc::new(Subscriptions::default()),
        session_boundary: DEFAULT_BOUNDARY,
        env,
        app: Weak::new(),
    })
}

/// A context on Thursday 29 October 2026, 10:15 IST (the goldens' clock).
pub fn ctx(client: Arc<FakeFlowClient>) -> NodeCtx {
    ctx_at(
        client,
        "2026-10-29T10:15:00",
        "Golden Flow",
        NodeEnv::default(),
    )
}

pub fn map(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Run the data node of type `node_type` with `data`.
pub async fn run(ctx: &NodeCtx, node_type: &str, data: Value) -> NodeOutcome {
    let registry = NodeRegistry::new(&[data::FAMILY]);
    let node = registry
        .get(node_type)
        .unwrap_or_else(|| panic!("{} is not a data node", node_type));
    node.run(ctx, &map(data)).await
}

/// The run's log as `(level, message)`.
pub fn logs(ctx: &NodeCtx) -> Vec<(String, String)> {
    ctx.logs()
        .iter()
        .map(|l| {
            (
                l["level"].as_str().unwrap_or_default().to_string(),
                l["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A strategy book holding `legs` (filtered by strategy, as the web's
/// `get_strategy_legs` filters), or one that cannot be read.
pub struct FakeBook(pub Result<Vec<Value>, String>);

impl StrategyBook for FakeBook {
    fn legs(&self, strategy: &str) -> Result<Vec<Value>, String> {
        let legs = self.0.clone()?;
        Ok(legs
            .into_iter()
            .filter(|l| l.get("strategy").and_then(Value::as_str) == Some(strategy))
            .collect())
    }
}

/// A market calendar: each year's entries, a year that cannot be read, or
/// (absent) a year with no holidays.
#[derive(Default)]
pub struct FakeCalendar(pub HashMap<i32, Result<Vec<Value>, String>>);

impl TradingCalendar for FakeCalendar {
    fn holidays(&self, year: i32) -> Result<Vec<Value>, String> {
        self.0.get(&year).cloned().unwrap_or_else(|| Ok(Vec::new()))
    }
}

/// An environment with this book and calendar.
pub fn env_with(book: FakeBook, calendar: FakeCalendar) -> NodeEnv {
    NodeEnv {
        strategy_book: Arc::new(book),
        calendar: Arc::new(calendar),
        ..Default::default()
    }
}
