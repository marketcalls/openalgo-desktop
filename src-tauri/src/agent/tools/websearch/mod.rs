//! The `websearch` toolkit (the web's `WebSearchToolkit`): `web_search`
//! for links from DuckDuckGo, Tavily, Firecrawl or TinyFish,
//! `web_research` for Perplexity's cited answer, and `web_fetch` for the
//! text of one page through Firecrawl or TinyFish. Offered on every
//! surface, only when the turn's Web search switch is on (the composer's
//! per-turn switch; absent means on).
//!
//! The safety envelope is the web's: the outgoing query is built from the
//! trader's own message (`websearch::taint`), a page address must be one
//! the trader wrote or a search of this run returned
//! (`websearch::fetch_url`), a per-turn budget and a daily cap are checked
//! before a call (the turn counts every request that went out, the day
//! only answers), results come back inside a
//! `<web_result trust="third-party">` block, and the decision is logged,
//! never the query or the address.
//!
//! D-020: the web returned a provider's failure as bare JSON while its
//! results were wrapped; a provider's failure is third-party text too, so
//! it is wrapped the same way (`kind="error"`).
//!
//! - this file: the group, the tools and what they share in one run, and
//!   the shapes a refusal takes.
//! - `args`: the model's arguments, checked as the web checks them.
//! - `budget`: the per-turn budget and the daily cap.
//! - `run_state`: what one run remembers: the URLs its searches returned,
//!   the pages it read, the preferred sources that failed.
//! - `search`: `web_search` on the open web, and the provider that
//!   answers it.
//! - `preferred`: `web_search` for company figures, on the trader's
//!   preferred sources first.
//! - `fetch`: `web_fetch`.
//! - `research`: `web_research`.

mod args;
mod budget;
mod fetch;
mod preferred;
mod research;
mod run_state;
mod search;

use crate::agent::contract::json::dumps_capped_ordered;
use crate::agent::contract::tool::{
    AgentTool, Capability, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::tools::specs::web_or_log;
use crate::agent::websearch::payloads::NotOk;
use crate::agent::websearch::providers::{self, Net, Outcome};
use crate::agent::websearch::settings;
use crate::agent::websearch::taint::{self, Constrained};
use crate::security::Secret;
use crate::state::AppState;
use async_trait::async_trait;
use budget::Budget;
use run_state::RunState;
use serde_json::{Map, Value};
use std::sync::Arc;

pub const GROUP: ToolGroup = ToolGroup {
    key: "websearch",
    surfaces: Surfaces::ALL,
    requires: &[Capability::WebSearch],
    order: 80,
    description: "Web search for links, and cited web research.",
    build,
};

const NO_OPERATOR_MESSAGE: &str = "Nothing was sent to any provider. Every outgoing query is built from the operator's own message for this turn, and that message is not available to this tool, so there was nothing safe to send. Tell the operator that web search is unavailable for this turn and answer from what you already have.";

/// What the toolkit's tools share in one run.
struct Shared {
    /// The trader's message for the turn, redacted.
    operator_message: String,
    /// Their earlier messages in the conversation, redacted (web 5e19ce407):
    /// a follow-up keeps the company it names only earlier.
    operator_history: String,
    budget: Budget,
    run: RunState,
    net: Net,
}

fn build(ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let shared = Arc::new(Shared {
        operator_message: taint::redact(&ctx.operator_message),
        operator_history: taint::redact(&ctx.operator_history),
        budget: Budget::new(),
        run: RunState::default(),
        net: providers::net(),
    });
    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
    for (name, kind) in [
        ("web_search", Kind::Search),
        ("web_research", Kind::Research),
        ("web_fetch", Kind::Fetch),
    ] {
        if let Some(spec) = web_or_log(GROUP.key, name) {
            tools.push(Arc::new(WebTool {
                spec,
                kind,
                shared: shared.clone(),
            }));
        }
    }
    tools
}

/// Which of the toolkit's tools one entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Search,
    Research,
    Fetch,
}

struct WebTool {
    spec: ToolSpec,
    kind: Kind,
    shared: Arc<Shared>,
}

#[async_trait]
impl AgentTool for WebTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        match self.kind {
            Kind::Search => self.search(ctx, &args).await,
            Kind::Research => self.research(ctx, &args).await,
            Kind::Fetch => self.fetch(ctx, &args).await,
        }
    }
}

/// A result as compact JSON in its own key order, within the cap.
fn json_text<T: serde::Serialize>(v: &T) -> String {
    dumps_capped_ordered(v, MAX_RESULT_CHARS)
}

/// A provider's failure, wrapped as third-party text (D-020).
fn failure<T: serde::Serialize>(provider: &str, payload: &T) -> ToolOutcome {
    ToolOutcome::Ok(wrap_web_result(
        provider,
        &json_text(payload),
        &[("kind", Some("error"))],
    ))
}

fn busy(outcome: &Outcome, notices: &[String]) -> ToolOutcome {
    let mut p = NotOk::new("platform_busy", outcome.error.clone()).by(outcome.provider);
    if !notices.is_empty() {
        p = p.with_notices(notices);
    }
    ToolOutcome::json_ordered(&p)
}

fn blocked(tool: &str) -> ToolOutcome {
    tracing::warn!(
        "{} refused: no operator message is available for this turn, so no query could be constructed",
        tool
    );
    ToolOutcome::json_ordered(&NotOk::new("no_operator_message", NO_OPERATOR_MESSAGE))
}

/// Names joined for a sentence (`_or_list`): `a`, `a or b`, `a, b or c`.
fn or_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} or {}", rest.join(", "), last),
    }
}

/// A provider's stored key, opened at the moment of use.
async fn stored_key(app: &Arc<AppState>, provider: &'static str) -> Option<Secret> {
    app.blocking(move |c| Ok(settings::stored_key(c, provider)))
        .await
        .ok()
        .flatten()
}

/// The providers of `list` whose key is stored, `first` at the head when it
/// is one of them and the rest in the list's order, each key opened once,
/// at the moment of use, on one pooled connection: the chain a link search
/// falls through (`_link_outcome`) and the pages' candidates
/// (`_fetch_candidates`).
async fn keyed_in_order(
    app: &Arc<AppState>,
    list: &'static [&'static str],
    first: &str,
) -> Vec<(&'static str, Secret)> {
    let order: Vec<&'static str> = list
        .iter()
        .copied()
        .filter(|name| *name == first)
        .chain(list.iter().copied().filter(|name| *name != first))
        .collect();
    app.blocking(move |c| {
        Ok(order
            .into_iter()
            .filter_map(|name| settings::stored_key(c, name).map(|key| (name, key)))
            .collect())
    })
    .await
    .unwrap_or_default()
}

/// Record that a provider was handed its key.
async fn mark_used(app: &Arc<AppState>, outcome: &Outcome) {
    if outcome.key_used {
        let p = outcome.provider;
        let _ = app
            .blocking(move |c| {
                settings::mark_used(c, p);
                Ok(())
            })
            .await;
    }
}

impl Shared {
    /// Everything the trader typed that an address may be matched to or
    /// built from (`_operator_words`): the turn's message and their earlier
    /// ones, or nothing when the turn has no message of its own, so a run
    /// with nothing from the trader fetches nothing they named earlier.
    fn operator_words(&self) -> String {
        if self.operator_message.is_empty() {
            return String::new();
        }
        format!("{}\n{}", self.operator_message, self.operator_history)
    }
}

impl WebTool {
    /// The decision about one call, never its content.
    fn log(&self, tool: &str, provider: &str, q: &Constrained, results: usize, per_turn: i64) {
        tracing::info!(
            "{}: provider={} decision={} kept={} dropped={} results={} turn={}/{}",
            tool,
            provider,
            q.decision.as_str(),
            q.kept,
            q.dropped,
            results,
            self.shared.budget.used(),
            per_turn
        );
    }
}
