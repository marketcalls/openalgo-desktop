//! `web_search`: links from the configured provider, DuckDuckGo behind it.
//! A search for company figures goes to the trader's preferred sources
//! first (`preferred`).

use super::args::{constrained, max_results, topic, Topic};
use super::{blocked, busy, failure, json_text, mark_used, stored_key, WebTool};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::payloads::{Links, NotOk};
use crate::agent::websearch::providers::Outcome;
use crate::agent::websearch::settings::Config;
use crate::agent::websearch::taint::Constrained;
use crate::agent::websearch::{
    duckduckgo, keyed, label, DUCKDUCKGO, KEYED_LINK_PROVIDERS, PERPLEXITY,
};
use serde_json::{Map, Value};

impl WebTool {
    /// `web_search`.
    pub(super) async fn search(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let (count, count_notice) = match max_results(args) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let topic = match topic(args) {
            Ok(t) => t,
            Err(refused) => return refused,
        };
        let q = match constrained(args, "query", &self.shared.operator_message) {
            Ok(q) => q,
            Err(refused) => return refused,
        };
        if q.blocked() {
            return blocked("web_search");
        }
        let config = self.shared.budget.config(&ctx.app).await;
        let mut notices: Vec<String> = count_notice.into_iter().collect();
        let preferred = self.shared.run.live_sources(&config.preferred_sources);
        if topic == Topic::Fundamentals && !preferred.is_empty() {
            let answer = self
                .preferred_search(ctx, &config, &q, count as usize, &mut notices)
                .await;
            if let Some(answer) = answer {
                return answer;
            }
        }
        self.general_search(ctx, &config, &q, count as usize, &mut notices)
            .await
    }

    /// The open web (`_general_search`).
    async fn general_search(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        q: &Constrained,
        count: usize,
        notices: &mut Vec<String>,
    ) -> ToolOutcome {
        if let Some(refusal) = self.shared.budget.refusal(&ctx.app).await {
            tracing::info!("Web search refused before dispatch: {}", refusal.error);
            return ToolOutcome::json_ordered(&refusal);
        }
        let outcome = self
            .link_outcome(ctx, config, &q.text, count, notices)
            .await;
        if outcome.busy {
            return busy(&outcome, notices.as_slice());
        }
        self.log(
            "web_search",
            outcome.provider,
            q,
            outcome.results.len(),
            config.max_calls_per_turn,
        );
        if !outcome.ok {
            self.shared.budget.record_attempt();
            let p = NotOk::new("provider_failed", outcome.error.clone())
                .by(outcome.provider)
                .with_notices(notices.as_slice());
            return failure(outcome.provider, &p);
        }
        self.shared.budget.record(&ctx.app).await;
        for row in &outcome.results {
            self.shared.run.remember(&row.url);
        }
        let payload = Links {
            ok: true,
            provider: outcome.provider,
            query_sent: &q.text,
            query_source: q.decision.as_str(),
            result_count: outcome.results.len(),
            results: &outcome.results,
            notices: notices.as_slice(),
        };
        ToolOutcome::Ok(wrap_web_result(
            outcome.provider,
            &json_text(&payload),
            &[("kind", Some("links"))],
        ))
    }

    /// One link search (`_link_outcome`): the configured provider, and
    /// DuckDuckGo when it has no key or fails. Perplexity answers questions
    /// rather than returning links, so with it configured the first keyed
    /// link provider that has a key answers (DuckDuckGo rate limits under
    /// load, and falling back to it while a working key sat unused made
    /// every search slow). A busy gate is the answer as it is: the provider
    /// never failed, so nothing falls back.
    pub(super) async fn link_outcome(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        text: &str,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Outcome {
        let mut provider = config.provider;
        if provider == PERPLEXITY {
            let mut borrowed = None;
            for name in KEYED_LINK_PROVIDERS.iter().copied() {
                if stored_key(&ctx.app, name).await.is_some() {
                    borrowed = Some(name);
                    break;
                }
            }
            let note = format!(
                "The configured provider is Perplexity, which answers questions rather than returning links, so {} answered this search. Use web_research for a Perplexity answer.",
                borrowed.map(label).unwrap_or("DuckDuckGo")
            );
            if !notices.contains(&note) {
                notices.push(note);
            }
            provider = borrowed.unwrap_or(DUCKDUCKGO);
        }
        if KEYED_LINK_PROVIDERS.contains(&provider) {
            let name = label(provider);
            match stored_key(&ctx.app, provider).await {
                None => {
                    let message = format!(
                        "{} is the configured provider but no {} key is stored in the agent settings, so DuckDuckGo answered instead.",
                        name, name
                    );
                    if !notices.contains(&message) {
                        notices.push(message);
                    }
                }
                Some(key) => {
                    let o = keyed::search_with(&self.shared.net, provider, text, count, &key).await;
                    mark_used(&ctx.app, &o).await;
                    if o.busy || o.ok {
                        return o;
                    }
                    notices.push(format!("{} DuckDuckGo answered instead.", o.error));
                }
            }
        }
        duckduckgo::search(&self.shared.net, text, count).await
    }
}
