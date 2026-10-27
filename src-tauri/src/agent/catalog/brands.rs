//! What the provider grid says about each provider (the web's `_BRANDS`,
//! `_KEYLESS_PROVIDERS`, `_BASE_URL_PROVIDERS` and `_PROVIDER_KINDS` in
//! `services/agent/catalog.py`), for the providers the desktop offers.
//!
//! The icon is a slug the page draws its own monogram for, never a logo,
//! a URL or an emoji.

/// (provider id, display name, icon slug).
const BRANDS: &[(&str, &str, &str)] = &[
    ("anthropic", "Anthropic", "anthropic"),
    ("cerebras", "Cerebras", "cerebras"),
    ("chatgpt", "ChatGPT", "openai"),
    ("cohere", "Cohere", "cohere"),
    ("custom", "Custom endpoint", ""),
    ("custom_openai", "Custom OpenAI endpoint", "openai"),
    ("deepseek", "DeepSeek", "deepseek"),
    ("fireworks_ai", "Fireworks AI", "fireworks"),
    ("gemini", "Google Gemini", "google"),
    ("groq", "Groq", "groq"),
    ("llamafile", "llamafile", "llamafile"),
    ("lm_studio", "LM Studio", "lmstudio"),
    ("mistral", "Mistral AI", "mistral"),
    ("ollama", "Ollama", "ollama"),
    ("openai", "OpenAI", "openai"),
    ("openai_like", "OpenAI-compatible endpoint", "openai"),
    ("openrouter", "OpenRouter", "openrouter"),
    ("perplexity", "Perplexity", "perplexity"),
    ("together_ai", "Together AI", "together"),
    ("vllm", "vLLM", "vllm"),
    ("xai", "xAI", "xai"),
];

/// Providers a pasted key does not apply to: a server on the trader's own
/// machine or network, and the ChatGPT plan (signed in, not keyed). Each
/// maps onto a kind whose save gate asks for no key either (D-027).
const KEYLESS: &[&str] = &[
    "chatgpt",
    "custom",
    "custom_openai",
    "llamafile",
    "lm_studio",
    "ollama",
    "openai_like",
    "vllm",
];

/// Providers whose endpoint the trader names.
const NEEDS_BASE_URL: &[&str] = &[
    "custom",
    "custom_openai",
    "llamafile",
    "lm_studio",
    "ollama",
    "openai_like",
    "vllm",
];

/// The `provider_kind` suggested for a provider's models; `litellm` (the
/// prefixed id) for the rest.
const KINDS: &[(&str, &str)] = &[
    ("anthropic", "anthropic"),
    ("custom", "openai_compatible"),
    ("custom_openai", "openai_compatible"),
    ("llamafile", "openai_compatible"),
    ("lm_studio", "openai_compatible"),
    ("ollama", "ollama"),
    ("openai", "openai"),
    ("openai_like", "openai_compatible"),
    ("vllm", "openai_compatible"),
];

pub const DEFAULT_PROVIDER_KIND: &str = "litellm";

/// Display name and icon; an unknown provider shows its id and no icon.
pub fn brand(id: &str) -> (String, String) {
    BRANDS
        .iter()
        .find(|(p, _, _)| *p == id)
        .map(|(_, name, icon)| (name.to_string(), icon.to_string()))
        .unwrap_or_else(|| (id.to_string(), String::new()))
}

/// The display name of a provider the grid knows.
pub fn known_name(id: &str) -> Option<&'static str> {
    BRANDS.iter().find(|(p, _, _)| *p == id).map(|(_, name, _)| *name)
}

pub fn needs_key(id: &str) -> bool {
    !KEYLESS.contains(&id)
}

pub fn needs_base_url(id: &str) -> bool {
    NEEDS_BASE_URL.contains(&id)
}

pub fn provider_kind(id: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(p, _)| *p == id)
        .map(|(_, k)| *k)
        .unwrap_or(DEFAULT_PROVIDER_KIND)
}
