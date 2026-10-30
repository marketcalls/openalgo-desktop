//! Wave 6: the walk's rules, the run's variables, the order resolver, the
//! wait estimate and the app-log mirror, each pinned where the sweep found
//! it unpinned (E-02, E-11, E-15, E-20, E-22, E-44, N-common-08/13/14,
//! N-gate-03, N-logic-03). The expected values were read from the web's
//! own code run under its Python.

use super::*;
use crate::flow::contract::model::{BRANCHING_TYPES, ONCE_PER_RUN_TYPES};
use crate::flow::contract::node::WARNING;
use crate::flow::contract::resolve::OrderFields;
use crate::flow::contract::vars::{VarStore, DEFAULT_BOUNDARY};
use crate::services::tunnel::fake::capture_logs;
use chrono::NaiveDateTime;

/// The level of the first captured line holding `needle` (the log layer
/// writes the time, then the level, then the line).
pub(super) fn level_of(text: &str, needle: &str) -> Option<String> {
    text.lines()
        .find(|l| l.contains(needle))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_string)
}

/// The rest of the web's branching nodes, scripted like the test family's.
const BRANCHING_FAMILY: NodeFamily = NodeFamily {
    key: "w6-branching",
    build: || {
        vec![
            Arc::new(Scripted("timeCondition")),
            Arc::new(Scripted("timeWindow")),
            Arc::new(Scripted("positionCheck")),
            Arc::new(Scripted("fundCheck")),
            Arc::new(Scripted("varCondition")),
        ]
    },
};

fn branching_registry() -> NodeRegistry {
    NodeRegistry::new(&[BRANCHING_FAMILY, TEST_FAMILY, FAMILIES[0], FAMILIES[1]])
}

/// Walk from `from` over `reg` with a bare context: the messages logged,
/// the run's errors, and the abort if any.
async fn walk_on(
    reg: &NodeRegistry,
    nodes: Value,
    edges: Value,
    from: &str,
) -> (Vec<String>, Vec<Value>, Option<String>) {
    let ctx = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    let graph = Graph::parse(&nodes, &edges).unwrap();
    let mut w = Walk::new(&graph, reg, &ctx);
    let abort = w.visit(from, 0).await.err();
    let logs = ctx
        .logs()
        .iter()
        .map(|l| l["message"].as_str().unwrap_or_default().to_string())
        .collect();
    (logs, w.errors, abort)
}

/// A start node wired to `children` scripted nodes and `ghosts` wires to
/// nodes that do not exist: a run of 1 + children + ghosts visits, one
/// level deep.
fn fan_out(children: usize, ghosts: usize) -> (Value, Value) {
    let mut nodes = vec![node("start", "start", json!({}))];
    let mut edges = Vec::new();
    for i in 0..children {
        let id = format!("c{}", i);
        nodes.push(node(&id, "getQuote", tagged(&i.to_string())));
        edges.push(edge("start", &id, None));
    }
    for i in 0..ghosts {
        edges.push(edge("start", &format!("ghost{}", i), None));
    }
    (json!(nodes), json!(edges))
}

fn ran_count(logs: &[String]) -> usize {
    logs.iter().filter(|l| l.starts_with("ran ")).count()
}

/// E-02: the visit cap is checked before each visit with `>=`, as the
/// web's: a run of exactly 500 visits completes, the 501st ends the run.
#[tokio::test]
async fn the_visit_cap_allows_exactly_500_visits() {
    assert_eq!(MAX_NODE_VISITS, 500);
    let (nodes, edges) = fan_out(MAX_NODE_VISITS - 1, 0);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert_eq!(abort, None);
    assert_eq!(ran_count(&logs), MAX_NODE_VISITS - 1);
    assert!(errors.is_empty());
    let (nodes, edges) = fan_out(MAX_NODE_VISITS, 0);
    let (logs, _, abort) = walk(nodes, edges).await;
    assert_eq!(abort.as_deref(), Some("Maximum node visits (500) exceeded"));
    assert_eq!(
        ran_count(&logs),
        MAX_NODE_VISITS - 1,
        "the 501st visit is refused"
    );
}

/// E-22: a wire to a node that is not in the graph returns silently (no
/// line, no error, siblings still run), and still counts as a visit, as
/// the web counts it before looking the node up.
#[tokio::test]
async fn a_missing_target_is_skipped_silently_but_counts_as_a_visit() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("q", "getQuote", tagged("q")),
    ]);
    let edges = json!([edge("start", "ghost", None), edge("start", "q", None)]);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert_eq!(logs, vec!["Workflow started", "ran q"]);
    assert!(errors.is_empty() && abort.is_none());
    let (nodes, edges) = fan_out(MAX_NODE_VISITS - 2, 1);
    assert_eq!(walk(nodes, edges).await.2, None);
    let (nodes, edges) = fan_out(MAX_NODE_VISITS - 1, 1);
    assert_eq!(
        walk(nodes, edges).await.2.as_deref(),
        Some("Maximum node visits (500) exceeded")
    );
}

/// E-22: the nodes evaluated once per run are the web's
/// BRANCHING_NODE_TYPES and priceAlert (decision 8).
#[test]
fn once_per_run_types_are_the_webs_branching_types_and_price_alert() {
    let web = [
        "priceCondition",
        "timeCondition",
        "timeWindow",
        "positionCheck",
        "fundCheck",
        "varCondition",
    ];
    assert_eq!(BRANCHING_TYPES, web);
    let mut want = web.to_vec();
    want.push("priceAlert");
    let mut ours = ONCE_PER_RUN_TYPES.to_vec();
    want.sort_unstable();
    ours.sort_unstable();
    assert_eq!(ours, want);
}

/// E-22: each of them reached by two paths decides once and fires its
/// branch once.
#[tokio::test]
async fn every_branching_type_runs_once_however_many_paths_reach_it() {
    let reg = branching_registry();
    for t in [
        "priceCondition",
        "timeCondition",
        "timeWindow",
        "positionCheck",
        "fundCheck",
        "varCondition",
        "priceAlert",
    ] {
        let nodes = json!([
            node("start", "start", json!({})),
            node("a", "group", json!({})),
            node("b", "group", json!({})),
            node(
                "c",
                t,
                json!({"tag": "cond", "answer": {"status": "success", "condition": true}})
            ),
            node(
                "o",
                "placeOrder",
                json!({"tag": "order", "answer": {"status": "success"}})
            ),
        ]);
        let edges = json!([
            edge("start", "a", None),
            edge("start", "b", None),
            edge("a", "c", None),
            edge("b", "c", None),
            edge("c", "o", Some("true")),
        ]);
        let (logs, _, _) = walk_on(&reg, nodes, edges, "start").await;
        let count = |l: &str| logs.iter().filter(|m| *m == l).count();
        assert_eq!((count("ran cond"), count("ran order")), (1, 1), "{}", t);
    }
}

/// E-22: a condition that could not be evaluated records no result, so a
/// second path reaching it runs it again and records a second failure,
/// as on the web.
#[tokio::test]
async fn an_errored_condition_reached_again_runs_and_fails_again() {
    let nodes = json!([
        node("start", "start", json!({})),
        node("a", "group", json!({})),
        node("b", "group", json!({})),
        node(
            "c",
            "priceCondition",
            json!({"tag": "cond", "answer": {"status": "error", "condition": false, "message": "no price"}})
        ),
        node("p", "getQuote", tagged("pass")),
    ]);
    let edges = json!([
        edge("start", "a", None),
        edge("start", "b", None),
        edge("a", "c", None),
        edge("b", "c", None),
        edge("c", "p", None),
    ]);
    let (logs, errors, abort) = walk(nodes, edges).await;
    assert_eq!(abort, None);
    assert_eq!(
        logs,
        vec![
            "Workflow started",
            "ran cond",
            "priceCondition could not be evaluated (no price)",
            "ran pass",
            "ran cond",
            "priceCondition could not be evaluated (no price)",
            "ran pass",
        ]
    );
    let one = json!({"node": "c", "type": "priceCondition", "message": "no price"});
    assert_eq!(errors, vec![one.clone(), one]);
}

/// N-logic-03: a wire leaving a handle that is not one of true/yes/false/no
/// (a named `output`, a blank or null handle, `True` in another case) is a
/// pass-through: followed whichever branch is taken, and when the
/// condition could not be evaluated.
#[tokio::test]
async fn named_and_blank_handles_are_pass_through() {
    let edges = json!([
        {"source": "c", "target": "t", "sourceHandle": "true"},
        {"source": "c", "target": "f", "sourceHandle": "false"},
        {"source": "c", "target": "out", "sourceHandle": "output"},
        {"source": "c", "target": "blank", "sourceHandle": ""},
        {"source": "c", "target": "nul", "sourceHandle": null},
        {"source": "c", "target": "cap", "sourceHandle": "True"},
    ]);
    let answer = |a: Value| json!({"tag": "cond", "answer": a});
    for (data, want) in [
        (
            answer(json!({"status": "success", "condition": true})),
            &["cond", "t", "out", "blank", "nul", "cap"][..],
        ),
        (
            answer(json!({"status": "success", "condition": false})),
            &["cond", "f", "out", "blank", "nul", "cap"][..],
        ),
        (
            answer(json!({"status": "error", "condition": false, "message": "x"})),
            &["cond", "out", "blank", "nul", "cap"][..],
        ),
    ] {
        let mut nodes = vec![node("c", "priceCondition", data.clone())];
        for t in ["t", "f", "out", "blank", "nul", "cap"] {
            nodes.push(node(t, "getQuote", tagged(t)));
        }
        let (logs, _, _) = walk_on(&registry(), json!(nodes), edges.clone(), "c").await;
        let order: Vec<&str> = logs.iter().filter_map(|l| l.strip_prefix("ran ")).collect();
        assert_eq!(order, want, "{}", data);
    }
}

fn gate_alone(gate: &str, data: Value) -> (Value, Value) {
    let nodes = json!([
        node("g", gate, data),
        node("y", "getQuote", tagged("yes")),
        node("n", "getQuote", tagged("no")),
    ]);
    let (yes, no) = if gate == "notGate" {
        ("yes", "no")
    } else {
        ("true", "false")
    };
    (
        nodes,
        json!([edge("g", "y", Some(yes)), edge("g", "n", Some(no))]),
    )
}

/// N-gate-03: a boolean inputCount counts as Python's int (`True` is 1,
/// `False` is 0) and the refusal writes it as the web's f-string does.
#[tokio::test]
async fn a_boolean_input_count_counts_as_pythons_int() {
    let refusal = |gate: &str| {
        format!(
            "{} is configured for True inputs but only 0 are wired. Wire the rest, or lower the input count, rather than evaluating on part of the condition.",
            gate
        )
    };
    for gate in ["notGate", "andGate"] {
        let (nodes, edges) = gate_alone(gate, json!({"inputCount": true}));
        let (logs, errors, _) = walk_on(&registry(), nodes, edges, "g").await;
        assert_eq!(logs, vec![refusal(gate)], "{}", gate);
        assert_eq!(
            errors,
            vec![json!({"node": "g", "type": gate, "message": refusal(gate)})]
        );
    }
    let (nodes, edges) = gate_alone("notGate", json!({"inputCount": false}));
    let (logs, errors, _) = walk_on(&registry(), nodes, edges, "g").await;
    assert_eq!(logs, vec!["NOT Gate: False -> True", "ran yes"]);
    assert!(errors.is_empty());
}

/// E-11: week_of_year and day_of_year are written without zero padding,
/// as the web's `str()` of the numbers (a `%V` / `%j` form would write
/// `02` and `005`).
#[test]
fn week_and_day_of_year_are_not_padded() {
    for (utc, want) in [
        ("2026-01-05T03:45:00Z", "2/5"),
        ("2026-01-01T03:45:00Z", "1/1"),
    ] {
        let s = VarStore::new(
            crate::clock::ManualClock::new(utc.parse().unwrap()),
            DEFAULT_BOUNDARY,
        );
        assert_eq!(s.interpolate("{{week_of_year}}/{{day_of_year}}"), want);
    }
}

/// E-15: an index segment reads a list only: on a dict (even one with a
/// key `0`) or at the root of the variables it stays unresolved, while a
/// dotted `0` reads the key.
#[test]
fn an_index_on_a_dict_or_at_the_root_stays_unresolved() {
    let v = VarStore::default();
    v.set("d", json!({"0": 1, "k": [5]}));
    v.set("0", json!("root"));
    v.set("l", json!([7, 8]));
    for (text, want) in [
        ("{{d[0]}}", "{{d[0]}}"),
        ("{{[0]}}", "{{[0]}}"),
        ("{{d[0].x}}", "{{d[0].x}}"),
        ("{{l.0}}", "{{l.0}}"),
        ("{{d.0}}", "1"),
        ("{{0}}", "root"),
        ("{{l[0]}}", "7"),
        ("{{d.k[0]}}", "5"),
    ] {
        assert_eq!(v.interpolate(text), want, "{}", text);
    }
    assert_eq!(v.resolve_raw(&json!("{{d[0]}}")), json!("{{d[0]}}"));
    assert_eq!(v.resolve_raw(&json!("{{[0]}}")), json!("{{[0]}}"));
    assert_eq!(v.resolve_raw(&json!("{{d.0}}")), json!(1));
}

fn data(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// N-common-13: the first key present wins, even blank or null, so a
/// filled alias behind it is never read.
#[test]
fn the_first_present_key_wins_even_when_blank() {
    let vars = VarStore::default();
    let price_type = |d: Value| {
        OrderFields::new(&vars, &data(d)).choice(
            "priceType",
            &["MARKET", "LIMIT"],
            Some("MARKET"),
            &["pricetype"],
        )
    };
    assert_eq!(
        price_type(json!({"priceType": "", "pricetype": "LIMIT"})),
        Err("priceType must not be blank".into())
    );
    assert_eq!(
        price_type(json!({"priceType": null, "pricetype": "LIMIT"})),
        Err("priceType must resolve to text, got None".into())
    );
    assert_eq!(
        price_type(json!({"pricetype": "limit"})),
        Ok("LIMIT".into())
    );
    let d = data(json!({"quantity": "", "qty": "5"}));
    assert_eq!(
        OrderFields::new(&vars, &d).integer("quantity", Some(1), &["qty"], Some(1)),
        Err("quantity must not be blank".into())
    );
}

/// N-common-14: a failure of a value read through an alias names the key,
/// not the alias; only the unresolved-reference sentence names the key it
/// read.
#[test]
fn a_failure_through_an_alias_names_the_key() {
    let vars = VarStore::default();
    let d = data(json!({"pricetype": "hold"}));
    let f = OrderFields::new(&vars, &d);
    assert_eq!(
        f.choice(
            "priceType",
            &["MARKET", "LIMIT"],
            Some("MARKET"),
            &["pricetype"]
        ),
        Err("priceType must be one of LIMIT, MARKET, got 'HOLD'".into())
    );
    let d = data(json!({"pricetype": "  "}));
    assert_eq!(
        OrderFields::new(&vars, &d).choice(
            "priceType",
            &["MARKET", "LIMIT"],
            Some("MARKET"),
            &["pricetype"]
        ),
        Err("priceType must not be blank".into())
    );
    for (raw, want) in [
        (json!(true), "quantity must be numeric, got boolean True"),
        (json!("0"), "quantity must be at least 1, got 0.0"),
        (json!("abc"), "quantity must be numeric, got 'abc'"),
    ] {
        let d = data(json!({ "qty": raw }));
        assert_eq!(
            OrderFields::new(&vars, &d).integer("quantity", Some(1), &["qty"], Some(1)),
            Err(want.to_string())
        );
    }
    let d = data(json!({"sym": 5}));
    assert_eq!(
        OrderFields::new(&vars, &d).text("symbol", None, &["sym"]),
        Err("symbol must resolve to text, got 5".into())
    );
    let d = data(json!({"pricetype": "{{nope}}"}));
    assert_eq!(
        OrderFields::new(&vars, &d).choice(
            "priceType",
            &["MARKET", "LIMIT"],
            Some("MARKET"),
            &["pricetype"]
        ),
        Err("pricetype has unresolved value '{{nope}}'".into())
    );
}

fn at(hms: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(&format!("2026-10-30 {}", hms), "%Y-%m-%d %H:%M:%S").unwrap()
}

/// E-44: a Delay with neither delayValue nor delayMs (or a null
/// delayValue) counts one second, so eleven of them run in the
/// background and ten inline; a Wait Until with no targetTime waits for
/// 09:30.
#[test]
fn absent_wait_fields_count_as_the_nodes_defaults() {
    for delay in [
        json!({"type": "delay", "data": {}}),
        json!({"type": "delay"}),
        json!({"type": "delay", "data": {"delayValue": null}}),
    ] {
        assert_eq!(
            waits::workflow_wait_seconds(&json!([delay.clone()]), at("09:15:00")),
            (1.0, 0.0),
            "{}",
            delay
        );
    }
    let delays = |n: usize| json!(vec![json!({"type": "delay", "data": {}}); n]);
    assert_eq!(
        waits::workflow_wait_seconds(&delays(11), at("09:15:00")),
        (11.0, 0.0)
    );
    assert!(waits::runs_in_background(&delays(11), at("09:15:00")));
    assert!(!waits::runs_in_background(&delays(10), at("09:15:00")));
    for until in [
        json!({"type": "waitUntil", "data": {}}),
        json!({"type": "waitUntil", "data": {"targetTime": null}}),
    ] {
        assert_eq!(
            waits::workflow_wait_seconds(&json!([until.clone()]), at("09:15:00")),
            (0.0, 900.0),
            "{}",
            until
        );
        assert_eq!(
            waits::workflow_wait_seconds(&json!([until]), at("09:29:55")),
            (0.0, 5.0)
        );
    }
}

/// E-20 / N-common-08 (decision 29): every entry of the run's log is
/// mirrored to the app log as `Flow: {message}`, at warn for an error
/// entry and at debug for every other level.
#[test]
fn every_entry_is_mirrored_to_the_app_log_at_warn_or_debug() {
    let logs = capture_logs();
    let c = NodeCtx::bare(Arc::new(FakeFlowClient::new()));
    c.info("w6 info line");
    c.error("w6 error line");
    c.log("w6 warning line", WARNING);
    c.log("w6 custom line", "critical");
    let text = logs.text();
    for (needle, level) in [
        ("Flow: w6 info line", "DEBUG"),
        ("Flow: w6 error line", "WARN"),
        ("Flow: w6 warning line", "DEBUG"),
        ("Flow: w6 custom line", "DEBUG"),
    ] {
        assert_eq!(
            level_of(&text, needle).as_deref(),
            Some(level),
            "{}\n{}",
            needle,
            text
        );
    }
    assert_eq!(c.logs().len(), 4);
}
