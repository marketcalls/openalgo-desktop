//! The fetch boundary (the web's `check_fetch_url`, `url_key`,
//! `host_matches`, `_blocked_host` and `_COMPANY_PAGE`): which address
//! `web_fetch` may hand a provider.
//!
//! A search query is built out of the trader's words; a URL cannot be,
//! because the page wanted is usually one the trader never typed. A URL
//! carries data in its path and query, so "fetch whatever the model asks
//! for" would send account details to a host of an attacker's choosing. An
//! address is accepted only when no character of it is the model's: the
//! trader wrote it (as written, or as the start of a link they pasted), a
//! search of this run returned it, or it is a company page on a preferred
//! source built from a symbol the trader wrote. What is sent is that text,
//! never the model's spelling of it (web a2ae9f1fa): a loose comparison
//! that then sent the model's own string would let the model choose among
//! spellings, a few bits a call. A fetched page returns no links, so a
//! page cannot add to what is accepted.
//!
//! The provider fetches the page from its own network; the desktop
//! connects only to the provider. Refused even when the trader typed it: a
//! scheme other than http and https, an address carrying a login, a port
//! other than 80 and 443 (the app's own ports among them), and a host that
//! can only mean a private service (a name with no dot, `localhost`,
//! `.local`, `.internal`, `.lan`, a private, loopback, link-local or
//! reserved address).
//!
//! Desktop, stricter than the web:
//! - the address is read the way the web reads it (Python's `urlsplit`,
//!   `flow::nodes::http::url`) and the way a provider will (WHATWG), and is
//!   refused when the two see different hosts or the WHATWG host is a
//!   private address the first reading missed (`127.1`, `0x7f.1`);
//! - the address that is sent (the trader's text, the search's URL) passes
//!   the same checks as the one asked for;
//! - a company page is built only from a symbol that is one of the
//!   trader's words, whole, as the search boundary takes words
//!   (`websearch::taint`); the web took any part of a word;
//! - an address the trader typed counts only where one of their links
//!   starts (the start of a word, after an opening bracket or quote, or
//!   right after `scheme://`) and ends there or at a path, query or
//!   fragment boundary, so a host inside a longer one (`evil.com` in
//!   `notevil.com`) is not the trader's; the web searches for the text
//!   anywhere in the message (security review, 2026-11-25);
//! - an address with white space (U+00A0 included), a control character or
//!   an invisible format character in it is refused before it is read, the
//!   one asked for and the one sent alike: Python's reading drops a tab or
//!   a leading control character, so a search's URL holding one matched a
//!   clean request and went out as returned.

use crate::agent::words::{printable, split, strip_edges};
use crate::flow::nodes::http::guard::{is_refused, same_host};
use crate::flow::nodes::http::url::{urlsplit, Split};
use regex::Regex;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::LazyLock;

/// The longest address accepted (`MAX_URL_CHARS`).
pub const MAX_URL_CHARS: usize = 2000;

/// What the model is told when an address is not one it may fetch.
pub const NOT_GIVEN: &str = "That address is neither in the operator's message nor in a search result from this turn. Run web_search for it first and fetch one of the URLs it returns, or ask the operator for the link. Never build a URL yourself.";

const INVALID: &str = "That is not a valid web address.";

/// Company pages whose address is a pure function of the NSE symbol, by
/// preferred source (`_COMPANY_PAGE`). A source not listed is reached only
/// through a search: its addresses are not derivable (ScanX names a page
/// after the company, not the symbol).
const SCREENER: &str = "screener.in";
static SCREENER_PAGE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^/company/([A-Za-z0-9&\-]{1,20})/(?:consolidated/?)?$").ok());

/// Where an accepted address came from (`FetchUrlCheck.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The trader wrote it.
    Operator,
    /// A search of this run returned it.
    Search,
    /// A company page built from a symbol the trader wrote.
    Preferred,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Operator => "operator",
            Source::Search => "search",
            Source::Preferred => "preferred",
        }
    }
}

/// The boundary's answer (`FetchUrlCheck`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// The address to send, never the model's spelling, and where it came
    /// from.
    Allowed { url: String, source: Source },
    /// A sentence the model can act on.
    Refused(String),
}

/// A form of a URL that two spellings of one page share (`url_key`):
/// `host/path?query` in lower case, without scheme, `www.`, port,
/// fragment or a trailing slash. `None` for text no URL reading accepts.
pub fn url_key(url: &str) -> Option<String> {
    let parts = urlsplit(url.trim()).ok()?;
    let host = parts.hostname().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let path = parts.path.trim_end_matches('/');
    let query = if parts.query.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.query)
    };
    Some(format!("{}{}{}", host, path, query).to_lowercase())
}

/// Whether a URL is on a domain or one of its subdomains (`host_matches`):
/// `screener.in` and `www.screener.in`, never `notscreener.in` or
/// `screener.in.evil.example`.
pub fn host_matches(url: &str, domain: &str) -> bool {
    let Ok(parts) = urlsplit(url.trim()) else {
        return false;
    };
    let host = parts.hostname().unwrap_or_default();
    let domain = domain.to_lowercase();
    let domain = domain.trim_matches('.');
    host == domain || host.ends_with(&format!(".{}", domain))
}

const NO_HOST: &str = "it has no public host name";
const PRIVATE: &str = "it is a private or local address";

/// Why a host may never be fetched (`_blocked_host`), or `None`.
fn blocked_host(host: &str) -> Option<&'static str> {
    let host = host.trim_end_matches('.');
    if host.is_empty() || (!host.contains('.') && !host.contains(':')) {
        return Some(NO_HOST);
    }
    let local = [".local", ".localhost", ".internal", ".lan"];
    if host == "localhost" || local.iter().any(|s| host.ends_with(s)) {
        return Some("it is a local address");
    }
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) if is_refused(ip) => Some(PRIVATE),
        _ => None,
    }
}

/// The host a WHATWG reader (the provider's) sees, judged as
/// [`blocked_host`] judges one; it reads `127.1` and `0x7f.1` as the
/// loopback address they are.
fn whatwg_blocked(parsed: &::url::Url) -> Option<&'static str> {
    match parsed.host() {
        Some(::url::Host::Ipv4(a)) => is_refused(IpAddr::V4(a)).then_some(PRIVATE),
        Some(::url::Host::Ipv6(a)) => is_refused(IpAddr::V6(a)).then_some(PRIVATE),
        Some(::url::Host::Domain(d)) => blocked_host(&d.to_lowercase()),
        None => Some(NO_HOST),
    }
}

fn not_fetched(why: &str) -> String {
    format!("That address is not fetched because {}.", why)
}

/// A character no page address carries as text: white space, a control
/// character, or one that is invisible when shown.
fn unseen(c: char) -> bool {
    c.is_whitespace() || !printable(c)
}

/// The checks every address passes, the one asked for and the one sent:
/// its parts, or the sentence that refuses it. The characters are checked
/// first, on the text as it would go out, since both readings below drop
/// some of them silently.
fn vet(text: &str) -> Result<Split, String> {
    if text.chars().any(unseen) {
        return Err(INVALID.into());
    }
    let parts = urlsplit(text).map_err(|_| INVALID.to_string())?;
    let port = parts.port().map_err(|_| INVALID.to_string())?;
    if parts.scheme != "http" && parts.scheme != "https" {
        return Err("Only http and https pages can be fetched.".into());
    }
    // The web refuses a user name or a password; any login part here.
    if parts.userinfo().is_some() {
        return Err("Addresses that carry a login are not fetched.".into());
    }
    if !matches!(port, None | Some(80) | Some(443)) {
        return Err("Only standard web ports can be fetched.".into());
    }
    let host = parts.hostname().unwrap_or_default();
    if let Some(why) = blocked_host(&host) {
        return Err(not_fetched(why));
    }
    let parsed = ::url::Url::parse(text).map_err(|_| INVALID.to_string())?;
    if !same_host(text, &host) {
        return Err(INVALID.into());
    }
    if let Some(why) = whatwg_blocked(&parsed) {
        return Err(not_fetched(why));
    }
    Ok(parts)
}

/// Why a page a provider ended up on may not be read, or `None`
/// (desktop): a redirect into a private or local host is refused as the
/// address itself would have been. A reading that finds no host judges
/// nothing.
pub fn redirect_refusal(final_url: &str) -> Option<String> {
    let text = final_url.trim();
    let python = urlsplit(text)
        .ok()
        .and_then(|p| p.hostname())
        .and_then(|h| blocked_host(&h));
    let whatwg = ::url::Url::parse(text)
        .ok()
        .filter(|u| u.host().is_some())
        .and_then(|u| whatwg_blocked(&u));
    python.or(whatwg).map(not_fetched)
}

/// The URL as the web keeps it after dropping the fragment
/// (`parts._replace(fragment="").geturl()`).
fn without_fragment(parts: &Split) -> String {
    let query = if parts.query.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.query)
    };
    format!("{}://{}{}{}", parts.scheme, parts.netloc, parts.path, query)
}

/// What can stand right before a link in the trader's text: white space,
/// or an opening bracket or quote. Anything else (a letter, a dot) means
/// the address is only the tail of a longer host or word.
fn opens_link(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | '<' | '"' | '\'' | '[' | '`')
}

/// What a sentence can put right after a link without it being part of it.
fn closes_link(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\'' | '`'
    )
}

/// Where a link can start in the trader's text: at its start, after an
/// opener, or right after `scheme://`.
fn link_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.char_indices().map(|(i, _)| i).filter(|&i| {
        let before = &text[..i];
        before.chars().next_back().is_none_or(opens_link) || before.ends_with("://")
    })
}

/// Whether the trader's link ends where the address asked for does: at its
/// end or closing punctuation only, or, the address being a prefix of the
/// link, where a further path segment, query or fragment begins (a query
/// parameter only once the address has a query).
fn ends_at_boundary(form: &str, after: &str) -> bool {
    match after.chars().next() {
        None | Some('/' | '?' | '#') => true,
        Some('&') => form.contains('?'),
        Some(_) => after.chars().all(closes_link),
    }
}

/// The trader's own text of the address, found however they cased it:
/// the address as written, or with `www.` added or taken off. Desktop
/// (security review, 2026-11-25): it counts only where one of the trader's
/// links starts and ends there or at a segment boundary, so `evil.com/x`
/// is not found inside `notevil.com/x`, `x.evil.com/x` or `evil.com/xyz`.
/// The web searches for the text anywhere in the message.
fn typed_by_operator(bare: &str, operator: &str) -> Option<String> {
    let www = bare.get(..4).filter(|p| p.eq_ignore_ascii_case("www."));
    let other = match www.and(bare.get(4..)) {
        Some(rest) => rest.to_string(),
        None => format!("www.{}", bare),
    };
    for form in [bare, other.as_str()] {
        if let Some(typed) = typed_link(form, operator) {
            return Some(format!("https://{}", typed));
        }
    }
    None
}

/// The trader's text of the first of their links that is `form`, or
/// starts with it and goes on at a boundary.
fn typed_link<'a>(form: &str, operator: &'a str) -> Option<&'a str> {
    link_starts(operator).find_map(|i| {
        let link = &operator[i..];
        let link = &link[..link.find(char::is_whitespace).unwrap_or(link.len())];
        let typed = link.get(..form.len())?;
        (typed.eq_ignore_ascii_case(form) && ends_at_boundary(form, &link[form.len()..]))
            .then_some(typed)
    })
}

/// A company page on a preferred source, rebuilt from a symbol that is one
/// of the trader's words.
fn company_page(
    parts: &Split,
    clean: &str,
    operator: &str,
    preferred: &[String],
) -> Option<String> {
    if !parts.query.is_empty() || !preferred.iter().any(|d| d == SCREENER) {
        return None;
    }
    if !host_matches(clean, SCREENER) {
        return None;
    }
    let symbol = SCREENER_PAGE
        .as_ref()?
        .captures(&parts.path)?
        .get(1)?
        .as_str();
    let named = split(operator).any(|w| strip_edges(w).eq_ignore_ascii_case(symbol));
    if !named {
        return None;
    }
    let tail = if parts.path.contains("consolidated") {
        "/consolidated/"
    } else {
        "/"
    };
    Some(format!(
        "https://www.{}/company/{}{}",
        SCREENER,
        symbol.to_uppercase(),
        tail
    ))
}

/// Decide whether `web_fetch` may send this address (`check_fetch_url`).
///
/// `operator` is the trader's text for the turn, already redacted (and
/// their earlier messages, where a run has them); `seen` maps the
/// [`url_key`] of every URL a search of this run returned to that URL
/// exactly as returned; `preferred` is the trader's preferred sources.
pub fn check(
    requested: &str,
    operator: &str,
    seen: &HashMap<String, String>,
    preferred: &[String],
) -> Check {
    let text = requested.trim();
    if text.is_empty() {
        return Check::Refused("No URL was given. Pass the full page address.".into());
    }
    if text.chars().count() > MAX_URL_CHARS {
        return Check::Refused("That URL is too long to be a page address.".into());
    }
    let text = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{}", text)
    };
    let parts = match vet(&text) {
        Ok(p) => p,
        Err(reason) => return Check::Refused(reason),
    };
    let clean = without_fragment(&parts);
    let bare = clean
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&clean)
        .trim_end_matches('/');
    let chosen = typed_by_operator(bare, operator)
        .map(|url| (url, Source::Operator))
        .or_else(|| {
            let returned = seen.get(&url_key(&clean)?)?;
            Some((returned.clone(), Source::Search))
        })
        .or_else(|| {
            company_page(&parts, &clean, operator, preferred).map(|url| (url, Source::Preferred))
        });
    match chosen {
        Some((url, source)) => match vet(&url) {
            Ok(_) => Check::Allowed { url, source },
            Err(reason) => Check::Refused(reason),
        },
        None => Check::Refused(NOT_GIVEN.into()),
    }
}
