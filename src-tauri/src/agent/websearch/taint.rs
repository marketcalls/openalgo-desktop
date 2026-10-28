//! The taint boundary (the web's `websearch.redact` and `constrain_query`).
//!
//! The model's query never reaches a provider. The outgoing query is built
//! from the trader's own message for the turn: a token of the model's
//! survives only when it is one of that message's words, whole (the web
//! kept any substring; a deviation), and when none survives
//! the message itself is sent. A word the model invented, or copied out of
//! a poisoned tool result, cannot leave the machine. With no message to
//! build from, nothing is sent.
//!
//! Redaction runs first, while the punctuation that marks `api_key=...`,
//! `Bearer ...`, prefixed keys and emails is still there; long high-entropy
//! tokens go too. The marker it leaves is never sent.

use regex::Regex;
use std::sync::LazyLock;

const REDACTED: &str = "[redacted]";
const REDACTION_TOKEN: &str = "redacted";

/// The longest outgoing query (the web's `MAX_QUERY_CHARS`).
pub const MAX_QUERY_CHARS: usize = 400;

/// A token this long is tested for being a key (`MIN_ENTROPY_CHARS`).
pub const MIN_ENTROPY_CHARS: usize = 24;
/// A pure hex run this long is a secret on its own (`MIN_HEX_CHARS`).
pub const MIN_HEX_CHARS: usize = 32;

/// Stripped from a token's ends before it is compared (`_EDGE_PUNCTUATION`).
const EDGE: &[char] = &[
    ' ', '\t', '\n', '\r', '"', '\'', '`', '.', ',', ';', ':', '!', '?', '(', ')', '[', ']', '{',
    '}', '<', '>', '|', '\\', '/', '*', '~', '#',
];

fn compile(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // key=value and key: value.
        r"(?i)\b(?:api[_\-]?key|apikey|access[_\-]?token|refresh[_\-]?token|auth[_\-]?token|feed[_\-]?token|id[_\-]?token|client[_\-]?secret|private[_\-]?key|secret|password|passwd|pwd|session[_\-]?id|sessionid|cookie|signature)\s*[:=]\s*[^\s,;]+",
        // Authorization headers and anything shaped like one.
        r"(?i)\b(?:bearer|basic|token)\s+[A-Za-z0-9._\-/+=]{8,}",
        // Provider-prefixed keys with a separator.
        r"(?i)\b(?:sk|pk|rk|ak|api|key|pplx|tvly|xai|gsk|hf|nvapi|shpat|glpat|dop_v1|ghp|gho|ghu|ghs|ghr|github_pat|xoxb|xoxp|xoxa|xoxs|xapp)[-_][A-Za-z0-9_\-]{8,}",
        // The prefixes with no separator.
        r"\b(?:AKIA|ASIA|AIza|ya29\.)[A-Za-z0-9_\-]{10,}",
        // Emails.
        r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
    ]
    .iter()
    .filter_map(|p| compile(p))
    .collect()
});

/// Python's `str.isspace` (what `str.split()` splits on): Unicode white
/// space plus the four information separators.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.split()`: runs of white space, none empty.
pub fn split(text: &str) -> impl Iterator<Item = &str> {
    text.split(is_space).filter(|t| !t.is_empty())
}

/// A token without its edge punctuation.
pub fn strip_edges(token: &str) -> &str {
    token.trim_matches(|c| EDGE.contains(&c))
}

/// A token that looks like a key rather than a word (`_is_high_entropy`).
pub fn is_high_entropy(token: &str) -> bool {
    if token.chars().count() < MIN_ENTROPY_CHARS {
        return false;
    }
    let symbol = |c: char| "+/=_-.".contains(c);
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || symbol(c))
    {
        return false;
    }
    let classes = [
        token.chars().any(|c| c.is_ascii_lowercase()),
        token.chars().any(|c| c.is_ascii_uppercase()),
        token.chars().any(|c| c.is_ascii_digit()),
        token.chars().any(symbol),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if classes >= 3 {
        return true;
    }
    token.chars().count() >= MIN_HEX_CHARS && token.chars().all(|c| c.is_ascii_hexdigit())
}

/// Credential-shaped text out, white space collapsed (`redact`).
pub fn redact(text: &str) -> String {
    if text.trim_matches(is_space).is_empty() {
        return String::new();
    }
    let mut cleaned = text.to_string();
    for p in PATTERNS.iter() {
        cleaned = p.replace_all(&cleaned, REDACTED).into_owned();
    }
    split(&cleaned)
        .map(|t| {
            if is_high_entropy(strip_edges(t)) {
                REDACTED
            } else {
                t
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How the outgoing query was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Every token the model asked for is the trader's.
    Verbatim,
    /// Some were dropped.
    Constrained,
    /// None survived: the trader's message went instead.
    OperatorMessage,
    /// No message to build from: nothing is sent.
    Blocked,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Verbatim => "verbatim",
            Decision::Constrained => "constrained",
            Decision::OperatorMessage => "operator_message",
            Decision::Blocked => "blocked",
        }
    }
}

/// The query that goes out (`ConstrainedQuery`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constrained {
    pub text: String,
    pub decision: Decision,
    pub kept: usize,
    pub dropped: usize,
}

impl Constrained {
    pub fn blocked(&self) -> bool {
        self.decision == Decision::Blocked || self.text.is_empty()
    }

    fn block() -> Constrained {
        Constrained {
            text: String::new(),
            decision: Decision::Blocked,
            kept: 0,
            dropped: 0,
        }
    }
}

/// Join tokens with spaces, stopping on a token boundary within `limit`
/// characters (`_join_within`).
fn join_within(tokens: &[&str], limit: usize) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut used = 0;
    for t in tokens {
        let cost = t.chars().count() + usize::from(!out.is_empty());
        if used + cost > limit {
            break;
        }
        out.push(t);
        used += cost;
    }
    out.join(" ")
}

/// The trader's own words, as a set: the redacted message split as
/// `str.split()` splits it, each word without its edge punctuation and
/// lower-cased, the redaction marker left out.
fn operator_words(operator: &str) -> std::collections::HashSet<String> {
    split(operator)
        .map(|t| strip_edges(t).to_lowercase())
        .filter(|w| !w.is_empty() && w != REDACTION_TOKEN)
        .collect()
}

/// Build the outgoing query out of the trader's own words
/// (`constrain_query`). A word of the model's is kept only when it is one
/// of the trader's words, whole: the web kept any substring of the
/// message, so a fragment of a word, of the redaction marker, or a single
/// letter or digit passed, and data could leave a piece at a time
/// (security review, 2026-10-28; a deviation from the web).
pub fn constrain(requested: &str, operator_message: &str) -> Constrained {
    let operator = redact(operator_message);
    if operator.is_empty() {
        return Constrained::block();
    }
    let words = operator_words(&operator);
    let requested = redact(requested);
    let mut kept: Vec<&str> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut dropped = 0;
    for raw in split(&requested) {
        let token = strip_edges(raw);
        let lowered = token.to_lowercase();
        if lowered.is_empty() || lowered == REDACTION_TOKEN {
            continue;
        }
        if !words.contains(&lowered) {
            dropped += 1;
            continue;
        }
        if seen.contains(&lowered) {
            continue;
        }
        seen.push(lowered);
        kept.push(token);
    }
    let text = join_within(&kept, MAX_QUERY_CHARS);
    if text.is_empty() {
        let own: Vec<&str> = split(&operator)
            .filter(|t| strip_edges(t).to_lowercase() != REDACTION_TOKEN)
            .collect();
        let fallback = join_within(&own, MAX_QUERY_CHARS);
        if fallback.is_empty() {
            return Constrained::block();
        }
        return Constrained {
            text: fallback,
            decision: Decision::OperatorMessage,
            kept: 0,
            dropped,
        };
    }
    Constrained {
        text,
        decision: if dropped == 0 {
            Decision::Verbatim
        } else {
            Decision::Constrained
        },
        kept: kept.len(),
        dropped,
    }
}
