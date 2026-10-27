//! The model a run talks to, and who decides it (the web's
//! `builder.ResolvedModel` and `resolve_model`).
//!
//! The config lane implements [`ModelResolver`] over the trader's saved
//! models, keys and ChatGPT sign-in; the run only ever sees a
//! [`ResolvedModel`]. Until a model is configured the resolver answers
//! [`ModelError::NotConfigured`], the setup gate.
//!
//! A credential is fetched at the moment of each provider request through
//! [`ProviderCredentials`], held only for that request and never cached on
//! a long-lived object, logged, or placed in the model's context.

use crate::agent::contract::frames::ErrorKind;
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use std::sync::Arc;

/// Why no model could be used for a run (the web's `AgentBuildError`
/// family). Raised before a stream opens, so a route answers with
/// [`ModelError::status`] and the message, never a half-written answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// No model was named and no enabled default exists (409).
    NotConfigured(String),
    /// The named model is not registered (404).
    NotFound(String),
    /// The named model is switched off (409).
    Disabled(String),
    /// The provider needs a key, or a ChatGPT sign-in, that is missing (409).
    MissingCredential(String),
    /// The saved row does not describe a usable model (409).
    InvalidConfig(String),
    /// The turn carries an image and the model cannot read one (400).
    VisionUnsupported(String),
    /// The resolver itself failed (a database read); 500, kind internal.
    Internal(String),
}

impl ModelError {
    /// The sentence shown to the trader. Never carries a secret.
    pub fn message(&self) -> &str {
        match self {
            ModelError::NotConfigured(m)
            | ModelError::NotFound(m)
            | ModelError::Disabled(m)
            | ModelError::MissingCredential(m)
            | ModelError::InvalidConfig(m)
            | ModelError::VisionUnsupported(m)
            | ModelError::Internal(m) => m,
        }
    }

    /// The HTTP status a route answers with.
    pub fn status(&self) -> u16 {
        match self {
            ModelError::NotFound(_) => 404,
            ModelError::VisionUnsupported(_) => 400,
            ModelError::Internal(_) => 500,
            _ => 409,
        }
    }

    /// The `kind` the route's error body carries.
    pub fn kind(&self) -> ErrorKind {
        match self {
            ModelError::VisionUnsupported(_) => ErrorKind::Input,
            ModelError::Internal(_) => ErrorKind::Internal,
            _ => ErrorKind::Config,
        }
    }
}

/// The web's message when nothing is configured (`ModelNotConfigured`).
pub const NOT_CONFIGURED_MESSAGE: &str =
    "No default model is configured. Add a model and set it as the default before using the agent.";

/// What a model can do, as the trader set it (never inferred over them).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelCapabilities {
    /// Reasoning effort may be sent.
    pub reasoning: bool,
    /// `off`, `low`, `medium` or `high`; `off` defers to the Agent setting.
    pub default_reasoning_effort: String,
    /// Images may be attached to a turn.
    pub vision: bool,
    /// The trader flagged the model's tool calling as unreliable; the run
    /// warns in the log (the agent is tool driven).
    pub tools_unreliable: bool,
}

/// Per-token prices in US dollars (LiteLLM's `input_cost_per_token`,
/// `output_cost_per_token`, `cache_read_input_token_cost`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Prices {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
}

/// How a turn on this model is paid for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pricing {
    /// Billed per token; `None` when the price table has no entry, so a
    /// turn reports tokens with an unknown cost rather than a guess.
    Metered(Option<Prices>),
    /// Out of a ChatGPT Plus or Pro plan: tokens, never a cost.
    Subscription,
}

/// Rounding of a computed cost (the web's `COST_DECIMALS`).
const COST_DECIMALS: i32 = 10;

impl Pricing {
    /// The web's `catalog.estimate_cost`: cached prompt tokens are a subset
    /// of the input, charged at the cache price (or the input price when
    /// none is published); a side that was used without a price makes the
    /// cost unknown. A plan turn has no cost.
    pub fn cost(&self, input: u64, output: u64, cached: u64) -> Option<f64> {
        let p = match self {
            Pricing::Subscription | Pricing::Metered(None) => return None,
            Pricing::Metered(Some(p)) => p,
        };
        let cached = cached.min(input);
        let billed_in = input - cached;
        let cache_price = p.cache_read.or(p.input);
        if (billed_in > 0 || cached > 0) && p.input.is_none() {
            return None;
        }
        if output > 0 && p.output.is_none() {
            return None;
        }
        if input == 0 && output == 0 {
            return Some(0.0);
        }
        let total = billed_in as f64 * p.input.unwrap_or(0.0)
            + cached as f64 * cache_price.unwrap_or(0.0)
            + output as f64 * p.output.unwrap_or(0.0);
        let scale = 10f64.powi(COST_DECIMALS);
        Some((total * scale).round() / scale)
    }

    pub fn is_subscription(&self) -> bool {
        matches!(self, Pricing::Subscription)
    }
}

/// What one provider request authenticates with.
pub struct ProviderAuth {
    /// The API key or access token; `None` for a keyless provider (a local
    /// Ollama).
    pub key: Option<Secret>,
    /// Extra request headers the provider needs (a ChatGPT plan's account
    /// id). Never a secret beyond what `key` holds.
    pub headers: Vec<(String, String)>,
}

impl std::fmt::Debug for ProviderAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderAuth")
            .field("key", &self.key.as_ref().map(|_| "[REDACTED]"))
            .field("headers", &self.headers.len())
            .finish()
    }
}

/// Fetches a model's credentials for one request: decrypts a saved key, or
/// refreshes a ChatGPT plan's token when it is about to expire.
#[async_trait]
pub trait ProviderCredentials: Send + Sync {
    async fn fetch(&self) -> Result<ProviderAuth, ModelError>;
}

/// A provider that needs nothing (a local Ollama), and the test double.
pub struct NoCredentials;

#[async_trait]
impl ProviderCredentials for NoCredentials {
    async fn fetch(&self) -> Result<ProviderAuth, ModelError> {
        Ok(ProviderAuth {
            key: None,
            headers: Vec::new(),
        })
    }
}

/// The model a run uses.
#[derive(Clone)]
pub struct ResolvedModel {
    /// The `ag_provider_model` row.
    pub id: i64,
    /// `openai`, `anthropic`, `ollama`, `openai_compatible`, `litellm`,
    /// `chatgpt`: the web's provider vocabulary.
    pub provider_kind: String,
    /// The provider's own model name (`gpt-4o-mini`).
    pub model_name: String,
    /// Shown in the picker and in messages.
    pub display_name: String,
    /// The catalogue id the web prices and reports by (LiteLLM's,
    /// `openai/gpt-4o-mini`, `chatgpt/gpt-5.4`); the usage frame's `model`
    /// until the provider reports its own.
    pub catalog_id: String,
    /// Which genai adapter speaks to the provider.
    pub adapter: genai::adapter::AdapterKind,
    /// The provider's base URL (`https://api.openai.com/v1/`, a local
    /// Ollama's); a trailing slash as genai expects.
    pub endpoint: String,
    pub credentials: Arc<dyn ProviderCredentials>,
    pub capabilities: ModelCapabilities,
    /// Prompt tokens the model accepts, when the catalogue knows.
    pub context_window: Option<u32>,
    pub pricing: Pricing,
}

impl std::fmt::Debug for ResolvedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModel")
            .field("id", &self.id)
            .field("provider_kind", &self.provider_kind)
            .field("catalog_id", &self.catalog_id)
            .field("adapter", &self.adapter)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

/// Decides which model a run uses (implemented by the config lane).
#[async_trait]
pub trait ModelResolver: Send + Sync {
    /// The setup gate: an enabled, tested default model exists. Reads the
    /// database; callers run it off the async runtime.
    fn is_configured(&self, app: &AppState) -> bool;

    /// The model for a run: the requested one, else the default. A named
    /// model that is missing or disabled is an error, never a silent fall
    /// back to the default.
    async fn resolve(
        &self,
        app: &Arc<AppState>,
        requested: Option<i64>,
    ) -> Result<ResolvedModel, ModelError>;
}

/// The resolver before any model is configured: every run is refused with
/// the setup gate's answer.
pub struct NoModelConfigured;

#[async_trait]
impl ModelResolver for NoModelConfigured {
    fn is_configured(&self, _app: &AppState) -> bool {
        false
    }

    async fn resolve(
        &self,
        _app: &Arc<AppState>,
        requested: Option<i64>,
    ) -> Result<ResolvedModel, ModelError> {
        Err(match requested {
            Some(id) => ModelError::NotFound(format!(
                "Model {} is not registered. Choose one of the configured models.",
                id
            )),
            None => ModelError::NotConfigured(NOT_CONFIGURED_MESSAGE.into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_is_the_web_arithmetic() {
        let p = Pricing::Metered(Some(Prices {
            input: Some(0.000_002_5),
            output: Some(0.000_01),
            cache_read: Some(0.000_001_25),
        }));
        // 800 billed in, 200 cached, 100 out.
        let c = p.cost(1000, 100, 200).unwrap();
        assert!(
            (c - (800.0 * 0.000_002_5 + 200.0 * 0.000_001_25 + 100.0 * 0.000_01)).abs() < 1e-12
        );
        // Cached beyond the prompt is clamped.
        assert_eq!(p.cost(10, 0, 50), p.cost(10, 0, 10));
        assert_eq!(p.cost(0, 0, 0), Some(0.0));
        // No cache price: charged at the input price.
        let q = Pricing::Metered(Some(Prices {
            input: Some(1.0),
            output: Some(2.0),
            cache_read: None,
        }));
        assert_eq!(q.cost(10, 1, 4), Some(12.0));
        // A used side without a price is unknown; an unused one is not.
        let r = Pricing::Metered(Some(Prices {
            input: Some(1.0),
            output: None,
            cache_read: None,
        }));
        assert_eq!(r.cost(3, 0, 0), Some(3.0));
        assert_eq!(r.cost(3, 1, 0), None);
        assert_eq!(Pricing::Metered(None).cost(3, 1, 0), None);
        assert_eq!(Pricing::Subscription.cost(3, 1, 0), None);
    }

    #[test]
    fn errors_answer_with_the_web_status_and_kind() {
        assert_eq!(ModelError::NotFound("x".into()).status(), 404);
        assert_eq!(ModelError::Disabled("x".into()).status(), 409);
        let v = ModelError::VisionUnsupported("x".into());
        assert_eq!((v.status(), v.kind()), (400, ErrorKind::Input));
        assert_eq!(
            ModelError::NotConfigured("x".into()).kind(),
            ErrorKind::Config
        );
    }
}
