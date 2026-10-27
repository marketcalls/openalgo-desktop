//! Tavily and Perplexity (the web's `_tavily_search`,
//! `_perplexity_research` and `_parse_perplexity_output`), what every
//! provider call produces, and the gate that keeps too many searches from
//! running at once.
//!
//! The web sends these requests on its hub and answers "the web server was
//! too busy" when the hub cannot take one. The desktop's counterpart is a
//! gate of eight searches at a time: a search that waits ten seconds for a
//! place is answered with the same `platform_busy` sentence, the provider
//! never asked, and no other provider tried in its stead.
//!
//! A failure is a sentence for the model, logged by status or kind, never
//! by its text (a transport error can quote the URL, and the URL the
//! query). The web's sentences put a Python error class in brackets; the
//! desktop has none, so that part is left out.

use super::{PERPLEXITY, TAVILY};
use crate::security::Secret;
use serde_json::{json, Value};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

pub const TAVILY_URL: &str = "https://api.tavily.com/search";
pub const PERPLEXITY_URL: &str = "https://api.perplexity.ai/v1/agent";
pub const DUCKDUCKGO_HTML_URL: &str = "https://html.duckduckgo.com/html/";
pub const DUCKDUCKGO_LITE_URL: &str = "https://lite.duckduckgo.com/lite/";

/// The web's `SEARCH_TIMEOUT_SECONDS` (Tavily).
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
/// The web's `RESEARCH_TIMEOUT_SECONDS` (Perplexity).
pub const RESEARCH_TIMEOUT: Duration = Duration::from_secs(60);
/// The web's `DUCKDUCKGO_TIMEOUT_SECONDS`, per page.
pub const DUCKDUCKGO_TIMEOUT: Duration = Duration::from_secs(10);

pub const MAX_SNIPPET_CHARS: usize = 400;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_ANSWER_CHARS: usize = 8000;
pub const MAX_CITATIONS: usize = 20;

pub const PERPLEXITY_INSTRUCTIONS: &str = "Answer the question from current web sources. Be concise and factual, lead with the answer, and cite the sources you used. The reader is a trader on an Indian markets platform, so prefer recent primary sources and state the date of anything time sensitive. Say plainly when the sources disagree or when you could not find an answer.";
pub const PERPLEXITY_MAX_OUTPUT_TOKENS: u32 = 1500;
pub const PERPLEXITY_SEARCH_CONTEXT_SIZE: &str = "medium";
pub const PERPLEXITY_MAX_SEARCH_RESULTS: u32 = 10;

/// How many searches may run at once, and how long one waits for a place
/// (the web's `HUB_WAIT_MARGIN_SECONDS`).
pub const MAX_IN_FLIGHT: usize = 8;
pub const GATE_WAIT: Duration = Duration::from_secs(10);

/// The sentence for a search the gate could not take (the web's
/// `PLATFORM_BUSY_MESSAGE`; `what` is `Web search` or `Web research`).
pub fn busy_message(what: &str) -> String {
    format!(
        "{} could not run because the web server was too busy to take it. Try the search again in a moment.",
        what
    )
}

/// Where the providers are, and the gate (a test's are its own).
#[derive(Clone)]
pub struct Net {
    pub tavily: String,
    pub perplexity: String,
    pub duckduckgo_html: String,
    pub duckduckgo_lite: String,
    pub gate: Arc<Semaphore>,
    pub gate_wait: Duration,
}

static DEFAULT_NET: LazyLock<Net> = LazyLock::new(|| Net {
    tavily: TAVILY_URL.into(),
    perplexity: PERPLEXITY_URL.into(),
    duckduckgo_html: DUCKDUCKGO_HTML_URL.into(),
    duckduckgo_lite: DUCKDUCKGO_LITE_URL.into(),
    gate: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
    gate_wait: GATE_WAIT,
});

#[cfg(test)]
thread_local! {
    static TEST_NET: std::cell::RefCell<Option<Net>> = const { std::cell::RefCell::new(None) };
}

/// Point this test thread's searches at local servers.
#[cfg(test)]
pub fn set_test_net(net: Option<Net>) {
    TEST_NET.with(|n| *n.borrow_mut() = net);
}

/// Where searches go now.
pub fn net() -> Net {
    #[cfg(test)]
    if let Some(n) = TEST_NET.with(|n| n.borrow().clone()) {
        return n;
    }
    DEFAULT_NET.clone()
}

/// One link result, in the web's key order (`{title, url, snippet}`).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct Link {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// One of Perplexity's sources, in the web's key order (`{id, url, title}`).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct Citation {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// What one provider call produced (the web's `ProviderOutcome`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    pub ok: bool,
    pub provider: &'static str,
    /// Links: `{title, url, snippet}`.
    pub results: Vec<Link>,
    pub answer: String,
    /// Perplexity's sources: `{id, url, title}`.
    pub citations: Vec<Citation>,
    pub cost_usd: Option<f64>,
    pub error: String,
    /// The gate could not take it: the provider was never asked.
    pub busy: bool,
    /// The provider answered, so its key was used.
    pub key_used: bool,
}

impl Outcome {
    pub fn failed(provider: &'static str, error: impl Into<String>) -> Outcome {
        Outcome {
            provider,
            error: error.into(),
            ..Default::default()
        }
    }

    pub fn busy(provider: &'static str, what: &str) -> Outcome {
        tracing::warn!(
            "{} request could not be sent: too many searches at once",
            provider
        );
        Outcome {
            busy: true,
            ..Outcome::failed(provider, busy_message(what))
        }
    }
}

/// A provider field as one bounded string: trimmed, and cut with `...`
/// (the web's `_text`).
pub fn bounded(value: Option<&Value>, limit: usize) -> String {
    let text = match value {
        None | Some(Value::Null) => return String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(other) => crate::agent::settings::pyrepr::py_str(other)
            .trim()
            .to_string(),
    };
    if text.chars().count() <= limit {
        return text;
    }
    let head: String = text.chars().take(limit - 3).collect();
    format!("{}...", head.trim_end())
}

/// A provider's HTTP status as a sentence naming the next step
/// (`_status_message`).
pub fn status_message(provider: &str, status: u16) -> String {
    let mut label = provider.to_string();
    if let Some(first) = label.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    match status {
        401 | 403 => format!(
            "{} rejected the stored key (HTTP {}). Check the {} key in the agent settings; it is wrong, expired or revoked.",
            label, status, provider
        ),
        429 => format!(
            "{} rate limited this request (HTTP {}). Wait before searching again.",
            label, status
        ),
        s if s >= 500 => format!("{} is having an outage (HTTP {}).", label, s),
        s => format!("{} rejected the request (HTTP {}).", label, s),
    }
}

/// One POST with a bearer key; the body as JSON when it is one.
async fn post_json(
    url: &str,
    key: &Secret,
    body: &Value,
    timeout: Duration,
    label: &str,
) -> Result<(u16, Option<Value>), String> {
    let client = crate::agent::outbound::client().map_err(|e| {
        tracing::error!("Web search could not build its HTTP client: {}", e);
        format!("{} could not be reached.", label)
    })?;
    let response = client
        .post(url)
        .timeout(timeout)
        .bearer_auth(key.expose())
        .json(body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!(
                "{} request failed: {}",
                label,
                crate::agent::outbound::error_kind(&e)
            );
            format!("{} could not be reached.", label)
        })?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    Ok((status, serde_json::from_str(&text).ok()))
}

/// Words that make a query a market question (`_FINANCE_WORDS`).
const FINANCE_WORDS: &[&str] = &[
    "bank",
    "banknifty",
    "bond",
    "bse",
    "bull",
    "bear",
    "buyback",
    "capital",
    "commodity",
    "crude",
    "currency",
    "dividend",
    "earnings",
    "equity",
    "etf",
    "fii",
    "finance",
    "financial",
    "finnifty",
    "fno",
    "forex",
    "fund",
    "futures",
    "gdp",
    "gold",
    "guidance",
    "index",
    "inflation",
    "investor",
    "ipo",
    "market",
    "mcx",
    "merger",
    "nifty",
    "nse",
    "option",
    "options",
    "profit",
    "quarter",
    "quarterly",
    "rbi",
    "repo",
    "results",
    "revenue",
    "rupee",
    "sebi",
    "sensex",
    "share",
    "shares",
    "stock",
    "stocks",
    "trade",
    "trading",
    "valuation",
    "yield",
];

/// Tavily's topic for a query: `finance` for a market question.
pub fn tavily_topic(query: &str) -> &'static str {
    let finance = super::taint::split(query)
        .map(|t| super::taint::strip_edges(t).to_lowercase())
        .any(|w| FINANCE_WORDS.contains(&w.as_str()));
    if finance {
        "finance"
    } else {
        "general"
    }
}

/// Search Tavily (`_tavily_search`). A failure is not final: the caller
/// falls back to DuckDuckGo and says so.
pub async fn tavily(net: &Net, query: &str, max_results: usize, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(TAVILY, "Web search");
    };
    let topic = tavily_topic(query);
    let body = json!({
        "query": query,
        "max_results": max_results,
        "search_depth": if topic == "finance" { "advanced" } else { "basic" },
        "topic": topic,
    });
    let started = Instant::now();
    let (status, payload) = match post_json(&net.tavily, key, &body, SEARCH_TIMEOUT, "Tavily").await
    {
        Ok(r) => r,
        Err(message) => return Outcome::failed(TAVILY, message),
    };
    if status != 200 {
        tracing::warn!("Tavily returned HTTP {}", status);
        return Outcome::failed(TAVILY, status_message(TAVILY, status));
    }
    let Some(payload) = payload else {
        tracing::warn!("Tavily returned a body that is not JSON");
        return Outcome::failed(TAVILY, "Tavily returned a body that is not JSON.");
    };
    let results: Vec<Link> = payload
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(max_results)
        .filter_map(|row| {
            let url = bounded(row.get("url"), MAX_SNIPPET_CHARS);
            (row.is_object() && !url.is_empty()).then(|| Link {
                title: bounded(row.get("title"), MAX_TITLE_CHARS),
                url,
                snippet: bounded(row.get("content"), MAX_SNIPPET_CHARS),
            })
        })
        .collect();
    tracing::info!(
        "Tavily answered with {} result(s) on topic={} in {} ms",
        results.len(),
        topic,
        started.elapsed().as_millis()
    );
    Outcome {
        ok: true,
        provider: TAVILY,
        results,
        key_used: true,
        ..Default::default()
    }
}

/// Ask Perplexity (`_perplexity_research`): its answer, its citations and
/// what it says the call cost.
pub async fn perplexity(net: &Net, question: &str, model: &str, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(PERPLEXITY, "Web research");
    };
    let body = json!({
        "model": model,
        "input": question,
        "tools": [{
            "type": "web_search",
            "search_context_size": PERPLEXITY_SEARCH_CONTEXT_SIZE,
            "max_results": PERPLEXITY_MAX_SEARCH_RESULTS,
        }],
        "instructions": PERPLEXITY_INSTRUCTIONS,
        "max_output_tokens": PERPLEXITY_MAX_OUTPUT_TOKENS,
    });
    let started = Instant::now();
    let (status, payload) =
        match post_json(&net.perplexity, key, &body, RESEARCH_TIMEOUT, "Perplexity").await {
            Ok(r) => r,
            Err(message) => return Outcome::failed(PERPLEXITY, message),
        };
    if status != 200 {
        tracing::warn!("Perplexity returned HTTP {}", status);
        return Outcome::failed(PERPLEXITY, status_message(PERPLEXITY, status));
    }
    let Some(payload) = payload else {
        tracing::warn!("Perplexity returned a body that is not JSON");
        return Outcome::failed(PERPLEXITY, "Perplexity returned a body that is not JSON.");
    };
    let (answer, citations) = parse_perplexity(&payload);
    let cost = payload
        .pointer("/usage/cost/total_cost")
        .filter(|v| !v.is_boolean())
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        });
    tracing::info!(
        "Perplexity answered with {} citation(s) in {} ms",
        citations.len(),
        started.elapsed().as_millis()
    );
    if answer.is_empty() && citations.is_empty() {
        return Outcome {
            cost_usd: cost,
            key_used: true,
            ..Outcome::failed(
                PERPLEXITY,
                "Perplexity returned no answer and no citations.",
            )
        };
    }
    Outcome {
        ok: true,
        provider: PERPLEXITY,
        answer,
        citations,
        cost_usd: cost,
        key_used: true,
        ..Default::default()
    }
}

/// The answer and the citations of a Perplexity response
/// (`_parse_perplexity_output`): `search_results` items give the sources
/// (first per URL, at most 20), `message` items the prose.
pub fn parse_perplexity(payload: &Value) -> (String, Vec<Citation>) {
    let mut parts: Vec<String> = Vec::new();
    let mut citations: Vec<Citation> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let items = payload
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for item in items.filter(|i| i.is_object()) {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        if kind == "search_results" {
            for source in item
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let url = bounded(source.get("url"), MAX_SNIPPET_CHARS);
                if !source.is_object()
                    || url.is_empty()
                    || seen.contains(&url)
                    || citations.len() >= MAX_CITATIONS
                {
                    continue;
                }
                seen.push(url.clone());
                citations.push(Citation {
                    id: bounded(source.get("id"), MAX_TITLE_CHARS),
                    url,
                    title: bounded(source.get("title"), MAX_TITLE_CHARS),
                });
            }
        } else if kind == "message" {
            for block in item
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let output = block
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("output_text"));
                if let Some(text) = block.get("text").and_then(Value::as_str).filter(|_| output) {
                    if !text.trim().is_empty() {
                        parts.push(text.trim().to_string());
                    }
                }
            }
        }
    }
    let joined = Value::String(parts.join("\n\n"));
    (bounded(Some(&joined), MAX_ANSWER_CHARS), citations)
}
