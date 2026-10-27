//! The one guard on a trader-supplied provider endpoint (the web's
//! `_validate_base_url`, D-028).
//!
//! OpenAlgo makes requests to this address, so it is checked before it is
//! saved: http or https only, no username or password in it, a host, and
//! never a cloud metadata address. A private or loopback host is allowed on
//! purpose: a model on the trader's own computer or network (Ollama, LM
//! Studio) is the point of these kinds. The host is not resolved: a lookup
//! in the save path would refuse a server that is simply not up yet.
//!
//! The host is read the way the WHATWG URL standard reads it, which is how
//! the HTTP client will read it too, so `http://2852039166/` and
//! `http://0xa9fea9fe/` are the metadata address here as they are on the
//! wire (the web's parser let them through). The add and edit dialogs run
//! the same steps in the browser (`src/lib/agent/baseUrlGuard.ts`) against
//! the same cases (`tests/fixtures/agent/base_url_cases.json`), so the three
//! agree (the web's two dialogs did not).

use super::{normalize_base_url, provider_spec};
use std::net::IpAddr;

pub const NOT_PARSED: &str = "The base URL could not be parsed";
pub const BAD_SCHEME: &str = "The base URL must start with http:// or https://";
pub const HAS_CREDENTIALS: &str = "The base URL must not carry a username or password";
pub const NO_HOST: &str = "The base URL must name a host";
pub const METADATA: &str = "That address is the cloud metadata endpoint and cannot be used";

/// Hosts never accepted: the cloud metadata service, however it is spelled.
const BLOCKED_HOSTS: &[&str] = &[
    "169.254.169.254",
    "fd00:ec2::254",
    "metadata.google.internal",
    "metadata",
];

/// Check an endpoint for a kind: the normalised URL, `None` when none was
/// given and the kind needs none, or the sentence that refuses it.
pub fn validate_base_url(raw: Option<&str>, kind: &str) -> Result<Option<String>, String> {
    let url = normalize_base_url(raw);
    if url.is_empty() {
        let spec = provider_spec(kind)?;
        if spec.needs_base_url {
            return Err(format!("{} requires a base URL", spec.label));
        }
        return Ok(None);
    }
    check(&url)?;
    Ok(Some(url))
}

/// The steps, in the web's order, on a normalised non-empty URL.
pub fn check(url: &str) -> Result<(), String> {
    let scheme = scheme_of(url);
    if scheme != "http" && scheme != "https" {
        return Err(BAD_SCHEME.into());
    }
    if authority_of(url).contains('@') {
        return Err(HAS_CREDENTIALS.into());
    }
    let parsed = url::Url::parse(url).map_err(|_| NOT_PARSED.to_string())?;
    let host = match parsed.host() {
        None => return Err(NO_HOST.into()),
        Some(url::Host::Domain(d)) => d.trim().to_lowercase().trim_end_matches('.').to_string(),
        Some(url::Host::Ipv4(a)) => a.to_string(),
        Some(url::Host::Ipv6(a)) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => a.to_string(),
        },
    };
    if host.is_empty() {
        return Err(NO_HOST.into());
    }
    if is_blocked(&host) {
        return Err(METADATA.into());
    }
    Ok(())
}

fn is_blocked(host: &str) -> bool {
    if BLOCKED_HOSTS.contains(&host) {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(a)) => {
            let canonical = a
                .to_ipv4_mapped()
                .map(|v4| v4.to_string())
                .unwrap_or_else(|| a.to_string());
            BLOCKED_HOSTS.contains(&canonical.as_str())
        }
        _ => false,
    }
}

/// The scheme as Python's `urlsplit` finds it (lower-cased), or empty.
fn scheme_of(url: &str) -> String {
    let Some((head, _)) = url.split_once(':') else {
        return String::new();
    };
    let valid = head.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if valid {
        head.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// The authority (`netloc`): after `//`, up to the path, query or fragment.
fn authority_of(url: &str) -> &str {
    let Some((_, rest)) = url.split_once("//") else {
        return "";
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}
