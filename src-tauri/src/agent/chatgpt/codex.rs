//! A plan turn's request, exactly as a web plan turn sends it (decision 10
//! of the 1.0.4 plan).
//!
//! On the web, agno calls LiteLLM with the run's messages and tools,
//! LiteLLM's Responses bridge turns them into a Responses request, and its
//! `chatgpt` provider (`ChatGPTResponsesAPIConfig`) rewrites that for the
//! Codex backend: its default instructions in front of the system prompt,
//! `store` false, `stream` true, `reasoning.encrypted_content` included,
//! every key outside its allowed list dropped, and its own headers
//! (`originator`, a user agent, the `ChatGPT-Account-Id`, a session id).
//! `scripts/agent/capture_codex_request.py` records that request against a
//! local server (`tests/fixtures/agent/codex_request.json`) and the tests
//! here hold the desktop's to it.
//!
//! The desktop sends it through genai's Responses adapter: the options keep
//! nothing genai would add outside the allowed list, and the body LiteLLM
//! would send goes in `extra_body`, which genai merges last, key by key.
//! The bearer token and these headers come per request from the plan's
//! credential ([`super::refresh::PlanCredentials`]).

use crate::agent::contract::model::ResolvedModel;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatRole, ContentPart, ReasoningEffort, ToolName,
};
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

/// LiteLLM's `DEFAULT_ORIGINATOR`.
pub const ORIGINATOR: &str = "codex_cli_rs";

/// The LiteLLM the plan request was recorded from: its version is the user
/// agent's. The web pins 1.106.6 since 8a2fbf1d0, whose request is the same
/// (its `chatgpt` provider adds only a `service_tier` the web never sends,
/// and its default instructions are unchanged), but in the web's locked
/// environment 1.106.6 cannot make one (its Responses path imports a module
/// that needs `mcp` 2.2 against the web's 1.30.0), so the recording stays
/// at 1.104.0 until it can be made again.
pub const LITELLM_VERSION: &str = "1.104.0";

/// The keys the provider lets through (its `allowed_keys`).
pub const ALLOWED_KEYS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "stream",
    "store",
    "include",
    "tools",
    "tool_choice",
    "reasoning",
    "previous_response_id",
    "truncation",
];

/// LiteLLM's `CHATGPT_DEFAULT_INSTRUCTIONS`, verbatim.
pub static DEFAULT_INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| {
    serde_json::from_str::<Value>(include_str!("codex_instructions.json"))
        .ok()
        .and_then(|v| v["instructions"].as_str().map(str::to_string))
        .unwrap_or_default()
});

/// Python's `platform.system()` names.
fn os_type() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        _ => "Unknown",
    }
}

/// Python's `platform.machine()` names.
fn machine() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        ("windows", "x86_64") => "AMD64",
        ("windows", "aarch64") => "ARM64",
        (_, arch) => arch,
    }
}

/// LiteLLM's `_safe_header_value`: printable ASCII, anything else `_`.
fn safe_header(value: &str) -> String {
    value
        .chars()
        .map(|c| if (' '..='~').contains(&c) { c } else { '_' })
        .collect()
}

/// LiteLLM's `get_chatgpt_user_agent` for a process with no terminal:
/// `codex_cli_rs/<litellm version> (<os> <release>; <arch>) unknown`.
pub static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    let release = sysinfo::System::kernel_version()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "0".into());
    safe_header(&format!(
        "{}/{} ({} {}; {}) unknown",
        ORIGINATOR,
        LITELLM_VERSION,
        os_type(),
        release.trim(),
        machine()
    ))
});

/// The headers a plan request carries besides its bearer token (LiteLLM's
/// `get_chatgpt_default_headers`): a fresh session id per request, as
/// LiteLLM takes its per-call id, and the account when it is known.
pub fn headers(account_id: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![
        ("accept".to_string(), "text/event-stream".to_string()),
        ("originator".to_string(), ORIGINATOR.to_string()),
        ("user-agent".to_string(), USER_AGENT.clone()),
        ("session_id".to_string(), uuid::Uuid::new_v4().to_string()),
    ];
    if let Some(a) = account_id.filter(|a| !a.is_empty()) {
        h.push(("ChatGPT-Account-Id".to_string(), safe_header(a)));
    }
    h
}

/// A plan model (D-026: the resolved row's `chatgpt/` id decides).
pub fn is_plan(model: &ResolvedModel) -> bool {
    crate::agent::providers::is_subscription(&model.catalog_id)
}

/// The request and options for one model call: unchanged for any model but
/// a plan's; for a plan's, LiteLLM's body and nothing genai would add
/// outside the allowed keys.
pub fn shape(
    model: &ResolvedModel,
    request: ChatRequest,
    mut options: ChatOptions,
) -> (ChatRequest, ChatOptions) {
    if !is_plan(model) {
        return (request, options);
    }
    let effort = options
        .reasoning_effort
        .as_ref()
        .and_then(ReasoningEffort::as_keyword);
    options.extra_body = Some(body(&model.model_name, &request, effort));
    options.temperature = None;
    options.max_tokens = None;
    options.top_p = None;
    options.stop_sequences.clear();
    options.seed = None;
    options.service_tier = None;
    options.verbosity = None;
    options.response_format = None;
    options.prompt_cache_key = None;
    options.cache_control = None;
    options.tool_choice = None;
    // No `summary` and no genai-added `include`: what LiteLLM sends.
    options.capture_reasoning_content = Some(false);
    if effort.is_none() {
        // An effort with no keyword: genai then adds no `reasoning` and does
        // not read one off the model's name (`gpt-5.1-codex-max` would
        // otherwise be sent as `gpt-5.1-codex` with effort `max`).
        options.reasoning_effort = Some(ReasoningEffort::Budget(0));
    }
    (request, options)
}

/// The body LiteLLM sends for this request (the bridge's
/// `transform_request`, then the provider's rewrite).
pub fn body(model_name: &str, request: &ChatRequest, effort: Option<&str>) -> Value {
    let (input, system) = input_items(request);
    let mut b = Map::new();
    b.insert("model".into(), json!(model_name));
    b.insert("input".into(), Value::Array(input));
    b.insert(
        "instructions".into(),
        json!(instructions(system.as_deref())),
    );
    if let Some(e) = effort {
        b.insert("reasoning".into(), json!({"effort": e}));
    }
    b.insert("stream".into(), json!(true));
    let tools = tools_of(request);
    if !tools.is_empty() {
        b.insert("tool_choice".into(), json!("auto"));
        b.insert("tools".into(), Value::Array(tools));
    }
    b.insert("store".into(), json!(false));
    b.insert("include".into(), json!(["reasoning.encrypted_content"]));
    Value::Object(b)
}

/// The provider's instructions: its default, then the run's own system
/// prompt after a blank line, unless the default is already in it.
pub fn instructions(system: Option<&str>) -> String {
    let base = DEFAULT_INSTRUCTIONS.as_str();
    match system.filter(|s| !s.is_empty()) {
        Some(s) if s.contains(base) => s.to_string(),
        Some(s) => format!("{}\n\n{}", base, s),
        None => base.to_string(),
    }
}

fn input_text(text: &str) -> Value {
    json!({"type": "input_text", "text": text})
}

/// A message's parts as the bridge writes them: text as `input_text`
/// (`output_text` for the model's own), an image as `input_image`.
fn content_parts(msg: &ChatMessage, output: bool) -> Vec<Value> {
    msg.content
        .parts()
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) if output => Some(json!({"type": "output_text", "text": t})),
            ContentPart::Text(t) => Some(input_text(t)),
            ContentPart::Binary(b) if b.is_image() => Some(json!({
                "type": "input_image",
                "image_url": b.clone().into_url(),
                "detail": "auto",
            })),
            _ => None,
        })
        .collect()
}

/// The conversation as Responses input items, and the system prompt (the
/// run's, then any leading system messages, joined with a space as the
/// bridge joins them).
fn input_items(request: &ChatRequest) -> (Vec<Value>, Option<String>) {
    let mut system = request.system.clone().filter(|s| !s.is_empty());
    let mut items = Vec::new();
    let mut leading = true;
    for msg in &request.messages {
        if msg.role != ChatRole::System {
            leading = false;
        }
        match msg.role {
            ChatRole::System => match msg.content.joined_texts() {
                Some(t) if leading => {
                    system = Some(match system {
                        Some(s) => format!("{} {}", s, t),
                        None => t,
                    });
                }
                _ => items.push(json!({"type": "message", "role": "system",
                                       "content": content_parts(msg, false)})),
            },
            ChatRole::User => items.push(json!({"type": "message", "role": "user",
                                                "content": content_parts(msg, false)})),
            ChatRole::Assistant => assistant_items(msg, &mut items),
            ChatRole::Tool => {
                for r in msg.content.tool_responses() {
                    items.push(json!({"type": "function_call_output", "call_id": r.call_id,
                                      "output": [input_text(&r.content)]}));
                }
            }
        }
    }
    (items, system)
}

/// The model's turn: its text (when it called tools, only if there was
/// some), then one item per call.
fn assistant_items(msg: &ChatMessage, items: &mut Vec<Value>) {
    let calls = msg.content.tool_calls();
    let text = msg.content.joined_texts().unwrap_or_default();
    if calls.is_empty() || !text.is_empty() {
        let mut content = content_parts(msg, true);
        if content.is_empty() {
            content.push(json!({"type": "output_text", "text": ""}));
        }
        items.push(json!({"type": "message", "role": "assistant", "content": content}));
    }
    for c in calls {
        let arguments = match &c.fn_arguments {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        items.push(json!({"type": "function_call", "call_id": c.call_id,
                          "name": c.fn_name, "arguments": arguments}));
    }
}

/// The tools as the bridge converts them (`FunctionToolParam`).
fn tools_of(request: &ChatRequest) -> Vec<Value> {
    request
        .tools
        .iter()
        .flatten()
        .map(|t| {
            let name = match &t.name {
                ToolName::WebSearch => "web_search".to_string(),
                ToolName::Custom(n) => n.clone(),
            };
            json!({
                "name": name,
                "parameters": t.schema.clone(),
                "strict": t.strict,
                "type": "function",
                "description": t.description.clone(),
            })
        })
        .collect()
}
