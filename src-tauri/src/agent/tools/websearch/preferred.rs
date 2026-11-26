//! `web_search` with `topic="fundamentals"` (the web's
//! `_preferred_search`): the trader's own words on their preferred
//! sources, best first, stopping at the first that has a page, so an
//! ordinary question costs one call and a lower source is paid for only
//! when the ones above it have nothing or failed to open.
//!
//! Each search is held to one site by a `site:` term this code appends, so
//! the model has no say in it, and every result is checked against the
//! domain afterwards because a provider may ignore the term. Each source
//! is one paid call and counted as one. When no source has a page the
//! caller searches the open web and the result says so.

use super::{busy, json_text, or_list, WebTool};
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::fetch_url::host_matches;
use crate::agent::websearch::payloads::{SourcedLink, SourcedLinks};
use crate::agent::websearch::settings::Config;
use crate::agent::websearch::taint::Constrained;

/// What a search of the preferred sources found: the pages of the first
/// source that had any, every source asked, and the provider that
/// answered.
struct Found {
    results: Vec<SourcedLink>,
    tried: Vec<String>,
    provider: &'static str,
}

impl WebTool {
    /// The finished result when a preferred source had a page or the call
    /// has to stop (a spent budget, a busy gate); `None` sends the caller
    /// on to the open web.
    pub(super) async fn preferred_search(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        q: &Constrained,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Option<ToolOutcome> {
        let found = match self.search_sources(ctx, config, q, count, notices).await {
            Ok(found) => found,
            Err(stop) => return Some(stop),
        };
        let shown = if found.provider.is_empty() {
            "none"
        } else {
            found.provider
        };
        self.log(
            "web_search",
            shown,
            q,
            found.results.len(),
            config.max_calls_per_turn,
        );
        if found.results.is_empty() {
            if !found.tried.is_empty() {
                notices.push(format!(
                    "Nothing was found on {}, so the open web was searched instead. Say so in the answer.",
                    or_list(&found.tried)
                ));
            }
            return None;
        }
        Some(self.sourced_result(config, q, &found, notices))
    }

    /// Each live source in turn until one has a page. `Err` is the answer
    /// when the call has to stop; nothing is found yet then, since the
    /// loop stops at the first find.
    async fn search_sources(
        &self,
        ctx: &ToolCtx,
        config: &Config,
        q: &Constrained,
        count: usize,
        notices: &mut Vec<String>,
    ) -> Result<Found, ToolOutcome> {
        let mut found = Found {
            results: Vec::new(),
            tried: Vec::new(),
            provider: "",
        };
        for domain in self.shared.run.live_sources(&config.preferred_sources) {
            if let Some(refusal) = self.shared.budget.refusal(&ctx.app).await {
                tracing::info!("Web search refused before dispatch: {}", refusal.error);
                return Err(ToolOutcome::json_ordered(&refusal));
            }
            let text = format!("{} site:{}", q.text, domain);
            let outcome = self.link_outcome(ctx, config, &text, count, notices).await;
            if outcome.busy {
                return Err(busy(&outcome, notices.as_slice()));
            }
            found.tried.push(domain.clone());
            if !outcome.ok {
                self.shared.budget.record_attempt();
                continue;
            }
            self.shared.budget.record(&ctx.app).await;
            found.provider = outcome.provider;
            found.results.extend(
                outcome
                    .results
                    .into_iter()
                    .filter(|link| host_matches(&link.url, &domain))
                    .map(|link| SourcedLink {
                        link,
                        preferred_source: domain.clone(),
                    }),
            );
            if !found.results.is_empty() {
                break;
            }
        }
        Ok(found)
    }

    /// The pages found, remembered for `web_fetch`, with the source to try
    /// next if the best one cannot be read.
    fn sourced_result(
        &self,
        config: &Config,
        q: &Constrained,
        found: &Found,
        notices: &[String],
    ) -> ToolOutcome {
        for row in &found.results {
            self.shared.run.remember(&row.link.url);
        }
        let used = found
            .results
            .first()
            .map(|r| r.preferred_source.clone())
            .unwrap_or_default();
        let later: Vec<String> = self
            .shared
            .run
            .live_sources(&config.preferred_sources)
            .into_iter()
            .filter(|d| *d != used && !found.tried.contains(d))
            .collect();
        let follow_up = if later.is_empty() {
            "If it cannot be read, search the open web with topic=\"general\" and say that the preferred sources were not available.".to_string()
        } else {
            format!(
                "If it cannot be read, call web_search again with topic=\"fundamentals\": {} is then dropped and {} is searched next.",
                used,
                or_list(&later)
            )
        };
        let mut all = notices.to_vec();
        all.push(format!(
            "These are pages from {}, the best preferred source that has the company. Open the best one with web_fetch. {}",
            used, follow_up
        ));
        let payload = SourcedLinks {
            ok: true,
            provider: found.provider,
            query_sent: &q.text,
            query_source: q.decision.as_str(),
            topic: "fundamentals",
            preferred_sources_searched: &found.tried,
            preferred_sources_still_to_try: &later,
            result_count: found.results.len(),
            results: &found.results,
            notices: &all,
        };
        ToolOutcome::Ok(wrap_web_result(
            found.provider,
            &json_text(&payload),
            &[("kind", Some("links"))],
        ))
    }
}
