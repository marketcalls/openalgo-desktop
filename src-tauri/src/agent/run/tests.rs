//! The provider stream as genai reads it, against the fake provider.

use super::client::ModelClient;
use crate::agent::testing::{fake_model, FakeProvider, Reply};
use futures_util::StreamExt;
use genai::chat::{ChatRequest, ChatStreamEvent};
use serde_json::{json, Value};

fn sse(events: &[Value]) -> String {
    events.iter().map(|e| format!("data: {}\n\n", e)).collect()
}

/// A batch of parallel calls whose arguments arrive only in the closing
/// events, with no deltas at all: what the Responses API sends for several
/// instruments in one turn (the web's `litellm_tool_args.py` repaired
/// LiteLLM dropping them; every call in the batch failed with "Missing
/// required argument"). genai must deliver each call whole, once.
fn batch_without_deltas() -> String {
    let call = |i: u64, id: &str, args: &str| {
        vec![
            json!({"type": "response.output_item.added", "output_index": i,
                   "item": {"type": "function_call", "id": format!("fc_{}", i), "call_id": id, "name": "get_history", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.done", "output_index": i, "item_id": format!("fc_{}", i), "arguments": args}),
            json!({"type": "response.output_item.done", "output_index": i,
                   "item": {"type": "function_call", "id": format!("fc_{}", i), "call_id": id, "name": "get_history", "arguments": args}}),
        ]
    };
    let mut events = vec![
        json!({"type": "response.created", "response": {"id": "resp1", "status": "in_progress"}}),
    ];
    events.extend(call(0, "call_tcs", r#"{"symbol":"TCS","exchange":"NSE"}"#));
    events.extend(call(
        1,
        "call_infy",
        r#"{"symbol":"INFY","exchange":"NSE"}"#,
    ));
    events.push(json!({"type": "response.completed", "response": {"id": "resp1", "status": "completed", "model": "gpt-5",
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}}}));
    sse(&events)
}

#[tokio::test]
async fn parallel_calls_with_arguments_only_at_the_end_arrive_whole_and_once() {
    let fake = FakeProvider::start(vec![Reply::Raw(batch_without_deltas())]).await;
    let model = fake_model(&fake.endpoint(), genai::adapter::AdapterKind::OpenAIResp);
    let client = ModelClient::new().unwrap();
    let resp = client
        .stream(
            &model,
            ChatRequest::from_user("history of TCS and INFY"),
            None,
        )
        .await
        .unwrap();
    let mut stream = resp.stream;
    let mut calls = Vec::new();
    while let Some(event) = stream.next().await {
        if let ChatStreamEvent::End(end) = event.unwrap() {
            calls = end
                .captured_tool_calls()
                .unwrap_or_default()
                .into_iter()
                .cloned()
                .collect();
        }
    }
    let got: Vec<(String, Value)> = calls
        .into_iter()
        .map(|c| (c.call_id, c.fn_arguments))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "call_tcs".to_string(),
                json!({"symbol": "TCS", "exchange": "NSE"})
            ),
            (
                "call_infy".to_string(),
                json!({"symbol": "INFY", "exchange": "NSE"})
            ),
        ]
    );
}

#[tokio::test]
async fn a_chat_completions_stream_assembles_split_arguments() {
    let fake = FakeProvider::start(vec![Reply::Calls(vec![
        (
            "c1",
            "get_quote",
            r#"{"symbol":"INFY","exchange":"NSE"}"#.into(),
        ),
        (
            "c2",
            "get_quote",
            r#"{"symbol":"TCS","exchange":"NSE"}"#.into(),
        ),
    ])])
    .await;
    let model = fake_model(&fake.endpoint(), genai::adapter::AdapterKind::OpenAI);
    let client = ModelClient::new().unwrap();
    let resp = client
        .stream(&model, ChatRequest::from_user("quotes"), None)
        .await
        .unwrap();
    let mut stream = resp.stream;
    let mut calls = Vec::new();
    while let Some(event) = stream.next().await {
        if let ChatStreamEvent::End(end) = event.unwrap() {
            calls = end
                .captured_tool_calls()
                .unwrap_or_default()
                .into_iter()
                .cloned()
                .collect();
        }
    }
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1].fn_arguments,
        json!({"symbol": "TCS", "exchange": "NSE"})
    );
}

/// The web's `_reasoning_effort`: none for a model that cannot reason; an
/// explicit request wins, its `off` included; else the model's own level.
#[test]
fn the_effort_sent_follows_the_web_rules() {
    use super::client::effective_effort;
    let mut m = fake_model(
        "http://127.0.0.1:9/v1/",
        genai::adapter::AdapterKind::OpenAI,
    );
    assert_eq!(effective_effort(&m, Some("high")), None);
    m.capabilities.reasoning = true;
    m.capabilities.default_reasoning_effort = "medium".into();
    assert_eq!(effective_effort(&m, Some("HIGH")), Some("high"));
    assert_eq!(effective_effort(&m, Some("off")), None);
    assert_eq!(effective_effort(&m, None), Some("medium"));
    m.capabilities.default_reasoning_effort = "off".into();
    assert_eq!(effective_effort(&m, None), None);
}

/// Effort fields a request may carry, on any provider format.
const EFFORT_FIELDS: [&str; 7] = [
    "/reasoning_effort",
    "/reasoning/effort",
    "/thinking",
    "/think",
    "/output_config/effort",
    "/generationConfig/thinkingConfig/thinkingBudget",
    "/generationConfig/thinkingConfig/thinkingLevel",
];

/// A model name goes out exactly as configured on every provider format,
/// and an effort is sent only when one is set: genai read a name ending in
/// an effort word as that effort (`gpt-5.1-codex-max` went out as
/// `gpt-5.1-codex` with effort max). A request to capture reasoning
/// (Responses `reasoning.summary`, Gemini `includeThoughts`) is not an
/// effort and is sent either way.
#[tokio::test]
async fn a_model_name_is_sent_exactly_as_configured() {
    use genai::adapter::AdapterKind as A;
    let fake = FakeProvider::start(vec![]).await;
    let client = ModelClient::new().unwrap();
    let mut sent = 0;
    let mut problems = Vec::new();
    let formats = [
        A::OpenAI,
        A::OpenAIResp,
        A::Anthropic,
        A::Gemini,
        A::Ollama,
        // OpenAI-compatible, with thinking managed in the body.
        A::DeepSeek,
    ];
    let names = [
        "gpt-5.1-codex-max",
        "acme-reasoner-high",
        "claude-sonnet-5-5",
        "claude-sonnet-5-5-high",
        "gemini-3.5-flash-zero",
    ];
    for adapter in formats {
        for name in names {
            for effort in [None, Some("low")] {
                let mut model = fake_model(&fake.endpoint(), adapter);
                model.model_name = name.into();
                if let Ok(resp) = client
                    .stream(&model, ChatRequest::from_user("hi"), effort)
                    .await
                {
                    let mut stream = resp.stream;
                    while let Some(Ok(_)) = stream.next().await {}
                }
                sent += 1;
                let case = format!("{:?} {} {:?}", adapter, name, effort);
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    fake.wait_for_requests(sent),
                )
                .await
                .unwrap_or_else(|_| panic!("{}: no request", case));
                let body = &fake.sent()[sent - 1];
                let path = &fake.paths()[sent - 1];
                let exact = if adapter == A::Gemini {
                    path.contains(&format!("models/{}:", name))
                } else {
                    body["model"] == name
                };
                if !exact {
                    problems.push(format!("{}: sent as {} {}", case, body["model"], path));
                }
                let efforts: Vec<&str> = EFFORT_FIELDS
                    .into_iter()
                    .filter(|f| body.pointer(f).is_some())
                    .collect();
                if effort.is_none() && !efforts.is_empty() {
                    problems.push(format!("{}: {:?} sent", case, efforts));
                }
                if effort.is_some() && efforts.is_empty() {
                    problems.push(format!("{}: no effort sent", case));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{:#?}", problems);
}
