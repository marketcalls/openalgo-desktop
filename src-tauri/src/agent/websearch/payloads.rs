//! What the three tools hand the model, field for field and in the web's
//! key order (a `serde_json::Value` map would be written sorted; the model
//! reads these as the web wrote its dicts).

use super::providers::{Citation, Link};
use serde::Serialize;

/// A call that did not reach a result: a cap, a refusal, a busy gate or a
/// provider's failure. Optional keys appear only where the web's payload
/// has them.
#[derive(Debug, Clone, Serialize)]
pub struct NotOk<'a> {
    pub ok: bool,
    pub error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<&'a str>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notices: Option<&'a [String]>,
    /// Present (possibly null) on a research failure only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<Option<f64>>,
}

impl<'a> NotOk<'a> {
    pub fn new(error: &'a str, message: impl Into<String>) -> NotOk<'a> {
        NotOk {
            ok: false,
            error,
            provider: None,
            message: message.into(),
            notices: None,
            cost_usd: None,
        }
    }

    pub fn by(mut self, provider: &'a str) -> NotOk<'a> {
        self.provider = Some(provider);
        self
    }

    pub fn with_notices(mut self, notices: &'a [String]) -> NotOk<'a> {
        self.notices = Some(notices);
        self
    }

    pub fn with_cost(mut self, cost: Option<f64>) -> NotOk<'a> {
        self.cost_usd = Some(cost);
        self
    }
}

/// `web_search`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct Links<'a> {
    pub ok: bool,
    pub provider: &'a str,
    pub query_sent: &'a str,
    pub query_source: &'a str,
    pub result_count: usize,
    pub results: &'a [Link],
    pub notices: &'a [String],
}

/// `web_research`'s result.
#[derive(Debug, Clone, Serialize)]
pub struct Answer<'a> {
    pub ok: bool,
    pub provider: &'a str,
    pub model: &'a str,
    pub question_sent: &'a str,
    pub question_source: &'a str,
    pub answer: &'a str,
    pub citation_count: usize,
    pub citations: &'a [Citation],
    pub cost_usd: Option<f64>,
}

/// A link a fundamentals search found on a preferred source: the link's
/// own fields, then the source.
#[derive(Debug, Clone, Serialize)]
pub struct SourcedLink {
    #[serde(flatten)]
    pub link: Link,
    pub preferred_source: String,
}

/// `web_search`'s result for company figures (`topic="fundamentals"`).
#[derive(Debug, Clone, Serialize)]
pub struct SourcedLinks<'a> {
    pub ok: bool,
    pub provider: &'a str,
    pub query_sent: &'a str,
    pub query_source: &'a str,
    pub topic: &'a str,
    pub preferred_sources_searched: &'a [String],
    pub preferred_sources_still_to_try: &'a [String],
    pub result_count: usize,
    pub results: &'a [SourcedLink],
    pub notices: &'a [String],
}

/// `web_fetch`'s result: one slice of a page (`_fetch_payload`).
#[derive(Debug, Clone, Serialize)]
pub struct PageSlice<'a> {
    pub ok: bool,
    pub provider: &'a str,
    pub url: &'a str,
    pub url_source: &'a str,
    pub title: &'a str,
    pub total_chars: usize,
    pub offset: usize,
    pub truncated: bool,
    /// Null when nothing is left to read.
    pub next_offset: Option<usize>,
    pub from_this_turn_copy: bool,
    pub text: &'a str,
    pub notices: &'a [String],
    /// Where the page ended up, when that is another page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_url: Option<&'a str>,
}

/// A page read that failed, with the next step when the page was on a
/// preferred source.
#[derive(Debug, Clone, Serialize)]
pub struct FetchFailed<'a> {
    pub ok: bool,
    pub error: &'a str,
    pub provider: &'a str,
    pub message: &'a str,
    pub notices: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_source_failed: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
}
