//! The keyless provider (the web's `_duckduckgo_search`), through
//! DuckDuckGo's own html page, and its lite page when the html page fails
//! or yields nothing (decision 12 of the 1.0.4 plan).
//!
//! The web uses the `ddgs` package, which aggregates several engines, so
//! the two do not return the same links for the same query; the shape of
//! the result is the same: `{title, url, snippet}`, at most the number
//! asked for, a row without a URL skipped, advertisements left out.
//!
//! Like the web's, a failure here is an outcome, never a panic or an
//! error: this provider is also the fallback for the other two.

use super::providers::{
    bounded, Link, Net, Outcome, DUCKDUCKGO_TIMEOUT, MAX_SNIPPET_CHARS, MAX_TITLE_CHARS,
};
use super::DUCKDUCKGO;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Instant;

/// What the pages are asked as: a browser's user agent, which is what the
/// web's scraper sends too (it impersonates one).
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

pub const FAILED: &str = "DuckDuckGo did not answer. It rate limits under load.";

static ANCHOR: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?is)<a\b([^>]*)>(.*?)</a\s*>").ok());
static CLASS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?is)\bclass\s*=\s*(?:"([^"]*)"|'([^']*)')"#).ok());
static HREF: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"(?is)\bhref\s*=\s*(?:"([^"]*)"|'([^']*)')"#).ok());
static HTML_SNIPPET: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)class\s*=\s*["'][^"']*\bresult__snippet\b[^"']*["'][^>]*>(.*?)</(?:a|div|td)\s*>"#,
    )
    .ok()
});
static LITE_SNIPPET: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?is)class\s*=\s*["'][^"']*\bresult-snippet\b[^"']*["'][^>]*>(.*?)</td\s*>"#).ok()
});
static TAG: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").ok());
static ENTITY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[A-Za-z]+);").ok());

fn attr(re: &LazyLock<Option<Regex>>, attrs: &str) -> Option<String> {
    let c = re.as_ref()?.captures(attrs)?;
    c.get(1)
        .or_else(|| c.get(2))
        .map(|m| m.as_str().to_string())
}

/// HTML text as plain text: tags out, entities decoded, white space
/// collapsed.
pub fn plain(html: &str) -> String {
    let no_tags = match TAG.as_ref() {
        Some(re) => re.replace_all(html, " ").into_owned(),
        None => html.to_string(),
    };
    let decoded = match ENTITY.as_ref() {
        Some(re) => re
            .replace_all(&no_tags, |c: &regex::Captures<'_>| decode_entity(&c[1]))
            .into_owned(),
        None => no_tags,
    };
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entity(name: &str) -> String {
    let code = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16).ok()
    } else if let Some(dec) = name.strip_prefix('#') {
        dec.parse().ok()
    } else {
        None
    };
    if let Some(c) = code.and_then(char::from_u32) {
        return c.to_string();
    }
    match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => return format!("&{};", name),
    }
    .to_string()
}

/// A result link as the page wrote it: DuckDuckGo's redirect unwrapped,
/// a protocol-relative one completed; an advertisement is `None`.
pub fn target_of(href: &str) -> Option<String> {
    let href = plain(href);
    let full = if href.starts_with("//") {
        format!("https:{}", href)
    } else {
        href
    };
    if full.contains("duckduckgo.com/y.js") || full.contains("ad_provider=") {
        return None;
    }
    if let Some(at) = full.find("uddg=") {
        let rest = &full[at + 5..];
        let encoded = rest.split('&').next().unwrap_or_default();
        return urlencoding::decode(encoded).ok().map(|s| s.into_owned());
    }
    (full.starts_with("http://") || full.starts_with("https://")).then_some(full)
}

/// The results of one page: anchors carrying `link_class`, each with the
/// first snippet between it and the next result.
fn parse(html: &str, link_class: &str, snippet: &LazyLock<Option<Regex>>) -> Vec<Link> {
    let Some(anchor) = ANCHOR.as_ref() else {
        return Vec::new();
    };
    let links: Vec<(usize, usize, String, String)> = anchor
        .captures_iter(html)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let attrs = c.get(1)?.as_str();
            let classes = attr(&CLASS, attrs)?;
            classes
                .split_whitespace()
                .any(|k| k == link_class)
                .then(|| {
                    (
                        whole.start(),
                        whole.end(),
                        attrs.to_string(),
                        c[2].to_string(),
                    )
                })
        })
        .collect();
    let mut out = Vec::new();
    for (i, (_, end, attrs, inner)) in links.iter().enumerate() {
        let Some(url) = attr(&HREF, attrs).and_then(|h| target_of(&h)) else {
            continue;
        };
        let next = links.get(i + 1).map(|l| l.0).unwrap_or(html.len());
        let between = html.get(*end..next).unwrap_or_default();
        let text = snippet
            .as_ref()
            .and_then(|re| re.captures(between))
            .map(|c| plain(&c[1]))
            .unwrap_or_default();
        let url = bounded(Some(&Value::String(url)), MAX_SNIPPET_CHARS);
        if url.is_empty() {
            continue;
        }
        out.push(Link {
            title: bounded(Some(&Value::String(plain(inner))), MAX_TITLE_CHARS),
            url,
            snippet: bounded(Some(&Value::String(text)), MAX_SNIPPET_CHARS),
        });
    }
    out
}

/// The results of the html page (`result__a` links, `result__snippet`).
pub fn parse_html(html: &str) -> Vec<Link> {
    parse(html, "result__a", &HTML_SNIPPET)
}

/// The results of the lite page (`result-link` links, `result-snippet`).
pub fn parse_lite(html: &str) -> Vec<Link> {
    parse(html, "result-link", &LITE_SNIPPET)
}

/// One page, posted as its own search form posts it.
async fn page(url: &str, query: &str) -> Result<String, String> {
    let client = crate::agent::outbound::client()?;
    let form = serde_urlencoded::to_string([("q", query), ("b", ""), ("kl", "wt-wt")])
        .map_err(|_| "the query could not be encoded".to_string())?;
    let response = client
        .post(url)
        .timeout(DUCKDUCKGO_TIMEOUT)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|e| crate::agent::outbound::error_kind(&e).to_string())?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {}", status));
    }
    response
        .text()
        .await
        .map_err(|e| crate::agent::outbound::error_kind(&e).to_string())
}

/// Search DuckDuckGo. Never fails loudly: an unanswered search is an
/// outcome with a sentence.
pub async fn search(net: &Net, query: &str, max_results: usize) -> Outcome {
    let Ok(_place) = tokio::time::timeout(net.gate_wait, net.gate.acquire()).await else {
        return Outcome::busy(DUCKDUCKGO, "Web search");
    };
    let started = Instant::now();
    let first = page(&net.duckduckgo_html, query)
        .await
        .map(|h| parse_html(&h));
    let rows = match first {
        Ok(rows) if !rows.is_empty() => Ok(rows),
        other => {
            if let Err(why) = &other {
                tracing::info!(
                    "DuckDuckGo's html page did not answer ({}); trying its lite page",
                    why
                );
            }
            page(&net.duckduckgo_lite, query)
                .await
                .map(|h| parse_lite(&h))
        }
    };
    match rows {
        Ok(mut rows) => {
            rows.truncate(max_results);
            tracing::info!(
                "DuckDuckGo answered with {} result(s) in {} ms",
                rows.len(),
                started.elapsed().as_millis()
            );
            Outcome {
                ok: true,
                provider: DUCKDUCKGO,
                results: rows,
                ..Default::default()
            }
        }
        Err(why) => {
            tracing::error!("DuckDuckGo search failed: {}", why);
            Outcome::failed(DUCKDUCKGO, FAILED)
        }
    }
}
