//! Web search and page fetch for the Agent (the web's
//! `services/agent/tools/websearch.py`, `services/agent/web_prompt.py` and
//! the web search block of `services/agent/settings.py`): links from
//! DuckDuckGo, Tavily, Firecrawl or TinyFish, a cited answer from
//! Perplexity, and the text of one page from Firecrawl or TinyFish.
//!
//! - this file: the providers, the setting keys and their defaults and
//!   bounds, the key names, the IST trading date the daily cap counts on.
//! - `settings`: the configuration the panel reads and writes, the keys,
//!   and the daily counter, incremented in one SQL statement (D-019).
//! - `sources`: the trader's preferred sources for company figures.
//! - `taint`: redaction, and the outgoing query built only from the
//!   trader's own words.
//! - `fetch_url`: the fetch boundary, which URLs a page fetch may send.
//! - `providers`: Tavily and Perplexity, the shared outcome, the request
//!   every keyed provider goes through, the status sentences, the busy
//!   gate.
//! - `keyed`: Firecrawl and TinyFish, each a link search and a page read.
//! - `duckduckgo`: the keyless provider through DuckDuckGo's html and lite
//!   pages (decision 12; its results may differ from the web's scraper).
//! - `probe`: one real query to test a provider from the panel.
//! - `prompt`: the prompt section that says where company figures are
//!   looked for first.
//!
//! The tools themselves (`web_search`, `web_research`, `web_fetch`) are the
//! `websearch` toolkit in `agent::tools::websearch`. Nothing here reads
//! the environment: the providers and the tunables are `ag_setting` rows
//! and the keys are vault rows (`websearch:<provider>`).

pub mod duckduckgo;
pub mod fetch_url;
pub mod keyed;
pub mod payloads;
pub mod probe;
pub mod prompt;
pub mod providers;
pub mod service;
pub mod settings;
pub mod sources;
pub mod taint;

#[cfg(test)]
pub(crate) mod tests;

use serde_json::{json, Value};

pub const DUCKDUCKGO: &str = "duckduckgo";
pub const TAVILY: &str = "tavily";
pub const PERPLEXITY: &str = "perplexity";
pub const FIRECRAWL: &str = "firecrawl";
pub const TINYFISH: &str = "tinyfish";

/// The keyed link providers, in the order one is tried when the
/// configured provider cannot answer (`KEYED_LINK_PROVIDERS`): Tavily
/// first, as it was the only one for a long time.
pub const KEYED_LINK_PROVIDERS: &[&str] = &[TAVILY, FIRECRAWL, TINYFISH];

/// The providers that read a page for `web_fetch`, in the order one is
/// borrowed (`FETCH_PROVIDERS`).
pub const FETCH_PROVIDERS: &[&str] = &[FIRECRAWL, TINYFISH];

pub const KEY_PROVIDER: &str = "websearch_provider";
pub const KEY_FETCH_PROVIDER: &str = "webfetch_provider";
pub const KEY_PREFERRED_SOURCES: &str = "websearch_preferred_sources";
pub const KEY_PERPLEXITY_MODEL: &str = "websearch_perplexity_model";
pub const KEY_MAX_CALLS_PER_TURN: &str = "websearch_max_calls_per_turn";
pub const KEY_DAILY_CAP: &str = "websearch_daily_cap";
pub const KEY_USAGE: &str = "websearch_usage";

pub const SECRET_PREFIX: &str = "websearch:";

pub const DEFAULT_PROVIDER: &str = DUCKDUCKGO;
/// Fetch has no keyless provider, so this only orders the keyed ones.
pub const DEFAULT_FETCH_PROVIDER: &str = FIRECRAWL;
/// Where company figures are looked for first, best first.
pub const DEFAULT_PREFERRED_SOURCES: &[&str] = &["screener.in", "scanx.trade", "tickertape.in"];
pub const MAX_PREFERRED_SOURCES: usize = 5;
pub const DEFAULT_PERPLEXITY_MODEL: &str = "openai/gpt-5.6-luna";
/// A search and a few pages fit in one turn (the web raised it from 5).
pub const DEFAULT_MAX_CALLS_PER_TURN: i64 = 10;
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
    /// It also reads a whole page for `web_fetch`, with the same key.
    pub fetch: bool,
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
            "fetch": self.fetch,
        })
    }
}

/// The providers, in the web's order.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: DUCKDUCKGO,
        label: "DuckDuckGo",
        needs_key: false,
        result_kind: "links",
        tool: "web_search",
        description: "Keyless and the default, so web search works with nothing configured and no key leaves the machine. It scrapes rather than calling an API, so it throttles under load.",
        fetch: false,
    },
    ProviderSpec {
        id: TAVILY,
        label: "Tavily",
        needs_key: true,
        result_kind: "links",
        tool: "web_search",
        description: "A paid search API with better snippets than the keyless provider, and a deeper crawl on market questions. It answers web_search in place of DuckDuckGo once its key is stored.",
        fetch: false,
    },
    ProviderSpec {
        id: PERPLEXITY,
        label: "Perplexity",
        needs_key: true,
        result_kind: "answer",
        tool: "web_research",
        // D-032: the web said choosing Perplexity leaves web_search on
        // DuckDuckGo, while its code routes link searches to the first of
        // Tavily, Firecrawl and TinyFish that has a key. The sentence says
        // what happens.
        description: "Answers a question from current sources and cites them. It returns no links, so it answers web_research rather than web_search; selecting it here leaves web_search on the first of Tavily, Firecrawl and TinyFish that has a key, and on DuckDuckGo otherwise.",
        fetch: false,
    },
    ProviderSpec {
        id: FIRECRAWL,
        label: "Firecrawl",
        needs_key: true,
        result_kind: "links",
        tool: "web_search",
        description: "Searches the web and reads a whole page as clean text, tables included. One key answers both web_search and web_fetch, so the agent can find a page and then open it.",
        fetch: true,
    },
    ProviderSpec {
        id: TINYFISH,
        label: "TinyFish",
        needs_key: true,
        result_kind: "links",
        tool: "web_search",
        description: "Searches the web and reads a whole page as text, with a free daily allowance. One key answers both web_search and web_fetch.",
        fetch: true,
    },
];

/// How a provider is written to a person (`PROVIDER_LABELS`): TinyFish,
/// not Tinyfish. An unknown id reads as itself.
pub fn label(provider: &str) -> &str {
    PROVIDERS
        .iter()
        .find(|p| p.id == provider)
        .map(|p| p.label)
        .unwrap_or(provider)
}

/// A provider that can read a page, or the web's refusal
/// (`webfetch_provider_spec`).
pub fn fetch_spec(raw: &str) -> Result<&'static ProviderSpec, String> {
    let key = raw.trim().to_lowercase();
    PROVIDERS
        .iter()
        .find(|p| p.id == key && p.fetch)
        .ok_or_else(|| {
            format!(
                "fetch_provider must be one of: {}",
                FETCH_PROVIDERS.join(", ")
            )
        })
}

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
