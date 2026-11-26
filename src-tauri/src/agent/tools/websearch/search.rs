//! `web_search`: links from the configured provider, DuckDuckGo behind it.
//! A search for company figures goes to the trader's preferred sources
//! first (`preferred`).

use super::args::{constrained, max_results, topic, Topic};
use super::{blocked, busy, failure, json_text, keyed_in_order, mark_used, WebTool};
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
        let q = match constrained(
            args,
            "query",
            &self.shared.operator_message,
            &self.shared.operator_history,
        ) {
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

    /// One link search (`_link_outcome`): the configured provider first,
    /// then every other keyed link provider that has a key, and DuckDuckGo
    /// only when none of them answered (web 5e19ce407). A provider that
    /// fails (a rate limit, a timeout, a bad key) is a reason to try the
    /// next paid one, not to fall straight to the slow keyless scraper while
    /// a working key sits unused: DuckDuckGo once took 22 seconds to answer
    /// the search Firecrawl had refused with a 429. Every request that went
    /// out and failed counts against the turn. Perplexity answers questions
    /// rather than returning links, so with it configured the first keyed
    /// link provider that has a key is asked first. A busy gate is the answer
    /// as it is: the provider never failed, so nothing falls back.
    pub(super) async fn link_outcome(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        text: &str,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Outcome {
        let chain = keyed_in_order(&ctx.app, KEYED_LINK_PROVIDERS, config.provider).await;
        let mut provider = config.provider;
        if provider == PERPLEXITY {
            let borrowed = chain.first().map(|(name, _)| *name);
            let note = format!(
                "The configured provider is Perplexity, which answers questions rather than returning links, so {} answered this search. Use web_research for a Perplexity answer.",
                borrowed.map(label).unwrap_or("DuckDuckGo")
            );
            if !notices.contains(&note) {
                notices.push(note);
            }
            provider = borrowed.unwrap_or(DUCKDUCKGO);
        }
        let configured_has_key = chain.first().is_some_and(|(n, _)| *n == provider);
        if KEYED_LINK_PROVIDERS.contains(&provider) && !configured_has_key {
            let name = label(provider);
            let instead = chain.first().map(|(n, _)| label(n)).unwrap_or("DuckDuckGo");
            let message = format!(
                "{} is the configured provider but no {} key is stored in the agent settings, so {} answered instead.",
                name, name, instead
            );
            if !notices.contains(&message) {
                notices.push(message);
            }
        }
        for (index, &(name, ref key)) in chain.iter().enumerate() {
            let o = keyed::search_with(&self.shared.net, name, text, count, key).await;
            mark_used(&ctx.app, &o).await;
            if o.busy || o.ok {
                return o;
            }
            self.shared.budget.record_attempt();
            let following = chain
                .get(index + 1)
                .map(|(n, _)| label(n))
                .unwrap_or("DuckDuckGo");
            notices.push(format!("{} {} was tried instead.", o.error, following));
        }
        // The last notice said "tried instead"; when DuckDuckGo is what
        // answers, it says so in the words the trader already knows.
        if let Some(last) = notices.last_mut().filter(|_| !chain.is_empty()) {
            if let Some(head) = last.strip_suffix("DuckDuckGo was tried instead.") {
                *last = format!("{}DuckDuckGo answered instead.", head);
            }
        }
        duckduckgo::search(&self.shared.net, text, count).await
    }
}
