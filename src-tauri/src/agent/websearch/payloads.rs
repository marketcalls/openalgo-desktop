//! What the two tools hand the model, field for field and in the web's key
//! order (a `serde_json::Value` map would be written sorted; the model
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
