//! Where a registered model's requests go: the genai adapter, the endpoint
//! and the model name the provider expects (the web hands LiteLLM a
//! prefixed id and an `api_base`; genai takes the three separately).
//!
//! Decision 9 of the 1.0.4 plan: OpenAI, Anthropic, Gemini, Ollama, Cohere,
//! the ChatGPT plan and OpenAI-format services (Groq, DeepSeek, xAI,
//! Mistral, OpenRouter, Together, Perplexity, Fireworks, Cerebras, and any
//! endpoint the trader names: LM Studio, vLLM, llamafile, a custom server)
//! are reached. A `litellm` row names its provider by prefix
//! (`groq/llama-3.3-70b-versatile`); one the desktop cannot reach (Bedrock,
//! SageMaker, Vertex and the rest of LiteLLM's list) is refused with a
//! sentence, at its test and before a run.

use super::{catalog_id, normalize_base_url, provider_spec};
use crate::agent::catalog::Catalog;
use genai::adapter::AdapterKind;

/// Where one model's requests go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub adapter: AdapterKind,
    /// With a trailing slash, as genai joins paths onto it.
    pub endpoint: String,
    /// The name the provider knows the model by (no catalogue prefix).
    pub model: String,
}

/// A hosted provider reached by prefix: (prefix, adapter, endpoint).
const HOSTED: &[(&str, AdapterKind, &str)] = &[
    ("openai", AdapterKind::OpenAI, "https://api.openai.com/v1/"),
    (
        "anthropic",
        AdapterKind::Anthropic,
        "https://api.anthropic.com/v1/",
    ),
    (
        "gemini",
        AdapterKind::Gemini,
        "https://generativelanguage.googleapis.com/v1beta/",
    ),
    ("cohere", AdapterKind::Cohere, "https://api.cohere.com/v1/"),
    ("groq", AdapterKind::Groq, "https://api.groq.com/openai/v1/"),
    (
        "deepseek",
        AdapterKind::DeepSeek,
        "https://api.deepseek.com/v1/",
    ),
    ("xai", AdapterKind::Xai, "https://api.x.ai/v1/"),
    (
        "mistral",
        AdapterKind::Mistral,
        "https://api.mistral.ai/v1/",
    ),
    (
        "openrouter",
        AdapterKind::OpenRouter,
        "https://openrouter.ai/api/v1/",
    ),
    (
        "together_ai",
        AdapterKind::Together,
        "https://api.together.xyz/v1/",
    ),
    (
        "perplexity",
        AdapterKind::OpenAI,
        "https://api.perplexity.ai/",
    ),
    (
        "fireworks_ai",
        AdapterKind::Fireworks,
        "https://api.fireworks.ai/inference/v1/",
    ),
    (
        "cerebras",
        AdapterKind::OpenAI,
        "https://api.cerebras.ai/v1/",
    ),
    // A ChatGPT plan: the Codex Responses endpoint, signed in through the
    // plan's own credential (the chatgpt part installs it).
    (
        "chatgpt",
        AdapterKind::OpenAIResp,
        "https://chatgpt.com/backend-api/codex/",
    ),
    // LiteLLM's own default for an `ollama/` id: the local server.
    ("ollama", AdapterKind::Ollama, "http://localhost:11434/"),
];

fn hosted(prefix: &str) -> Option<(AdapterKind, &'static str)> {
    HOSTED
        .iter()
        .find(|(p, _, _)| *p == prefix)
        .map(|(_, a, e)| (*a, *e))
}

/// The sentence for a provider the desktop does not reach.
pub fn unreachable_message(provider: &str) -> String {
    format!(
        "OpenAlgo cannot reach the provider '{}' from this computer. Choose a provider from the catalog, or an OpenAI-compatible endpoint.",
        provider
    )
}

/// An OpenAI model the catalogue lists as Responses-only goes to the
/// Responses API (LiteLLM routes it by the entry's `mode` the same way).
fn openai_adapter(catalog: &Catalog, id: &str) -> AdapterKind {
    let responses = catalog
        .entry(id)
        .or_else(|| catalog.entry(id.split_once('/').map(|(_, n)| n).unwrap_or(id)))
        .and_then(|e| e.mode.as_deref())
        == Some("responses");
    if responses {
        AdapterKind::OpenAIResp
    } else {
        AdapterKind::OpenAI
    }
}

/// The target for a row. The row is already through the gate
/// (`validate_provider_config`), so its base URL is present where needed.
pub fn target_for(
    catalog: &Catalog,
    kind: &str,
    model_name: &str,
    base_url: Option<&str>,
) -> Result<Target, String> {
    let spec = provider_spec(kind)?;
    let id = catalog_id(kind, model_name)?;
    let own_endpoint = || format!("{}/", normalize_base_url(base_url));
    let after_prefix = |prefix: &str| -> String {
        id.strip_prefix(&format!("{}/", prefix))
            .unwrap_or(&id)
            .to_string()
    };
    match spec.kind {
        "openai" => Ok(Target {
            adapter: openai_adapter(catalog, &id),
            endpoint: "https://api.openai.com/v1/".into(),
            model: after_prefix("openai"),
        }),
        "anthropic" => Ok(Target {
            adapter: AdapterKind::Anthropic,
            endpoint: "https://api.anthropic.com/v1/".into(),
            model: after_prefix("anthropic"),
        }),
        "ollama" => Ok(Target {
            adapter: AdapterKind::Ollama,
            endpoint: own_endpoint(),
            model: after_prefix("ollama"),
        }),
        "openai_compatible" => Ok(Target {
            adapter: AdapterKind::OpenAI,
            endpoint: own_endpoint(),
            model: after_prefix("openai"),
        }),
        _ => litellm_target(catalog, &id),
    }
}

/// A `litellm` row: the prefix names the provider; a bare name is placed
/// by the catalogue (`gpt-4o` is OpenAI's, as LiteLLM infers it).
fn litellm_target(catalog: &Catalog, id: &str) -> Result<Target, String> {
    let (prefix, model) = match id.split_once('/') {
        Some((p, rest)) if hosted(p).is_some() => (p.to_string(), rest.to_string()),
        Some((p, _)) if catalog.is_provider(p) => return Err(unreachable_message(p)),
        _ => match catalog.entry(id).map(|e| e.provider.clone()) {
            Some(p) if hosted(&p).is_some() => (p, id.to_string()),
            Some(p) => return Err(unreachable_message(&p)),
            None => return Err(unknown_provider_message(id)),
        },
    };
    let Some((adapter, endpoint)) = hosted(&prefix) else {
        return Err(unreachable_message(&prefix));
    };
    let adapter = if prefix == "openai" {
        openai_adapter(catalog, id)
    } else {
        adapter
    };
    Ok(Target {
        adapter,
        endpoint: endpoint.into(),
        model,
    })
}

/// A `litellm` name that names no provider the catalogue knows.
fn unknown_provider_message(id: &str) -> String {
    format!(
        "OpenAlgo cannot tell which provider serves {}. Put the provider before the model name, for example groq/llama-3.3-70b-versatile.",
        id
    )
}
