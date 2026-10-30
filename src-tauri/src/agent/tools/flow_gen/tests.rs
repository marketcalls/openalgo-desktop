//! `validate_flow` and `save_flow` (the web's `flow_gen.py`, its
//! `test_validate_flow_accepts_an_object` and the toolkit assertions of
//! `test_agent_voice.py`) against the desktop's validator and the Flow
//! store the `/flow` import route writes through, in the app's temporary
//! data folder. The web has no behaviour tests for the toolkit; these are
//! parity A-41's.

use super::{GROUP, MAX_JSON_CHARS, MAX_REPORTED_ERRORS};
use crate::agent::chat::tests::{harness, types};
use crate::agent::contract::tool::{AgentTool, Capabilities, Surface, ToolGroup, ToolOutcome};
use crate::agent::testing::Reply;
use crate::agent::tools::testing::ctx;
use crate::agent::tools::testkit::call_checked;
use crate::agent::tools::{select, spec_contract, TOOLKITS};
use crate::flow::contract::model::Workflow;
use crate::flow::service::{transfer, Body};
use crate::flow::store::workflows;
use crate::flow::validator::{validate_workflow, ValidateOptions};
use crate::state::testing::{build, TestCtx};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;

/// The skill's own example: a TradingView alert to a market order.
fn webhook_entry() -> Value {
    json!({
        "name": "Webhook long entry",
        "description": "TV alert to a market order",
        "nodes": [
            {"id": "node_1", "type": "webhookTrigger", "position": {"x": 0, "y": 0},
             "data": {"label": "TV alert"}},
            {"id": "node_2", "type": "placeOrder", "position": {"x": 0, "y": 120},
             "data": {"symbol": "{{webhook.symbol}}", "exchange": "NSE",
                      "action": "{{webhook.action}}", "quantity": "{{webhook.quantity}}",
                      "product": "MIS", "priceType": "MARKET", "outputVariable": "orderResult"}}
        ],
        "edges": [{"id": "e1", "source": "node_1", "target": "node_2", "type": "default"}]
    })
}

/// A daily schedule with a legacy priceCondition (`threshold`, which the
/// import migration renames to `value`).
fn legacy_schedule() -> Value {
    json!({
        "name": "Legacy",
        "nodes": [
            {"id": "s", "type": "start", "position": {"x": 0, "y": 0},
             "data": {"label": "Daily", "time": "09:20", "scheduleType": "daily",
                      "days": [0, 1, 2, 3, 4], "marketHoursOnly": true}},
            {"id": "p", "type": "priceCondition", "position": {"x": 0, "y": 1},
             "data": {"symbol": "SBIN", "exchange": "NSE", "operator": ">", "threshold": 800}},
            {"id": "l", "type": "log", "position": {"x": 0, "y": 2}, "data": {"message": "up"}}
        ],
        "edges": [
            {"id": "e1", "source": "s", "target": "p"},
            {"id": "e2", "source": "p", "sourceHandle": "true", "target": "l"}
        ]
    })
}

fn app() -> TestCtx {
    build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now())
}

fn tool(t: &TestCtx, name: &str) -> Arc<dyn AgentTool> {
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    (GROUP.build)(&c)
        .into_iter()
        .find(|x| x.spec().name == name)
        .unwrap()
}

/// The tool itself, with no argument guard in front.
async fn direct(t: &TestCtx, name: &str, args: Value) -> ToolOutcome {
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    tool(t, name)
        .call(&c, args.as_object().cloned().unwrap_or_default())
        .await
}

/// As a run calls it: the argument guard, then the tool.
async fn call(t: &TestCtx, name: &str, args: Value) -> ToolOutcome {
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    call_checked(tool(t, name).as_ref(), &c, &args).await
}

/// The JSON of a `<tool_result>` block, and its raw text.
fn block(text: &str) -> (Value, String) {
    let start = text.find("<tool_result").unwrap();
    let end = text.find("</tool_result>").unwrap();
    let inner = &text[start..end];
    let json_text = inner.split_once('\n').unwrap().1.trim_end().to_string();
    (serde_json::from_str(&json_text).unwrap(), json_text)
}

fn retry(out: &ToolOutcome) -> &str {
    match out {
        ToolOutcome::Retry(t) => t,
        ToolOutcome::Ok(t) => panic!("expected a refusal, got {}", t),
    }
}

fn rows(t: &TestCtx) -> Vec<Workflow> {
    let conn = t.ctx.sqlite.conn().unwrap();
    workflows::list(&conn)
        .unwrap()
        .into_iter()
        .map(|w| {
            workflows::get(&conn, &t.ctx.security, w.id)
                .unwrap()
                .unwrap()
        })
        .collect()
}

// ------------------------------------------------------------ the toolkit

/// A-01, A-02, A-04, A-05: key, surfaces, capabilities, order, wording, the
/// two tools in the web's order and their arguments, byte for byte.
#[tokio::test]
async fn the_toolkit_is_the_webs() {
    let t = app();
    let c = ctx(
        t.ctx.clone(),
        Surface::Chat,
        spec_contract::all_capabilities(),
    );
    spec_contract::assert_toolkit(&GROUP, &c);
    let approval: Vec<(&str, bool)> = (GROUP.build)(&c)
        .iter()
        .map(|x| (x.spec().name, x.spec().needs_approval))
        .collect();
    assert_eq!(approval, [("validate_flow", false), ("save_flow", true)]);
}

/// A-06, X-338 (the web's `test_voice_never_reaches_the_toolkits_that_
/// produce_something_to_review`, `test_chat_still_gets_them`,
/// `test_the_chart_panel_is_unchanged_by_the_new_surface`): chat with every
/// capability combination, never the chart panel or voice.
#[tokio::test]
async fn offered_on_chat_with_any_capabilities_never_on_the_chart_or_voice() {
    let t = app();
    for trading_enabled in [false, true] {
        for web_search_enabled in [false, true] {
            for mcp_enabled in [false, true] {
                let caps = Capabilities {
                    trading_enabled,
                    web_search_enabled,
                    mcp_enabled,
                };
                let keys = |s| -> Vec<&'static str> {
                    select(TOOLKITS, &ctx(t.ctx.clone(), s, caps))
                        .iter()
                        .map(|g| g.key)
                        .collect()
                };
                assert!(keys(Surface::Chat).contains(&"flow_gen"), "{:?}", caps);
                assert!(keys(Surface::Chat).contains(&"openui"));
                assert!(!keys(Surface::Chart).contains(&"flow_gen"), "{:?}", caps);
                assert!(!keys(Surface::Voice).contains(&"flow_gen"), "{:?}", caps);
            }
        }
    }
    // Between save_openscript and web_search, as the web's chat list has it.
    let all = ctx(
        t.ctx.clone(),
        Surface::Chat,
        spec_contract::all_capabilities(),
    );
    let names: Vec<&str> = crate::agent::tools::build_tools(TOOLKITS, &all)
        .iter()
        .map(|x| x.spec().name)
        .collect();
    let at = names.iter().position(|n| *n == "validate_flow").unwrap();
    assert_eq!(
        &names[at - 1..at + 3],
        [
            "save_openscript",
            "validate_flow",
            "save_flow",
            "web_search"
        ]
    );
}

// ---------------------------------------------------------- validate_flow

/// X-335, the web's `test_validate_flow_accepts_an_object`: an object the
/// runtime already parsed is taken as it is, and never measured.
#[tokio::test]
async fn validate_flow_accepts_an_object() {
    let t = app();
    let out = call(
        &t,
        "validate_flow",
        json!({"workflow_json": webhook_entry()}),
    )
    .await;
    assert_eq!(block(out.text()).0["valid"], true, "{}", out.text());
    let mut big = webhook_entry();
    big["nodes"][1]["data"]["label"] = json!("x".repeat(MAX_JSON_CHARS + 1));
    let out = call(&t, "validate_flow", json!({"workflow_json": big})).await;
    assert!(out.is_ok(), "{}", out.text());
}

/// A-07, A-08: the success payload in the web's key order, the trigger
/// configuration in its own order (TRIGGER_CONFIG_FIELDS, not sorted), the
/// migration notes, and nothing written.
#[tokio::test]
async fn a_valid_workflow_is_described_and_nothing_is_saved() {
    let t = app();
    let out = call(
        &t,
        "validate_flow",
        json!({"workflow_json": legacy_schedule().to_string()}),
    )
    .await;
    assert!(out
        .text()
        .starts_with("<tool_result tool=\"validate_flow\">\n"));
    let (_, text) = block(out.text());
    assert_eq!(
        text,
        r#"{"ok":true,"valid":true,"saved":false,"name":"Legacy","node_count":3,"edge_count":2,"trigger":{"start":{"scheduleType":"daily","time":"09:20","days":[0,1,2,3,4],"marketHoursOnly":true}},"migrations":["priceCondition 'p': threshold -> value"],"next_step":"Nothing was saved. Show the workflow to the user, then call save_flow with a name to import it."}"#
    );
    assert!(rows(&t).is_empty());
    // No name is needed at this stage; one that is not text is passed back.
    let mut unnamed = webhook_entry();
    unnamed.as_object_mut().unwrap().remove("name");
    let p = block(
        call(&t, "validate_flow", json!({"workflow_json": unnamed}))
            .await
            .text(),
    )
    .0;
    assert_eq!(
        (p["valid"].clone(), p["name"].clone()),
        (json!(true), Value::Null)
    );
}

/// V-143, decision 17: validate_flow carries none of the skill check's
/// warnings. A data key nothing reads (the check's first warning) still
/// validates, and the answer is the web's nine keys and nothing about it.
#[tokio::test]
async fn validate_flow_adds_no_warnings() {
    let t = app();
    let mut wf = webhook_entry();
    wf["nodes"][1]["data"]["strikeOffset"] = json!("OTM2");
    wf["nodes"][1]["data"]["message"] = json!("{{nothing.here}}");
    let out = call(&t, "validate_flow", json!({"workflow_json": wf})).await;
    let (p, raw) = block(out.text());
    let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
    let mut expected = [
        "ok",
        "valid",
        "saved",
        "name",
        "node_count",
        "edge_count",
        "trigger",
        "migrations",
        "next_step",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected, "{}", raw);
    assert_eq!(p["valid"], true);
    assert!(!raw.contains("strikeOffset") && !raw.contains("nothing.here"));
}

/// A-09, A-10: strict, always; the validator's own errors in a Retry with
/// the web's sentences.
#[tokio::test]
async fn an_invalid_workflow_comes_back_with_the_validators_errors() {
    let t = app();
    let mut wf = webhook_entry();
    wf["nodes"][1]["data"]
        .as_object_mut()
        .unwrap()
        .remove("quantity");
    let expected = validate_workflow(
        &wf,
        ValidateOptions {
            require_name: false,
            strict: true,
        },
    );
    assert_eq!(expected.len(), 1);
    let out = call(&t, "validate_flow", json!({"workflow_json": wf})).await;
    let text = retry(&out);
    assert!(text.starts_with("The workflow failed Flow's own validator with 1 error(s), so nothing was saved. Each entry names the JSON path, a code and a reason.\n<tool_result tool=\"validate_flow\">\n"), "{}", text);
    assert!(text.ends_with("</tool_result>\nFix exactly those paths and call the tool again. Node types and node data fields are defined in docs/prompt/flow-import-format.md; copy them verbatim and never invent a node type. If a requirement has no matching node type, tell the user in prose instead."), "{}", text);
    let (p, raw) = block(text);
    assert_eq!(
        p,
        json!({"valid": false, "saved": false, "error_count": 1, "errors": serde_json::to_value(&expected).unwrap()})
    );
    assert!(raw.starts_with(r#"{"valid":false,"saved":false,"error_count":1,"errors":[{"path":"/nodes/1/data/quantity","code":"#));
    // A draft without a trigger fails the strict level the import enforces.
    let draft = json!({"nodes": [{"id": "l", "type": "log", "position": {"x": 0, "y": 0}, "data": {}}], "edges": []});
    assert!(!call(&t, "validate_flow", json!({"workflow_json": draft}))
        .await
        .is_ok());
}

/// A-10, A-41, V-142: more than 50 errors report the first 50 and say how
/// many more (`MAX_REPORTED_ERRORS = 50`, flow_gen.py:79); exactly 50 say
/// nothing more. The 50 is written out, so a changed cap fails here.
#[tokio::test]
async fn fifty_errors_are_listed_and_the_rest_counted() {
    assert_eq!(MAX_REPORTED_ERRORS, 50);
    let t = app();
    let bogus = |k: usize| {
        let nodes: Vec<Value> = (0..k)
            .map(|i| json!({"id": format!("n{}", i), "type": "bogus", "position": {"x": 0, "y": 0}, "data": {}}))
            .collect();
        json!({"name": "Many", "nodes": nodes, "edges": []})
    };
    let errors_of = |wf: &Value| {
        validate_workflow(
            wf,
            ValidateOptions {
                require_name: false,
                strict: true,
            },
        )
    };
    // A graph with exactly `n` errors, one more per unknown node.
    let with_errors = |n: usize| {
        (1..200)
            .map(bogus)
            .find(|wf| errors_of(wf).len() == n)
            .unwrap_or_else(|| panic!("no graph with {} errors", n))
    };
    for (total, omitted) in [(60, Some(10)), (51, Some(1)), (50, None)] {
        let wf = with_errors(total);
        let all = errors_of(&wf);
        let out = call(
            &t,
            "validate_flow",
            json!({"workflow_json": wf.to_string()}),
        )
        .await;
        let text = retry(&out);
        let head = match omitted {
            Some(more) => format!(
                "The workflow failed Flow's own validator with {} error(s), so nothing was saved. Each entry names the JSON path, a code and a reason. {} further error(s) are not listed.\n",
                total, more
            ),
            None => format!(
                "The workflow failed Flow's own validator with {} error(s), so nothing was saved. Each entry names the JSON path, a code and a reason.\n",
                total
            ),
        };
        assert!(text.starts_with(&head), "{}", text);
        let (p, _) = block(text);
        assert_eq!(p["error_count"], total);
        assert_eq!(p["errors"].as_array().map(Vec::len), Some(50));
        assert_eq!(p["errors"], serde_json::to_value(&all[..50]).unwrap());
    }
}

// ------------------------------------------------------ argument parsing

/// A-12: anything but text or an object, named as Python names it. The
/// argument guard refuses these first in a run; the tool refuses them too.
#[tokio::test]
async fn a_value_that_is_not_text_or_an_object_is_refused() {
    let t = app();
    for (value, name) in [
        (json!(5), "int"),
        (json!(1.5), "float"),
        (json!(true), "bool"),
        (json!([1]), "list"),
        (Value::Null, "NoneType"),
    ] {
        let out = direct(&t, "validate_flow", json!({"workflow_json": value})).await;
        assert_eq!(
            retry(&out),
            format!("The 'workflow_json' argument is invalid: it is a {}, not a JSON string. Send the workflow object as JSON text, as described in docs/prompt/flow-import-format.md.", name)
        );
        assert!(!call(&t, "validate_flow", json!({"workflow_json": value}))
            .await
            .is_ok());
    }
    let out = direct(&t, "save_flow", json!({"name": "A"})).await;
    assert!(retry(&out).contains("it is a NoneType, not a JSON string."));
}

/// A-13: over 400,000 characters (counted in characters, before the fence
/// is removed) is refused unparsed.
#[tokio::test]
async fn text_over_the_limit_is_refused_by_its_length_in_characters() {
    let t = app();
    let long = "\u{20b9}".repeat(MAX_JSON_CHARS + 1);
    let out = call(&t, "validate_flow", json!({"workflow_json": long})).await;
    assert_eq!(
        retry(&out),
        "The 'workflow_json' argument is invalid: it is 400001 characters, over the 400000 limit. Build a smaller workflow."
    );
    // Exactly at the limit it is read (and here found not to be JSON).
    let at = "x".repeat(MAX_JSON_CHARS);
    let out = call(&t, "validate_flow", json!({"workflow_json": at})).await;
    assert!(retry(&out).starts_with("The 'workflow_json' argument is not valid JSON"));
}

/// A-14: a fence around the whole of it is removed; nothing inside one is
/// empty.
#[tokio::test]
async fn a_fenced_workflow_is_read_and_an_empty_one_refused() {
    let t = app();
    let fenced = format!("```json\n{}\n```", webhook_entry());
    let out = call(&t, "validate_flow", json!({"workflow_json": fenced})).await;
    assert_eq!(block(out.text()).0["valid"], true, "{}", out.text());
    for empty in ["```json\n```", "   \n  "] {
        let out = call(&t, "validate_flow", json!({"workflow_json": empty})).await;
        assert_eq!(
            retry(&out),
            "The 'workflow_json' argument is invalid: it is empty. Send the whole workflow object as JSON text, as described in docs/prompt/flow-import-format.md."
        );
    }
}

/// A-15, lead decision 17: JSON that does not parse is described in our own
/// words, at its line and its column in characters.
#[tokio::test]
async fn json_that_does_not_parse_names_what_and_where() {
    let t = app();
    let tail = " Nothing was validated and nothing was saved. Emit the JSON object alone, with no code fence, no comments and no trailing commas, as docs/prompt/flow-import-format.md requires, and call the tool again.";
    for (text, said) in [
        ("{\"a\": }", "a value was expected at line 1, column 7."),
        (
            "{\"a\": 1,}",
            "a comma comes right before a closing bracket at line 1, column 9.",
        ),
        (
            "{\n  \"\u{20b9}\u{20b9}\": 1 2}",
            "a comma or a closing } was expected at line 2, column 11.",
        ),
        (
            "{\"a\": 1",
            "the text ends before the JSON is complete at line 1, column 7.",
        ),
        (
            "{a: 1}",
            "an object key must be a double-quoted string at line 1, column 2.",
        ),
        (
            "{\"a\": 1} x",
            "more text follows the JSON object at line 1, column 10.",
        ),
        (
            "{\"a\": NaN}",
            "NaN and Infinity are not JSON values; write a number or null at line 1, column 7.",
        ),
        (
            "{\"a\": -Infinity}",
            "NaN and Infinity are not JSON values; write a number or null at line 1, column 8.",
        ),
    ] {
        let out = call(&t, "validate_flow", json!({"workflow_json": text})).await;
        assert_eq!(
            retry(&out),
            format!(
                "The 'workflow_json' argument is not valid JSON: {}{}",
                said, tail
            ),
            "{}",
            text
        );
    }
}

/// A-16: JSON that is not an object, named as Python names it.
#[tokio::test]
async fn json_that_is_not_an_object_is_refused() {
    let t = app();
    for (text, name) in [
        ("[1]", "list"),
        ("\"x\"", "str"),
        ("1", "int"),
        ("1.5", "float"),
        ("true", "bool"),
        ("null", "NoneType"),
    ] {
        let out = direct(&t, "validate_flow", json!({"workflow_json": text})).await;
        assert_eq!(
            retry(&out),
            format!("The 'workflow_json' argument is invalid: it parses to a JSON {}, not an object. The workflow must be {{\"name\": ..., \"nodes\": [...], \"edges\": [...]}}. See docs/prompt/flow-import-format.md.", name),
            "{}",
            text
        );
    }
}

// --------------------------------------------------------------- save_flow

/// A-17, A-18: the name first, trimmed as Python trims, at most 200
/// characters; it overrides any name in the JSON.
#[tokio::test]
async fn the_name_is_checked_first_and_overrides_the_json() {
    let t = app();
    let empty = "The 'name' argument is invalid: it is empty. Pass a short descriptive name such as 'NIFTY opening range breakout'.";
    for name in [json!(""), json!("  \u{1f}\t"), json!(7), Value::Null] {
        let out = direct(
            &t,
            "save_flow",
            json!({"name": name, "workflow_json": "not json"}),
        )
        .await;
        assert_eq!(retry(&out), empty, "{}", name);
    }
    let long = "\u{20b9}".repeat(201);
    let out = direct(
        &t,
        "save_flow",
        json!({"name": long, "workflow_json": "{}"}),
    )
    .await;
    assert_eq!(
        retry(&out),
        "The 'name' argument is invalid: it is 201 characters, over the 200 limit. Shorten it to a title rather than a description."
    );
    let at_limit = format!("  {}\u{1c}", "N".repeat(200));
    let out = call(
        &t,
        "save_flow",
        json!({"name": at_limit, "workflow_json": webhook_entry()}),
    )
    .await;
    let p = block(out.text()).0;
    assert_eq!(p["name"], format!("{} (imported)", "N".repeat(200)));
}

/// A-19, A-23, A-24: one inactive row named "<name> (imported)", the
/// migrated nodes, the description when it is text, the web's answer in
/// its key order, and no run, schedule or webhook started.
#[tokio::test]
async fn a_valid_workflow_is_saved_inactive_under_the_import_name() {
    let t = app();
    let mut wf = legacy_schedule();
    wf["description"] = json!("Morning check");
    let out = call(
        &t,
        "save_flow",
        json!({"name": " Morning SBIN ", "workflow_json": wf.to_string()}),
    )
    .await;
    let saved = rows(&t);
    assert_eq!(saved.len(), 1);
    let w = &saved[0];
    let (_, text) = block(out.text());
    assert_eq!(
        text,
        format!(
            r#"{{"ok":true,"saved":true,"workflow_id":{},"name":"Morning SBIN (imported)","is_active":false,"node_count":3,"edge_count":2,"migrations":["priceCondition 'p': threshold -> value"],"next_step":"The workflow was imported and is INACTIVE, so it is not running. The user opens /flow to review and activate it. This tool cannot activate or execute a workflow."}}"#,
            w.id
        )
    );
    assert_eq!(w.name, "Morning SBIN (imported)");
    assert_eq!(w.description.as_deref(), Some("Morning check"));
    assert!(!w.is_active);
    assert_eq!(w.nodes[1]["data"]["value"], 800);
    assert!(w.nodes[1]["data"].get("threshold").is_none());
    assert_eq!(w.edges, wf["edges"]);
    assert!(w.webhook_token.is_some() && w.webhook_secret.is_some());
    assert!(
        t.ctx.flow.try_claim(w.id).is_some(),
        "no run holds the workflow"
    );
    let conn = t.ctx.sqlite.conn().unwrap();
    let runs: i64 = conn
        .query_row("SELECT COUNT(*) FROM flow_workflow_executions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(runs, 0);
    // A description that is not text is not stored (the web's flow_gen).
    wf["description"] = json!(5);
    call(
        &t,
        "save_flow",
        json!({"name": "Second", "workflow_json": wf}),
    )
    .await;
    let second = rows(&t)
        .into_iter()
        .find(|w| w.name == "Second (imported)")
        .unwrap();
    assert_eq!(second.description, None);
}

/// A-20, A-41: the row is what the /flow import route stores for the same
/// JSON, through the same function.
#[tokio::test]
async fn the_saved_row_is_the_import_routes_row() {
    let t = app();
    let wf = legacy_schedule();
    let mut named = wf.clone();
    named["name"] = json!("Same");
    named["description"] = json!("d");
    let reply = transfer::import(&t.ctx, Body::Json(named.clone())).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    call(
        &t,
        "save_flow",
        json!({"name": "Same", "workflow_json": named}),
    )
    .await;
    let saved = rows(&t);
    assert_eq!(saved.len(), 2);
    let shape = |w: &Workflow| {
        (
            w.name.clone(),
            w.description.clone(),
            w.nodes.to_string(),
            w.edges.to_string(),
            w.is_active,
            w.webhook_enabled,
            w.webhook_auth_type.clone(),
        )
    };
    assert_eq!(shape(&saved[0]), shape(&saved[1]));
}

/// A-41: an invalid workflow is never saved, and the refusal is the
/// validator's, name included.
#[tokio::test]
async fn an_invalid_workflow_writes_nothing() {
    let t = app();
    let mut wf = webhook_entry();
    wf["edges"] = json!([]);
    let out = call(
        &t,
        "save_flow",
        json!({"name": "Broken", "workflow_json": wf}),
    )
    .await;
    let text = retry(&out);
    assert!(text.contains("<tool_result tool=\"save_flow\">"));
    assert!(rows(&t).is_empty());
    assert!(
        audit_rows(&t).is_empty(),
        "no attempt before validation passes"
    );
}

fn audit_rows(t: &TestCtx) -> Vec<Value> {
    use crate::agent::safety::audit::{store, Filter};
    let filter = Filter {
        tool: Some("save_flow".into()),
        limit: 10,
        ..Filter::default()
    };
    let mut rows = store::list(&t.ctx.logs.conn().unwrap(), &filter).unwrap();
    rows.reverse();
    rows
}

/// A-23: a save logs the web's line (`flow_gen.py:236`) at info, naming the
/// stored name and the new id, and nothing of the graph.
#[tokio::test]
async fn a_save_logs_the_webs_line() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app();
    call(
        &t,
        "save_flow",
        json!({"name": "Logged", "workflow_json": webhook_entry()}),
    )
    .await;
    let id = rows(&t)[0].id;
    let text = logs.text();
    let line = text
        .lines()
        .find(|l| l.contains("Agent imported Flow workflow"))
        .unwrap_or_else(|| panic!("no import line in:\n{}", text));
    assert!(
        line.ends_with(&format!(
            "Agent imported Flow workflow Logged (imported) as id {}",
            id
        )),
        "{}",
        line
    );
    assert!(line.contains(" INFO "), "{}", line);
    assert!(
        !text.contains("webhook.symbol"),
        "the graph is never logged"
    );
}

/// A-21: the attempt row before the write, the result row after, the stored
/// name and the graph's size, never the graph.
#[tokio::test]
async fn a_save_writes_its_attempt_and_result_to_the_audit_trail() {
    let t = app();
    call(
        &t,
        "save_flow",
        json!({"name": "Trail", "workflow_json": webhook_entry()}),
    )
    .await;
    let id = rows(&t)[0].id;
    let audit = audit_rows(&t);
    let shape: Vec<(Value, Value, Value)> = audit
        .iter()
        .map(|r| (r["phase"].clone(), r["ok"].clone(), r["response"].clone()))
        .collect();
    assert_eq!(
        shape,
        [
            (json!("attempt"), Value::Null, Value::Null),
            (
                json!("result"),
                json!(true),
                json!({"workflow_id": id, "name": "Trail (imported)"})
            ),
        ]
    );
    assert_eq!(
        audit[0]["args"],
        json!({"name": "Trail (imported)", "node_count": 2, "edge_count": 1})
    );
    assert_eq!(audit[1]["attempt_id"], audit[0]["id"]);
    assert!(!Value::Array(audit).to_string().contains("webhook.symbol"));
}

/// A-22: a store that cannot write answers the platform failure, not a
/// Retry, and the trail records it.
#[tokio::test]
async fn a_store_failure_is_a_platform_failure_and_is_recorded() {
    let t = app();
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("DROP TABLE flow_workflow_executions; DROP TABLE flow_workflows;")
        .unwrap();
    let out = call(
        &t,
        "save_flow",
        json!({"name": "Lost", "workflow_json": webhook_entry()}),
    )
    .await;
    assert!(out.is_ok());
    let (_, text) = block(out.text());
    assert_eq!(
        text,
        r#"{"ok":false,"saved":false,"error":"The workflow passed validation but could not be written to the database. This is a platform failure, not a problem with the JSON. Report it to the user rather than calling the tool again."}"#
    );
    let audit = audit_rows(&t);
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[1]["ok"], false);
    assert_eq!(
        audit[1]["response"],
        json!({"status": "error", "message": "create failed"})
    );
}

// ------------------------------------------------------- the whole turn

const ONLY_FLOW: &[ToolGroup] = &[GROUP];

/// A-02, A-25, A-41: validate_flow answers within the turn; save_flow
/// pauses for the trader, writes nothing before the approval, and its card
/// carries the workflow's outline whole, so the card counts the nodes and
/// connections of a workflow far longer than the 2,000 characters the
/// shared bounding keeps.
#[tokio::test]
async fn saving_pauses_for_approval_with_a_card_that_counts_the_workflow() {
    let mut wf = webhook_entry();
    for i in 0..30 {
        wf["nodes"].as_array_mut().unwrap().push(json!({
            "id": format!("log_{}", i), "type": "log", "position": {"x": i, "y": 300},
            "data": {"message": format!("leg {} placed {{{{orderResult.orderid}}}}", i)}
        }));
        wf["edges"].as_array_mut().unwrap().push(json!({
            "id": format!("el_{}", i), "source": "node_2", "target": format!("log_{}", i)
        }));
    }
    let text = wf.to_string();
    assert!(text.len() > 4000);
    let check = json!({"workflow_json": text}).to_string();
    let save = json!({"name": "Big entry", "workflow_json": text}).to_string();
    let h = harness(vec![
        Reply::Calls(vec![("c1", "validate_flow", check)]),
        Reply::Calls(vec![("c2", "save_flow", save)]),
        Reply::Text(vec!["Saved."]),
    ])
    .await;
    h.ctx().agent.set_toolkits(ONLY_FLOW);
    let (_, f, raw) = h
        .chat(
            "/agent/api/chat/stream",
            json!({"message": "build my entry"}),
        )
        .await;
    let kinds = types(&f);
    assert!(kinds.contains(&"tool_end".to_string()), "{}", raw);
    assert_eq!(kinds.last().map(String::as_str), Some("confirm"), "{}", raw);
    let confirm = f.last().unwrap();
    assert!(workflows::list(&h.ctx().sqlite.conn().unwrap())
        .unwrap()
        .is_empty());
    let req = &confirm["requirements"][0];
    assert_eq!(req["tool_name"], "save_flow");
    assert_eq!(req["args"]["name"], "Big entry");
    let card = &req["args"]["workflow_json"];
    assert_eq!(card["nodes"].as_array().map(Vec::len), Some(32));
    assert_eq!(card["edges"].as_array().map(Vec::len), Some(31));
    assert_eq!(
        card["nodes"][1],
        json!({"id": "node_2", "type": "placeOrder"})
    );
    assert_eq!(
        card["edges"][0],
        json!({"source": "node_1", "target": "node_2"})
    );
    let body = json!({
        "run_id": confirm["run_id"], "session_id": confirm["session_id"],
        "conversation_id": f[0]["conversation_id"], "decisions": {"c2": true}
    });
    let (s, _, raw) = h.chat("/agent/api/chat/confirm", body).await;
    assert_eq!(s, StatusCode::OK, "{}", raw);
    let stored = workflows::list(&h.ctx().sqlite.conn().unwrap()).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, "Big entry (imported)");
    assert!(!stored[0].is_active);
}

/// The card's outline: an object argument is outlined too; text that is
/// not a workflow is left as the shared bounding leaves it, so the card
/// says it cannot read it; validate_flow's card is never asked for.
#[tokio::test]
async fn the_card_outlines_a_workflow_and_leaves_anything_else() {
    let t = app();
    let save = tool(&t, "save_flow");
    let card = save.card_args(
        &json!({"name": " A\u{1f} ", "workflow_json": webhook_entry()})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(card["name"], "A", "the name as it is stored");
    assert_eq!(
        card["workflow_json"]["nodes"].as_array().map(Vec::len),
        Some(2)
    );
    let fenced = format!("```json\n{}\n```", webhook_entry());
    let card = save.card_args(
        &json!({"workflow_json": fenced})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert_eq!(
        card["workflow_json"]["edges"].as_array().map(Vec::len),
        Some(1)
    );
    let broken = "{".repeat(3000);
    let card = save.card_args(
        &json!({"workflow_json": broken})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(card["workflow_json"]
        .as_str()
        .unwrap()
        .contains("[truncated, 3000 characters]"));
    let validate = tool(&t, "validate_flow");
    let card = validate.card_args(
        &json!({"workflow_json": webhook_entry().to_string()})
            .as_object()
            .unwrap()
            .clone(),
    );
    assert!(card["workflow_json"].is_string());
}
