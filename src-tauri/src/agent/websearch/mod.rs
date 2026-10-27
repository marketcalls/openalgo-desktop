//! Web search for the Agent (the web's `services/agent/tools/websearch.py`
//! and the web search block of `services/agent/settings.py`): links from
//! DuckDuckGo or Tavily, a cited answer from Perplexity.
//!
//! - this file: the providers, the setting keys and their defaults and
//!   bounds, the key names, the IST trading date the daily cap counts on.
//! - `settings`: the configuration the panel reads and writes, the keys,
//!   and the daily counter, incremented in one SQL statement (D-019).
//! - `taint`: redaction, and the outgoing query built only from the
//!   trader's own words.
//! - `providers`: Tavily and Perplexity, the shared outcome, the status
//!   sentences, the busy gate.
//! - `duckduckgo`: the keyless provider through DuckDuckGo's html and lite
//!   pages (decision 12; its results may differ from the web's scraper).
//! - `probe`: one real query to test a provider from the panel.
//!
//! The tools themselves (`web_search`, `web_research`) are the `websearch`
//! toolkit in `agent::tools::websearch`. Nothing here reads the
//! environment: the provider and the tunables are `ag_setting` rows and
//! the keys are vault rows (`websearch:tavily`, `websearch:perplexity`).

pub mod duckduckgo;
pub mod payloads;
pub mod probe;
pub mod providers;
pub mod service;
pub mod settings;
pub mod taint;

#[cfg(test)]
pub(crate) mod tests;

use serde_json::{json, Value};

pub const DUCKDUCKGO: &str = "duckduckgo";
pub const TAVILY: &str = "tavily";
pub const PERPLEXITY: &str = "perplexity";

pub const KEY_PROVIDER: &str = "websearch_provider";
pub const KEY_PERPLEXITY_MODEL: &str = "websearch_perplexity_model";
pub const KEY_MAX_CALLS_PER_TURN: &str = "websearch_max_calls_per_turn";
pub const KEY_DAILY_CAP: &str = "websearch_daily_cap";
pub const KEY_USAGE: &str = "websearch_usage";

pub const SECRET_PREFIX: &str = "websearch:";

pub const DEFAULT_PROVIDER: &str = DUCKDUCKGO;
pub const DEFAULT_PERPLEXITY_MODEL: &str = "openai/gpt-5.6-luna";
pub const DEFAULT_MAX_CALLS_PER_TURN: i64 = 5;
pub const DEFAULT_DAILY_CAP: i64 = 200;
pub const MAX_CALLS_PER_TURN: i64 = 50;
pub const MAX_DAILY_CAP: i64 = 10_000;
pub const MAX_PERPLEXITY_MODEL_CHARS: usize = 120;
pub const MAX_KEY_CHARS: usize = 500;

/// One selectable provider (the web's `WebSearchProviderSpec`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub needs_key: bool,
    /// `links` or `answer`.
    pub result_kind: &'static str,
    /// The tool it answers.
    pub tool: &'static str,
    pub description: &'static str,
}

impl ProviderSpec {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "needs_key": self.needs_key,
            "result_kind": self.result_kind,
            "tool": self.tool,
            "description": self.description,
        })
    }
}

/// The three providers, in the web's order.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: DUCKDUCKGO,
        label: "DuckDuckGo",
        needs_key: false,
        result_kind: "links",
        tool: "web_search",
        description: "Keyless and the default, so web search works with nothing configured and no key leaves the machine. It scrapes rather than calling an API, so it throttles under load.",
    },
    ProviderSpec {
        id: TAVILY,
        label: "Tavily",
        needs_key: true,
        result_kind: "links",
        tool: "web_search",
        description: "A paid search API with better snippets than the keyless provider, and a deeper crawl on market questions. It answers web_search in place of DuckDuckGo once its key is stored.",
    },
    ProviderSpec {
        id: PERPLEXITY,
        label: "Perplexity",
        needs_key: true,
        result_kind: "answer",
        tool: "web_research",
        // D-032: the web said choosing Perplexity leaves web_search on
        // DuckDuckGo, while its code routes link searches to Tavily
        // whenever a Tavily key is stored. The sentence says what happens.
        description: "Answers a question from current sources and cites them. It returns no links, so it answers web_research rather than web_search; selecting it here leaves web_search on Tavily when a Tavily key is stored, and on DuckDuckGo otherwise.",
    },
];

/// A provider id in any case and padding, or the web's refusal.
pub fn provider_spec(raw: &str) -> Result<&'static ProviderSpec, String> {
    let key = raw.trim().to_lowercase();
    PROVIDERS.iter().find(|p| p.id == key).ok_or_else(|| {
        format!(
            "provider must be one of: {}",
            PROVIDERS
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// A provider that may hold a key, or the refusal.
pub fn keyed_spec(raw: &str) -> Result<&'static ProviderSpec, String> {
    let spec = provider_spec(raw)?;
    if !spec.needs_key {
        return Err(format!("{} needs no API key", spec.label));
    }
    Ok(spec)
}

/// The vault row holding a provider's key (`websearch:tavily`).
pub fn secret_name(provider: &str) -> String {
    format!("{}{}", SECRET_PREFIX, provider.trim().to_lowercase())
}

/// Today's trading date in IST (`YYYY-MM-DD`), on the app's clock: the
/// daily cap resets on it, with no scheduled job.
pub fn today(now: chrono::DateTime<chrono::Utc>) -> String {
    now.with_timezone(&chrono_tz::Asia::Kolkata)
        .format("%Y-%m-%d")
        .to_string()
}
