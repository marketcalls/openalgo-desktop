//! The flow-builder skill's own checks (test only). The web runs them as
//! Python scripts beside the skill; the desktop has no Python, so they run
//! here, against the desktop's validator:
//!
//! - `validate.py`: [`check`], the importer's own validator, then the
//!   warnings for a `data` key nothing reads and a `{{reference}}` nothing
//!   produces. [`check_the_workflow_file`] is the command the skill tells a
//!   reader to run (`FLOW_WORKFLOW=... cargo test ... -- --ignored`).
//! - `coverage.py`: every node type listed, fielded and explained.
//! - `generate_reference.py`: the desktop does not regenerate
//!   `reference/nodes.md`; it carries the web's page and pins it to the web's
//!   text and to the desktop's validator (lead decision 17).
//!
//! Two web defects are fixed in the check, not copied: its built-in list
//! missed five of the executor's built-ins, so `{{session_date}}` was called
//! dangling (parity A-33); and the web's generated table misses the keys the
//! executor reads through an alias or a helper its scan cannot see, so the
//! import-format's own custom iron fly was told its `legs` are read by
//! nothing ([`READ_BUT_NOT_IN_THE_TABLE`]).

use crate::agent::skills::loader::normalise;
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::flow::contract::model::{BRANCHING_TYPES, GATE_TYPES, ORDER_TYPES, TRIGGER_TYPES};
use crate::flow::contract::pyfmt;
use crate::flow::contract::vars::VarStore;
use crate::flow::node_contracts::{is_option_offset, VALID_EXPIRY_TYPES, VALID_LEG_STRIKE_MODES};
use crate::flow::validator::{
    self, ValidateOptions, ValidationError, REQUIRED_NODE_FIELDS, VALID_NODE_TYPES,
};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const NODES_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.claude/skills/flow-builder/reference/nodes.md"
));
const LONG_FORM: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../docs/prompt/flow-import-format.md"
));
const SKILL_MD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../.claude/skills/flow-builder/SKILL.md"
));

/// SHA-256 of the web's `.claude/skills/flow-builder/reference/nodes.md`
/// at web commit 96e5927e5 (unchanged at dd198ba99), line endings as LF.
/// A web change to the page is carried over by copying it again and
/// updating this, never by editing the page here.
const WEB_NODES_MD_SHA256: &str =
    "2330e0d0f0622a905f129728354885991291c52d10d86ca7c7f2be03f4ba3b20";

/// Read by the runtime for every node, not by any one node (`validate.py`
/// `UNIVERSAL_KEYS`).
const UNIVERSAL_KEYS: &[&str] = &[
    "label",
    "outputVariable",
    "strategyTag",
    "notes",
    "description",
];

/// Keys the web executor reads that its generated table leaves out: its
/// scan sees `get_*`, `node_data.get`, four `values.*` readers and a
/// helper's first key, not `values.value`, an alias, a helper's later keys
/// or a key looked up with `in` (web `flow_executor_service.py` at
/// 96e5927e5, line by line below).
const READ_BUT_NOT_IN_THE_TABLE: &[(&str, &[&str])] = &[
    // `common_value(...)`, 1834-1848.
    (
        "basketOrder",
        &["price", "priceType", "product", "triggerPrice"],
    ),
    // `_parse_margin_positions`, 2613-2624.
    ("margin", &["positions", "positionsJson"]),
    // `_supplied(node_data, first, second)`, 1650, 1674-1676.
    (
        "modifyOrder",
        &["price", "pricetype", "quantity", "triggerPrice"],
    ),
    // `values.text("strategy", aliases=("strategyType",))`, 1205-1207;
    // `values.value("legs", aliases=("orderLegs",))`, 1240.
    ("optionsMultiOrder", &["legs", "orderLegs", "strategyType"]),
];

/// One row of the node table.
#[derive(Debug)]
struct Row {
    node_type: String,
    kind: String,
    required: Vec<String>,
    also_read: Vec<String>,
}

fn backticked(cell: &str) -> Vec<String> {
    let re = Regex::new(r"`([^`]+)`").unwrap();
    re.captures_iter(cell).map(|c| c[1].to_string()).collect()
}

/// The `## Every node` table of `nodes.md`.
fn rows() -> Vec<Row> {
    let section = NODES_MD
        .split("## Every node")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .unwrap();
    section
        .lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cells: Vec<&str> = l.trim().trim_matches('|').split('|').collect();
            assert_eq!(cells.len(), 4, "{}", l);
            Row {
                node_type: backticked(cells[0]).remove(0),
                kind: cells[1].trim().to_string(),
                required: backticked(cells[2]),
                also_read: backticked(cells[3]),
            }
        })
        .collect()
}

/// The enumerated values table: field to the backticked values.
fn enumerations() -> Vec<(String, Vec<String>)> {
    let section = NODES_MD.split("## Enumerated values").nth(1).unwrap();
    section
        .lines()
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| l.starts_with("| ") && !l.starts_with("| Field") && !l.starts_with("|---"))
        .map(|l| {
            let cells: Vec<&str> = l.trim().trim_matches('|').split('|').collect();
            (cells[0].trim().to_string(), backticked(cells[1]))
        })
        .collect()
}

/// Every `data` key a node type responds to: the table's two columns and
/// what the table misses; `None` for a type the table does not list.
fn known_keys(node_type: &str) -> Option<BTreeSet<String>> {
    let row = rows().into_iter().find(|r| r.node_type == node_type)?;
    let mut keys: BTreeSet<String> = row.required.into_iter().chain(row.also_read).collect();
    for (t, extra) in READ_BUT_NOT_IN_THE_TABLE {
        if *t == node_type {
            keys.extend(extra.iter().map(|k| k.to_string()));
        }
    }
    Some(keys)
}

fn id_of(node: &Value) -> String {
    pyfmt::str(node.get("id").unwrap_or(&Value::Null))
}

/// `validate.py`'s `unknown_keys`: a `data` key nothing reads.
fn unknown_keys(nodes: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for node in nodes {
        let Some(t) = node.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !VALID_NODE_TYPES.contains(&t) || TRIGGER_TYPES.contains(&t) {
            continue;
        }
        let Some(mut known) = known_keys(t) else {
            continue;
        };
        known.retain(|k| !UNIVERSAL_KEYS.contains(&k.as_str()));
        if known.is_empty() {
            continue; // nothing listed for this node; do not guess
        }
        let data = node.get("data").and_then(Value::as_object);
        for key in data.into_iter().flat_map(|d| d.keys()) {
            if !known.contains(key) && !UNIVERSAL_KEYS.contains(&key.as_str()) {
                out.push(format!(
                    "{} ({}): data key '{}' is read by nothing. Flow ignores it, so the node keeps its default. It reads: {}",
                    id_of(node),
                    t,
                    key,
                    known.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    out
}

/// `validate.py`'s `dangling_references`: a `{{path}}` whose root nothing
/// in the workflow produces, built-ins being the executor's own.
fn dangling_references(nodes: &[Value]) -> Vec<String> {
    let mut produced: BTreeSet<String> = BTreeSet::from(["webhook".to_string()]);
    for node in nodes {
        let data = node.get("data");
        let text = |k: &str| data.and_then(|d| d.get(k)).filter(|v| pyfmt::truthy(v));
        if let Some(var) = text("outputVariable").and_then(Value::as_str) {
            let var = validator::py::strip(var);
            if !var.is_empty() {
                produced.insert(var.to_string());
            }
        }
        let name = text("variableName").or_else(|| text("name"));
        if node.get("type").and_then(Value::as_str) == Some("variable") {
            if let Some(name) = name.and_then(Value::as_str).map(validator::py::strip) {
                if !name.is_empty() {
                    produced.insert(name.to_string());
                }
            }
        }
    }
    let builtins = VarStore::default();
    let token = Regex::new(r"\{\{([^}]+)\}\}").unwrap();
    let mut out = Vec::new();
    for node in nodes {
        let data = node.get("data").and_then(Value::as_object);
        for (key, value) in data.into_iter().flatten() {
            let Some(value) = value.as_str() else {
                continue;
            };
            for c in token.captures_iter(value) {
                let path = validator::py::strip(&c[1]);
                let root = path.split('.').next().unwrap_or("");
                let root = root.split('[').next().unwrap_or("");
                if !root.is_empty() && !produced.contains(root) && builtins.builtin(root).is_none()
                {
                    out.push(format!(
                        "{} ({}): {} references '{{{{{}}}}}' but nothing produces '{}'. Available: {}",
                        id_of(node),
                        pyfmt::str(node.get("type").unwrap_or(&Value::Null)),
                        key,
                        path,
                        root,
                        produced.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
    }
    out
}

/// What `validate.py` finds in one workflow.
struct Report {
    errors: Vec<ValidationError>,
    warnings: Vec<String>,
    nodes: usize,
}

/// Both layers: exactly what the importer runs, then the mistakes that
/// import cleanly and misbehave.
fn check(payload: &Value) -> Report {
    let nodes: Vec<Value> = payload
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let errors = validator::validate_workflow(payload, ValidateOptions::COMPLETE);
    let mut warnings = unknown_keys(&nodes);
    warnings.extend(dangling_references(&nodes));
    Report {
        errors,
        warnings,
        nodes: nodes.len(),
    }
}

impl Report {
    /// What `validate.py` prints, line for line.
    fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .errors
            .iter()
            .map(|e| format!("  ERROR    {}: {}", e.path, e.message))
            .collect();
        out.extend(self.warnings.iter().map(|w| format!("  WARNING  {}", w)));
        out.push(String::new());
        out.push(self.verdict());
        out
    }

    fn verdict(&self) -> String {
        if !self.errors.is_empty() {
            format!(
                "FAILED: {} error(s), {} warning(s). Will not import.",
                self.errors.len(),
                self.warnings.len()
            )
        } else if !self.warnings.is_empty() {
            format!(
                "PASSED with {} warning(s). It will import, but a warned field is being ignored at run time.",
                self.warnings.len()
            )
        } else {
            format!("PASSED: {} node(s), no errors, no warnings.", self.nodes)
        }
    }
}

/// The skill's "validate before you hand anything over", as a command:
/// `FLOW_WORKFLOW=/absolute/path.json cargo test --locked --lib
/// flow_builder::check_the_workflow_file -- --ignored --nocapture`. It
/// fails, as `validate.py` exits 1, when the workflow would not import.
#[test]
#[ignore = "a tool, not a test: FLOW_WORKFLOW=/path/workflow.json ... -- --ignored --nocapture"]
fn check_the_workflow_file() {
    let path = std::env::var("FLOW_WORKFLOW")
        .expect("set FLOW_WORKFLOW to the absolute path of a workflow JSON file");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("FAILED: {}: {}", path, e));
    let payload: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("FAILED: {} is not valid JSON: {}", path, e));
    let report = check(&payload);
    for line in report.lines() {
        println!("{}", line);
    }
    assert!(report.errors.is_empty(), "{}", report.verdict());
}

/// Every ```json block of a document that is a whole workflow, with its
/// line.
fn workflows_in(doc: &str) -> Vec<(usize, Value)> {
    let fence = Regex::new(r"(?s)```json[^\n]*\n(.*?)```").unwrap();
    fence
        .captures_iter(doc)
        .filter_map(|c| {
            let v: Value = serde_json::from_str(&c[1]).ok()?;
            let whole = v.get("nodes").is_some_and(Value::is_array)
                && v.get("edges").is_some_and(Value::is_array);
            let at = c.get(0).map_or(0, |m| m.start());
            whole.then(|| (doc[..at].matches('\n').count() + 1, v))
        })
        .collect()
}

// ------------------------------------------------------- the node table

/// Decision 17: the page is the web's, byte for byte.
#[test]
fn the_node_reference_is_the_webs_page() {
    let digest = Sha256::digest(normalise(NODES_MD).as_bytes());
    assert_eq!(
        hex::encode(digest),
        WEB_NODES_MD_SHA256,
        "reference/nodes.md is the web's generated page: copy it from the web again, never edit it here"
    );
}

/// The web's page agrees with the desktop's validator and vocabularies, so
/// what it teaches is what the desktop accepts (the web's "cannot drift").
#[test]
fn the_node_reference_agrees_with_the_desktop_validator() {
    let rows = rows();
    let listed: BTreeSet<&str> = rows.iter().map(|r| r.node_type.as_str()).collect();
    let valid: BTreeSet<&str> = VALID_NODE_TYPES.iter().copied().collect();
    assert_eq!(listed, valid);
    assert!(NODES_MD.contains(&format!("**{} node types.**", valid.len())));
    for row in &rows {
        let required = REQUIRED_NODE_FIELDS
            .iter()
            .find(|(t, _)| *t == row.node_type)
            .map_or(&[][..], |(_, f)| *f);
        assert_eq!(
            row.required, required,
            "required fields of {}",
            row.node_type
        );
    }
    let of_kind = |kind: &str| -> BTreeSet<&str> {
        rows.iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.node_type.as_str())
            .collect()
    };
    assert_eq!(
        of_kind("trigger"),
        TRIGGER_TYPES.iter().copied().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        of_kind("condition"),
        BRANCHING_TYPES.iter().copied().collect::<BTreeSet<_>>()
    );
    assert!(of_kind("gate").iter().all(|g| GATE_TYPES.contains(g)));
    assert!(ORDER_TYPES.iter().all(|o| of_kind("order").contains(o)));
    let sorted = |v: Vec<&str>| {
        let mut v: Vec<String> = v.into_iter().map(str::to_string).collect();
        v.sort();
        v
    };
    for (field, values) in enumerations() {
        let expected = match field.as_str() {
            "`action`" => sorted(Action::ALL.iter().map(|a| a.as_str()).collect()),
            "`exchange`" => sorted(Exchange::ALL.iter().map(|e| e.as_str()).collect()),
            "`product`" => sorted(Product::ALL.iter().map(|p| p.as_str()).collect()),
            "`priceType`" => sorted(PriceType::ALL.iter().map(|p| p.as_str()).collect()),
            "`optionType`" => sorted(vec!["CE", "PE"]),
            "`expiryType`" => {
                let mut v = sorted(VALID_EXPIRY_TYPES.to_vec());
                v.push("DDMMMYY".into());
                v
            }
            "leg `strikeMode`" => sorted(VALID_LEG_STRIKE_MODES.to_vec()),
            "`offset`" => {
                assert!(["ATM", "ITM1", "ITM50", "OTM1", "OTM50"]
                    .iter()
                    .all(|o| is_option_offset(o)));
                assert!(!is_option_offset("ITM51") && !is_option_offset("OTM0"));
                vec![
                    "ATM".into(),
                    "ITM1".into(),
                    "ITM50".into(),
                    "OTM1".into(),
                    "OTM50".into(),
                ]
            }
            other => panic!("an enumeration the test does not know: {}", other),
        };
        assert_eq!(values, expected, "{}", field);
    }
}

/// `coverage.py`: every node type the validator accepts is LISTED in the
/// table, FIELDED (its required fields named there) and EXPLAINED (named in
/// the import-format long form).
#[test]
fn every_node_type_is_listed_fielded_and_explained() {
    let (mut listed, mut fielded, mut explained) = (Vec::new(), Vec::new(), Vec::new());
    for t in VALID_NODE_TYPES {
        if !NODES_MD.contains(&format!("`{}`", t)) {
            listed.push(t);
            continue;
        }
        let required = REQUIRED_NODE_FIELDS
            .iter()
            .find(|(n, _)| *n == t)
            .map_or(&[][..], |(_, f)| *f);
        if !required
            .iter()
            .all(|f| NODES_MD.contains(&format!("`{}`", f)))
        {
            fielded.push(t);
        }
        if !LONG_FORM.contains(t) {
            explained.push(t);
        }
    }
    assert_eq!(
        (listed, fielded, explained),
        (vec![], vec![], vec![]),
        "GAPS: listed, fielded, explained"
    );
    assert_eq!(VALID_NODE_TYPES.len(), 61);
}

// ------------------------------------------------------- the two layers

fn workflow(nodes: Value, edges: Value) -> Value {
    json!({"name": "Check", "nodes": nodes, "edges": edges})
}

/// Parity A-33: the executor's built-ins are not dangling references. The
/// web's list stops at `iso_timestamp`, so these five were each warned.
#[test]
fn the_executors_built_ins_are_not_dangling() {
    for name in [
        "timestamp",
        "date",
        "time",
        "year",
        "month",
        "day",
        "hour",
        "minute",
        "second",
        "weekday",
        "iso_timestamp",
        "weekday_num",
        "quarter",
        "week_of_year",
        "day_of_year",
        "session_date",
    ] {
        let nodes = json!([{"id": "n1", "type": "log", "data": {"message": format!("on {{{{{}}}}}", name)}}]);
        assert_eq!(
            dangling_references(nodes.as_array().unwrap()),
            Vec::<String>::new(),
            "{}",
            name
        );
    }
    let nodes = json!([{"id": "n1", "type": "log", "data": {"message": "{{ sessiondate }}"}}]);
    assert_eq!(
        dangling_references(nodes.as_array().unwrap()),
        ["n1 (log): message references '{{sessiondate}}' but nothing produces 'sessiondate'. Available: webhook"]
    );
}

/// `validate.py`'s two warnings, worded as the web words them.
#[test]
fn an_unread_key_and_a_dangling_reference_are_warned() {
    let payload = workflow(
        json!([
            {"id": "t", "type": "webhookTrigger", "position": {"x": 0, "y": 0}, "data": {"label": "TV"}},
            {"id": "o", "type": "optionsOrder", "position": {"x": 0, "y": 1},
             "data": {"underlying": "NIFTY", "action": "BUY", "quantity": 1, "strikeOffset": "OTM2",
                      "outputVariable": " entry ", "notes": "x"}},
            {"id": "v", "type": "variable", "position": {"x": 0, "y": 2},
             "data": {"variableName": "", "name": "counter", "value": "{{entry.orderid}} {{counter}} {{quote.ltp}} {{webhook.legs[0].qty}}"}},
        ]),
        json!([]),
    );
    let report = check(&payload);
    assert_eq!(
        report.warnings,
        [
            "o (optionsOrder): data key 'strikeOffset' is read by nothing. Flow ignores it, so the node keeps its default. It reads: action, exchange, expiryType, offset, optionType, price, priceType, product, quantity, splitSize, triggerPrice, underlying",
            "v (variable): value references '{{quote.ltp}}' but nothing produces 'quote'. Available: counter, entry, webhook",
        ]
    );
}

/// The verdicts `validate.py` prints, and a refused workflow fails.
#[test]
fn the_report_reads_as_the_webs() {
    let clean = check(&workflow(
        json!([{"id": "s", "type": "start", "position": {"x": 0, "y": 0}, "data": {}}]),
        json!([]),
    ));
    assert_eq!(
        clean.lines(),
        ["", "PASSED: 1 node(s), no errors, no warnings."]
    );
    let warned = check(&workflow(
        json!([
            {"id": "s", "type": "start", "position": {"x": 0, "y": 0}, "data": {}},
            {"id": "l", "type": "log", "position": {"x": 0, "y": 1}, "data": {"message": "{{nope}}"}},
        ]),
        json!([{"id": "e", "source": "s", "target": "l"}]),
    ));
    assert!(warned.errors.is_empty(), "{:?}", warned.lines());
    assert_eq!(warned.verdict(), "PASSED with 1 warning(s). It will import, but a warned field is being ignored at run time.");
    let refused = check(&json!({"nodes": [], "edges": []}));
    let lines = refused.lines();
    assert_eq!(
        lines[0],
        "  ERROR    /name: Workflow needs a non-empty name"
    );
    assert!(refused.verdict().starts_with("FAILED: "));
    assert!(refused
        .verdict()
        .ends_with("error(s), 0 warning(s). Will not import."));
}

/// Every whole workflow the long form and the skill show imports and is
/// clean on both layers. The web's check warned the long form's own
/// custom iron fly that its `legs` are read by nothing.
#[test]
fn the_documented_workflows_pass_both_layers() {
    let mut seen = 0;
    for (doc, text) in [("flow-import-format.md", LONG_FORM), ("SKILL.md", SKILL_MD)] {
        for (line, wf) in workflows_in(text) {
            let report = check(&wf);
            assert!(
                report.errors.is_empty() && report.warnings.is_empty(),
                "{}:{}: {:?}",
                doc,
                line,
                report.lines()
            );
            seen += 1;
        }
    }
    assert_eq!(seen, 18);
}

// ------------------------------------------------- what the page says here

/// Parity A-28, A-29: the skill says what the desktop does. A wait no
/// longer holds the triggering request (the web's page said it did; the
/// executor answers `accepted` past 10 s and goes on in the background), the
/// caps are the executor's, and no web-only command is left to run.
#[test]
fn the_skill_states_what_the_desktop_does() {
    // A Windows checkout may hold the page with CRLF line endings.
    let skill_md = normalise(SKILL_MD);
    use crate::flow::contract::time::WAIT_UNTIL_MAX_SECONDS;
    use crate::flow::executor::waits::INLINE_WAIT_SECONDS;
    assert!(!skill_md.contains("sleeps inside the triggering request"));
    assert!(skill_md.contains(&format!(
        "**`waitUntil` is capped at {} minutes.** A run whose Delay and Wait Until steps\nadd up to more than {} seconds is answered at once (`accepted`) and finishes in\nthe background",
        WAIT_UNTIL_MAX_SECONDS / 60,
        INLINE_WAIT_SECONDS
    )));
    for web_only in [
        "uv run python",
        "FLOW_HISTORY_CACHE_TTL",
        "Set it to 2-3",
        "| `validate.py` |",
    ] {
        assert!(!skill_md.contains(web_only), "{}", web_only);
    }
    assert!(skill_md.contains("## Desktop: where this differs from the web skill"));
    assert!(skill_md.contains("flow_builder::check_the_workflow_file"));
    // The frontmatter is the web's, word for word.
    assert!(skill_md.starts_with("---\nname: flow-builder\ndescription: Build, edit or debug an OpenAlgo Flow workflow - the no-code node graph at /flow. Use when asked to create a workflow, wire a webhook or TradingView alert to an order, add a node, port a strategy into Flow, or work out why a workflow imported but did nothing. Produces a workflow JSON that is validated against the real importer before it is handed over.\n---\n"));
}
