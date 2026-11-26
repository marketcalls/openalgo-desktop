//! `web_search`: links from the configured provider, the other keyed
//! providers that hold a key and DuckDuckGo behind it, or DuckDuckGo first
//! when it is the one configured. A search for company figures goes to the
//! trader's preferred sources first (`preferred`).

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
use crate::security::Secret;
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
    ///
    /// Desktop: with DuckDuckGo configured, DuckDuckGo is asked first and a
    /// keyed provider only once it failed. The web's chain puts every keyed
    /// provider that holds a key ahead of it, so a trader who kept the
    /// keyless default and stored a Firecrawl key for `web_fetch` had every
    /// search sent to Firecrawl's paid search, with no notice, while the
    /// panel said searches run on DuckDuckGo (a web defect not copied:
    /// `docs/web-sync/agent-fetch-boundary-2026-11-26.md`).
    pub(super) async fn link_outcome(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        text: &str,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Outcome {
        let chain = keyed_in_order(&ctx.app, KEYED_LINK_PROVIDERS, config.provider).await;
        let provider = link_provider(config.provider, &chain, notices);
        if provider == DUCKDUCKGO {
            return self
                .duckduckgo_first(ctx, &chain, text, count, notices)
                .await;
        }
        let configured_has_key = chain.first().is_some_and(|(n, _)| *n == provider);
        if KEYED_LINK_PROVIDERS.contains(&provider) && !configured_has_key {
            let name = label(provider);
            let instead = chain.first().map(|(n, _)| label(n)).unwrap_or("DuckDuckGo");
            push_once(
                notices,
                format!(
                    "{} is the configured provider but no {} key is stored in the agent settings, so {} answered instead.",
                    name, name, instead
                ),
            );
        }
        if let Some(answer) = self
            .keyed_in_turn(ctx, &chain, (text, count), notices, Some("DuckDuckGo"))
            .await
        {
            return answer;
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

    /// DuckDuckGo first (configured, or Perplexity with no keyed provider
    /// holding a key), and the keyed providers that hold a key only when it
    /// failed (desktop, see [`Self::link_outcome`]).
    async fn duckduckgo_first(
        &self,
        ctx: &ToolCtx,
        chain: &[(&'static str, Secret)],
        text: &str,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Outcome {
        let o = duckduckgo::search(&self.shared.net, text, count).await;
        let Some((next, _)) = chain.first().filter(|_| !o.ok && !o.busy) else {
            return o;
        };
        self.shared.budget.record_attempt();
        notices.push(format!("{} {} was tried instead.", o.error, label(next)));
        self.keyed_in_turn(ctx, chain, (text, count), notices, None)
            .await
            .unwrap_or(o)
    }

    /// Ask the providers of `chain` in turn until one answers or its gate
    /// is busy, which is the answer. A failure with something still to try
    /// (the next provider, else `then`) is said in a notice naming it and
    /// counts against the turn; the last failure, with nothing after it,
    /// is the answer. `None` when every provider failed and `then` is next.
    async fn keyed_in_turn(
        &self,
        ctx: &ToolCtx,
        chain: &[(&'static str, Secret)],
        (text, count): (&str, usize),
        notices: &mut Vec<String>,
        then: Option<&'static str>,
    ) -> Option<Outcome> {
        for (index, &(name, ref key)) in chain.iter().enumerate() {
            let o = keyed::search_with(&self.shared.net, name, text, count, key).await;
            mark_used(&ctx.app, &o).await;
            if o.busy || o.ok {
                return Some(o);
            }
            let following = chain.get(index + 1).map(|(n, _)| label(n)).or(then);
            let Some(following) = following else {
                return Some(o);
            };
            self.shared.budget.record_attempt();
            notices.push(format!("{} {} was tried instead.", o.error, following));
        }
        None
    }
}

/// The provider a link search starts from: Perplexity returns no links, so
/// with it configured the first keyed link provider that holds a key, else
/// DuckDuckGo, and the model is told which.
fn link_provider(
    configured: &'static str,
    chain: &[(&'static str, Secret)],
    notices: &mut Vec<String>,
) -> &'static str {
    if configured != PERPLEXITY {
        return configured;
    }
    let borrowed = chain.first().map(|(name, _)| *name);
    push_once(
        notices,
        format!(
            "The configured provider is Perplexity, which answers questions rather than returning links, so {} answered this search. Use web_research for a Perplexity answer.",
            borrowed.map(label).unwrap_or("DuckDuckGo")
        ),
    );
    borrowed.unwrap_or(DUCKDUCKGO)
}

/// Add a notice unless the call already carries it (a fundamentals search
/// asks once per source).
fn push_once(notices: &mut Vec<String>, note: String) {
    if !notices.contains(&note) {
        notices.push(note);
    }
}
