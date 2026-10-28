//! `list_openscripts` and `save_openscript` (the web's
//! `test/test_agent_openscript_tool.py`) against the desktop's OpenScript
//! store, the one the /trading editor's route writes through, in the app's
//! temporary data folder.

use super::GROUP;
use crate::agent::chat::tests::{harness, types};
use crate::agent::contract::tool::{Capabilities, Surface, ToolGroup, ToolOutcome};
use crate::agent::testing::Reply;
use crate::agent::tools::testing::ctx;
use crate::state::testing::{build, TestCtx};
use crate::trading::scripts::{source_hash, MAX_SOURCE_BYTES};
use axum::http::StatusCode;
use serde_json::{json, Value};

const SOURCE: &str = "version 1\nstudy(\"Range\", overlay = true)\nplot(close, \"C\", aqua)\n";

fn app() -> TestCtx {
    build(crate::brokers::BrokerRegistry::new(), chrono::Utc::now())
}

async fn call(t: &TestCtx, tool: &str, args: Value) -> ToolOutcome {
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    let tools = (GROUP.build)(&c);
    let found = tools.iter().find(|x| x.spec().name == tool).unwrap();
    found
        .call(&c, args.as_object().cloned().unwrap_or_default())
        .await
}

fn payload(out: &ToolOutcome) -> Value {
    let lines: Vec<&str> = out.text().trim().lines().collect();
    assert!(lines[0].starts_with("<tool_result tool="), "{}", out.text());
    serde_json::from_str(&lines[1..lines.len() - 1].join("\n")).unwrap()
}

fn dir(t: &TestCtx) -> std::path::PathBuf {
    t.ctx.trading.scripts.dir().to_path_buf()
}

/// What the editor's route stores: a source and the program compiled
/// from it.
fn route_save(t: &TestCtx, name: &str, source: &str) {
    let program = json!({"format": 1, "source": {"hash": source_hash(source)}}).to_string();
    t.ctx
        .trading
        .scripts
        .save(
            name,
            source,
            Some(&program),
            crate::trading::scripts::Replace::ANY,
        )
        .unwrap();
}

#[tokio::test]
async fn the_route_and_the_tool_both_write_through_the_store() {
    let h = harness(vec![]).await;
    let (s, _) = h
        .json(
            "POST",
            "/openscript/a.oscript",
            Some(json!({"source": SOURCE})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let c = ctx(h.ctx().clone(), Surface::Chat, Capabilities::default());
    let tools = (GROUP.build)(&c);
    let save = tools
        .iter()
        .find(|t| t.spec().name == "save_openscript")
        .unwrap();
    assert!(save
        .call(
            &c,
            json!({"filename": "b", "source": SOURCE})
                .as_object()
                .cloned()
                .unwrap()
        )
        .await
        .is_ok());
    assert_eq!(
        *h.ctx().trading.scripts.saves.lock(),
        [
            ("a.oscript".to_string(), false),
            ("b.oscript".to_string(), false)
        ]
    );
    // The route lists what the tool saved.
    let (_, index) = h.json("GET", "/openscript/index.json", None).await;
    let b = index
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["file"] == "b.oscript")
        .unwrap();
    assert_eq!(b["program"], false);
}

#[tokio::test]
async fn a_tool_save_never_replaces_a_script_the_route_compiled() {
    for replace in [false, true] {
        let t = app();
        route_save(&t, "range.oscript", SOURCE);
        let out = call(&t, "save_openscript", json!({"filename": "range.oscript", "source": "version 1\n// edited", "replace": replace})).await;
        let ToolOutcome::Retry(m) = &out else {
            panic!("{:?}", out)
        };
        assert!(
            m.contains("compiled") && m.contains("/trading") && m.contains("new name"),
            "{}",
            m
        );
        assert!(t.ctx.trading.scripts.has_program("range.oscript"));
        assert_eq!(
            t.ctx.trading.scripts.source("range.oscript").unwrap(),
            SOURCE.as_bytes()
        );
        assert!(!dir(&t).join("range.oscript.bak").exists());
    }
}

#[tokio::test]
async fn a_new_script_is_written_as_source_and_says_it_is_not_compiled() {
    let t = app();
    let out = call(
        &t,
        "save_openscript",
        json!({"filename": "ema_cross", "source": SOURCE}),
    )
    .await;
    let p = payload(&out);
    assert_eq!(
        (
            p["ok"].clone(),
            p["saved"].clone(),
            p["file"].clone(),
            p["compiled"].clone(),
            p["replaced"].clone(),
            p["backup"].clone(),
            p["lines"].clone()
        ),
        (
            json!(true),
            json!(true),
            json!("ema_cross.oscript"),
            json!(false),
            json!(false),
            Value::Null,
            json!(3)
        )
    );
    let next = p["next_step"].as_str().unwrap();
    assert!(next.contains("/trading") && next.contains("ema_cross.oscript"));
    assert!(out
        .text()
        .contains(r#"{"ok":true,"saved":true,"file":"ema_cross.oscript","bytes":"#));
    assert_eq!(
        std::fs::read(dir(&t).join("ema_cross.oscript")).unwrap(),
        SOURCE.as_bytes()
    );
    assert!(!t.ctx.trading.scripts.has_program("ema_cross.oscript"));
}

#[tokio::test]
async fn a_taken_name_is_refused_without_replace_and_replaced_with_it() {
    let t = app();
    call(
        &t,
        "save_openscript",
        json!({"filename": "taken", "source": SOURCE}),
    )
    .await;
    let out = call(
        &t,
        "save_openscript",
        json!({"filename": "taken", "source": "version 1\n// edited"}),
    )
    .await;
    assert!(
        matches!(&out, ToolOutcome::Retry(m) if m.contains("already exists") && m.contains("replace=true")),
        "{:?}",
        out
    );
    assert_eq!(
        t.ctx.trading.scripts.source("taken.oscript").unwrap(),
        SOURCE.as_bytes()
    );
    assert!(!dir(&t).join("taken.oscript.bak").exists());
    let p = payload(
        &call(
            &t,
            "save_openscript",
            json!({"filename": "taken", "source": "edited", "replace": true}),
        )
        .await,
    );
    assert_eq!(
        (p["replaced"].clone(), p["backup"].clone()),
        (json!(true), json!("taken.oscript.bak"))
    );
    assert_eq!(
        std::fs::read_to_string(dir(&t).join("taken.oscript.bak")).unwrap(),
        SOURCE
    );
    let fresh = payload(
        &call(
            &t,
            "save_openscript",
            json!({"filename": "fresh", "source": SOURCE, "replace": true}),
        )
        .await,
    );
    assert_eq!(fresh["replaced"], false);
}

#[test]
fn the_schema_tells_the_model_about_replace() {
    let t = app();
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    let tools = (GROUP.build)(&c);
    let d = &tools[1].spec().description;
    assert!(d.contains("replace=true") && d.contains("compiled") && d.contains("new name"));
}

#[tokio::test]
async fn a_fence_around_the_whole_source_is_removed() {
    let t = app();
    call(
        &t,
        "save_openscript",
        json!({"filename": "fenced", "source": format!("```openscript\n{}```", SOURCE)}),
    )
    .await;
    assert_eq!(
        t.ctx.trading.scripts.source("fenced.oscript").unwrap(),
        SOURCE.trim().as_bytes()
    );
}

#[tokio::test]
async fn a_name_the_store_would_refuse_is_refused_here() {
    let t = app();
    for name in [
        "",
        "   ",
        "../escape",
        "a/b",
        ".hidden",
        &"x".repeat(70),
        "bad name",
        "C:evil",
    ] {
        let out = call(
            &t,
            "save_openscript",
            json!({"filename": name, "source": SOURCE}),
        )
        .await;
        assert!(
            matches!(&out, ToolOutcome::Retry(m) if m.contains("'filename'")),
            "{:?}: {:?}",
            name,
            out
        );
    }
    assert!(t.ctx.trading.scripts.index().is_empty());
    let out = call(
        &t,
        "save_openscript",
        json!({"filename": "bad name", "source": SOURCE}),
    )
    .await;
    assert_eq!(out.text(), "The 'filename' argument is invalid: 'bad name' is not a valid script name. Use letters, digits, dot, dash or underscore, starting with a letter or digit, at most 64 characters before .oscript, for example 'ema_cross'.");
}

#[tokio::test]
async fn an_empty_or_oversized_source_is_refused() {
    let t = app();
    let out = call(
        &t,
        "save_openscript",
        json!({"filename": "e", "source": "  \n "}),
    )
    .await;
    assert_eq!(
        out.text(),
        "The 'source' argument is invalid: it is empty. Pass the complete OpenScript source."
    );
    let big = "a".repeat(MAX_SOURCE_BYTES + 1);
    let out = call(
        &t,
        "save_openscript",
        json!({"filename": "e", "source": big}),
    )
    .await;
    assert!(matches!(&out, ToolOutcome::Retry(m) if m.contains("262144")));
    let out = call(&t, "save_openscript", json!({"filename": "e", "source": 5})).await;
    assert_eq!(out.text(), "The 'source' argument is invalid: it is a int, not a string. Pass the complete OpenScript source as one string.");
}

#[tokio::test]
async fn a_write_failure_is_reported_in_plain_words() {
    let t = app();
    // The scripts folder cannot be made: a file stands where it would go.
    std::fs::write(dir(&t), "not a folder").unwrap();
    let p = payload(
        &call(
            &t,
            "save_openscript",
            json!({"filename": "x", "source": SOURCE}),
        )
        .await,
    );
    assert_eq!(
        (p["ok"].clone(), p["saved"].clone()),
        (json!(false), json!(false))
    );
    let error = p["error"].as_str().unwrap();
    assert!(error.starts_with("x.oscript could not be written to the scripts folder."));
    for word in ["Errno", "PermissionError", "os error"] {
        assert!(!error.contains(word));
    }
    let listed = payload(&call(&t, "list_openscripts", json!({})).await);
    assert_eq!(
        listed,
        json!({"ok": false, "error": "The OpenScript folder could not be read. This is a problem on the server; tell the operator rather than calling again."})
    );
}

#[test]
fn saving_pauses_for_approval_and_listing_does_not() {
    let t = app();
    let c = ctx(t.ctx.clone(), Surface::Chat, Capabilities::default());
    let approval: Vec<(&str, bool)> = (GROUP.build)(&c)
        .iter()
        .map(|x| (x.spec().name, x.spec().needs_approval))
        .collect();
    assert_eq!(
        approval,
        [("list_openscripts", false), ("save_openscript", true)]
    );
}

#[tokio::test]
async fn it_lists_names_and_whether_each_is_compiled() {
    let t = app();
    assert_eq!(
        payload(&call(&t, "list_openscripts", json!({})).await)["scripts"],
        json!([])
    );
    route_save(&t, "built.oscript", SOURCE);
    call(
        &t,
        "save_openscript",
        json!({"filename": "draft", "source": SOURCE}),
    )
    .await;
    let p = payload(&call(&t, "list_openscripts", json!({})).await);
    let compiled: Vec<(String, bool)> = p["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["file"].as_str().unwrap().to_string(),
                s["compiled"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        compiled,
        [
            ("built.oscript".into(), true),
            ("draft.oscript".into(), false)
        ]
    );
    assert!(p["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["modified"].as_str().unwrap().ends_with("+05:30")));
    assert_eq!(
        (
            p["count"].clone(),
            p["total_on_disk"].clone(),
            p["truncated"].clone()
        ),
        (json!(2), json!(2), json!(false))
    );
}

#[test]
fn offered_on_chat_and_chart_not_voice() {
    for group in [&GROUP, &crate::agent::tools::skills::GROUP] {
        assert!(group.surfaces.contains(Surface::Chat) && group.surfaces.contains(Surface::Chart));
        assert!(!group.surfaces.contains(Surface::Voice), "{}", group.key);
        assert!(group.requires.is_empty(), "offered with trading off");
    }
}

#[test]
fn the_store_is_the_apps_openscript_folder() {
    let t = app();
    assert_eq!(dir(&t), t.ctx.data_dir.join("openscript"));
}

const ONLY_SCRIPTS: &[ToolGroup] = &[GROUP];

/// The whole path: the model asks to save, the turn pauses for the
/// trader's approval, nothing is written until it is given, and the
/// approved save lands in the store the editor reads.
#[tokio::test]
async fn an_approved_save_lands_in_the_store_and_nothing_before() {
    let args = json!({"filename": "approved", "source": SOURCE}).to_string();
    let h = harness(vec![
        Reply::Calls(vec![("c1", "save_openscript", args)]),
        Reply::Text(vec!["Saved."]),
    ])
    .await;
    h.ctx().agent.set_toolkits(ONLY_SCRIPTS);
    let (_, f, raw) = h
        .chat(
            "/agent/api/chat/stream",
            json!({"message": "save my range study"}),
        )
        .await;
    assert_eq!(types(&f), ["start", "usage", "confirm"], "{}", raw);
    assert!(
        !h.ctx().trading.scripts.has_source("approved.oscript"),
        "nothing is written before the approval"
    );
    let confirm = &f[2];
    assert_eq!(confirm["requirements"][0]["tool_name"], "save_openscript");
    let body = json!({
        "run_id": confirm["run_id"], "session_id": confirm["session_id"],
        "conversation_id": f[0]["conversation_id"], "decisions": {"c1": true}
    });
    let (s, f, raw) = h.chat("/agent/api/chat/confirm", body).await;
    assert_eq!(s, StatusCode::OK, "{}", raw);
    assert!(types(&f).contains(&"tool_end".to_string()), "{}", raw);
    assert_eq!(
        h.ctx().trading.scripts.source("approved.oscript").unwrap(),
        SOURCE.as_bytes()
    );
    let sent = h.fake.sent();
    let results = serde_json::to_string(&sent.last().unwrap()["messages"]).unwrap();
    assert!(
        results.contains("compiled") && results.contains("approved.oscript"),
        "{}",
        results
    );
}
