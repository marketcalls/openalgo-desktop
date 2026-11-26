//! The trader's preferred sources for company figures (the web's
//! `normalise_source`, `preferred_sources_from` and
//! `_validated_preferred_sources`): bare domains, best first, at most five.
//! A search with `topic="fundamentals"` tries them in order before the
//! open web, and the prompt names them.
//!
//! A stored row is read leniently: no row is the shipped default, a stored
//! empty list turns the preference off, and a row that is not a list of
//! domains reads as the default (a damaged row must not take the
//! preference away silently in either direction). A submitted list is
//! refused whole, naming the entry that is not a website.

use super::{DEFAULT_PREFERRED_SOURCES, KEY_PREFERRED_SOURCES, MAX_PREFERRED_SOURCES};
use crate::agent::settings::pyrepr::py_str;
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// The longest domain accepted.
const MAX_DOMAIN_CHARS: usize = 120;

static DOMAIN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$").ok()
});

fn is_domain(host: &str) -> bool {
    DOMAIN.as_ref().is_some_and(|re| re.is_match(host))
}

/// The shipped default, best first.
pub fn defaults() -> Vec<String> {
    DEFAULT_PREFERRED_SOURCES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// What a trader typed, reduced to a bare domain: `screener.in`,
/// `https://www.Screener.in/company/TCS/` and `Screener.in` all read as
/// `screener.in`. The error names the entry, as the web's does.
pub fn normalise(raw: &Value) -> Result<String, String> {
    let shown = py_str(raw);
    let text = if crate::agent::models::truthy(raw) {
        shown.trim().to_lowercase()
    } else {
        String::new()
    };
    let after_scheme = text.split_once("://").map(|(_, r)| r).unwrap_or(&text);
    let netloc = after_scheme
        .split('/')
        .next()
        .and_then(|h| h.split('?').next())
        .and_then(|h| h.split('#').next())
        .unwrap_or_default();
    let host_port = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let host = host_port
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() || host.chars().count() > MAX_DOMAIN_CHARS || !is_domain(host) {
        let quoted: String = shown.trim().chars().take(60).collect();
        return Err(format!(
            "'{}' is not a website address such as screener.in",
            quoted
        ));
    }
    Ok(host.to_string())
}

/// The domains of a list, each once, in order, or the first refusal.
fn domains_of(items: &[Value]) -> Result<Vec<String>, String> {
    let mut found: Vec<String> = Vec::new();
    for item in items {
        let domain = normalise(item)?;
        if !found.contains(&domain) {
            found.push(domain);
        }
    }
    Ok(found)
}

/// The stored row, read leniently (`preferred_sources_from`).
pub fn from_row(raw: Option<&str>) -> Vec<String> {
    let text = raw.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return defaults();
    }
    let parsed = match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(items)) => domains_of(&items),
        _ => Err(String::new()),
    };
    match parsed {
        Ok(mut found) => {
            found.truncate(MAX_PREFERRED_SOURCES);
            found
        }
        Err(_) => {
            tracing::warn!(
                "Agent setting {} is not a list of domains; using the default",
                KEY_PREFERRED_SOURCES
            );
            defaults()
        }
    }
}

/// A submitted list, checked whole, as the text to store
/// (`_validated_preferred_sources`): a list of addresses, or one text of
/// them separated by commas or spaces. An empty list turns the preference
/// off.
pub fn validated(raw: &Value) -> Result<String, String> {
    let items: Vec<Value> = match raw {
        Value::String(s) => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .map(|p| Value::String(p.to_string()))
            .collect(),
        Value::Array(items) => items.clone(),
        _ => return Err("preferred_sources must be a list of website addresses".into()),
    };
    let found = domains_of(&items)?;
    if found.len() > MAX_PREFERRED_SOURCES {
        return Err(format!(
            "preferred_sources can hold at most {} sites",
            MAX_PREFERRED_SOURCES
        ));
    }
    // Python's `json.dumps(list)`, so the stored text is the web's.
    let quoted: Vec<String> = found
        .iter()
        .map(|d| serde_json::to_string(d).unwrap_or_default())
        .collect();
    Ok(format!("[{}]", quoted.join(", ")))
}
