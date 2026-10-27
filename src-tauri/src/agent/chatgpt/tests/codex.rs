//! A plan turn's request against a fake Codex backend that records it,
//! compared with what a web plan turn sends (recorded from the web's
//! LiteLLM by `scripts/agent/capture_codex_request.py`).

use super::*;
use crate::agent::chatgpt::codex::{self, ALLOWED_KEYS, DEFAULT_INSTRUCTIONS, LITELLM_VERSION};
use crate::agent::contract::model::Pricing;
use crate::agent::run::client::ModelClient;
use crate::agent::testing::fake_model;
use futures_util::StreamExt;
use genai::chat::{ChatMessage, ChatRequest, MessageContent, Tool, ToolCall, ToolResponse};

const RESPONSES: &str = "/backend-api/codex/responses";

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/agent/codex_request.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

const SSE: &str = concat!(
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\",\"status\":\"in_progress\"}}\n\n",
    "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"output_index\":0,\"content_index\":0,\"delta\":\"ok\"}\n\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\",\"model\":\"gpt-5.4\",",
    "\"usage\":{\"input_tokens\":10,\"output_tokens\":1,\"total_tokens\":11}}}\n\n"
);

/// A plan model on the fake, signed in with a stored credential.
async fn plan_model(
    t: &TestCtx,
    name: &str,
) -> (
    FakeServer,
    crate::agent::contract::model::ResolvedModel,
    String,
) {
    let fake = FakeServer::start().await;
    fake.script(RESPONSES, vec![Answer::text(200, "text/event-stream", SSE)]);
    install(t, flow_at(&fake, Duration::from_secs(30)));
    let record = record_expiring(t, 3600, Some("rt-codex-0123456789"));
    let token = record.access().unwrap().to_string();
    put(t, &record);
    let mut m = fake_model(
        &fake.url("/backend-api/codex/"),
        genai::adapter::AdapterKind::OpenAIResp,
    );
    m.provider_kind = "litellm".into();
    m.catalog_id = format!("chatgpt/{}", name);
    m.model_name = name.into();
    m.pricing = Pricing::Subscription;
    m.credentials = t.ctx.agent.plan().credentials(&t.ctx);
    (fake, m, token)
}

fn quote_tool(f: &Value) -> Tool {
    let t = &f["inputs"]["tool"]["function"];
    Tool::new(t["name"].as_str().unwrap())
        .with_description(t["description"].as_str().unwrap())
        .with_schema(t["parameters"].clone())
}

async fn send(
    client: &ModelClient,
    m: &crate::agent::contract::model::ResolvedModel,
    r: ChatRequest,
    effort: Option<&str>,
) {
    let resp = client.stream(m, r, effort).await.unwrap();
    let mut s = resp.stream;
    while let Some(e) = s.next().await {
        e.unwrap();
    }
}

/// The recorded headers that are the provider's (the transport's own,
/// such as `connection`, are not).
fn check_headers(seen: &crate::agent::outbound::fake::Seen, recorded: &Value, token: &str) {
    assert_eq!(
        seen.header("authorization"),
        Some(format!("Bearer {}", token).as_str())
    );
    for name in ["accept", "content-type", "originator", "chatgpt-account-id"] {
        let want = recorded[name].as_str().unwrap();
        let want = if name == "chatgpt-account-id" {
            ACCOUNT
        } else {
            want
        };
        assert_eq!(seen.header(name), Some(want), "{}", name);
    }
    let session = seen.header("session_id").unwrap();
    assert!(uuid::Uuid::parse_str(session).is_ok(), "{}", session);
    let ua = seen.header("user-agent").unwrap();
    assert!(
        ua.starts_with(&format!("codex_cli_rs/{} (", LITELLM_VERSION)),
        "{}",
        ua
    );
    assert!(ua.ends_with(") unknown"), "{}", ua);
    assert_eq!(
        recorded["user-agent"],
        json!(format!(
            "codex_cli_rs/{} (<os> <release>; <arch>) unknown",
            LITELLM_VERSION
        ))
    );
}

/// The body with the fixture's account and arguments made comparable: a
/// call's arguments are compared as JSON (genai keeps them parsed).
fn normalised(mut body: Value) -> Value {
    for item in body["input"].as_array_mut().into_iter().flatten() {
        if item["type"] == "function_call" {
            let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
            item["arguments"] = args;
        }
    }
    body
}

#[tokio::test]
async fn a_first_plan_turn_is_sent_as_the_web_sends_it() {
    let t = ctx();
    let f = fixture();
    let (fake, m, token) = plan_model(&t, "gpt-5.4").await;
    let client = ModelClient::new().unwrap();
    let r = ChatRequest::new(vec![ChatMessage::user(
        f["inputs"]["user"].as_str().unwrap(),
    )])
    .with_system(f["inputs"]["system"].as_str().unwrap())
    .with_tools(vec![quote_tool(&f)]);
    send(&client, &m, r, Some("medium")).await;
    let seen = &fake.seen_at(RESPONSES)[0];
    assert_eq!(seen.method, "POST");
    check_headers(seen, &f["first"]["headers"], &token);
    assert_eq!(seen.json(), f["first"]["body"]);
    let keys: Vec<String> = seen.json().as_object().unwrap().keys().cloned().collect();
    assert!(
        keys.iter().all(|k| ALLOWED_KEYS.contains(&k.as_str())),
        "{:?}",
        keys
    );
}

#[tokio::test]
async fn a_turn_after_a_tool_call_is_sent_as_the_web_sends_it() {
    let t = ctx();
    let f = fixture();
    let (fake, m, token) = plan_model(&t, "gpt-5.4").await;
    let client = ModelClient::new().unwrap();
    let inputs = &f["inputs"];
    let call = ToolCall {
        call_id: inputs["call_id"].as_str().unwrap().into(),
        fn_name: "get_quote".into(),
        fn_arguments: serde_json::from_str(inputs["call_arguments"].as_str().unwrap()).unwrap(),
        thought_signatures: None,
    };
    let messages = vec![
        ChatMessage::user(inputs["user"].as_str().unwrap()),
        ChatMessage::assistant(MessageContent::from_tool_calls(vec![call])),
        ChatMessage::tool(MessageContent::from_tool_responses(vec![
            ToolResponse::new(
                inputs["call_id"].as_str().unwrap(),
                inputs["tool_result"].as_str().unwrap(),
            ),
        ])),
    ];
    let r = ChatRequest::new(messages)
        .with_system(inputs["system"].as_str().unwrap())
        .with_tools(vec![quote_tool(&f)]);
    send(&client, &m, r, Some("medium")).await;
    let seen = &fake.seen_at(RESPONSES)[0];
    check_headers(seen, &f["after_tool"]["headers"], &token);
    assert_eq!(
        normalised(seen.json()),
        normalised(f["after_tool"]["body"].clone())
    );
}

#[tokio::test]
async fn nothing_outside_the_allowed_keys_is_sent_even_from_a_model_test() {
    let t = ctx();
    let (fake, m, _) = plan_model(&t, "gpt-5.1-codex-max").await;
    let client = ModelClient::new().unwrap();
    let resp = client
        .stream_with(&m, ChatRequest::from_user("ok"), None, |o| {
            o.with_max_tokens(1).with_temperature(0.2)
        })
        .await
        .unwrap();
    let mut s = resp.stream;
    while s.next().await.is_some() {}
    let body = fake.seen_at(RESPONSES)[0].json();
    let keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for k in &keys {
        assert!(ALLOWED_KEYS.contains(k), "{} was sent", k);
    }
    assert!(
        !keys.contains(&"reasoning"),
        "no effort, no reasoning: {:?}",
        keys
    );
    assert_eq!(
        body["model"],
        json!("gpt-5.1-codex-max"),
        "the name is never cut at an effort word"
    );
    assert_eq!(
        body["instructions"],
        json!(DEFAULT_INSTRUCTIONS.as_str()),
        "no system prompt: the default alone"
    );
    assert_eq!(
        (body["store"].clone(), body["stream"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[test]
fn a_model_that_is_not_the_plans_is_left_alone() {
    let m = fake_model(
        "http://127.0.0.1:1/v1/",
        genai::adapter::AdapterKind::OpenAIResp,
    );
    let options = genai::chat::ChatOptions::default().with_max_tokens(1);
    let (_, o) = codex::shape(&m, ChatRequest::from_user("x"), options);
    assert_eq!(o.max_tokens, Some(1));
    assert!(o.extra_body.is_none());
}

#[test]
fn the_instructions_are_litellms_and_the_system_prompt_follows() {
    let f = fixture();
    assert_eq!(f["litellm"], json!(LITELLM_VERSION));
    let system = f["inputs"]["system"].as_str().unwrap();
    assert_eq!(
        json!(codex::instructions(Some(system))),
        f["first"]["body"]["instructions"]
    );
    assert!(DEFAULT_INSTRUCTIONS.starts_with("You are Codex, based on GPT-5."));
    let already = codex::instructions(Some(system));
    assert_eq!(
        codex::instructions(Some(&already)),
        already,
        "never prepended twice"
    );
}
