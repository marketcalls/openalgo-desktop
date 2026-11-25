//! The `websearch` toolkit (the web's `WebSearchToolkit`): `web_search`
//! for links from DuckDuckGo or Tavily, `web_research` for Perplexity's
//! cited answer. Offered on every surface, only when the turn's Web search
//! switch is on (the composer's per-turn switch; absent means on).
//!
//! The safety envelope is the web's: the outgoing query is built from the
//! trader's own message (`websearch::taint`), a per-turn budget and a
//! daily cap are checked before a call and counted only after one
//! succeeds, results come back inside a `<web_result trust="third-party">`
//! block, and the decision is logged, never the query.
//!
//! D-020: the web returned a provider's failure as bare JSON while its
//! results were wrapped; a provider's failure is third-party text too, so
//! it is wrapped the same way (`kind="error"`).
//!
//! - this file: the group, the tools and what they share in one run, and
//!   the shapes a refusal takes.
//! - `args`: the model's arguments, checked as the web checks them.
//! - `budget`: the per-turn budget and the daily cap.
//! - `search`: `web_search` and the provider that answers it.
//! - `research`: `web_research`.

mod args;
mod budget;
mod research;
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
    operator_message: String,
    budget: Budget,
    net: Net,
}

fn build(ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let shared = Arc::new(Shared {
        operator_message: taint::redact(&ctx.operator_message),
        budget: Budget::new(),
        net: providers::net(),
    });
    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
    for (name, kind) in [
        ("web_search", Kind::Search),
        ("web_research", Kind::Research),
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
        }
    }
}

/// A result as compact JSON in its own key order, within the cap.
fn json_text<T: serde::Serialize>(v: &T) -> String {
    dumps_capped_ordered(v, MAX_RESULT_CHARS)
}

/// A provider's failure, wrapped as third-party text (D-020).
fn failure(provider: &str, payload: &NotOk<'_>) -> ToolOutcome {
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

/// A provider's stored key, opened at the moment of use.
async fn stored_key(app: &Arc<AppState>, provider: &'static str) -> Option<Secret> {
    app.blocking(move |c| Ok(settings::stored_key(c, provider)))
        .await
        .ok()
        .flatten()
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
