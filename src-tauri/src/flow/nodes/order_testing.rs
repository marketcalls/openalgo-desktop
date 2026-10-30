//! What the order families' tests share: a run context over the scripted
//! client at a fixed IST time with a scripted master contract, and running
//! one node alone or through the walk (which adds the unresolved-field
//! refusal and "Stopping this branch").

use crate::brokers::common::symbols::SymToken;
use crate::clock::ManualClock;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::client::ApiCall;
use crate::flow::contract::env::{NodeEnv, SymbolMaster, SymbolQuery};
use crate::flow::contract::model::Graph;
use crate::flow::contract::node::{NodeCtx, NodeCtxParts, NodeOutcome};
use crate::flow::contract::subscriptions::Subscriptions;
use crate::flow::contract::vars::DEFAULT_BOUNDARY;
use crate::flow::executor::walk::Walk;
use crate::flow::nodes::NodeRegistry;
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// The workflow name the goldens run under (the default order tag).
pub const WORKFLOW: &str = "Golden Flow";

/// A master contract row: symbol, name, exchange and lot size.
pub fn master_row(symbol: &str, name: &str, exchange: &str, lot: i32) -> SymToken {
    SymToken {
        symbol: symbol.into(),
        brsymbol: symbol.into(),
        name: name.into(),
        exchange: exchange.into(),
        brexchange: exchange.into(),
        token: String::new(),
        expiry: String::new(),
        strike: 0.0,
        lot_size: lot,
        instrument_type: String::new(),
        tick_size: 0.05,
    }
}

/// A scripted master contract (`None`: every read fails).
pub struct Master(pub Option<Vec<SymToken>>);

impl SymbolMaster for Master {
    fn rows(&self, q: &SymbolQuery) -> Result<Vec<SymToken>, String> {
        let rows = self.0.as_ref().ok_or("database is locked")?;
        let same = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        let mut out: Vec<SymToken> = rows
            .iter()
            .filter(|r| {
                same(&q.symbol, &r.symbol)
                    && same(&q.name, &r.name)
                    && same(&q.exchange, &r.exchange)
                    && q.symbol_prefix
                        .as_deref()
                        .is_none_or(|p| r.symbol.starts_with(p))
            })
            .cloned()
            .collect();
        if let Some(n) = q.limit {
            out.truncate(n);
        }
        Ok(out)
    }
}

/// The NIFTY, BANKNIFTY, SENSEX and CRUDEOIL contracts most tests size
/// from.
pub fn standard_master() -> Vec<SymToken> {
    vec![
        master_row("NIFTY25NOV2624000CE", "NIFTY", "NFO", 75),
        master_row("BANKNIFTY25NOV2652000CE", "BANKNIFTY", "NFO", 30),
        master_row("SENSEX27NOV2680000CE", "SENSEX", "BFO", 20),
        master_row("CRUDEOIL17NOV266000CE", "CRUDEOIL", "MCX", 100),
    ]
}

/// One run: the scripted client and the context over it.
pub struct Run {
    pub client: Arc<FakeFlowClient>,
    pub ctx: NodeCtx,
}

impl Run {
    /// A run at `ist` (`2026-10-29T10:15:00`) with `master`.
    pub fn at(ist: &str, master: Option<Vec<SymToken>>) -> Run {
        let naive = NaiveDateTime::parse_from_str(ist, "%Y-%m-%dT%H:%M:%S").unwrap_or_default();
        let now = Kolkata
            .from_local_datetime(&naive)
            .single()
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default();
        let client = Arc::new(FakeFlowClient::new());
        let env = NodeEnv {
            symbols: Arc::new(Master(master)),
            ..NodeEnv::default()
        };
        let ctx = NodeCtx::new(NodeCtxParts {
            client: client.clone(),
            clock: ManualClock::new(now),
            cancel: CancellationToken::new(),
            workflow_id: None,
            default_strategy: WORKFLOW.into(),
            subscriptions: Arc::new(Subscriptions::default()),
            session_boundary: DEFAULT_BOUNDARY,
            env,
            app: Weak::new(),
        });
        Run { client, ctx }
    }

    /// The usual run: 29 October 2026, 10:15 IST, the standard master.
    pub fn new() -> Run {
        Self::default()
    }

    /// Script `endpoint`'s reply.
    pub fn reply(&self, endpoint: &str, reply: Value) -> &Self {
        self.client.reply(endpoint, reply);
        self
    }

    /// Run `node_type` on `data` through the walk, as the only node; the
    /// node's own outcome is not visible there, so the walk's failures
    /// are returned.
    pub async fn walk(&self, node_type: &str, data: Value) -> Vec<Value> {
        let nodes = json!([{"id": "n1", "type": node_type, "data": data}]);
        let Ok(graph) = Graph::parse(&nodes, &json!([])) else {
            return vec![json!("graph did not parse")];
        };
        let registry = NodeRegistry::default();
        let mut walk = Walk::new(&graph, &registry, &self.ctx);
        if let Err(abort) = walk.visit("n1", 0).await {
            return vec![json!({"abort": abort})];
        }
        walk.errors
    }

    /// Run `node_type` on `data` directly.
    pub async fn node(&self, node_type: &str, data: Value) -> NodeOutcome {
        let registry = NodeRegistry::default();
        match registry.get(node_type) {
            Some(n) => n.run(&self.ctx, &object(data)).await,
            None => NodeOutcome::abort(format!("{} is not registered", node_type)),
        }
    }

    pub fn calls(&self) -> Vec<ApiCall> {
        self.client.calls()
    }

    /// The single call made (panics, failing the test, when there is not
    /// exactly one).
    pub fn only_call(&self) -> ApiCall {
        let calls = self.calls();
        assert_eq!(calls.len(), 1, "{:?}", calls);
        calls[0].clone()
    }

    /// The run log, `[level, message]`.
    pub fn log(&self) -> Vec<(String, String)> {
        self.ctx
            .logs()
            .iter()
            .map(|l| {
                let s = |k: &str| l[k].as_str().unwrap_or_default().to_string();
                (s("level"), s("message"))
            })
            .collect()
    }

    /// Only the messages of the run log.
    pub fn messages(&self) -> Vec<String> {
        self.log().into_iter().map(|(_, m)| m).collect()
    }
}

impl Default for Run {
    fn default() -> Self {
        Self::at("2026-10-29T10:15:00", Some(standard_master()))
    }
}

/// A JSON object as node data.
pub fn object(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// The `{"status": "success", "orderid": ...}` a placement answers.
pub fn placed() -> Value {
    json!({"status": "success", "orderid": "250408000123456"})
}

/// The message of an error outcome.
pub fn message(o: &NodeOutcome) -> String {
    o.value()
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
