//! The model catalogue (the web's `services/agent/catalog.py`): the
//! providers the setup page offers, their models with context window,
//! price and tool support, and the price table a turn is costed from.
//!
//! The web reads it from LiteLLM, which fetches its price and context
//! table from GitHub when imported (5 s) and falls back to the copy in its
//! wheel. The desktop bundles that copy, recorded from the web's pinned
//! LiteLLM (`snapshot.json`, `scripts/agent/capture_catalog.py`), and
//! refreshes the table once at startup from the same address with the same
//! 5 s timeout (`refresh`). The catalogue is advisory: a model it does not
//! list can still be added by name, and one it cannot price shows its
//! tokens with an unknown cost, never a guessed one or zero. At LiteLLM
//! 1.106.6 the table lists no OpenRouter model, so its picker is empty and
//! its models are added by name, as on the web (8a2fbf1d0).
//!
//! - `brands`: names, icons and what each provider needs.
//! - `load`: the snapshot and a fetched table as a [`Catalog`].
//! - `refresh`: the startup refresh.
//!
//! One catalogue serves the process, read-only, swapped whole when the
//! refresh lands ([`current`]).

pub mod brands;
pub mod load;
pub mod refresh;

#[cfg(test)]
mod tests;

use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// Prices are shown per million tokens, rounded to 6 places (display only).
const TOKENS_PER_MILLION: f64 = 1_000_000.0;
const PRICE_DECIMALS: usize = 6;
/// A turn's cost is rounded to 10 places.
const COST_DECIMALS: usize = 10;

/// The modes a conversation can run in (`responses` is OpenAI's Responses
/// API; `completion` keeps legacy-only providers listed).
pub const CHAT_MODES: &[&str] = &["chat", "completion", "responses"];

/// One price table entry, the fields the catalogue reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    /// LiteLLM's `litellm_provider`.
    pub provider: String,
    pub mode: Option<String>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// US dollars per token.
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub cache_read_cost: Option<f64>,
    pub supports_function_calling: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_reasoning: Option<bool>,
}

/// The catalogue.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: HashMap<String, Entry>,
    /// Every provider id LiteLLM names: what makes a slash in a model id a
    /// provider prefix.
    provider_ids: HashSet<String>,
    /// The offered providers in LiteLLM's order, with their model names.
    providers: Vec<(String, Vec<String>)>,
}

static CURRENT: LazyLock<RwLock<Arc<Catalog>>> =
    LazyLock::new(|| RwLock::new(Arc::new(load::bundled())));

/// The catalogue now: the bundled one until the startup refresh replaces it.
pub fn current() -> Arc<Catalog> {
    CURRENT.read().clone()
}

/// Swap the whole catalogue (the refresh).
fn replace(next: Catalog) {
    *CURRENT.write() = Arc::new(next);
}

/// One provider card (the web's `ProviderInfo.as_dict`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub icon: String,
    pub provider_kind: String,
    pub needs_key: bool,
    pub needs_base_url: bool,
    pub model_count: usize,
    pub total_model_count: usize,
}

impl ProviderInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "display_name": self.display_name,
            "icon": self.icon,
            "provider_kind": self.provider_kind,
            "needs_key": self.needs_key,
            "needs_base_url": self.needs_base_url,
            "model_count": self.model_count,
            "total_model_count": self.total_model_count,
        })
    }
}

/// One catalogue model (the web's `ModelInfo.as_dict`). Every enriched
/// field is `None` when the table does not say (unknown, not zero).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub catalog_key: Option<String>,
    pub entry: Option<Entry>,
}

impl ModelInfo {
    /// The id with its provider before it, unless already there: what a
    /// `litellm` row stores.
    pub fn qualified_id(&self) -> String {
        if self.provider.is_empty() || self.id.starts_with(&format!("{}/", self.provider)) {
            self.id.clone()
        } else {
            format!("{}/{}", self.provider, self.id)
        }
    }

    fn mode(&self) -> Option<&str> {
        self.entry.as_ref().and_then(|e| e.mode.as_deref())
    }

    /// Conversational, or of unknown mode (an unpriced model is kept).
    pub fn is_chat(&self) -> bool {
        self.mode().is_none_or(|m| CHAT_MODES.contains(&m))
    }

    pub fn supports_function_calling(&self) -> Option<bool> {
        self.entry
            .as_ref()
            .and_then(|e| e.supports_function_calling)
    }

    pub fn to_json(&self) -> Value {
        let e = self.entry.clone().unwrap_or_default();
        json!({
            "id": self.id,
            "provider": self.provider,
            "qualified_id": self.qualified_id(),
            "catalog_key": self.catalog_key,
            "mode": e.mode,
            "max_input_tokens": e.max_input_tokens,
            "max_output_tokens": e.max_output_tokens,
            "input_price_per_million": e.input_cost.map(per_million),
            "output_price_per_million": e.output_cost.map(per_million),
            "supports_function_calling": e.supports_function_calling,
            "supports_vision": e.supports_vision,
            "supports_reasoning": e.supports_reasoning,
            "in_catalog": self.catalog_key.is_some(),
            "is_chat": self.is_chat(),
        })
    }
}

/// Python's `round(value, places)` for display: the correctly rounded
/// decimal, read back.
fn round_to(value: f64, places: usize) -> f64 {
    format!("{:.*}", places, value).parse().unwrap_or(value)
}

fn per_million(per_token: f64) -> f64 {
    round_to(per_token * TOKENS_PER_MILLION, PRICE_DECIMALS)
}

impl Catalog {
    /// The bundled table's entry for a key, as stored.
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// Whether an id names a provider LiteLLM knows.
    pub fn is_provider(&self, id: &str) -> bool {
        self.provider_ids.contains(id)
    }

    /// The prefix of a model id when it names a known provider.
    fn prefix_of<'a>(&self, name: &'a str) -> Option<&'a str> {
        match name.split_once('/') {
            Some((head, tail)) if !tail.is_empty() && self.is_provider(head) => Some(head),
            _ => None,
        }
    }

    /// The keys worth trying for a name, best first (the web's
    /// `_candidates`): the provider-qualified form first, so a name several
    /// providers share finds this provider's price.
    fn candidates(&self, name: &str, provider: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut push = |k: String| {
            if !k.is_empty() && !out.contains(&k) {
                out.push(k);
            }
        };
        if let Some(p) = provider {
            push(format!("{}/{}", p, name));
            if let Some(rest) = name.strip_prefix(&format!("{}/", p)) {
                push(name.to_string());
                push(rest.to_string());
            }
        }
        push(name.to_string());
        if let Some(head) = self.prefix_of(name) {
            push(name[head.len() + 1..].to_string());
        }
        out
    }

    /// The best entry among the candidates (the web's `_lookup`): first one
    /// priced under this provider (so Azure never gets OpenAI's price
    /// through a shared bare name), then any.
    fn lookup(&self, candidates: &[String], provider: Option<&str>) -> Option<(&String, &Entry)> {
        if let Some(p) = provider {
            let owned = candidates.iter().find_map(|k| {
                self.entries
                    .get_key_value(k)
                    .filter(|(_, e)| e.provider == p)
            });
            if owned.is_some() {
                return owned;
            }
        }
        candidates
            .iter()
            .find_map(|k| self.entries.get_key_value(k))
    }

    fn resolve(&self, name: &str, provider: Option<&str>) -> Option<(&String, &Entry)> {
        let owner = provider.or_else(|| self.prefix_of(name));
        self.lookup(&self.candidates(name, provider), owner)
    }

    /// One model's metadata (the web's `get_model_meta`): a bare name, a
    /// prefixed id, or a name and its provider; `None` when the table does
    /// not know it.
    pub fn model_meta(&self, model_name: &str, provider: Option<&str>) -> Option<ModelInfo> {
        let name = model_name.trim();
        if name.is_empty() {
            return None;
        }
        let provider = provider.map(str::trim).filter(|p| !p.is_empty());
        let (key, entry) = self.resolve(name, provider)?;
        let owner = if !entry.provider.is_empty() {
            entry.provider.clone()
        } else {
            provider
                .or_else(|| self.prefix_of(name))
                .unwrap_or_default()
                .to_string()
        };
        Some(ModelInfo {
            id: name.to_string(),
            provider: owner,
            catalog_key: Some(key.clone()),
            entry: Some(entry.clone()),
        })
    }

    /// A turn's cost in US dollars from the table's per-token prices (the
    /// web's `estimate_cost`). Cached prompt tokens are part of the input
    /// and charged at the cache price (or the input price when none is
    /// published). `None` when the model is not priced, or a side that was
    /// used has no price: never a guess.
    pub fn estimate_cost(
        &self,
        model_name: &str,
        input: u64,
        output: u64,
        cached: u64,
    ) -> Option<f64> {
        let (_, entry) = self.resolve(model_name.trim(), None)?;
        let cached = cached.min(input);
        let billed_in = input - cached;
        let price_cached = entry.cache_read_cost.or(entry.input_cost);
        if (billed_in > 0 || cached > 0) && entry.input_cost.is_none() {
            return None;
        }
        if output > 0 && entry.output_cost.is_none() {
            return None;
        }
        if input == 0 && output == 0 {
            return Some(0.0);
        }
        let total = billed_in as f64 * entry.input_cost.unwrap_or(0.0)
            + cached as f64 * price_cached.unwrap_or(0.0)
            + output as f64 * entry.output_cost.unwrap_or(0.0);
        Some(round_to(total, COST_DECIMALS))
    }

    /// The offered providers, sorted by display name case-insensitively.
    pub fn list_providers(&self) -> Vec<ProviderInfo> {
        let mut out: Vec<ProviderInfo> = self
            .providers
            .iter()
            .map(|(id, _)| self.provider_info(id))
            .collect();
        out.sort_by_key(|p| p.display_name.to_lowercase());
        out
    }

    /// One offered provider, or `None` for an id not offered.
    pub fn get_provider(&self, id: &str) -> Option<ProviderInfo> {
        let id = id.trim();
        self.providers
            .iter()
            .any(|(p, _)| p == id)
            .then(|| self.provider_info(id))
    }

    fn provider_info(&self, id: &str) -> ProviderInfo {
        let models = self.models_of(id);
        let (display_name, icon) = brands::brand(id);
        ProviderInfo {
            id: id.to_string(),
            display_name,
            icon,
            provider_kind: brands::provider_kind(id).to_string(),
            needs_key: brands::needs_key(id),
            needs_base_url: brands::needs_base_url(id),
            model_count: models.iter().filter(|m| m.is_chat()).count(),
            total_model_count: models.len(),
        }
    }

    fn models_of(&self, id: &str) -> Vec<ModelInfo> {
        let Some((_, names)) = self.providers.iter().find(|(p, _)| p == id) else {
            return Vec::new();
        };
        names
            .iter()
            .map(|name| {
                let found = self.lookup(&self.candidates(name, Some(id)), Some(id));
                ModelInfo {
                    id: name.clone(),
                    provider: id.to_string(),
                    catalog_key: found.map(|(k, _)| k.clone()),
                    entry: found.map(|(_, e)| e.clone()),
                }
            })
            .collect()
    }

    /// A provider's models (the web's `list_models`); `chat_only` drops the
    /// ones whose mode is known and not conversational. An unknown provider
    /// has none.
    pub fn list_models(&self, provider: &str, chat_only: bool) -> Vec<ModelInfo> {
        let models = self.models_of(provider.trim());
        if chat_only {
            models.into_iter().filter(ModelInfo::is_chat).collect()
        } else {
            models
        }
    }
}

impl Catalog {
    /// Whether there is a catalogue to browse (the web's `is_available`:
    /// false when LiteLLM cannot be imported; here, when the bundled copy
    /// could not be read).
    pub fn is_available(&self) -> bool {
        !self.providers.is_empty()
    }
}

/// `GET /agent/api/catalog/providers`.
pub fn providers_answer() -> Value {
    let catalog = current();
    crate::agent::models::success(json!({
        "available": catalog.is_available(),
        "data": catalog
            .list_providers()
            .iter()
            .map(ProviderInfo::to_json)
            .collect::<Vec<_>>(),
    }))
}

/// `GET /agent/api/catalog/models?provider=&chat_only=`: an unknown
/// provider answers with no models, not an error; only the word `false`
/// turns `chat_only` off.
pub fn models_answer(
    provider: Option<&str>,
    chat_only: Option<&str>,
) -> Result<Value, crate::agent::reply::Refusal> {
    let provider = provider.unwrap_or("").trim();
    if provider.is_empty() {
        return Err(crate::agent::reply::Refusal::bad_request(
            "A provider is required",
        ));
    }
    let chat_only = chat_only.unwrap_or("true").trim().to_lowercase() != "false";
    let catalog = current();
    Ok(crate::agent::models::success(json!({
        "available": catalog.is_available(),
        "provider": catalog.get_provider(provider).map(|p| p.to_json()),
        "data": catalog
            .list_models(provider, chat_only)
            .iter()
            .map(ModelInfo::to_json)
            .collect::<Vec<_>>(),
    })))
}
