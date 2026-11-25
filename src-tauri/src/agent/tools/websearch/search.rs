//! `web_search`: links from the configured provider, DuckDuckGo behind it.

use super::args::{constrained, max_results};
use super::{blocked, busy, failure, json_text, mark_used, stored_key, WebTool};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::payloads::{Links, NotOk};
use crate::agent::websearch::providers::{self, Outcome};
use crate::agent::websearch::settings::Config;
use crate::agent::websearch::taint::Constrained;
use crate::agent::websearch::{duckduckgo, DUCKDUCKGO, PERPLEXITY, TAVILY};
use serde_json::{Map, Value};

const PERPLEXITY_TAVILY: &str = "The configured provider is Perplexity, which answers questions rather than returning links, so Tavily answered this search. Use web_research for a Perplexity answer.";
const PERPLEXITY_DUCKDUCKGO: &str = "The configured provider is Perplexity, which answers questions rather than returning links, so DuckDuckGo answered this search. Use web_research for a Perplexity answer.";
const TAVILY_WITHOUT_KEY: &str = "Tavily is the configured provider but no Tavily key is stored in the agent settings, so DuckDuckGo answered instead.";

impl WebTool {
    /// `web_search`.
    pub(super) async fn search(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let (count, count_notice) = match max_results(args) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let q = match constrained(args, "query", &self.shared.operator_message) {
            Ok(q) => q,
            Err(refused) => return refused,
        };
        if q.blocked() {
            return blocked("web_search");
        }
        if let Some(refusal) = self.shared.budget.refusal(&ctx.app).await {
            tracing::info!("Web search refused before dispatch: {}", refusal.error);
            return ToolOutcome::json_ordered(&refusal);
        }
        let config = self.shared.budget.config(&ctx.app).await;
        let mut notices: Vec<String> = count_notice.into_iter().collect();
        let outcome = self
            .link_search(ctx, &config, &q, count as usize, &mut notices)
            .await;
        if outcome.busy {
            return busy(&outcome, &notices);
        }
        self.log(
            "web_search",
            outcome.provider,
            &q,
            outcome.results.len(),
            config.max_calls_per_turn,
        );
        if !outcome.ok {
            let p = NotOk::new("provider_failed", outcome.error.clone())
                .by(outcome.provider)
                .with_notices(&notices);
            return failure(outcome.provider, &p);
        }
        self.shared.budget.record(&ctx.app).await;
        let payload = Links {
            ok: true,
            provider: outcome.provider,
            query_sent: &q.text,
            query_source: q.decision.as_str(),
            result_count: outcome.results.len(),
            results: &outcome.results,
            notices: &notices,
        };
        ToolOutcome::Ok(wrap_web_result(
            outcome.provider,
            &json_text(&payload),
            &[("kind", Some("links"))],
        ))
    }

    /// The link provider's answer: Perplexity configured falls back to
    /// Tavily when a Tavily key is stored, else DuckDuckGo; Tavily without
    /// a key, or failing, falls back to DuckDuckGo; a busy gate does not.
    async fn link_search(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        q: &Constrained,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Outcome {
        let mut provider = config.provider;
        let mut tavily_key = None;
        if provider == PERPLEXITY {
            tavily_key = stored_key(&ctx.app, TAVILY).await;
            if tavily_key.is_some() {
                notices.push(PERPLEXITY_TAVILY.into());
                provider = TAVILY;
            } else {
                notices.push(PERPLEXITY_DUCKDUCKGO.into());
                provider = DUCKDUCKGO;
            }
        }
        if provider == TAVILY {
            if tavily_key.is_none() {
                tavily_key = stored_key(&ctx.app, TAVILY).await;
            }
            match tavily_key {
                None => notices.push(TAVILY_WITHOUT_KEY.into()),
                Some(key) => {
                    let o = providers::tavily(&self.shared.net, &q.text, count, &key).await;
                    mark_used(&ctx.app, &o).await;
                    if o.busy || o.ok {
                        return o;
                    }
                    notices.push(format!("{} DuckDuckGo answered instead.", o.error));
                }
            }
        }
        duckduckgo::search(&self.shared.net, &q.text, count).await
    }
}
