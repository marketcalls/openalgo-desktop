//! Prompt caching for Claude models (web d941bb3b1,
//! `builder.prompt_cache_points`).
//!
//! Every model call in a turn sends the same tool definitions and system
//! prompt again, about 43,000 tokens, and a turn that searches and reads a
//! page makes several calls. Claude reads a cached prefix back only up to a
//! point the request marks, so a Claude model gets the web's three
//! breakpoints, where the web's LiteLLM puts them: on the last tool
//! definition, on the system prompt, and on the last message, so each later
//! call in a turn reuses what the one before it sent, tool results included.
//! Anthropic reads the prefix in the order tools, system, messages, and the
//! tools stay cached when the clock at the end of the prompt moves on.
//!
//! No other model gets any. OpenAI, Gemini and DeepSeek cache a prompt that
//! stays the same on their own, and a `cache_control` field sent to a
//! provider that does not document it can be refused. Of the four providers
//! the web marks, the desktop reaches Anthropic and OpenRouter; Bedrock and
//! Vertex are refused before a run (`providers::target`).
//!
//! genai marks a system prompt only when it is sent as a system message:
//! `ChatRequest::with_system` is never marked. On OpenRouter genai marked
//! nothing until change 4 of `vendor/genai-0.7.1/VENDORED.md`, which also
//! leaves the tools unmarked there, as LiteLLM does.

use genai::chat::{CacheControl, ChatMessage, ChatRequest, Tool};

/// Where a breakpoint goes (one of the web's
/// `cache_control_injection_points`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CachePoint {
    /// `{"location": "tool_config"}`: the last tool definition.
    Tools,
    /// `{"location": "message", "role": "system"}`: the system prompt.
    System,
    /// `{"location": "message", "index": -1}`: the last message.
    LastMessage,
}

/// The providers whose API takes explicit breakpoints for a Claude model
/// (the web's `_PROMPT_CACHE_PROVIDERS`).
const PROVIDERS: [&str; 4] = ["anthropic", "bedrock", "vertex_ai", "openrouter"];

const ALL_POINTS: &[CachePoint] = &[
    CachePoint::Tools,
    CachePoint::System,
    CachePoint::LastMessage,
];

/// The breakpoints for a model, by its catalogue id (the id the web hands
/// LiteLLM, `anthropic/claude-haiku-5-5`): all three for a Claude model on
/// a provider that takes them, otherwise none.
pub(crate) fn prompt_cache_points(catalog_id: &str) -> &'static [CachePoint] {
    let name = catalog_id.trim().to_lowercase();
    let provider = name.split('/').next().unwrap_or_default();
    if !PROVIDERS.contains(&provider) {
        return &[];
    }
    if provider != "anthropic" && !name.contains("claude") && !name.contains("anthropic") {
        return &[];
    }
    ALL_POINTS
}

/// One model request with the model's breakpoints. With none it is the
/// request a run always sent: the system prompt in the request's own
/// field, the tools as declared.
pub(crate) fn request(
    system: &str,
    mut messages: Vec<ChatMessage>,
    mut tools: Vec<Tool>,
    points: &[CachePoint],
) -> ChatRequest {
    let marked = |point| points.contains(&point);
    if marked(CachePoint::LastMessage) {
        if let Some(last) = messages.last_mut() {
            let options = last.options.take().unwrap_or_default();
            last.options = Some(options.with_cache_control(CacheControl::Ephemeral));
        }
    }
    if marked(CachePoint::Tools) {
        if let Some(last) = tools.pop() {
            tools.push(last.with_cache_control(CacheControl::Ephemeral));
        }
    }
    let mut request = if marked(CachePoint::System) {
        let mut all = Vec::with_capacity(messages.len() + 1);
        all.push(ChatMessage::system(system).with_options(CacheControl::Ephemeral));
        all.extend(messages);
        ChatRequest::new(all)
    } else {
        ChatRequest::new(messages).with_system(system)
    };
    if !tools.is_empty() {
        request = request.with_tools(tools);
    }
    request
}
