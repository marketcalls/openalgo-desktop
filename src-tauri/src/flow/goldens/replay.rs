//! Loading a golden, running it on the desktop, and collecting what happened.

use super::compare::{diff, diff_logs};
use super::env::GoldenEnv;
use crate::clock::ManualClock;
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::model::{is_node_type, Graph, GATE_TYPES};
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeCtxParts, NodeOutcome};
use crate::flow::contract::pyfmt;
use crate::flow::contract::subscriptions::Subscriptions;
use crate::flow::executor::walk::Walk;
use crate::flow::nodes::{NodeFamily, NodeRegistry, FAMILIES};
use async_trait::async_trait;
use chrono::{NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// Where the recorded goldens live.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/web/flow/goldens")
}

/// The variable a probe child sets when the walk reaches it.
const PROBE: &str = "__golden_probe__";
/// The variable the recording wrapper keeps the node's own result in.
const RESULT: &str = "__golden_result__";
const PROBE_TYPE: &str = "goldenProbe";

/// What the harness can give a run besides the scripted client. Whoever
/// adds another (a trusted monitor-fired run, the app's store, the
/// restacked validator) adds its name here; until then those goldens are
/// reported, not run.
const HOOKS: &[&str] = &[
    "master_contract",
    "strategy_book",
    "http_stub",
    "feed",
    "trading_calendar",
    "validator",
    // The triggers lane: a run the price monitor started, the feed's
    // session check, a feed that fails outright.
    "run_context",
    "feed_session",
    "feed_exception",
];

pub struct Golden {
    pub family: String,
    pub name: String,
    pub v: Value,
}

pub enum Verdict {
    Matched,
    Failed(Vec<String>),
    NotBuilt(String),
    NeedsHook(&'static str),
    /// The web's behaviour on something the desktop client never does
    /// (a service that raises, parity CL-26): recorded, not replayed.
    EvidenceOnly,
}

pub fn load_all() -> Vec<Golden> {
    let mut out = Vec::new();
    let Ok(families) = std::fs::read_dir(root()) else {
        return out;
    };
    let mut dirs: Vec<PathBuf> = families
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let family = dir
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|r| r.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().is_some_and(|x| x == "json"));
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let name = format!(
                "{family}/{}",
                f.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            );
            out.push(Golden {
                family: family.clone(),
                name,
                v,
            });
        }
    }
    out
}

/// Records the node's own result where the harness can read it.
struct Recording(Arc<dyn FlowNode>);

#[async_trait]
impl FlowNode for Recording {
    fn node_type(&self) -> &'static str {
        self.0.node_type()
    }
    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let out = self.0.run(ctx, data).await;
        ctx.vars
            .set(RESULT, out.value().cloned().unwrap_or(Value::Null));
        out
    }
}

/// A probe child: notes that the walk reached it.
struct Probe;

#[async_trait]
impl FlowNode for Probe {
    fn node_type(&self) -> &'static str {
        PROBE_TYPE
    }
    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let label = data.get("probe").map(pyfmt::str).unwrap_or_default();
        ctx.vars.set(&format!("{PROBE}{label}"), json!(true));
        NodeOutcome::none()
    }
}

fn recorded_nodes() -> Vec<Arc<dyn FlowNode>> {
    let mut out: Vec<Arc<dyn FlowNode>> = vec![Arc::new(Probe)];
    for family in FAMILIES {
        for n in (family.build)() {
            out.push(Arc::new(Recording(n)));
        }
    }
    out
}

const GOLDEN_FAMILY: NodeFamily = NodeFamily {
    key: "goldens",
    build: recorded_nodes,
};

/// The registry as the app builds it: what the node lanes have landed.
static LANDED: std::sync::LazyLock<NodeRegistry> = std::sync::LazyLock::new(NodeRegistry::default);

fn registered(t: &str) -> bool {
    !is_node_type(t) || GATE_TYPES.contains(&t) || LANDED.get(t).is_some()
}

/// The node types a golden runs.
fn types(g: &Value) -> Vec<String> {
    let nodes = g
        .pointer("/graph/nodes")
        .or_else(|| g.pointer("/workflow/nodes"));
    let mut t: Vec<String> = nodes
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| n["type"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    t.retain(|x| x != PROBE_TYPE);
    t.sort();
    t.dedup();
    t
}

/// The hook a golden needs that the harness does not have yet.
fn missing_hook(g: &Value, types: &[String]) -> Option<&'static str> {
    let env = &g["env"];
    let has = |t: &str| types.iter().any(|x| x == t);
    let feed = &env["feed"];
    // The web's unsubscribe checks the session before releasing, and a feed
    // that raises is logged apart from a refusal; `MarketFeed` has neither.
    let feed_session = has("unsubscribe")
        && (feed.get("username").is_some_and(Value::is_null)
            || feed.get("connect_error").is_some());
    let feed_exception =
        feed.get("verify_raise").is_some() || feed.get("unsubscribe_raise").is_some();
    let needs: [(&'static str, bool); 9] = [
        (
            "master_contract",
            env.get("master_contract").is_some()
                && (has("optionsOrder") || has("optionsMultiOrder")),
        ),
        ("strategy_book", has("strategyPnl")),
        ("http_stub", has("httpRequest")),
        (
            "feed",
            has("subscribeLtp")
                || has("subscribeQuote")
                || has("subscribeDepth")
                || has("unsubscribe"),
        ),
        ("trading_calendar", has("calendar")),
        (
            "run_context",
            g.get("run_context").is_some_and(|r| !r.is_null()),
        ),
        ("app", g.get("special").is_some_and(|s| !s.is_null())),
        ("feed_session", feed_session),
        ("feed_exception", feed_exception),
    ];
    let validator = g["expected"]["result"]["message"]
        .as_str()
        .is_some_and(|m| m.starts_with("Workflow cannot be executed: "));
    needs
        .into_iter()
        .chain(std::iter::once(("validator", validator)))
        .find(|(h, need)| *need && !HOOKS.contains(h))
        .map(|(h, _)| h)
}

pub async fn replay(g: &Golden) -> Verdict {
    let v = &g.v;
    let ts = types(v);
    if let Some(t) = ts.iter().find(|t| !registered(t)) {
        return Verdict::NotBuilt(t.clone());
    }
    let script = script_of(v);
    if script.iter().any(|(_, r)| r.get("$raise").is_some()) {
        return Verdict::EvidenceOnly;
    }
    if let Some(h) = missing_hook(v, &ts) {
        return Verdict::NeedsHook(h);
    }
    let want = expected(v);
    let client = Arc::new(FakeFlowClient::new());
    for (endpoint, reply) in &script {
        client.reply(endpoint, reply.clone());
    }
    if let Some(r) = v.get("alert_reply").filter(|r| !r.is_null()) {
        client.alert_reply(r.clone());
    }
    let (genv, node_env) = GoldenEnv::new(&v["env"]);
    let ctx = context(v, client.clone()).with_env(node_env);
    let registry = NodeRegistry::new(&[GOLDEN_FAMILY]);
    let mut diffs = Vec::new();
    if v["kind"] == "workflow" {
        run_workflow(v, &ctx, &registry, &want, &mut diffs).await;
    } else {
        run_node(v, &ctx, &registry, &want, &mut diffs).await;
    }
    let calls: Vec<Value> = client
        .calls()
        .into_iter()
        .map(|c| json!({"method": c.method, "endpoint": c.endpoint, "payload": Value::Object(c.payload)}))
        .collect();
    let (want_calls, got_calls) = normalize_calls(&want["calls"], &calls);
    diff(&want_calls, &got_calls, "calls", &mut diffs);
    let alerts: Vec<Value> = client
        .alerts()
        .into_iter()
        .map(|a| json!({"channel": a.channel, "message": a.message, "to": a.to}))
        .collect();
    diff(
        &listed(&want, "alerts"),
        &json!(alerts),
        "alerts",
        &mut diffs,
    );
    diff_logs(&want["logs"], &logs(&ctx), &mut diffs);
    compare_env(&want, &ctx, &genv, &mut diffs);
    if diffs.is_empty() {
        Verdict::Matched
    } else {
        Verdict::Failed(diffs)
    }
}

/// The web's expectation with the desktop's decided fixes laid over it.
fn expected(v: &Value) -> Value {
    let mut want = v["expected"].clone();
    if let (Some(w), Some(d)) = (
        want.as_object_mut(),
        v.get("desktop_expected").and_then(Value::as_object),
    ) {
        for (k, val) in d {
            if !matches!(
                k.as_str(),
                "parity" | "why" | "fix" | "script" | "script_extra"
            ) {
                w.insert(k.clone(), val.clone());
            }
        }
    }
    want
}

/// The replies, in call order, the desktop's run is scripted with.
fn script_of(v: &Value) -> Vec<(String, Value)> {
    let d = v.get("desktop_expected");
    let base = d.and_then(|d| d.get("script")).unwrap_or(&v["script"]);
    let extra = d.and_then(|d| d.get("script_extra"));
    base.as_array()
        .into_iter()
        .chain(extra.and_then(Value::as_array))
        .flatten()
        .filter_map(|e| Some((e["endpoint"].as_str()?.to_string(), e["reply"].clone())))
        .collect()
}

fn context(v: &Value, client: Arc<FakeFlowClient>) -> NodeCtx {
    let clock = v["clock"]
        .as_str()
        .and_then(|c| NaiveDateTime::parse_from_str(c, "%Y-%m-%dT%H:%M:%S").ok())
        .and_then(|n| Kolkata.from_local_datetime(&n).single())
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_default();
    let name = v["workflow_name"]
        .as_str()
        .or_else(|| v.pointer("/workflow/name").and_then(Value::as_str))
        .unwrap_or("");
    let ctx = NodeCtx::new(NodeCtxParts {
        client,
        clock: ManualClock::new(clock),
        cancel: CancellationToken::new(),
        workflow_id: Some(1),
        default_strategy: name.to_string(),
        subscriptions: Arc::new(Subscriptions::default()),
        session_boundary: (3, 0),
        env: Default::default(),
        app: Weak::new(),
    });
    let ctx = match v.pointer("/run_context/trigger").and_then(Value::as_str) {
        Some("price_alert") => {
            ctx.with_trigger(crate::flow::contract::node::RunTrigger::PriceAlert)
        }
        _ => ctx,
    };
    for (k, val) in v["vars_in"].as_object().into_iter().flatten() {
        ctx.vars.set(k, val.clone());
    }
    for (k, val) in v["conditions_in"].as_object().into_iter().flatten() {
        if let Some(b) = val.as_bool() {
            ctx.vars.set_condition(k, b);
        }
    }
    ctx
}

async fn run_node(
    v: &Value,
    ctx: &NodeCtx,
    registry: &NodeRegistry,
    want: &Value,
    diffs: &mut Vec<String>,
) {
    let graph = match Graph::parse(&v["graph"]["nodes"], &v["graph"]["edges"]) {
        Ok(g) => g,
        Err(e) => return diffs.push(format!("graph: {e}")),
    };
    let start = v["start"].as_str().unwrap_or("n1");
    let mut walk = Walk::new(&graph, registry, ctx);
    let abort = walk.visit(start, 0).await.err();
    if let Some(a) = &abort {
        ctx.error(format!("Error: {a}"));
    }
    let error = if walk.errors.is_empty() && abort.is_none() {
        Value::Null
    } else {
        json!({"errors": walk.errors, "abort": abort})
    };
    diff(&want["error"], &error, "error", diffs);
    if want["outputs_by"] == "node" {
        let got = ctx.vars.get(RESULT).unwrap_or(Value::Null);
        diff(&want["outputs"], &got, "outputs", diffs);
    }
    let mut next = Vec::new();
    for n in &graph.nodes {
        if n.node_type == PROBE_TYPE {
            let label = n.data.get("probe").map(pyfmt::str).unwrap_or_default();
            if ctx.vars.get(&format!("{PROBE}{label}")).is_some() {
                next.push(json!(label));
            }
        }
    }
    let conditions: Map<String, Value> = ctx
        .vars
        .conditions()
        .into_iter()
        .map(|(k, b)| (k, Value::Bool(b)))
        .collect();
    diff(
        &want["conditions_out"],
        &Value::Object(conditions),
        "conditions_out",
        diffs,
    );
    next.sort_by_key(pyfmt::str);
    diff(&want["next"], &Value::Array(next), "next", diffs);
    diff(&want["vars_out"], &variables(ctx), "vars_out", diffs);
}

/// Every variable the run ended with, the harness's own markers aside.
fn variables(ctx: &NodeCtx) -> Value {
    let mut all = ctx.vars.snapshot();
    all.retain(|k, _| k != RESULT && !k.starts_with(PROBE));
    Value::Object(all)
}

/// A recorded list (`alerts`, `http_requests`): absent and `null` (a
/// desktop_expected saying the fixed run made none) both mean `[]`.
fn listed(want: &Value, key: &str) -> Value {
    want.get(key)
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!([]))
}

/// The feed and HTTP traffic and the subscriptions the run holds, where
/// the golden recorded any.
fn compare_env(want: &Value, ctx: &NodeCtx, genv: &GoldenEnv, diffs: &mut Vec<String>) {
    let web_feed = want["feed"].as_array().cloned().unwrap_or_default();
    let all_released = web_feed.iter().any(|o| o["op"] == "unsubscribe_all");
    let mut want_ops = Vec::new();
    for (i, op) in web_feed.iter().enumerate() {
        match op["op"].as_str() {
            Some("subscribe") => {
                let s = &op["symbols"][0];
                let mut o = json!({"op": "subscribe", "symbol": s["symbol"], "exchange": s["exchange"], "mode": op["mode"]});
                if let Some(w) = web_feed.get(i + 1).filter(|w| w["op"] == "wait") {
                    o["timeout"] = w["timeout"].clone();
                }
                want_ops.push(o);
            }
            Some("unsubscribe") => {
                let s = &op["symbols"][0];
                want_ops.push(json!({"op": "unsubscribe", "symbol": s["symbol"], "exchange": s["exchange"], "mode": op["mode"]}));
            }
            _ => {}
        }
    }
    let mut got_ops = genv.feed.ops.lock().clone();
    if all_released {
        // decisions.md 9: `all` releases this workflow's own subscriptions.
        got_ops.retain(|o| o["op"] != "unsubscribe");
    }
    for (w, g) in want_ops.iter_mut().zip(got_ops.iter_mut()) {
        if w.get("timeout").is_none() {
            if let Some(m) = g.as_object_mut() {
                m.remove("timeout");
            }
        }
    }
    diff(
        &Value::Array(want_ops),
        &Value::Array(got_ops),
        "feed",
        diffs,
    );
    let mut want_http = listed(want, "http_requests");
    for r in want_http.as_array_mut().into_iter().flatten() {
        r["vetted"] = json!(true);
        let body = r["body"].as_str().unwrap_or("").to_string();
        if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
            r["body"] = parsed;
        }
    }
    let mut got_http = Value::Array(genv.http.requests.lock().clone());
    for r in got_http.as_array_mut().into_iter().flatten() {
        let body = r["body"].as_str().unwrap_or("").to_string();
        if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
            r["body"] = parsed;
        }
    }
    diff(&want_http, &got_http, "http_requests", diffs);
    if let Some(w) = want.get("subscriptions").filter(|w| !w.is_null()) {
        let mut held: Vec<Value> = ctx
            .subscriptions
            .entries(1)
            .into_iter()
            .map(|(s, e, m)| json!([s, e, m]))
            .collect();
        held.sort_by_key(|x| x.to_string());
        diff(w, &Value::Array(held), "subscriptions", diffs);
    }
}

async fn run_workflow(
    v: &Value,
    ctx: &NodeCtx,
    registry: &NodeRegistry,
    want: &Value,
    diffs: &mut Vec<String>,
) {
    if let Some(w) = v.get("webhook").filter(|w| pyfmt::truthy(w)) {
        ctx.vars.set("webhook", w.clone());
    }
    let wf = &v["workflow"];
    let name = wf["name"].as_str().unwrap_or("");
    let walked: Result<Vec<Value>, String> = async {
        if v["api_key"] == false {
            return Err("API key required for workflow execution".to_string());
        }
        ctx.info(format!("Starting workflow: {name}"));
        let graph = Graph::parse(&wf["nodes"], &wf["edges"])?;
        let trigger = graph
            .trigger()
            .ok_or_else(|| "No trigger node found".to_string())?;
        let mut walk = Walk::new(&graph, registry, ctx);
        walk.visit(&trigger.id, 0).await?;
        Ok(walk.errors)
    }
    .await;
    let (result, record) = match walked {
        Ok(errors) if errors.is_empty() => (
            json!({"status": "success", "message": "Workflow executed successfully"}),
            json!({"status": "completed", "error": null}),
        ),
        Ok(errors) => {
            let summary = errors
                .iter()
                .map(|e| format!("{}: {}", pyfmt::str(&e["type"]), pyfmt::str(&e["message"])))
                .collect::<Vec<_>>()
                .join("; ");
            let message = format!("{} node(s) failed: {}", errors.len(), summary);
            (
                json!({"status": "error", "message": message, "errors": errors}),
                json!({"status": "failed", "error": summary}),
            )
        }
        Err(e) => {
            ctx.error(format!("Error: {e}"));
            (
                json!({"status": "error", "message": e}),
                json!({"status": "failed", "error": e}),
            )
        }
    };
    diff(&want["result"], &result, "result", diffs);
    diff(&want["record"], &record, "record", diffs);
    diff(&want["vars_out"], &variables(ctx), "vars_out", diffs);
}

fn logs(ctx: &NodeCtx) -> Vec<(String, String)> {
    ctx.logs()
        .into_iter()
        .map(|e| (pyfmt::str(&e["level"]), pyfmt::str(&e["message"])))
        .collect()
}

/// Calls as both sides send them: a `null` field is the same as an absent
/// one (the web passes `strike_int=None`, `start_date=None`), and the
/// desktop's endpoints that require a `strategy` the web's services do not
/// (cancel, cancel all, close, order status: parity CL-07, CL-08, CL-09,
/// CL-23) carry the client's default, which the web never sent.
fn normalize_calls(want: &Value, got: &[Value]) -> (Value, Value) {
    let clean = |c: &Value, web: Option<&Value>| -> Value {
        let mut c = c.clone();
        let endpoint = c["endpoint"].as_str().unwrap_or("").to_string();
        if let Some(p) = c.get_mut("payload").and_then(Value::as_object_mut) {
            p.retain(|_, x| !x.is_null());
            let web_has = web.and_then(|w| w["payload"].get("strategy")).is_some();
            let added = matches!(
                endpoint.as_str(),
                "cancelorder" | "cancelallorder" | "closeposition" | "orderstatus"
            );
            if added && !web_has {
                p.remove("strategy");
            }
        }
        c
    };
    if want.get("$any").is_some() {
        return (want.clone(), want.clone());
    }
    let w: Vec<Value> = want
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| clean(c, Some(c)))
        .collect();
    let g: Vec<Value> = got
        .iter()
        .enumerate()
        .map(|(i, c)| clean(c, w.get(i)))
        .collect();
    (Value::Array(w), Value::Array(g))
}
