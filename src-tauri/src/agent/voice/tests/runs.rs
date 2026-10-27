//! Spoken runs: approval decided server side and only for runs started on
//! the voice surface (D-017), trading decided by stored settings and never
//! by the page, the configured name reaching the prompt (D-018), and every
//! spoken line recorded.

use super::{put_voice, switch_on, voice_harness};
use crate::agent::chat::tests::{types, Harness, TEST_TOOLKITS};
use crate::agent::contract::tool::{
    AgentTool, Capability, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec,
};
use crate::agent::testing::Reply;
use crate::agent::voice::approval::{NOT_A_CONFIRMATION, PASSED, SWITCHED_OFF};
use crate::agent::voice::transcript::{audit_with, SpokenLine};
use async_trait::async_trait;
use axum::http::StatusCode;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::sync::Arc;

async fn trading_on(h: &Harness, voice: bool) {
    let (s, _) = h
        .json(
            "PUT",
            "/agent/api/settings",
            Some(json!({"trading_enabled": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    put_voice(h, json!({"voice_trading_enabled": voice})).await;
}

/// A turn asking for an order on `surface`, paused for approval: the
/// confirm frame and the conversation.
async fn paused_on(h: &Harness, surface: &str) -> (Value, i64) {
    let (_, f, raw) = h
        .chat(
            "/agent/api/chat/stream",
            json!({"message": "buy 1 SBIN", "surface": surface, "trading_enabled": true}),
        )
        .await;
    assert_eq!(types(&f), ["start", "usage", "confirm"], "{}", raw);
    (f[2].clone(), f[0]["conversation_id"].as_i64().unwrap())
}

async fn approve(h: &Harness, run_id: &Value, heard: &str) -> Value {
    let (s, v) = h
        .json(
            "POST",
            "/agent/api/voice/approve",
            Some(json!({"run_id": run_id, "transcript": heard})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    v["data"].clone()
}

fn order_call() -> Reply {
    Reply::Calls(vec![(
        "c9",
        "place_order",
        r#"{"symbol":"SBIN","quantity":1}"#.into(),
    )])
}

#[tokio::test]
async fn a_spoken_yes_approves_a_paused_voice_run_once_and_the_card_resumes_it() {
    let (h, _fake) = voice_harness(vec![order_call(), Reply::Text(vec!["Placed."])]).await;
    h.ctx().agent.set_toolkits(TEST_TOOLKITS);
    trading_on(&h, true).await;
    let (confirm, conv) = paused_on(&h, "voice").await;
    let run = &confirm["run_id"];
    let (s, v) = h
        .json(
            "POST",
            "/agent/api/voice/approve",
            Some(json!({"transcript": "yes"})),
        )
        .await;
    assert_eq!(
        (s, v["message"].as_str()),
        (StatusCode::BAD_REQUEST, Some("A run_id is required"))
    );
    let question = approve(&h, run, "what is bank nifty doing").await;
    assert_eq!(
        question,
        json!({"approved": false, "reason": NOT_A_CONFIRMATION})
    );
    assert_eq!(
        approve(&h, run, "Yes, go ahead.").await,
        json!({"approved": true, "reason": ""})
    );
    assert_eq!(
        approve(&h, run, "yes").await["reason"],
        PASSED,
        "one approval per window"
    );
    // The page then resumes through the card's own route.
    let (s, f, raw) = h
        .chat(
            "/agent/api/chat/confirm",
            json!({"run_id": run, "session_id": confirm["session_id"],
                   "conversation_id": conv, "decisions": {"c9": true}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", raw);
    assert_eq!(types(&f).last().map(String::as_str), Some("done"));
}

/// D-017: the web opened a window for every pause, so a spoken "yes"
/// approved an order staged by a typed question.
#[tokio::test]
async fn a_typed_run_is_never_approved_by_voice() {
    let (h, _fake) = voice_harness(vec![order_call()]).await;
    h.ctx().agent.set_toolkits(TEST_TOOLKITS);
    trading_on(&h, true).await;
    let (confirm, _) = paused_on(&h, "chat").await;
    let verdict = approve(&h, &confirm["run_id"], "yes").await;
    assert_eq!(verdict, json!({"approved": false, "reason": PASSED}));
}

#[tokio::test]
async fn answering_on_screen_closes_the_spoken_window_and_voice_trading_off_refuses() {
    let (h, _fake) = voice_harness(vec![
        order_call(),
        Reply::Text(vec!["Not placed."]),
        order_call(),
    ])
    .await;
    h.ctx().agent.set_toolkits(TEST_TOOLKITS);
    trading_on(&h, true).await;
    let (confirm, conv) = paused_on(&h, "voice").await;
    h.chat(
        "/agent/api/chat/confirm",
        json!({"run_id": confirm["run_id"], "session_id": confirm["session_id"],
               "conversation_id": conv, "decisions": {"c9": false}}),
    )
    .await;
    assert_eq!(
        approve(&h, &confirm["run_id"], "yes").await["reason"],
        PASSED
    );
    let (confirm, _) = paused_on(&h, "voice").await;
    put_voice(&h, json!({"voice_trading_enabled": false})).await;
    assert_eq!(
        approve(&h, &confirm["run_id"], "yes").await["reason"],
        SWITCHED_OFF
    );
}

struct Named(ToolSpec);

#[async_trait]
impl AgentTool for Named {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }
    async fn call(&self, _ctx: &ToolCtx, _args: Map<String, Value>) -> ToolOutcome {
        ToolOutcome::Ok("ok".into())
    }
}

fn named(name: &'static str, approval: bool) -> Arc<dyn AgentTool> {
    Arc::new(Named(ToolSpec {
        name,
        description: format!("{} things.", name).into(),
        parameters: json!({"type": "object", "properties": {}, "required": []}),
        needs_approval: approval,
    }))
}

fn market(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    vec![named("get_quote", false)]
}

fn orders(_: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    vec![named("place_order", true)]
}

/// A read toolkit on every surface and an order toolkit that needs the
/// trading capability, as the registry's `orders` group does.
const GATED: &[ToolGroup] = &[
    ToolGroup {
        key: "market",
        surfaces: Surfaces::ALL,
        requires: &[],
        order: 10,
        description: "",
        build: market,
    },
    ToolGroup {
        key: "orders",
        surfaces: Surfaces::ALL,
        requires: &[Capability::Trading],
        order: 50,
        description: "",
        build: orders,
    },
];

/// The tools the last model request offered.
fn offered(h: &Harness) -> Vec<String> {
    let sent = h.fake.sent();
    sent.last().unwrap()["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

async fn ask(h: &Harness, surface: &str, trading: bool) -> Vec<String> {
    h.fake.push(Reply::Text(vec!["ok"]));
    let (s, _, raw) = h
        .chat(
            "/agent/api/chat/stream",
            json!({"message": "hi", "surface": surface, "trading_enabled": trading}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", raw);
    offered(h)
}

/// The web's TestTheBrowserDoesNotDecideItMayTrade, through the run
/// builder: a spoken run trades only when it asked and both stored
/// switches are on; the page cannot add the capability; a typed run is not
/// governed by the voice switch; an unreadable setting withholds it.
#[tokio::test]
async fn trading_on_the_voice_surface_is_decided_by_stored_settings() {
    let (h, _fake) = voice_harness(vec![]).await;
    h.ctx().agent.set_toolkits(GATED);
    let with_orders = vec!["get_quote".to_string(), "place_order".to_string()];
    let read_only = vec!["get_quote".to_string()];
    assert_eq!(ask(&h, "voice", true).await, read_only, "both switches off");
    trading_on(&h, false).await;
    assert_eq!(ask(&h, "voice", true).await, read_only, "voice switch off");
    assert_eq!(
        ask(&h, "chat", true).await,
        with_orders,
        "typed runs ignore it"
    );
    put_voice(&h, json!({"voice_trading_enabled": true})).await;
    assert_eq!(ask(&h, "voice", true).await, with_orders);
    assert_eq!(ask(&h, "voice", false).await, read_only, "voice never adds");
    let conn = h.ctx().sqlite.conn().unwrap();
    conn.execute_batch("DROP TABLE ag_setting").unwrap();
    drop(conn);
    assert_eq!(
        ask(&h, "voice", true).await,
        read_only,
        "unreadable: withheld"
    );
}

/// D-018: the web built the spoken surface's prompt with the shipped name;
/// the configured one reaches it here.
#[tokio::test]
async fn the_configured_name_reaches_the_spoken_prompt() {
    let (h, _fake) = voice_harness(vec![]).await;
    put_voice(&h, json!({"voice_agent_name": " Tara  Rose "})).await;
    ask(&h, "voice", false).await;
    let sent = h.fake.sent();
    let system = sent.last().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        system.contains("You are Tara Rose here"),
        "{}",
        &system[..200.min(system.len())]
    );
    assert!(!system.contains("You are Vega here"));
}

#[tokio::test]
async fn every_spoken_line_is_recorded_and_the_first_opens_a_thread() {
    let (h, _fake) = voice_harness(vec![]).await;
    switch_on(&h).await;
    let heard: Arc<Mutex<Vec<SpokenLine>>> = Arc::default();
    let spy = heard.clone();
    h.ctx()
        .agent
        .voice()
        .set_transcript_audit(audit_with(move |_, line| {
            let spy = spy.clone();
            async move { spy.lock().push(line) }
        }));
    let post = |body: Value| {
        let h = &h;
        async move {
            let (s, v) = h
                .json("POST", "/agent/api/voice/transcript", Some(body))
                .await;
            assert_eq!(s, StatusCode::OK, "{}", v);
            v["data"].clone()
        }
    };
    assert_eq!(
        post(json!({"text": "   "})).await,
        json!({"recorded": false})
    );
    let first = "x".repeat(70);
    let d = post(json!({"role": "trader", "text": first})).await;
    let conv = d["conversation_id"].as_i64().expect("a thread was opened");
    assert_eq!(d["recorded"], true);
    let (_, list) = h
        .json("GET", "/agent/api/conversations?surface=voice", None)
        .await;
    assert!(list.to_string().contains(&"x".repeat(60)), "{}", list);
    let d = post(
        json!({"role": "agent", "text": "Nifty is up.", "conversation_id": conv,
                        "run_id": "r1"}),
    )
    .await;
    assert_eq!(d["conversation_id"], conv);
    let lines = heard.lock().clone();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (lines[0].role, lines[0].conversation_id),
        ("trader", Some(conv))
    );
    assert_eq!(
        (
            lines[1].role,
            lines[1].text.as_str(),
            lines[1].run_id.as_deref()
        ),
        ("agent", "Nifty is up.", Some("r1"))
    );
    let anonymous = h.call(
        axum::http::Request::post("/agent/api/voice/transcript")
            .header("content-type", "application/json")
            .body(axum::body::Body::from("{}"))
            .unwrap(),
    );
    assert_ne!(anonymous.await.0, StatusCode::OK);
}
