//! Firecrawl and TinyFish (the web's `_firecrawl_search`,
//! `_tinyfish_search`, `_firecrawl_fetch`, `_tinyfish_fetch` and the
//! `_LINK_SEARCH` and `_PAGE_FETCH` tables): each answers `web_search`
//! with links and `web_fetch` with the text of one page, on one key.
//!
//! A page is asked for as markdown, with no links: a fetched page cannot
//! hand the model new addresses to fetch (the fetch boundary,
//! `fetch_url`). Each request goes through `providers::ask_json`, so a
//! failure reads the same for every provider.

use super::fetch_url::MAX_URL_CHARS;
use super::providers::{
    self, ask_json, bounded, Ask, Auth, Link, Net, Outcome, FETCH_PROVIDER_TIMEOUT_MS,
    FETCH_TIMEOUT, MAX_SNIPPET_CHARS, MAX_TITLE_CHARS, SEARCH_TIMEOUT,
};
use super::{label, FIRECRAWL, TAVILY, TINYFISH};
use crate::security::Secret;
use serde_json::{json, Value};
use std::time::Instant;

/// TinyFish's own key header.
const TINYFISH_KEY_HEADER: &str = "X-API-Key";

/// How long a TinyFish failure reason may be when it is quoted.
const MAX_REASON_CHARS: usize = 120;

/// A link search on a keyed provider (`_LINK_SEARCH`).
pub async fn search_with(
    net: &Net,
    provider: &'static str,
    query: &str,
    max_results: usize,
    key: &Secret,
) -> Outcome {
    match provider {
        TAVILY => providers::tavily(net, query, max_results, key).await,
        FIRECRAWL => firecrawl_search(net, query, max_results, key).await,
        TINYFISH => tinyfish_search(net, query, max_results, key).await,
        other => Outcome::failed(other, format!("{} does not return links.", label(other))),
    }
}

/// A page read on a fetch provider (`_PAGE_FETCH`).
pub async fn fetch_with(net: &Net, provider: &'static str, url: &str, key: &Secret) -> Outcome {
    match provider {
        FIRECRAWL => firecrawl_fetch(net, url, key).await,
        TINYFISH => tinyfish_fetch(net, url, key).await,
        other => Outcome::failed(other, format!("{} does not read pages.", label(other))),
    }
}

/// A provider's result rows as `{title, url, snippet}` (`_link_rows`): the
/// first `limit` rows, a row without a URL left out, the extract from the
/// first of `snippet_keys` that holds one.
fn link_rows(rows: Option<&Value>, limit: usize, snippet_keys: &[&str]) -> Vec<Link> {
    rows.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(limit)
        .filter(|row| row.is_object())
        .filter_map(|row| {
            let url = bounded(row.get("url"), MAX_URL_CHARS);
            if url.is_empty() {
                return None;
            }
            let snippet = snippet_keys
                .iter()
                .filter_map(|k| row.get(*k))
                .find(|v| crate::agent::models::truthy(v));
            Some(Link {
                title: bounded(row.get("title"), MAX_TITLE_CHARS),
                url,
                snippet: bounded(snippet, MAX_SNIPPET_CHARS),
            })
        })
        .collect()
}

/// The first text in a field that may be a text or a list of them
/// (`_first_text`, Firecrawl's `metadata.title`).
fn first_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .find(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The objects in a list field of an answer.
fn objects<'a>(payload: &'a Value, field: &str) -> Vec<&'a Value> {
    payload
        .get(field)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|r| r.is_object()).collect())
        .unwrap_or_default()
}

/// A failure after the provider answered, so its key was used.
fn answered_failure(provider: &'static str, error: impl Into<String>) -> Outcome {
    Outcome {
        key_used: true,
        ..Outcome::failed(provider, error)
    }
}

/// A page the provider read, as the tool keeps it.
fn page(provider: &'static str, text: String, title: String, final_url: String) -> Outcome {
    Outcome {
        ok: true,
        provider,
        text,
        title,
        final_url,
        key_used: true,
        ..Default::default()
    }
}

/// Search Firecrawl (`_firecrawl_search`). A failure is not final: the
/// tool tries the next provider and says so.
pub async fn firecrawl_search(net: &Net, query: &str, max_results: usize, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(FIRECRAWL, "Web search");
    };
    let started = Instant::now();
    let body = json!({"query": query, "limit": max_results, "sources": ["web"]});
    let payload = match ask_json(Ask {
        provider: FIRECRAWL,
        url: &net.firecrawl_search,
        auth: Auth::Bearer(key),
        body: Some(&body),
        query: &[],
        timeout: SEARCH_TIMEOUT,
    })
    .await
    {
        Ok(p) => p,
        Err(message) => return Outcome::failed(FIRECRAWL, message),
    };
    // v2 nests web results under data.web; an older shape is the list itself.
    let data = payload.get("data");
    let rows = match data {
        Some(d) if d.is_object() => d.get("web"),
        other => other,
    };
    let results = link_rows(rows, max_results, &["description", "snippet"]);
    tracing::info!(
        "Firecrawl answered with {} result(s) in {} ms",
        results.len(),
        started.elapsed().as_millis()
    );
    Outcome {
        ok: true,
        provider: FIRECRAWL,
        results,
        key_used: true,
        ..Default::default()
    }
}

/// Search TinyFish (`_tinyfish_search`): a GET with the key in its own
/// header. It answers a page of about ten and takes no count, so the rows
/// are trimmed here.
pub async fn tinyfish_search(net: &Net, query: &str, max_results: usize, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(TINYFISH, "Web search");
    };
    let started = Instant::now();
    let payload = match ask_json(Ask {
        provider: TINYFISH,
        url: &net.tinyfish_search,
        auth: Auth::Header(TINYFISH_KEY_HEADER, key),
        body: None,
        query: &[("query", query)],
        timeout: SEARCH_TIMEOUT,
    })
    .await
    {
        Ok(p) => p,
        Err(message) => return Outcome::failed(TINYFISH, message),
    };
    let results = link_rows(
        payload.get("results"),
        max_results,
        &["snippet", "description"],
    );
    tracing::info!(
        "TinyFish answered with {} result(s) in {} ms",
        results.len(),
        started.elapsed().as_millis()
    );
    Outcome {
        ok: true,
        provider: TINYFISH,
        results,
        key_used: true,
        ..Default::default()
    }
}

/// Read one page as markdown through Firecrawl (`_firecrawl_fetch`). The
/// page's own error status, which Firecrawl reports inside a 200 answer,
/// is a failure.
pub async fn firecrawl_fetch(net: &Net, url: &str, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(FIRECRAWL, "Web fetch");
    };
    let started = Instant::now();
    let body = json!({
        "url": url,
        "formats": ["markdown"],
        "onlyMainContent": true,
        "timeout": FETCH_PROVIDER_TIMEOUT_MS,
    });
    let payload = match ask_json(Ask {
        provider: FIRECRAWL,
        url: &net.firecrawl_scrape,
        auth: Auth::Bearer(key),
        body: Some(&body),
        query: &[],
        timeout: FETCH_TIMEOUT,
    })
    .await
    {
        Ok(p) => p,
        Err(message) => return Outcome::failed(FIRECRAWL, message),
    };
    let data = payload.get("data").filter(|d| d.is_object());
    let meta = data
        .and_then(|d| d.get("metadata"))
        .filter(|m| m.is_object());
    let text = data
        .and_then(|d| d.get("markdown"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let status = meta
        .and_then(|m| m.get("statusCode"))
        .and_then(Value::as_i64);
    tracing::info!(
        "Firecrawl fetched {} character(s) in {} ms",
        text.chars().count(),
        started.elapsed().as_millis()
    );
    if let Some(status) = status.filter(|s| *s >= 400) {
        return answered_failure(
            FIRECRAWL,
            format!(
                "The page answered with HTTP {}, so there was nothing to read.",
                status
            ),
        );
    }
    if text.trim().is_empty() {
        return answered_failure(
            FIRECRAWL,
            "Firecrawl returned the page with no readable text.",
        );
    }
    page(
        FIRECRAWL,
        text,
        first_text(meta.and_then(|m| m.get("title"))),
        first_text(meta.and_then(|m| m.get("url"))),
    )
}

/// Read one page as markdown through TinyFish (`_tinyfish_fetch`), which
/// reports a page it could not read inside a 200 answer.
pub async fn tinyfish_fetch(net: &Net, url: &str, key: &Secret) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(TINYFISH, "Web fetch");
    };
    let started = Instant::now();
    let body = json!({
        "urls": [url],
        "format": "markdown",
        "links": false,
        "image_links": false,
        "iframe_links": false,
        "per_url_timeout_ms": FETCH_PROVIDER_TIMEOUT_MS,
    });
    let payload = match ask_json(Ask {
        provider: TINYFISH,
        url: &net.tinyfish_fetch,
        auth: Auth::Header(TINYFISH_KEY_HEADER, key),
        body: Some(&body),
        query: &[],
        timeout: FETCH_TIMEOUT,
    })
    .await
    {
        Ok(p) => p,
        Err(message) => return Outcome::failed(TINYFISH, message),
    };
    let (results, errors) = (objects(&payload, "results"), objects(&payload, "errors"));
    let Some(row) = results.first() else {
        let reason = errors
            .first()
            .map(|e| bounded(e.get("error"), MAX_REASON_CHARS))
            .unwrap_or_default();
        // Desktop: the reason is not logged, as it can quote the address.
        tracing::warn!(
            "TinyFish could not fetch the page ({})",
            if reason.is_empty() {
                "no reason given"
            } else {
                "reason given"
            }
        );
        return answered_failure(
            TINYFISH,
            if reason.is_empty() {
                "TinyFish could not read the page.".to_string()
            } else {
                format!("TinyFish could not read the page ({}).", reason)
            },
        );
    };
    let text = row
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    tracing::info!(
        "TinyFish fetched {} character(s) in {} ms",
        text.chars().count(),
        started.elapsed().as_millis()
    );
    if text.trim().is_empty() {
        return answered_failure(
            TINYFISH,
            "TinyFish returned the page with no readable text.",
        );
    }
    page(
        TINYFISH,
        text,
        first_text(row.get("title")),
        first_text(row.get("final_url")),
    )
}
