//! The web's own executor, replayed: every recording under
//! `tests/fixtures/web/flow/goldens/data/` (the goldens lane, format in that
//! folder's README) runs through the data family and the walk, and must
//! make the same client calls with the same payloads and end with the same
//! result, variables, condition results, run log, branch and failures. A
//! recording that pins a web defect the desktop fixes carries the desktop's
//! values in `desktop_expected`, which win.
//!
//! Comparison rules (README "How values are compared"): numbers keep their
//! kind, floats within `1e-6 + 1e-9 * |want|`; a null payload field equals
//! an absent one, and the strategy the desktop adds to an order-status
//! call is ignored where the web sent none; a Python dict written in a log
//! line is compared with its keys in any order (decision 19); `$any` and
//! `$prefix` wildcards stand where the desktop's own words replace the web's.

use super::support::{ctx_at, env_with, FakeBook, FakeCalendar};
use crate::flow::contract::client::fake::FakeFlowClient;
use crate::flow::contract::model::Graph;
use crate::flow::contract::node::{FlowNode, NodeCtx, NodeOutcome};
use crate::flow::executor::walk::Walk;
use crate::flow::nodes::history::shared_cache;
use crate::flow::nodes::{data, NodeFamily, NodeRegistry};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

thread_local! {
    /// The probes the walk reached in the current replay.
    static REACHED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// What each data node answered in the current replay.
    static ANSWERED: RefCell<Vec<Option<Value>>> = const { RefCell::new(Vec::new()) };
}

/// The recordings' child nodes: they record that the walk reached them.
struct Probe;

#[async_trait]
impl FlowNode for Probe {
    fn node_type(&self) -> &'static str {
        "goldenProbe"
    }

    async fn run(&self, _: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let name = data.get("probe").and_then(Value::as_str).unwrap_or("?");
        REACHED.with(|r| r.borrow_mut().push(name.to_string()));
        NodeOutcome::none()
    }
}

/// A data node that records the dict it answered.
struct Recorded(Arc<dyn FlowNode>);

#[async_trait]
impl FlowNode for Recorded {
    fn node_type(&self) -> &'static str {
        self.0.node_type()
    }

    async fn run(&self, ctx: &NodeCtx, data: &Map<String, Value>) -> NodeOutcome {
        let out = self.0.run(ctx, data).await;
        ANSWERED.with(|a| a.borrow_mut().push(out.value().cloned()));
        out
    }
}

const REPLAY: NodeFamily = NodeFamily {
    key: "replay",
    build: || {
        let mut nodes: Vec<Arc<dyn FlowNode>> = (data::FAMILY.build)()
            .into_iter()
            .map(|n| Arc::new(Recorded(n)) as Arc<dyn FlowNode>)
            .collect();
        nodes.push(Arc::new(Probe));
        nodes
    },
};

fn folder() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/web/flow/goldens/data")
}

/// The recording's expectations with `desktop_expected` laid over them.
fn expectations(g: &Value) -> Map<String, Value> {
    let mut want = g["expected"].as_object().cloned().unwrap_or_default();
    if let Some(Value::Object(fix)) = g.get("desktop_expected") {
        for (k, v) in fix {
            if !["parity", "why", "fix", "script", "script_extra"].contains(&k.as_str()) {
                want.insert(k.clone(), v.clone());
            }
        }
    }
    want
}

fn script(g: &Value) -> Vec<Value> {
    let fix = g.get("desktop_expected");
    let mut list = fix
        .and_then(|f| f.get("script"))
        .or_else(|| g.get("script"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(Value::Array(extra)) = fix.and_then(|f| f.get("script_extra")) {
        list.extend(extra.iter().cloned());
    }
    list
}

fn env_of(g: &Value) -> crate::flow::contract::env::NodeEnv {
    let env = &g["env"];
    let book = match env.get("strategy_book") {
        Some(Value::Array(legs)) => Ok(legs.clone()),
        Some(Value::Object(o)) => Err(o
            .get("unavailable")
            .and_then(Value::as_str)
            .unwrap_or("unavailable")
            .to_string()),
        _ => Ok(Vec::new()),
    };
    let mut years = HashMap::new();
    if let Some(Value::Object(table)) = env.get("calendar_holidays") {
        for (year, entries) in table {
            let parsed = match entries {
                Value::Array(list) => Ok(list.clone()),
                other => Err(other.as_str().unwrap_or("unavailable").to_string()),
            };
            if let Ok(y) = year.parse::<i32>() {
                years.insert(y, parsed);
            }
        }
    }
    env_with(FakeBook(book), FakeCalendar(years))
}

/// Replay one recording; every difference found.
async fn replay(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let g: Value = match serde_json::from_str(&text) {
        Ok(g) => g,
        Err(e) => return vec![format!("unreadable: {}", e)],
    };
    REACHED.with(|r| r.borrow_mut().clear());
    ANSWERED.with(|a| a.borrow_mut().clear());
    shared_cache().clear();
    let client = Arc::new(FakeFlowClient::new());
    for step in script(&g) {
        client.reply(
            step["endpoint"].as_str().unwrap_or_default(),
            step["reply"].clone(),
        );
    }
    let ctx = ctx_at(
        client.clone(),
        g["clock"].as_str().unwrap_or("2026-10-29T10:15:00"),
        g["workflow_name"].as_str().unwrap_or("Golden Flow"),
        env_of(&g),
    );
    for (k, v) in g["vars_in"].as_object().cloned().unwrap_or_default() {
        ctx.vars.set(&k, v);
    }
    for (k, v) in g["conditions_in"].as_object().cloned().unwrap_or_default() {
        ctx.vars.set_condition(&k, v.as_bool().unwrap_or(false));
    }
    let graph = match Graph::parse(&g["graph"]["nodes"], &g["graph"]["edges"]) {
        Ok(graph) => graph,
        Err(e) => return vec![format!("graph: {}", e)],
    };
    let registry = NodeRegistry::new(&[REPLAY]);
    let start = g["start"].as_str().unwrap_or("n1");
    let mut walk = Walk::new(&graph, &registry, &ctx);
    let abort = walk.visit(start, 0).await.err();
    let want = expectations(&g);
    let mut diffs = Vec::new();
    check_calls(field(&want, "calls"), &client, &mut diffs);
    let answered = ANSWERED.with(|a| a.borrow().first().cloned().flatten());
    same(
        "outputs",
        field(&want, "outputs"),
        &answered.unwrap_or(Value::Null),
        &mut diffs,
    );
    same(
        "vars_out",
        field(&want, "vars_out"),
        &Value::Object(ctx.vars.snapshot()),
        &mut diffs,
    );
    let conditions: Map<String, Value> = ctx
        .vars
        .conditions()
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect();
    same(
        "conditions_out",
        field(&want, "conditions_out"),
        &Value::Object(conditions),
        &mut diffs,
    );
    check_logs(field(&want, "logs"), &ctx, &mut diffs);
    let mut reached = REACHED.with(|r| r.borrow().clone());
    reached.sort();
    same("next", field(&want, "next"), &json!(reached), &mut diffs);
    let error = if walk.errors.is_empty() && abort.is_none() {
        Value::Null
    } else {
        json!({"errors": walk.errors, "abort": abort})
    };
    same("error", &failures_as_text(field(&want, "error")), &error, &mut diffs);
    diffs
}

fn field<'a>(want: &'a Map<String, Value>, key: &str) -> &'a Value {
    want.get(key).unwrap_or(&Value::Null)
}

/// The walk records a failure's message as text (core's walk, parity
/// N-dataCommon-03): a service whose `error` is a dict of field errors
/// (`multiquotes` with no symbols) is recorded as that dict's Python text,
/// where the web kept the dict. Compared as the text, and reported to the
/// lead.
fn failures_as_text(want: &Value) -> Value {
    let mut want = want.clone();
    if let Some(errors) = want.get_mut("errors").and_then(Value::as_array_mut) {
        for e in errors {
            if let Some(m) = e.get_mut("message") {
                if !m.is_string() && wildcard_ok(m, &Value::Null).is_none() {
                    *m = json!(crate::flow::contract::pyfmt::str(m));
                }
            }
        }
    }
    want
}

fn check_calls(want: &Value, client: &FakeFlowClient, diffs: &mut Vec<String>) {
    let want = want.as_array().cloned().unwrap_or_default();
    let got = client.calls();
    if want.len() != got.len() {
        diffs.push(format!(
            "calls: want {} got {}: {:?}",
            want.len(),
            got.len(),
            got.iter()
                .map(|c| (c.endpoint, &c.payload))
                .collect::<Vec<_>>()
        ));
        return;
    }
    for (w, g) in want.iter().zip(got) {
        if w["method"] != g.method || w["endpoint"] != g.endpoint {
            diffs.push(format!(
                "call: want {} {} got {} {}",
                w["method"], w["endpoint"], g.method, g.endpoint
            ));
            continue;
        }
        let want_payload = without_nulls(&w["payload"]);
        let mut got_payload = without_nulls(&Value::Object(g.payload.clone()));
        let strategy_added = [
            "cancelorder",
            "cancelallorder",
            "closeposition",
            "orderstatus",
        ]
        .contains(&g.endpoint)
            && want_payload.get("strategy").is_none();
        if strategy_added {
            got_payload.remove("strategy");
        }
        same(
            &format!("payload of {}", g.endpoint),
            &Value::Object(want_payload),
            &Value::Object(got_payload),
            diffs,
        );
    }
}

fn without_nulls(v: &Value) -> Map<String, Value> {
    v.as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn check_logs(want: &Value, ctx: &NodeCtx, diffs: &mut Vec<String>) {
    let want = want.as_array().cloned().unwrap_or_default();
    let got = ctx.logs();
    if want.len() != got.len() {
        diffs.push(format!(
            "logs: want {:?} got {:?}",
            want,
            got.iter()
                .map(|l| (&l["level"], &l["message"]))
                .collect::<Vec<_>>()
        ));
        return;
    }
    for (w, g) in want.iter().zip(&got) {
        let level_ok = w[0] == g["level"];
        let text = g["message"].as_str().unwrap_or_default();
        let message_ok = match &w[1] {
            Value::String(s) => s == text || canon_line(s) == canon_line(text),
            wildcard => wildcard_ok(wildcard, &g["message"]).unwrap_or(false),
        };
        if !level_ok || !message_ok {
            diffs.push(format!(
                "log: want [{} {}] got [{} {}]",
                w[0], w[1], g["level"], g["message"]
            ));
        }
    }
}

/// `Some(ok)` when `want` is a wildcard, `None` otherwise.
fn wildcard_ok(want: &Value, got: &Value) -> Option<bool> {
    let w = want.as_object()?;
    if w.len() != 1 {
        return None;
    }
    if w.contains_key("$any") {
        return Some(true);
    }
    let prefix = w.get("$prefix")?.as_str()?;
    Some(got.as_str().is_some_and(|g| g.starts_with(prefix)))
}

/// Record a difference at `path` between `want` and `got`.
fn same(path: &str, want: &Value, got: &Value, diffs: &mut Vec<String>) {
    if let Some(ok) = wildcard_ok(want, got) {
        if !ok {
            diffs.push(format!("{}: want {} got {}", path, want, got));
        }
        return;
    }
    match (want, got) {
        (Value::Number(w), Value::Number(g)) => {
            let w_int = w.is_i64() || w.is_u64();
            let g_int = g.is_i64() || g.is_u64();
            let (wf, gf) = (
                w.as_f64().unwrap_or(f64::NAN),
                g.as_f64().unwrap_or(f64::NAN),
            );
            let close = (wf - gf).abs() <= 1e-6 + 1e-9 * wf.abs();
            if w_int != g_int || !close {
                diffs.push(format!("{}: want {} got {}", path, want, got));
            }
        }
        (Value::Array(w), Value::Array(g)) if w.len() == g.len() => {
            for (i, (a, b)) in w.iter().zip(g).enumerate() {
                same(&format!("{}[{}]", path, i), a, b, diffs);
            }
        }
        (Value::Object(w), Value::Object(g)) => {
            let mut keys: Vec<&String> = w.keys().chain(g.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                match (w.get(k), g.get(k)) {
                    (Some(a), Some(b)) => same(&format!("{}.{}", path, k), a, b, diffs),
                    (a, b) => diffs.push(format!("{}.{}: want {:?} got {:?}", path, k, a, b)),
                }
            }
        }
        _ if want == got => {}
        _ => diffs.push(format!("{}: want {} got {}", path, want, got)),
    }
}

/// A log line with every Python dict in it written with sorted keys, so two
/// lines that differ only in key order compare equal.
pub fn canon_line(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '{' | '[') {
            if let Some((lit, end)) = literal(&chars, i) {
                out.push_str(&lit);
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// One Python literal at `i`, written canonically, and where it ends.
fn literal(c: &[char], i: usize) -> Option<(String, usize)> {
    let ch = *c.get(i)?;
    match ch {
        '{' => {
            let mut items = Vec::new();
            let mut j = i + 1;
            if c.get(j) == Some(&'}') {
                return Some(("{}".into(), j + 1));
            }
            loop {
                let (k, after_k) = literal(c, j)?;
                if c.get(after_k) != Some(&':') || c.get(after_k + 1) != Some(&' ') {
                    return None;
                }
                let (v, after_v) = literal(c, after_k + 2)?;
                items.push(format!("{}: {}", k, v));
                match c.get(after_v) {
                    Some('}') => {
                        items.sort();
                        return Some((format!("{{{}}}", items.join(", ")), after_v + 1));
                    }
                    Some(',') if c.get(after_v + 1) == Some(&' ') => j = after_v + 2,
                    _ => return None,
                }
            }
        }
        '[' => {
            let mut items = Vec::new();
            let mut j = i + 1;
            if c.get(j) == Some(&']') {
                return Some(("[]".into(), j + 1));
            }
            loop {
                let (v, after) = literal(c, j)?;
                items.push(v);
                match c.get(after) {
                    Some(']') => return Some((format!("[{}]", items.join(", ")), after + 1)),
                    Some(',') if c.get(after + 1) == Some(&' ') => j = after + 2,
                    _ => return None,
                }
            }
        }
        '\'' | '"' => {
            let mut j = i + 1;
            while let Some(&x) = c.get(j) {
                if x == '\\' {
                    j += 2;
                    continue;
                }
                if x == ch {
                    return Some((c[i..=j].iter().collect(), j + 1));
                }
                j += 1;
            }
            None
        }
        _ => {
            let mut j = i;
            while c
                .get(j)
                .is_some_and(|x| x.is_ascii_alphanumeric() || matches!(x, '.' | '-' | '+' | '_'))
            {
                j += 1;
            }
            (j > i).then(|| (c[i..j].iter().collect(), j))
        }
    }
}

/// Every data golden replays as the web's executor ran it.
#[tokio::test]
async fn the_web_executors_data_goldens_replay() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder())
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|e| e == "json"));
    files.sort();
    assert!(files.len() >= 184, "data goldens missing: {}", files.len());
    let mut failures = Vec::new();
    for path in &files {
        let diffs = replay(path).await;
        if !diffs.is_empty() {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            failures.push(format!("{}:\n    {}", name, diffs.join("\n    ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} data goldens differ:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

#[test]
fn log_lines_compare_dicts_in_any_key_order() {
    assert_eq!(
        canon_line("Quote result: {'status': 'success', 'data': {'ltp': 1.5, 'ask': 2}}"),
        canon_line("Quote result: {'data': {'ask': 2, 'ltp': 1.5}, 'status': 'success'}")
    );
    assert_ne!(canon_line("x: {'a': 1}"), canon_line("x: {'a': 1.0}"));
    assert_eq!(
        canon_line("check the {{variable}} references"),
        "check the {{variable}} references"
    );
}
