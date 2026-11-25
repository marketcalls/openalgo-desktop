//! Which models get cache breakpoints, and where (web d941bb3b1,
//! `test/test_agent_prompt_cache.py`). What the provider receives is
//! pinned through a whole turn in `agent::chat::prompt_cache_tests`.

use super::prompt_cache::{prompt_cache_points, request, CachePoint};
use genai::chat::{CacheControl, ChatMessage, ChatRole, Tool};

const ALL: [CachePoint; 3] = [
    CachePoint::Tools,
    CachePoint::System,
    CachePoint::LastMessage,
];

/// Web `test_a_claude_model_gets_a_breakpoint_on_the_tools_the_system_message_and_the_last_message`.
#[test]
fn a_claude_model_gets_a_breakpoint_on_the_tools_the_system_message_and_the_last_message() {
    for id in [
        "anthropic/claude-haiku-5-5",
        "anthropic/claude-sonnet-5-5",
        "Anthropic/Claude-Opus-5-5",
        "bedrock/anthropic.claude-sonnet-5-5-v1:0",
        "vertex_ai/claude-sonnet-5-5",
        "openrouter/anthropic/claude-sonnet-5-5",
    ] {
        assert_eq!(prompt_cache_points(id), ALL, "{}", id);
    }
}

/// Web `test_no_other_provider_is_sent_a_cache_field`. The ones named for
/// Claude on a provider that takes no breakpoints pin the provider check
/// apart from the name check.
#[test]
fn no_other_provider_is_sent_a_cache_field() {
    for id in [
        "openai/gpt-5.4",
        "gpt-4o",
        "chatgpt/gpt-6-sol",
        "ollama/llama3.2",
        "gemini/gemini-2.5-pro",
        "bedrock/amazon.nova-pro-v1:0",
        "vertex_ai/gemini-2.5-pro",
        "openrouter/openai/gpt-5.4",
        "ollama/claude-like-local",
        "azure/claude-sonnet-5-5",
        "huggingface/anthropic-distill",
        "",
    ] {
        assert!(prompt_cache_points(id).is_empty(), "{}", id);
    }
}

fn tools() -> Vec<Tool> {
    vec![Tool::new("get_quote"), Tool::new("get_depth")]
}

fn conversation() -> Vec<ChatMessage> {
    vec![
        ChatMessage::user("earlier question"),
        ChatMessage::assistant("earlier answer"),
        ChatMessage::user("quote of INFY"),
    ]
}

fn cached(message: &ChatMessage) -> bool {
    message
        .options
        .as_ref()
        .and_then(|o| o.cache_control.as_ref())
        == Some(&CacheControl::Ephemeral)
}

/// The three points: the system prompt goes first as a marked system
/// message (genai never marks the request's own system field), and only
/// the last tool and the last message are marked.
#[test]
fn a_claude_request_marks_the_last_tool_the_system_prompt_and_the_last_message() {
    let r = request("SYSTEM", conversation(), tools(), &ALL);

    assert_eq!(r.system, None);
    assert_eq!(r.messages.len(), 4);
    assert_eq!(r.messages[0].role, ChatRole::System);
    assert_eq!(r.messages[0].content.first_text(), Some("SYSTEM"));
    let marked: Vec<bool> = r.messages.iter().map(cached).collect();
    assert_eq!(marked, [true, false, false, true]);
    let tools = r.tools.expect("the tools");
    let marked: Vec<bool> = tools.iter().map(|t| t.cache_control.is_some()).collect();
    assert_eq!(marked, [false, true]);
}

/// Without breakpoints the request is the one a run sent before caching.
#[test]
fn a_request_without_breakpoints_is_unchanged() {
    let r = request("SYSTEM", conversation(), tools(), &[]);

    assert_eq!(r.system.as_deref(), Some("SYSTEM"));
    assert_eq!(r.messages.len(), 3);
    assert!(r.messages.iter().all(|m| m.options.is_none()));
    assert!(r
        .tools
        .expect("the tools")
        .iter()
        .all(|t| t.cache_control.is_none()));
    assert!(request("SYSTEM", conversation(), vec![], &[])
        .tools
        .is_none());
}
