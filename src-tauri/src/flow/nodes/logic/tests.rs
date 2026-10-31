//! Log and Variable, ported from the web's
//! `test/test_flow_qa_regressions.py` (1395-1486) and parity N-log-*,
//! N-variable-*.

use super::*;
use crate::flow::nodes::testkit::{answer, ctx, logs, messages, run};
use crate::flow::nodes::NodeRegistry;
use serde_json::json;

#[tokio::test]
async fn log_writes_the_interpolated_message_at_its_level() {
    let (_, c) = ctx();
    c.vars.set("sym", json!("SBIN"));
    let out = run(
        &Log,
        &c,
        json!({"message": "Bought {{sym}} {{missing}}", "level": "warn"}),
    )
    .await;
    assert_eq!(
        answer(&out),
        json!({"status": "success", "message": "Bought SBIN {{missing}}"})
    );
    assert_eq!(
        logs(&c),
        vec![(
            "warn".to_string(),
            "[LOG] Bought SBIN {{missing}}".to_string()
        )]
    );
    // An error-level line is only a line: it does not fail the run.
    let out = run(&Log, &c, json!({"message": "bad", "level": "error"})).await;
    assert!(!out.is_error());
    let out = run(&Log, &c, json!({})).await;
    assert_eq!(answer(&out)["message"], "");
    assert_eq!(logs(&c).last().unwrap().0, "info");
}

async fn variable(c: &NodeCtx, d: Value) -> Value {
    answer(&run(&Variable, c, d).await)
}

/// X-209: every operation's stored value and answer.
#[tokio::test]
async fn variable_operations_store_what_the_web_stores() {
    let cases = [
        (
            json!({"operation": "set", "value": "{\"ok\": true}"}),
            json!({"ok": true}),
        ),
        (
            json!({"operation": "get", "sourceVariable": "portfolio", "jsonPath": "orders[1].price"}),
            json!(102.5),
        ),
        (json!({"operation": "add", "value": "3"}), json!(5.0)),
        (json!({"operation": "subtract", "value": "4"}), json!(5.0)),
        (json!({"operation": "multiply", "value": "2.5"}), json!(7.5)),
        (json!({"operation": "divide", "value": "2"}), json!(4.5)),
        (json!({"operation": "increment"}), json!(4.0)),
        (json!({"operation": "decrement"}), json!(2.0)),
        (
            json!({"operation": "parse_json", "value": "{\"ok\": true}"}),
            json!({"ok": true}),
        ),
        (
            json!({"operation": "stringify", "sourceVariable": "payload"}),
            json!("{\"ok\": true}"),
        ),
    ];
    let starts = [
        json!(0),
        json!(0),
        json!(2),
        json!(9),
        json!(3),
        json!(9),
        json!(3),
        json!(3),
        json!(0),
        json!(0),
    ];
    for ((mut d, want), start) in cases.into_iter().zip(starts) {
        let (_, c) = ctx();
        c.vars.set("target", start);
        c.vars.set(
            "portfolio",
            json!({"orders": [{"price": 101.0}, {"price": 102.5}]}),
        );
        c.vars.set("payload", json!({"ok": true}));
        d["variableName"] = json!("target");
        let got = variable(&c, d.clone()).await;
        assert_eq!(
            got,
            json!({"status": "success", "variable": "target", "value": want}),
            "{}",
            d
        );
        assert_eq!(c.vars.get("target"), Some(want));
    }
    let (_, c) = ctx();
    let got = variable(
        &c,
        json!({"variableName": "target", "operation": "append", "value": "done"}),
    )
    .await;
    assert_eq!(got["value"], "done");
}

/// X-210: a stored null is a value, not a missing variable.
#[tokio::test]
async fn get_keeps_a_stored_null() {
    let (_, c) = ctx();
    c.vars.set("source", Value::Null);
    let got = variable(
        &c,
        json!({"variableName": "target", "operation": "get", "sourceVariable": "source"}),
    )
    .await;
    assert_eq!(
        got,
        json!({"status": "success", "variable": "target", "value": null})
    );
}

/// X-211, X-212: a failed operation leaves the target as it was.
#[tokio::test]
async fn a_failed_operation_leaves_the_target_unchanged() {
    let cases = [
        json!({"operation": "unknown"}),
        json!({"operation": "get"}),
        json!({"operation": "get", "sourceVariable": "portfolio", "jsonPath": "orders[1].price"}),
        json!({"operation": "parse_json", "value": "not json"}),
        json!({"operation": "add", "value": "x"}),
    ];
    for mut d in cases {
        let (_, c) = ctx();
        c.vars.set("target", json!("keep"));
        c.vars.set("portfolio", json!({"orders": [{"price": 1}]}));
        d["variableName"] = json!("target");
        let out = run(&Variable, &c, d.clone()).await;
        assert!(out.is_error(), "{}", d);
        assert_eq!(c.vars.get("target"), Some(json!("keep")), "{}", d);
    }
    let (_, c) = ctx();
    c.vars.set("target", json!("text"));
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "target", "operation": "add", "value": "1"}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'add' failed: could not convert string to float: 'text'"
    );
    let (_, c) = ctx();
    c.vars.set("target", json!(9));
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "target", "operation": "divide", "value": 0}),
    )
    .await;
    assert!(out
        .failure_message()
        .to_lowercase()
        .contains("divide by zero"));
    assert_eq!(c.vars.get("target"), Some(json!(9)));
}

#[tokio::test]
async fn messages_use_python_reprs() {
    let (_, c) = ctx();
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "x", "operation": null}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "Variable operation None failed: Unknown variable operation: None"
    );
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "x", "operation": "parse_json", "value": 5}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'parse_json' failed: the JSON object must be str, bytes or bytearray, not int"
    );
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "x", "operation": "parse_json", "value": "nope"}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'parse_json' failed: Expecting value: line 1 column 1 (char 0)"
    );
    let out = run(
        &Variable,
        &c,
        json!({"variableName": "x", "operation": "get", "sourceVariable": "nope"}),
    )
    .await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'get' failed: 'Source variable not found: nope'"
    );
    // A bad set text stays text; a list set from text is a list.
    variable(&c, json!({"variableName": "t", "value": "{oops"})).await;
    assert_eq!(c.vars.get("t"), Some(json!("{oops")));
    variable(&c, json!({"variableName": "t", "value": "[1, 2.5, \"x\"]"})).await;
    assert_eq!(c.vars.get("t"), Some(json!([1, 2.5, "x"])));
    assert_eq!(
        messages(&c).last().unwrap(),
        "Set variable t = [1, 2.5, 'x']"
    );
    // An infinite sum is kept as Python writes it.
    c.vars.set("big", json!(1e308));
    variable(
        &c,
        json!({"variableName": "big", "operation": "multiply", "value": "10"}),
    )
    .await;
    assert_eq!(c.vars.get("big"), Some(json!("inf")));
}

#[test]
fn json_paths_refuse_what_they_cannot_walk() {
    let v = json!({"a": [{"b": 1}]});
    assert_eq!(walk_json_path(v.clone(), "a[0].b"), Ok(json!(1)));
    assert_eq!(walk_json_path(v.clone(), " a[0] "), Ok(json!({"b": 1})));
    assert_eq!(
        walk_json_path(v.clone(), "a[x]"),
        Err("Invalid jsonPath index: x".into())
    );
    assert_eq!(
        walk_json_path(v.clone(), "a[-1]"),
        Err("Invalid jsonPath index: -1".into())
    );
    assert_eq!(
        walk_json_path(v.clone(), "a.b"),
        Err("jsonPath key requires an object".into())
    );
    assert_eq!(
        walk_json_path(v.clone(), "[0]"),
        Err("jsonPath index requires a list or tuple".into())
    );
    assert_eq!(
        walk_json_path(v.clone(), "a[3]"),
        Err("list index out of range".into())
    );
    assert_eq!(walk_json_path(v.clone(), "c"), Err("'c'".into()));
    assert_eq!(
        walk_json_path(v.clone(), "a..b"),
        Err("Invalid jsonPath: a..b".into())
    );
    assert_eq!(
        walk_json_path(v.clone(), "a[0"),
        Err("Invalid jsonPath: a[0".into())
    );
    assert_eq!(walk_json_path(v, "a]"), Err("Invalid jsonPath: a]".into()));
}

/// N-*-00: every node of the family is registered under the editor's type id.
#[test]
fn the_family_registers_every_logic_node() {
    let reg = NodeRegistry::default();
    for t in [
        "httpRequest",
        "positionCheck",
        "fundCheck",
        "timeWindow",
        "timeCondition",
        "priceCondition",
        "varCondition",
        "telegramAlert",
        "whatsappAlert",
        "delay",
        "waitUntil",
        "variable",
        "mathExpression",
        "log",
    ] {
        assert!(reg.get(t).is_some(), "{} is not registered", t);
    }
}

/// Nesting a webhook can send (100,000 levels) fails the node with
/// Python's sentence and never overflows a tokio worker's 2 MiB stack in a
/// debug build: the JSON decoder keeps no recursion, the evaluator stops
/// long before its frames add up.
#[test]
fn deep_input_fails_the_node_on_a_worker_sized_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(deep_inputs());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn deep_inputs() {
    use crate::flow::nodes::math::MathExpression;
    let (_, c) = ctx();
    for (text, kind) in [
        ("[".repeat(100_000), "array"),
        ("{\"a\": ".repeat(100_000), "object"),
    ] {
        for op in ["parse_json", "set"] {
            let out = run(
                &Variable,
                &c,
                json!({"variableName": "x", "operation": op, "value": text}),
            )
            .await;
            if op == "set" {
                // Text that is not JSON is kept as text.
                assert_eq!(c.vars.get("x"), Some(json!(text)));
                continue;
            }
            assert_eq!(
                out.failure_message(),
                format!("Variable operation 'parse_json' failed: maximum recursion depth exceeded while decoding a JSON {} from a unicode string", kind)
            );
        }
    }
    let unary = format!("{}1", "-".repeat(100_000));
    let out = run(&MathExpression, &c, json!({"expression": unary})).await;
    assert_eq!(out.failure_message(), "maximum recursion depth exceeded");
    let sum = vec!["1"; 100_000].join("+");
    let out = run(&MathExpression, &c, json!({"expression": sum})).await;
    assert_eq!(out.failure_message(), "maximum recursion depth exceeded");
    let parens = "(".repeat(100_000);
    let out = run(&MathExpression, &c, json!({"expression": parens})).await;
    assert!(out
        .failure_message()
        .starts_with("Invalid expression syntax: "));
}

/// N-logic-01: a failed result holding both keys reports its `message`
/// (the walk's `message or error or "node failed"`).
#[test]
fn a_failure_reads_message_before_error() {
    let both = NodeOutcome::result(json!({"status": "error", "message": "m", "error": "e"}));
    assert_eq!(both.failure_message(), "m");
    assert_eq!(both.failure_value(), json!("m"));
    let blank = NodeOutcome::result(json!({"status": "error", "message": "", "error": "e"}));
    assert_eq!(blank.failure_message(), "e");
    let neither = NodeOutcome::result(json!({"status": "error"}));
    assert_eq!(neither.failure_message(), "node failed");
}

/// N-log-02: an `error` line is mirrored to the app log as a warning
/// (decision 29), any other level at debug.
#[tokio::test]
async fn an_error_line_reaches_the_app_log_as_a_warning() {
    let captured = crate::services::tunnel::fake::capture_logs();
    let (_, c) = ctx();
    run(&Log, &c, json!({"message": "Stop {{x}}", "level": "error"})).await;
    run(&Log, &c, json!({"message": "Note", "level": "info"})).await;
    let text = captured.text();
    let line = |m: &str| {
        text.lines()
            .find(|l| l.contains(m))
            .unwrap_or_default()
            .to_string()
    };
    assert!(
        line("Flow: [LOG] Stop {{x}}").contains(" WARN "),
        "{}",
        text
    );
    assert!(line("Flow: [LOG] Note").contains("DEBUG"), "{}", text);
}

/// N-variable-01: variableName is the name as written (not stripped, not
/// interpolated; the legacy `name` when it is absent), a text value is
/// interpolated once and an object kept raw, and sourceVariable and
/// jsonPath are never interpolated.
#[tokio::test]
async fn variable_names_and_sources_are_read_raw() {
    let (_, c) = ctx();
    c.vars.set("who", json!("x"));
    c.vars.set("src", json!("portfolio"));
    c.vars.set("p", json!("orders"));
    c.vars.set("portfolio", json!({"orders": [1]}));
    variable(&c, json!({"variableName": " spaced ", "value": 1})).await;
    assert_eq!(c.vars.get(" spaced "), Some(json!(1)));
    assert!(!c.vars.contains("spaced"));
    variable(&c, json!({"variableName": "{{who}}", "value": "{{who}}"})).await;
    assert_eq!(c.vars.get("{{who}}"), Some(json!("x")));
    assert!(!c.vars.contains("x"));
    variable(&c, json!({"name": "legacy", "value": {"a": "{{who}}"}})).await;
    assert_eq!(c.vars.get("legacy"), Some(json!({"a": "{{who}}"})));
    let get = |d: Value| {
        let mut d = d;
        d["variableName"] = json!("t");
        d["operation"] = json!("get");
        d
    };
    let out = run(&Variable, &c, get(json!({"sourceVariable": "{{src}}"}))).await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'get' failed: 'Source variable not found: {{src}}'"
    );
    let path = json!({"sourceVariable": "portfolio", "jsonPath": "{{p}}"});
    let out = run(&Variable, &c, get(path)).await;
    assert_eq!(
        out.failure_message(),
        "Variable operation 'get' failed: '{{p}}'"
    );
}

/// N-variable-03: a blank or non-text sourceVariable is required, even
/// when a variable of that text exists.
#[tokio::test]
async fn a_blank_or_non_text_source_is_required() {
    for op in ["get", "stringify"] {
        for source in [json!("  "), json!(5), Value::Null] {
            let (_, c) = ctx();
            c.vars.set("5", json!(1));
            let d = json!({"variableName": "t", "operation": op, "sourceVariable": source});
            let out = run(&Variable, &c, d.clone()).await;
            assert_eq!(
                out.failure_message(),
                format!(
                    "Variable operation '{}' failed: sourceVariable is required",
                    op
                ),
                "{}",
                d
            );
        }
    }
}

/// N-variable-04: after `]` only `.` or `[` may follow.
#[test]
fn a_bracket_is_followed_by_a_dot_or_a_bracket() {
    let v = json!({"a": [[{"b": 1}]]});
    assert_eq!(walk_json_path(v.clone(), "a[0][0].b"), Ok(json!(1)));
    assert_eq!(
        walk_json_path(v, "a[0]b"),
        Err("Invalid jsonPath: a[0]b".into())
    );
}

/// N-variable-08: stringify dumps the whole source; a jsonPath is not read.
#[tokio::test]
async fn stringify_ignores_a_json_path() {
    let (_, c) = ctx();
    c.vars.set("payload", json!({"ok": true}));
    let got = variable(
        &c,
        json!({"variableName": "t", "operation": "stringify",
               "sourceVariable": "payload", "jsonPath": "ok"}),
    )
    .await;
    assert_eq!(got["value"], "{\"ok\": true}");
}

/// N-variable-09: append writes Python's str() of the current value, and
/// of a non-text value for a new name.
#[tokio::test]
async fn append_writes_python_text() {
    for (current, want) in [
        (json!(5.0), "5.0x"),
        (json!(true), "Truex"),
        (Value::Null, "Nonex"),
        (json!({"a": 1}), "{'a': 1}x"),
        (json!([1, 2]), "[1, 2]x"),
    ] {
        let (_, c) = ctx();
        c.vars.set("t", current.clone());
        let d = json!({"variableName": "t", "operation": "append", "value": "x"});
        assert_eq!(variable(&c, d).await["value"], want, "{}", current);
    }
    for (value, want) in [
        (json!(5), "5"),
        (json!(2.5), "2.5"),
        (json!(true), "True"),
        (Value::Null, "None"),
        (json!({"a": 1}), "{'a': 1}"),
    ] {
        let (_, c) = ctx();
        let d = json!({"variableName": "fresh", "operation": "append", "value": value});
        assert_eq!(variable(&c, d.clone()).await["value"], want, "{}", d);
    }
}
