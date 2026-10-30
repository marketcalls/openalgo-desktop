//! What every node implements and what it is given (the web's
//! `NodeExecutor`: one `execute_<type>` method per node, the shared
//! accessors, the run's log).
//!
//! A node is a [`FlowNode`]: its type id and an async [`FlowNode::run`]
//! that reads its `data` through the [`NodeCtx`] and answers a
//! [`NodeOutcome`], the web's result dict. The walk decides what runs next
//! from that dict exactly as the web's `execute_node_chain` does:
//!
//! | The node answered | The walk |
//! |---|---|
//! | nothing, or `status` other than `error`, no `condition` | runs every child |
//! | `"condition": <bool>` | runs the matching `true`/`yes` or `false`/`no` children, every wire into a gate, and every unhandled wire |
//! | `"status": "error"`, no `condition` | records the failure, stops this branch |
//! | `"status": "error"` with `condition` | records the failure, runs neither branch, still runs the unhandled wires |
//! | [`NodeOutcome::abort`] | ends the whole run as failed, branches not yet visited included (the web: a handler that raises) |
//!
//! Run state lives on the context, never in node data: the client, the
//! variables, the log, the cancel token, the clock and the workflow id.

use super::client::FlowClient;
use super::env::NodeEnv;
use super::pyfmt;
use super::resolve::OrderFields;
use super::subscriptions::Subscriptions;
use super::vars::{iso_ist, VarStore};
use crate::clock::Clock;
use crate::state::AppState;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// One node type's behaviour.
#[async_trait]
pub trait FlowNode: Send + Sync {
    /// The type id in the stored JSON (`placeOrder`), one of
    /// [`super::model::NODE_TYPES`].
    fn node_type(&self) -> &'static str;

    /// Run the node with its `data`. Never panics on bad data: a value it
    /// cannot use is an error outcome with the web's sentence.
    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome;
}

/// A field the web's accessor could not read: its `int()` or `float()`
/// raised, nothing in the node caught it, and the web's run ended there
/// (`Error: {text}` in the log, the run failed). The text is Python's.
/// A node stops at the field, before any call, with `?` inside a block
/// that answers `Result<NodeOutcome, NodeAbort>`, converted with `.into()`:
///
/// ```ignore
/// async {
///     let quantity = ctx.try_get_int(data, "quantity", 1)?;
///     Ok(place(ctx, quantity).await)
/// }
/// .await
/// .into()
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAbort(String);

impl NodeAbort {
    /// Python's exception text.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl From<NodeAbort> for NodeOutcome {
    fn from(a: NodeAbort) -> Self {
        NodeOutcome::abort(a.0)
    }
}

impl From<Result<NodeOutcome, NodeAbort>> for NodeOutcome {
    fn from(r: Result<NodeOutcome, NodeAbort>) -> Self {
        r.unwrap_or_else(NodeOutcome::from)
    }
}

/// What a node answered: the web's result dict, or nothing (`None` for
/// the nodes that only log, as `start` and `group`), or an abort.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeOutcome {
    result: Option<Value>,
    abort: Option<String>,
}

impl NodeOutcome {
    /// No result (the web's `None`): every child runs.
    pub fn none() -> Self {
        Self::default()
    }

    /// The web's dict as the node built it (a client reply, or its own).
    pub fn result(v: Value) -> Self {
        Self {
            result: Some(v),
            abort: None,
        }
    }

    /// `{"status": "error", "message": message}`: the branch stops.
    pub fn error(message: impl Into<String>) -> Self {
        Self::result(json!({"status": "error", "message": message.into()}))
    }

    /// `{"status": "success", "condition": met}`: one branch fires.
    pub fn condition(met: bool) -> Self {
        Self::result(json!({"status": "success", "condition": met}))
    }

    /// A condition that could not be evaluated:
    /// `{"status": "error", "condition": false, "message": message}`.
    pub fn undecided(message: impl Into<String>) -> Self {
        Self::result(json!({"status": "error", "condition": false, "message": message.into()}))
    }

    /// End the whole run as failed with `message` (the web's handler that
    /// raises: caught at the run, `Error: {message}` in the log). Only for
    /// what the web lets propagate; a failure the web returns as a dict is
    /// [`NodeOutcome::error`].
    pub fn abort(message: impl Into<String>) -> Self {
        Self {
            result: None,
            abort: Some(message.into()),
        }
    }

    /// The abort message, when the node ended the run.
    pub fn aborted(&self) -> Option<&str> {
        self.abort.as_deref()
    }

    /// The dict, if any.
    pub fn value(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    /// The node reported failure (`status == "error"`).
    pub fn is_error(&self) -> bool {
        self.result
            .as_ref()
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str)
            == Some("error")
    }

    /// The failure as the run logs it: `message`, else `error`, else
    /// "node failed", as Python's `str()` of the value.
    pub fn failure_message(&self) -> String {
        pyfmt::str(&self.failure_value())
    }

    /// The failure as the run's errors list keeps it: the raw `message`
    /// (else `error`) value, which may be a dict of field errors, else
    /// "node failed" (the web's `result.get("message") or
    /// result.get("error") or "node failed"`).
    pub fn failure_value(&self) -> Value {
        self.result
            .as_ref()
            .and_then(|v| {
                ["message", "error"]
                    .iter()
                    .filter_map(|k| v.get(*k))
                    .find(|x| pyfmt::truthy(x))
                    .cloned()
            })
            .unwrap_or_else(|| Value::String("node failed".into()))
    }

    /// `Some(met)` when the dict carries a `condition` (read as Python's
    /// truth of the value), `None` otherwise.
    pub fn condition_met(&self) -> Option<bool> {
        let v = self.result.as_ref()?;
        if !pyfmt::truthy(v) {
            return None;
        }
        v.get("condition").map(pyfmt::truthy)
    }
}

/// What started a run, as the code that started it says (never a payload
/// field, which a webhook caller controls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunTrigger {
    /// Run Now, the Agent, a bare test context.
    #[default]
    Manual,
    /// A `/flow/webhook/...` call.
    Webhook,
    /// The scheduler.
    Schedule,
    /// The price monitor, which has already evaluated the alert on the
    /// tick that fired it.
    PriceAlert,
    /// The order-update monitor.
    OrderUpdate,
}

/// The level names a log entry is written with (the Log node may write any
/// text the author chose; only these three come from the executor).
pub const INFO: &str = "info";
pub const WARNING: &str = "warning";
pub const ERROR: &str = "error";

/// Everything a run gives its nodes.
pub struct NodeCtx {
    /// OpenAlgo, as the web's `FlowOpenAlgoClient`.
    pub client: Arc<dyn FlowClient>,
    /// The run's variables and condition results.
    pub vars: VarStore,
    /// Fires when the run must stop (sign-out, quit): a node that waits
    /// returns at once.
    pub cancel: CancellationToken,
    /// The app's clock (a test's manual clock).
    pub clock: Arc<dyn Clock>,
    /// The workflow this run belongs to; `None` for a bare test context.
    pub workflow_id: Option<i64>,
    /// The feed subscriptions each workflow holds, given back when it is
    /// deactivated or deleted.
    pub subscriptions: Arc<Subscriptions>,
    /// The symbol master, strategy book, feed, HTTP and calendar.
    pub env: NodeEnv,
    /// What started the run ([`RunTrigger::Manual`] unless the starter set
    /// it with [`NodeCtx::with_trigger`]).
    pub trigger: RunTrigger,
    default_strategy: String,
    logs: Mutex<Vec<Value>>,
    app: Weak<AppState>,
}

/// The parts a run builds its context from.
pub struct NodeCtxParts {
    pub client: Arc<dyn FlowClient>,
    pub clock: Arc<dyn Clock>,
    pub cancel: CancellationToken,
    pub workflow_id: Option<i64>,
    /// The tag every order of the run carries unless a node sets
    /// `strategyTag`: the workflow's name.
    pub default_strategy: String,
    pub subscriptions: Arc<Subscriptions>,
    /// The trading day's boundary, IST hour and minute (the app's session
    /// expiry setting), which `{{session_date}}` turns at.
    pub session_boundary: (u32, u32),
    pub env: NodeEnv,
    pub app: Weak<AppState>,
}

impl NodeCtx {
    pub fn new(p: NodeCtxParts) -> Self {
        let strategy = if p.default_strategy.is_empty() {
            super::client::DEFAULT_STRATEGY.to_string()
        } else {
            p.default_strategy
        };
        Self {
            vars: VarStore::new(p.clock.clone(), p.session_boundary),
            client: p.client,
            cancel: p.cancel,
            clock: p.clock,
            workflow_id: p.workflow_id,
            subscriptions: p.subscriptions,
            env: p.env,
            trigger: RunTrigger::Manual,
            default_strategy: strategy,
            logs: Mutex::new(Vec::new()),
            app: p.app,
        }
    }

    /// A context over `client` with the system clock and no app, as the
    /// web's tests build `NodeExecutor(client, WorkflowContext(), [])`.
    pub fn bare(client: Arc<dyn FlowClient>) -> Self {
        Self::new(NodeCtxParts {
            client,
            clock: Arc::new(crate::clock::SystemClock),
            cancel: CancellationToken::new(),
            workflow_id: None,
            default_strategy: String::new(),
            subscriptions: Arc::new(Subscriptions::default()),
            session_boundary: super::vars::DEFAULT_BOUNDARY,
            env: NodeEnv::default(),
            app: Weak::new(),
        })
    }

    /// This context with `env` (a test's or a golden replay's fakes).
    pub fn with_env(mut self, env: NodeEnv) -> Self {
        self.env = env;
        self
    }

    /// This context as started by `trigger` (a monitor's run, or a golden
    /// replay of one).
    pub fn with_trigger(mut self, trigger: RunTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    /// The app, for a node that needs a service the client does not cover
    /// (the symbol master, the shared HTTP client); `None` in a bare test.
    pub fn app(&self) -> Option<Arc<AppState>> {
        self.app.upgrade()
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// Add an entry to the run's log (`{"time", "message", "level"}`, the
    /// time as the web's `isoformat()`, in IST).
    pub fn log(&self, message: impl Into<String>, level: &str) {
        let message = message.into();
        if level == ERROR {
            tracing::warn!("Flow: {}", message);
        } else {
            tracing::debug!("Flow: {}", message);
        }
        self.logs.lock().push(json!({
            "time": iso_ist(self.now()),
            "message": message,
            "level": level,
        }));
    }

    pub fn info(&self, message: impl Into<String>) {
        self.log(message, INFO);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.log(message, ERROR);
    }

    /// The log so far.
    pub fn logs(&self) -> Vec<Value> {
        self.logs.lock().clone()
    }

    /// Store `result` under the node's `outputVariable`, when one is set
    /// (`store_output`).
    pub fn store_output(&self, data: &Map<String, Value>, result: Value) {
        let Some(Value::String(name)) = data.get("outputVariable") else {
            return;
        };
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        self.vars.set(trimmed, result);
        self.info(format!("Stored result in variable: {}", name));
    }

    /// The order tag: the node's `strategyTag`, else the workflow's name.
    pub fn strategy_tag(&self, data: &Map<String, Value>) -> String {
        let tag = self.get_str(data, "strategyTag", "");
        if tag.is_empty() {
            self.default_strategy.clone()
        } else {
            tag
        }
    }

    /// An interpolated text field; `default` when absent or falsy
    /// (`get_str`).
    pub fn get_str(&self, data: &Map<String, Value>, key: &str, default: &str) -> String {
        match data.get(key) {
            Some(v) if pyfmt::truthy(v) => self.vars.interpolate(&pyfmt::str(v)),
            _ => default.to_string(),
        }
    }

    /// An interpolated whole number; `default` when absent, blank or not a
    /// number (`get_int`: `int(float(text))`).
    pub fn get_int(&self, data: &Map<String, Value>, key: &str, default: i64) -> i64 {
        self.get_float_opt(data, key)
            .filter(|f| f.is_finite())
            .map(|f| f.trunc() as i64)
            .unwrap_or(default)
    }

    /// An interpolated number; `default` when absent, blank or not a
    /// number (`get_float`).
    pub fn get_float(&self, data: &Map<String, Value>, key: &str, default: f64) -> f64 {
        self.get_float_opt(data, key).unwrap_or(default)
    }

    /// The web's `get_int` where it raises: as [`NodeCtx::get_int`], but a
    /// list or dict (`int()`'s TypeError) or text that reads as infinity
    /// (`int(float("inf"))`'s OverflowError) ends the run. Text that is not
    /// a number, or NaN, is the web's caught ValueError: `default`.
    pub fn try_get_int(
        &self,
        data: &Map<String, Value>,
        key: &str,
        default: i64,
    ) -> Result<i64, NodeAbort> {
        match data.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(Value::String(s)) if s.is_empty() => Ok(default),
            Some(Value::String(s)) => match pyfmt::parse_float(&self.vars.interpolate(s)) {
                Some(f) if f.is_infinite() => {
                    Err(NodeAbort("cannot convert float infinity to integer".into()))
                }
                Some(f) if f.is_finite() => Ok(f.trunc() as i64),
                _ => Ok(default),
            },
            Some(v @ (Value::Array(_) | Value::Object(_))) => Err(NodeAbort(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                pyfmt::type_name(v)
            ))),
            Some(other) => Ok(pyfmt::to_int(other).unwrap_or(default)),
        }
    }

    /// The web's `get_float` where it raises: as [`NodeCtx::get_float`],
    /// but a list or dict (`float()`'s TypeError) ends the run. Text
    /// reads as Python's `float()` (`inf` and `nan` included); text that
    /// is not a number is `default`. The web's `str()` cannot raise, so
    /// [`NodeCtx::get_str`] has no fallible form.
    pub fn try_get_float(
        &self,
        data: &Map<String, Value>,
        key: &str,
        default: f64,
    ) -> Result<f64, NodeAbort> {
        match data.get(key) {
            Some(v @ (Value::Array(_) | Value::Object(_))) => Err(NodeAbort(format!(
                "float() argument must be a string or a real number, not '{}'",
                pyfmt::type_name(v)
            ))),
            _ => Ok(self.get_float(data, key, default)),
        }
    }

    fn get_float_opt(&self, data: &Map<String, Value>, key: &str) -> Option<f64> {
        match data.get(key)? {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => pyfmt::parse_float(&self.vars.interpolate(s)),
            other => pyfmt::to_float(other),
        }
    }

    /// The node's order fields, read without defaults for supplied values
    /// (`RuntimeOrderResolver`).
    pub fn order_fields<'a>(&'a self, data: &'a Map<String, Value>) -> OrderFields<'a> {
        OrderFields::new(&self.vars, data)
    }

    /// Wait `d`, or less if the run is stopped; `false` when it was.
    pub async fn sleep(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.cancel.cancelled() => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::contract::client::fake::FakeFlowClient;

    fn ctx() -> NodeCtx {
        NodeCtx::bare(Arc::new(FakeFlowClient::new()))
    }

    fn data(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn outcomes_read_as_the_walk_reads_the_web_dict() {
        assert_eq!(NodeOutcome::none().condition_met(), None);
        assert!(!NodeOutcome::none().is_error());
        assert_eq!(NodeOutcome::condition(true).condition_met(), Some(true));
        let failed = NodeOutcome::result(
            json!({"status": "error", "error": "Broker session expired", "code": 401}),
        );
        assert!(failed.is_error());
        assert_eq!(failed.failure_message(), "Broker session expired");
        assert_eq!(
            NodeOutcome::result(json!({"status": "error"})).failure_message(),
            "node failed"
        );
        let undecided = NodeOutcome::undecided("operand did not resolve");
        assert!(undecided.is_error());
        assert_eq!(undecided.condition_met(), Some(false));
        // A truthy non-boolean condition counts as met, as in Python.
        assert_eq!(
            NodeOutcome::result(json!({"condition": 1})).condition_met(),
            Some(true)
        );
    }

    /// The run's errors keep the failure as the node gave it: text, or a
    /// dict of field errors (golden data/multiQuotes__empty), first
    /// `message` then `error`, else "node failed"; the log line reads it as
    /// Python's `str()`.
    #[test]
    fn a_failure_keeps_its_raw_message() {
        let fields = NodeOutcome::result(json!({
            "status": "error",
            "error": {"symbols": ["Shorter than minimum length 1."]},
            "code": 400
        }));
        assert_eq!(
            fields.failure_value(),
            json!({"symbols": ["Shorter than minimum length 1."]})
        );
        assert_eq!(
            fields.failure_message(),
            "{'symbols': ['Shorter than minimum length 1.']}"
        );
        let text = NodeOutcome::result(json!({"status": "error", "message": "", "error": "x"}));
        assert_eq!(text.failure_value(), json!("x"));
        assert_eq!(
            NodeOutcome::result(json!({"status": "error"})).failure_value(),
            json!("node failed")
        );
    }

    #[test]
    fn accessors_forgive_as_the_web_accessors_do() {
        let c = ctx();
        c.vars.set("q", json!("25"));
        let d = data(
            json!({"qty": "{{q}}", "bad": "{{nope}}", "zero": 0, "f": 2.75, "blank": "", "flag": true}),
        );
        assert_eq!(c.get_int(&d, "qty", 1), 25);
        assert_eq!(c.get_int(&d, "bad", 1), 1);
        assert_eq!(c.get_int(&d, "f", 1), 2);
        assert_eq!(c.get_int(&d, "blank", 7), 7);
        // A zero is a value, not a missing field (parity X-181).
        assert_eq!(c.get_int(&d, "zero", 1), 0);
        assert_eq!(c.get_float(&d, "zero", 1.0), 0.0);
        assert_eq!(c.get_int(&d, "absent", 1), 1);
        assert_eq!(c.get_int(&d, "flag", 0), 1);
        assert_eq!(c.get_float(&d, "f", 0.0), 2.75);
        assert_eq!(c.get_str(&d, "zero", "NSE"), "NSE");
        assert_eq!(c.get_str(&d, "f", ""), "2.75");
        assert_eq!(c.get_str(&d, "qty", ""), "25");
    }

    /// N-common-11/12: where the web's `int()`/`float()` raises, the run
    /// ends with Python's text, and the node stops before its call.
    #[tokio::test]
    async fn a_list_or_dict_where_a_number_belongs_ends_the_run() {
        let c = ctx();
        c.vars.set("big", json!("1e400"));
        let d = data(
            json!({"legs": [1], "opts": {}, "inf": "inf", "big": "{{big}}",
                            "nan": "nan", "qty": "{{nope}}", "n": 3.9, "flag": true,
                            "blank": "", "none": null, "neg": "-inf"}),
        );
        let int_err = |k: &str| c.try_get_int(&d, k, 1).unwrap_err().message().to_string();
        assert_eq!(
            int_err("legs"),
            "int() argument must be a string, a bytes-like object or a real number, not 'list'"
        );
        assert_eq!(
            int_err("opts"),
            "int() argument must be a string, a bytes-like object or a real number, not 'dict'"
        );
        for k in ["inf", "big", "neg"] {
            assert_eq!(
                int_err(k),
                "cannot convert float infinity to integer",
                "{}",
                k
            );
        }
        // The web's caught ValueError and the forgiving cases.
        for (k, want) in [
            ("nan", 1),
            ("qty", 1),
            ("n", 3),
            ("flag", 1),
            ("blank", 1),
            ("none", 1),
            ("absent", 1),
        ] {
            assert_eq!(c.try_get_int(&d, k, 1), Ok(want), "{}", k);
        }
        assert_eq!(
            c.try_get_float(&d, "legs", 0.0).unwrap_err().message(),
            "float() argument must be a string or a real number, not 'list'"
        );
        assert_eq!(
            c.try_get_float(&d, "opts", 0.0).unwrap_err().message(),
            "float() argument must be a string or a real number, not 'dict'"
        );
        assert_eq!(c.try_get_float(&d, "inf", 0.0), Ok(f64::INFINITY));
        assert_eq!(c.try_get_float(&d, "n", 0.0), Ok(3.9));
        assert_eq!(c.try_get_float(&d, "qty", 2.5), Ok(2.5));
        // A node using `?` stops at the field: no call is made.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let outcome: NodeOutcome = async {
            let quantity = c.try_get_int(&d, "legs", 1)?;
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(NodeOutcome::result(json!({"quantity": quantity})))
        }
        .await
        .into();
        assert_eq!(
            outcome.aborted(),
            Some(
                "int() argument must be a string, a bytes-like object or a real number, not 'list'"
            )
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let fine: NodeOutcome = Ok::<_, NodeAbort>(NodeOutcome::none()).into();
        assert_eq!(fine, NodeOutcome::none());
    }

    #[test]
    fn output_is_stored_under_the_trimmed_name_and_logged() {
        let c = ctx();
        c.store_output(&data(json!({"outputVariable": " ltp "})), json!(101.5));
        assert_eq!(c.vars.get("ltp"), Some(json!(101.5)));
        let logs = c.logs();
        assert_eq!(logs[0]["message"], "Stored result in variable:  ltp ");
        assert_eq!(logs[0]["level"], "info");
        c.store_output(&data(json!({"outputVariable": "  "})), json!(1));
        assert_eq!(c.logs().len(), 1);
    }

    #[test]
    fn orders_carry_the_workflow_name_unless_tagged() {
        let mut parts_ctx = ctx();
        parts_ctx.default_strategy = "Gap fade".into();
        assert_eq!(parts_ctx.strategy_tag(&data(json!({}))), "Gap fade");
        assert_eq!(
            parts_ctx.strategy_tag(&data(json!({"strategyTag": "x"}))),
            "x"
        );
        assert_eq!(ctx().strategy_tag(&data(json!({}))), "flow_workflow");
    }

    #[tokio::test]
    async fn a_stopped_run_does_not_wait() {
        let c = ctx();
        c.cancel.cancel();
        assert!(!c.sleep(Duration::from_secs(3600)).await);
    }

    #[test]
    fn a_context_carries_its_workflow_id() {
        assert_eq!(ctx().workflow_id, None);
    }

    #[test]
    fn a_test_context_takes_its_services() {
        struct Calendar;
        impl crate::flow::contract::env::TradingCalendar for Calendar {
            fn holidays(&self, _: i32) -> Result<Vec<Value>, String> {
                Ok(vec![json!({"date": "2026-11-09"})])
            }
        }
        let env = NodeEnv {
            calendar: Arc::new(Calendar),
            ..Default::default()
        };
        let c = ctx().with_env(env);
        assert_eq!(c.env.calendar.holidays(2026).unwrap().len(), 1);
    }
}
