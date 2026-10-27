//! The one guard on a trader-supplied provider endpoint (the web's
//! `_validate_base_url`, D-028).
//!
//! OpenAlgo makes requests to this address, so it is checked before it is
//! saved: http or https only, no username or password in it, a host, and
//! never a cloud metadata address. A model on the trader's own computer or
//! network is the point of the two kinds that take an endpoint (Ollama, and
//! an OpenAI-compatible server such as LM Studio, vLLM or llamafile), so a
//! loopback or private host is allowed for both, as the web allows it. The
//! host is not resolved: a lookup in the save path would refuse a server
//! that is simply not up yet.
//!
//! Every decision is made on ONE parse, the WHATWG URL standard's (the `url`
//! crate, which the HTTP client parses the request with too), and the
//! parsed form is what is stored and later requested, so the guard and the
//! request cannot read the address differently. Text a parser may read two
//! ways is refused before parsing: a backslash (read as a slash), and any
//! space, tab, line break or control character (dropped by the standard).
//! The host decides by what it is, however it is spelled: `2130706433`,
//! `0x7f.1` and `[::ffff:127.0.0.1]` are the loopback address, and
//! `2852039166` and `[::ffff:a9fe:a9fe]` the metadata address.
//!
//! The add and edit dialogs run the same steps in the browser
//! (`src/lib/agent/baseUrlGuard.ts`) on the same cases
//! (`tests/fixtures/agent/base_url_cases.json`), so the three decide alike.

use super::{normalize_base_url, provider_spec};
use std::net::{Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

pub const BAD_CHARACTERS: &str =
    "The base URL must not contain spaces, line breaks, control characters or backslashes";
pub const NOT_PARSED: &str = "The base URL could not be parsed";
pub const BAD_SCHEME: &str = "The base URL must start with http:// or https://";
pub const HAS_CREDENTIALS: &str = "The base URL must not carry a username or password";
pub const NO_HOST: &str = "The base URL must name a host";
pub const METADATA: &str = "That address is the cloud metadata endpoint and cannot be used";

/// The cloud metadata service's names.
const METADATA_NAMES: &[&str] = &["metadata.google.internal", "metadata"];
const METADATA_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const METADATA_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254);

/// What a host is, as far as the guard cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostClass {
    /// The cloud metadata service: never.
    Metadata,
    /// This computer (`localhost`, `*.localhost`, 127/8, `::1`): a local
    /// model server, allowed.
    Loopback,
    /// Anything else, private networks included: allowed.
    Other,
}

impl HostClass {
    pub fn as_str(self) -> &'static str {
        match self {
            HostClass::Metadata => "metadata",
            HostClass::Loopback => "loopback",
            HostClass::Other => "other",
        }
    }
}

/// Check an endpoint for a kind: the URL to store and request (the parsed
/// form, without a trailing slash), `None` when none was given and the kind
/// needs none, or the sentence that refuses it.
pub fn validate_base_url(raw: Option<&str>, kind: &str) -> Result<Option<String>, String> {
    let url = normalize_base_url(raw);
    if url.is_empty() {
        let spec = provider_spec(kind)?;
        if spec.needs_base_url {
            return Err(format!("{} requires a base URL", spec.label));
        }
        return Ok(None);
    }
    check(&url).map(|parsed| Some(parsed.as_str().trim_end_matches('/').to_string()))
}

/// The steps, on trimmed non-empty text: the parsed URL, or the refusal.
pub fn check(text: &str) -> Result<Url, String> {
    if text
        .chars()
        // U+FEFF: the browser's guard reads it as space too.
        .any(|c| c == '\\' || c == '\u{feff}' || c.is_whitespace() || c.is_control())
    {
        return Err(BAD_CHARACTERS.into());
    }
    if !matches!(scheme_of(text).as_str(), "http" | "https") {
        return Err(BAD_SCHEME.into());
    }
    let parsed = Url::parse(text).map_err(|_| NOT_PARSED.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(BAD_SCHEME.into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(HAS_CREDENTIALS.into());
    }
    match host_class(&parsed) {
        None => Err(NO_HOST.into()),
        Some(HostClass::Metadata) => Err(METADATA.into()),
        Some(_) => Ok(parsed),
    }
}

/// The parsed host's class; `None` when there is no host.
pub fn host_class(parsed: &Url) -> Option<HostClass> {
    match parsed.host()? {
        Host::Domain(d) => {
            let name = d.to_lowercase();
            let name = name.trim_end_matches('.');
            if name.is_empty() {
                return None;
            }
            Some(if METADATA_NAMES.contains(&name) {
                HostClass::Metadata
            } else if name == "localhost" || name.ends_with(".localhost") {
                HostClass::Loopback
            } else {
                HostClass::Other
            })
        }
        Host::Ipv4(a) => Some(class_v4(a)),
        Host::Ipv6(a) => Some(match a.to_ipv4_mapped() {
            Some(v4) => class_v4(v4),
            None if a == METADATA_V6 => HostClass::Metadata,
            None if a.is_loopback() => HostClass::Loopback,
            None => HostClass::Other,
        }),
    }
}

fn class_v4(a: Ipv4Addr) -> HostClass {
    if a == METADATA_V4 {
        HostClass::Metadata
    } else if a.is_loopback() {
        HostClass::Loopback
    } else {
        HostClass::Other
    }
}

/// The scheme as written (lower-cased), or empty: checked before parsing so
/// a scheme the guard refuses gets the web's sentence, not "could not be
/// parsed".
fn scheme_of(text: &str) -> String {
    let Some((head, _)) = text.split_once(':') else {
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
