//! Provider kinds and model addressing (the web's
//! `services/agent/providers.py`), and how a registered model reaches its
//! provider through genai.
//!
//! - this file: the five kinds the registry stores, what each needs (a key,
//!   a base URL), the catalogue id a model is priced and described by, the
//!   save and run gate's sentences, and the capability checks that let the
//!   catalogue overrule a wrongly ticked box.
//! - `target`: the genai adapter, endpoint and model name for a row
//!   (decision 9 of the 1.0.4 plan: what genai reaches is offered;
//!   Bedrock, SageMaker and Vertex are not).
//! - `base_url`: the one guard on a trader-supplied endpoint (D-028).
//! - `egress`: where the Agent's HTTP client may connect (no link-local
//!   address, no redirect), for what the text guard cannot see.
//!
//! Desktop differences: an OpenAI-compatible endpoint takes a key when it
//! has one and works without one, as a local server (LM Studio, vLLM,
//! llamafile) does; the web asked every such endpoint for a key while its
//! catalogue said they needed none (D-027). A sentence about a `litellm`
//! row names its provider, not LiteLLM (decision 9).

pub mod base_url;
pub mod egress;
pub mod target;

#[cfg(test)]
mod egress_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod trader_sentence_tests;

use crate::agent::catalog::Catalog;

/// The closed vocabulary `ag_provider_model.provider_kind` holds, in the
/// web's order.
pub const PROVIDER_KINDS: &[&str] = &[
    "openai",
    "anthropic",
    "ollama",
    "openai_compatible",
    "litellm",
];

/// Kinds that address an endpoint of the trader's own and so need one.
pub const BASE_URL_KINDS: &[&str] = &["ollama", "openai_compatible"];

/// What one kind needs and how its models are addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    pub kind: &'static str,
    /// Used in the gate's sentences ("Ollama requires a base URL").
    pub label: &'static str,
    pub needs_key: bool,
    pub needs_base_url: bool,
    /// The catalogue prefix put before the model name; empty for `litellm`,
    /// whose model name carries its own.
    pub prefix: &'static str,
}

const SPECS: &[ProviderSpec] = &[
    ProviderSpec {
        kind: "openai",
        label: "OpenAI",
        needs_key: true,
        needs_base_url: false,
        prefix: "openai",
    },
    ProviderSpec {
        kind: "anthropic",
        label: "Anthropic",
        needs_key: true,
        needs_base_url: false,
        prefix: "anthropic",
    },
    ProviderSpec {
        kind: "ollama",
        label: "Ollama",
        needs_key: false,
        needs_base_url: true,
        prefix: "ollama",
    },
    ProviderSpec {
        kind: "openai_compatible",
        label: "OpenAI-compatible endpoint",
        needs_key: false,
        needs_base_url: true,
        prefix: "openai",
    },
    ProviderSpec {
        kind: "litellm",
        label: "LiteLLM",
        needs_key: true,
        needs_base_url: false,
        prefix: "",
    },
];

/// The spec of a kind (trimmed), or the web's unknown-kind sentence.
pub fn provider_spec(kind: &str) -> Result<&'static ProviderSpec, String> {
    let kind = kind.trim();
    SPECS
        .iter()
        .find(|s| s.kind == kind)
        .ok_or_else(|| unknown_kind_message(kind))
}

/// "Unknown provider kind 'x'. Expected one of: openai, ..." (the kind as
/// Python's `repr` writes it).
pub fn unknown_kind_message(kind: &str) -> String {
    format!(
        "Unknown provider kind {}. Expected one of: {}",
        crate::agent::settings::pyrepr::repr_str(kind),
        PROVIDER_KINDS.join(", ")
    )
}

/// Whitespace and trailing slashes off; empty when nothing usable is left.
pub fn normalize_base_url(raw: Option<&str>) -> String {
    raw.unwrap_or("").trim().trim_end_matches('/').to_string()
}

/// The ChatGPT plan's model prefix: a `chatgpt/` model runs on the plan's
/// sign-in, not on a key, and is billed to the plan.
pub const PLAN_PREFIX: &str = "chatgpt/";

/// Whether a model runs on a ChatGPT plan (the prefix decides; the web
/// consults the catalogue as a second opinion, and the catalogue lists no
/// plan model under another name).
pub fn is_subscription(model: &str) -> bool {
    model.trim().starts_with(PLAN_PREFIX)
}

/// The id the catalogue knows a row by (the web's `litellm_model_id`):
/// the kind's prefix put before the name once at most, the `litellm` kind's
/// name as typed.
pub fn catalog_id(kind: &str, model_name: &str) -> Result<String, String> {
    let spec = provider_spec(kind)?;
    let name = model_name.trim();
    if name.is_empty() {
        return Err("A model name is required".into());
    }
    if spec.prefix.is_empty() || name.starts_with(&format!("{}/", spec.prefix)) {
        return Ok(name.to_string());
    }
    Ok(format!("{}/{}", spec.prefix, name))
}

/// The save, test and run gate (the web's `validate_provider_config`): the
/// first problem as the sentence the trader reads, or none. Takes whether a
/// key exists, never the key.
pub fn validate_provider_config(
    kind: &str,
    model_name: &str,
    base_url: Option<&str>,
    has_key: bool,
) -> Option<String> {
    let spec = match provider_spec(kind) {
        Ok(s) => s,
        Err(message) => return Some(message),
    };
    if model_name.trim().is_empty() {
        return Some("A model name is required".into());
    }
    let url = normalize_base_url(base_url);
    if spec.needs_base_url && url.is_empty() {
        return Some(format!("{} requires a base URL", spec.label));
    }
    // Case-insensitive, as the save guard reads the scheme (the web's run
    // gate refused `HTTP://` after its save guard had accepted it).
    let lower = url.to_ascii_lowercase();
    if !url.is_empty() && !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Some("The base URL must start with http:// or https://".into());
    }
    if spec.needs_key && !has_key && !is_subscription(model_name) {
        return Some(format!(
            "{} requires an API key",
            key_label(spec, model_name)
        ));
    }
    None
}

/// Who asks for the key, in the sentence: a `litellm` row's own provider
/// by its prefix ("Groq requires an API key"), never the library's name,
/// which means nothing to a trader.
fn key_label(spec: &ProviderSpec, model_name: &str) -> String {
    if spec.kind != "litellm" {
        return spec.label.to_string();
    }
    model_name
        .trim()
        .split_once('/')
        .and_then(|(prefix, _)| crate::agent::catalog::brands::known_name(prefix))
        .unwrap_or("This provider")
        .to_string()
}

/// What the catalogue says about one capability of a model: `None` when it
/// does not know the model (the web's `_litellm_opinion`). Looked up with
/// its prefix, then by the bare name; a flag the entry leaves out is read
/// from the bare name's entry, as LiteLLM's own lookup does.
fn catalogue_opinion(catalog: &Catalog, id: &str, probe: Probe) -> Option<bool> {
    let bare = id.split_once('/').map(|(_, rest)| rest).unwrap_or(id);
    let entry = catalog.entry(id).or_else(|| catalog.entry(bare))?;
    let answer = probe
        .read(entry)
        .or_else(|| catalog.entry(bare).and_then(|e| probe.read(e)));
    Some(answer.unwrap_or(false))
}

/// The capability a model is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Reasoning,
    Vision,
}

impl Probe {
    fn read(self, entry: &crate::agent::catalog::Entry) -> Option<bool> {
        match self {
            Probe::Reasoning => entry.supports_reasoning,
            Probe::Vision => entry.supports_vision,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Probe::Reasoning => "reasoning",
            Probe::Vision => "vision",
        }
    }
}

/// The capability honoured for a model (the web's `_resolved_capability`):
/// the catalogue decides for a model it knows, even over a ticked box; the
/// trader's box decides for one it has never heard of. A reasoning effort
/// sent to a model that cannot reason, or an image to one that cannot see,
/// fails every turn or is silently ignored.
pub fn capable(catalog: &Catalog, id: &str, operator_flag: bool, probe: Probe) -> bool {
    match catalogue_opinion(catalog, id, probe) {
        None => operator_flag,
        Some(opinion) => {
            if operator_flag && !opinion {
                tracing::info!(
                    "Model {} is marked as supporting {} but the model catalogue says it does not; the capability is treated as absent",
                    id,
                    probe.label()
                );
            }
            opinion
        }
    }
}
