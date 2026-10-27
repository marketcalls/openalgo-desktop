//! Trader-facing wording for a failed run (the web's
//! `services/agent/error_messages.py` and the classification in
//! `stream.py`).
//!
//! A provider reports a failure in developer text: a status, a JSON body,
//! part of a key. It is read here to choose the sentence and kept in the
//! log, but never shown: the trader reads the cause in plain words and the
//! one thing to do next. A cause is named only when the failure says so.

use crate::agent::contract::frames::ErrorKind;
use crate::agent::redact::redact_text;
use regex::Regex;
use std::sync::LazyLock;

/// The sentence for an internal failure.
pub const INTERNAL_MESSAGE: &str =
    "Something went wrong while answering. The details are in the error log. Try again.";

/// A provider failure as read off the error: which kinds of failure the
/// error type names (the web's exception class names), its status, and its
/// own text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderFailure {
    pub names: Vec<&'static str>,
    pub status: Option<u16>,
    pub text: String,
}

/// Read a genai failure.
pub fn read_genai_error(err: &genai::Error) -> ProviderFailure {
    use genai::webc::Error as W;
    let web = |e: &W| -> ProviderFailure {
        match e {
            W::ResponseFailedStatus { status, body, .. } => ProviderFailure {
                names: Vec::new(),
                status: Some(status.as_u16()),
                text: body.clone(),
            },
            W::Reqwest(r) => ProviderFailure {
                names: if r.is_timeout() {
                    vec!["Timeout"]
                } else if r.is_connect() {
                    vec!["APIConnectionError"]
                } else {
                    Vec::new()
                },
                status: r.status().map(|s| s.as_u16()),
                text: r.to_string(),
            },
            other => ProviderFailure {
                text: other.to_string(),
                ..Default::default()
            },
        }
    };
    match err {
        genai::Error::HttpError { status, body, .. } => ProviderFailure {
            names: Vec::new(),
            status: Some(status.as_u16()),
            text: body.clone(),
        },
        genai::Error::WebModelCall { webc_error, .. }
        | genai::Error::WebAdapterCall { webc_error, .. } => web(webc_error),
        genai::Error::ChatResponse { body, .. } => ProviderFailure {
            text: body.to_string(),
            ..Default::default()
        },
        other => ProviderFailure {
            text: other.to_string(),
            ..Default::default()
        },
    }
}

/// Display names of providers, keyed lower case with non-alphanumerics
/// removed (the web's `_PROVIDER_LABELS`).
const PROVIDER_LABELS: &[(&str, &str)] = &[
    ("openai", "OpenAI"),
    ("textcompletionopenai", "OpenAI"),
    ("azure", "Azure OpenAI"),
    ("azureopenai", "Azure OpenAI"),
    ("anthropic", "Anthropic"),
    ("gemini", "Google Gemini"),
    ("googleaistudio", "Google Gemini"),
    ("vertexai", "Google Vertex AI"),
    ("groq", "Groq"),
    ("mistral", "Mistral"),
    ("deepseek", "DeepSeek"),
    ("openrouter", "OpenRouter"),
    ("ollama", "Ollama"),
    ("xai", "xAI"),
    ("cohere", "Cohere"),
    ("bedrock", "Amazon Bedrock"),
    ("togetherai", "Together AI"),
    ("fireworksai", "Fireworks AI"),
    ("perplexity", "Perplexity"),
    ("cerebras", "Cerebras"),
    ("sambanova", "SambaNova"),
    ("chatgpt", "ChatGPT"),
];

static EXCEPTION_PREFIX: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\b([A-Za-z_]+?)Exception\b").ok());

static STATUS_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#""status"\s*:\s*(\d{3})"#,
        r"(?i)\berror code:?\s*(\d{3})",
        r"(?i)\bstatus[_ ]code\s*[=:]?\s*(\d{3})",
        r"\bHTTP\s+(\d{3})\b",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Name the provider behind a failure: the provider the run used, else one
/// named in the text.
pub fn provider_label(text: &str, provider: Option<&str>) -> Option<&'static str> {
    let mut candidates: Vec<String> = provider.map(str::to_string).into_iter().collect();
    if let Some(re) = EXCEPTION_PREFIX.as_ref() {
        candidates.extend(
            re.captures_iter(text)
                .filter_map(|c| c.get(1))
                .map(|m| m.as_str().to_string()),
        );
    }
    candidates.iter().find_map(|c| {
        let key: String = c
            .to_lowercase()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect();
        PROVIDER_LABELS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, l)| *l)
    })
}

/// The status of a failure, from the error or its text.
pub fn status_code(text: &str, status: Option<u16>) -> Option<u16> {
    if let Some(s) = status.filter(|s| (100..=599).contains(s)) {
        return Some(s);
    }
    STATUS_PATTERNS.iter().find_map(|re| {
        re.captures(text)
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse().ok())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Context,
    Quota,
    Auth,
    Permission,
    NotFound,
    Rate,
    Content,
    Timeout,
    Connection,
    Unavailable,
    BadRequest,
}

const CATEGORY_NAMES: &[(Category, &[&str])] = &[
    (Category::Context, &["ContextWindowExceededError"]),
    (
        Category::Auth,
        &["AuthenticationError", "ModelAuthenticationError"],
    ),
    (Category::Permission, &["PermissionDeniedError"]),
    (Category::NotFound, &["NotFoundError"]),
    (Category::Rate, &["RateLimitError", "ModelRateLimitError"]),
    (Category::Content, &["ContentPolicyViolationError"]),
    (Category::Timeout, &["Timeout", "APITimeoutError"]),
    (Category::Connection, &["APIConnectionError"]),
    (
        Category::Unavailable,
        &[
            "ServiceUnavailableError",
            "InternalServerError",
            "RemoteServerUnavailableError",
        ],
    ),
];

const GENERIC_NAMES: &[&str] = &["BadRequestError", "UnprocessableEntityError"];

const CATEGORY_MARKERS: &[(Category, &[&str])] = &[
    (
        Category::Context,
        &[
            "context_length_exceeded",
            "maximum context length",
            "context window",
            "prompt is too long",
            "too many tokens",
        ],
    ),
    (
        Category::Quota,
        &[
            "insufficient_quota",
            "exceeded your current quota",
            "credit balance is too low",
            "insufficient credits",
            "insufficient balance",
            "billing",
            "payment required",
        ],
    ),
    (
        Category::Auth,
        &[
            "invalid_api_key",
            "incorrect api key",
            "invalid api key",
            "invalid x-api-key",
            "api key not valid",
            "api_key_invalid",
            "authenticationerror",
            "unauthorized",
        ],
    ),
    (
        Category::Permission,
        &[
            "permissiondeniederror",
            "permission denied",
            "does not have access",
        ],
    ),
    (
        Category::NotFound,
        &[
            "model_not_found",
            "notfounderror",
            "does not exist",
            "model is not supported",
        ],
    ),
    (
        Category::Rate,
        &[
            "rate_limit",
            "rate limit",
            "ratelimiterror",
            "too many requests",
        ],
    ),
    (
        Category::Content,
        &[
            "content_policy",
            "contentpolicyviolation",
            "content management policy",
        ],
    ),
    (Category::Timeout, &["timed out", "timeout"]),
    (
        Category::Connection,
        &[
            "apiconnectionerror",
            "connection error",
            "connection refused",
            "name or service not known",
            "temporary failure in name resolution",
        ],
    ),
    (
        Category::Unavailable,
        &[
            "overloaded",
            "service unavailable",
            "internalservererror",
            "bad gateway",
        ],
    ),
];

fn by_status(status: u16) -> Option<Category> {
    match status {
        401 => Some(Category::Auth),
        402 => Some(Category::Quota),
        403 => Some(Category::Permission),
        404 => Some(Category::NotFound),
        408 | 504 => Some(Category::Timeout),
        413 => Some(Category::Context),
        429 => Some(Category::Rate),
        s if s >= 500 => Some(Category::Unavailable),
        _ => None,
    }
}

fn category(names: &[&str], text: &str, status: Option<u16>) -> Option<Category> {
    let lowered = text.to_lowercase();
    let marked = |list: &[(Category, &[&str])]| {
        list.iter()
            .find(|(_, markers)| markers.iter().any(|m| lowered.contains(m)))
            .map(|(c, _)| *c)
    };
    // An exhausted balance usually arrives as a rate limit, and an
    // over-long prompt as a bad request: their markers go first.
    if let Some(c) = marked(&CATEGORY_MARKERS[..2]) {
        return Some(c);
    }
    if let Some((c, _)) = CATEGORY_NAMES
        .iter()
        .find(|(_, cls)| cls.iter().any(|n| names.contains(n)))
    {
        return Some(*c);
    }
    if let Some(c) = marked(&CATEGORY_MARKERS[2..]) {
        return Some(c);
    }
    if GENERIC_NAMES.iter().any(|n| names.contains(n)) {
        return Some(Category::BadRequest);
    }
    status.and_then(by_status)
}

fn sentence(cat: Option<Category>, label: Option<&str>) -> String {
    if label == Some("ChatGPT") {
        match cat {
            Some(Category::Auth) => {
                return "Your ChatGPT sign-in has expired or was removed. Sign in to ChatGPT again from agent settings.".into()
            }
            Some(Category::Permission | Category::NotFound) => {
                return "ChatGPT could not run the model chosen in agent settings. Choose another model there.".into()
            }
            _ => {}
        }
    }
    let who = label.unwrap_or("the AI provider");
    let cap = label.unwrap_or("The AI provider");
    match cat {
        Some(Category::Context) => "This conversation has grown too long for the model. Start a new chat and ask again.".into(),
        Some(Category::Quota) => format!("Your {} account has no credits left. Add credits under billing, then try again.", who),
        Some(Category::Auth) => format!("{} did not accept the API key for this model. Enter a valid key in agent settings.", cap),
        Some(Category::Permission) => format!(
            "{} does not allow this API key to use this model. Check the key's access, or choose another model in agent settings.",
            cap
        ),
        Some(Category::NotFound) => format!("{} does not offer the model chosen in agent settings. Choose another model there.", cap),
        Some(Category::Rate) => format!("{} is limiting requests right now. Wait a minute, then try again.", cap),
        Some(Category::Content) => format!("{} declined to answer this request. Rephrase it and try again.", cap),
        Some(Category::Timeout) => format!("{} took too long to answer. Try again.", cap),
        Some(Category::Connection) => format!("OpenAlgo could not reach {}. Check that the server is online, then try again.", who),
        Some(Category::Unavailable) => format!("{} is having trouble right now. Try again in a few minutes.", cap),
        Some(Category::BadRequest) => format!(
            "{} could not process this request. Try again, or choose another model in agent settings.",
            cap
        ),
        None => format!(
            "{} could not complete this request. Try again, or check the model in agent settings.",
            cap
        ),
    }
}

/// The sentence a trader sees for a provider failure; the provider's own
/// text (redacted) goes to the log.
pub fn provider_message(failure: &ProviderFailure, provider_kind: &str) -> String {
    let detail = redact_text(failure.text.trim());
    let code = status_code(&detail, failure.status);
    let text = sentence(
        category(&failure.names, &detail, code),
        provider_label(&detail, Some(provider_kind)),
    );
    tracing::warn!(
        "Agent run failed ({:?}); shown to the trader as: {} | provider said: {}",
        ErrorKind::Provider,
        text,
        if detail.is_empty() {
            "The run failed without reporting a reason."
        } else {
            detail.as_str()
        }
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(names: Vec<&'static str>, status: Option<u16>, text: &str, provider: &str) -> String {
        provider_message(
            &ProviderFailure {
                names,
                status,
                text: text.into(),
            },
            provider,
        )
    }

    #[test]
    fn each_cause_reads_as_the_web_words_it() {
        assert_eq!(
            said(
                vec![],
                Some(429),
                "You exceeded your current quota",
                "openai"
            ),
            "Your OpenAI account has no credits left. Add credits under billing, then try again."
        );
        assert_eq!(
            said(vec![], Some(401), "{\"error\":{\"code\":\"invalid_api_key\"}}", "anthropic"),
            "Anthropic did not accept the API key for this model. Enter a valid key in agent settings."
        );
        assert_eq!(
            said(vec![], Some(429), "slow down", "groq"),
            "Groq is limiting requests right now. Wait a minute, then try again."
        );
        assert_eq!(
            said(
                vec!["APIConnectionError"],
                None,
                "error sending request",
                "ollama"
            ),
            "OpenAlgo could not reach Ollama. Check that the server is online, then try again."
        );
        assert_eq!(
            said(
                vec![],
                Some(403),
                "Enable JavaScript and cookies",
                "chatgpt"
            ),
            "ChatGPT could not run the model chosen in agent settings. Choose another model there."
        );
        assert_eq!(
            said(vec![], None, "something odd", "nobody"),
            "The AI provider could not complete this request. Try again, or check the model in agent settings."
        );
        assert_eq!(
            said(
                vec![],
                Some(400),
                "This model's maximum context length is 8192",
                "openai"
            ),
            "This conversation has grown too long for the model. Start a new chat and ask again."
        );
    }

    #[test]
    fn the_status_is_read_from_the_text_when_the_error_has_none() {
        assert_eq!(status_code("Error code: 529 overloaded", None), Some(529));
        assert_eq!(status_code(r#"{"status": 503}"#, None), Some(503));
        assert_eq!(status_code("nothing", Some(700)), None);
    }
}
