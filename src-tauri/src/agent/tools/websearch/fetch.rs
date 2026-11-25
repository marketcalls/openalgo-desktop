//! `web_fetch` (the web's `web_fetch`, `_fetch_candidates` and
//! `_fetch_payload`): the readable text of one page, as markdown, through
//! Firecrawl or TinyFish, the configured one first and the other when it
//! fails. Only an address the fetch boundary accepts is sent
//! (`websearch::fetch_url`). A long page comes back in slices; the page is
//! kept for the run, so reading on costs no second call.
//!
//! Desktop: a page a provider ended up on after a redirect into a private
//! or local address is not returned, as that address itself would have
//! been refused; and the page is cut to `MAX_PAGE_CHARS` before its first
//! slice as well as when it is kept, so every slice of it reads the same
//! text.

use super::args::{fetch_chars, offset};
use super::run_state::{bounded_text, Page};
use super::{busy, failure, mark_used, or_list, stored_key, WebTool};
use crate::agent::contract::json::dumps_capped_ordered;
use crate::agent::contract::tool::{ToolCtx, ToolOutcome};
use crate::agent::prompts::untrusted::wrap_web_result;
use crate::agent::websearch::fetch_url::{
    self, host_matches, redirect_refusal, url_key, Check, Source,
};
use crate::agent::websearch::payloads::{FetchFailed, NotOk, PageSlice};
use crate::agent::websearch::providers::Outcome;
use crate::agent::websearch::settings::Config;
use crate::agent::websearch::{keyed, label, FETCH_PROVIDERS};
use crate::security::Secret;
use serde_json::{Map, Value};

/// The ceiling on one `web_fetch` result as JSON (`FETCH_JSON_CHARS`):
/// above every other tool's, since a slice is already bounded and JSON
/// escaping adds to it.
const FETCH_JSON_CHARS: usize = 60_000;

const NO_FETCH_KEY: &str = "Reading a page needs a Firecrawl or TinyFish key, and neither is stored in the agent settings. Tell the operator they can add one under Web search in the agent settings. Until then, answer from the search snippets and say the page itself was not read.";

const OTHER_NOT_TRIED: &str =
    "The other provider was not tried because this turn's web calls are used.";

fn not_configured() -> ToolOutcome {
    ToolOutcome::json_ordered(&NotOk::new("not_configured", NO_FETCH_KEY))
}

/// The characters of `text` from `start` up to `end`.
fn chars_between(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// One slice of a page, wrapped as third-party text (`_fetch_payload`).
fn slice(
    page: &Page,
    url: &str,
    source: Source,
    (size, start): (usize, usize),
    notices: &[String],
    cached: bool,
) -> ToolOutcome {
    let total = page.text.chars().count();
    let end = total.min(start.saturating_add(size));
    let truncated = end < total;
    let text = chars_between(&page.text, start, end);
    let mut notices = notices.to_vec();
    if start >= total && total > 0 {
        notices.push("The offset is past the end of the page.".into());
    }
    let elsewhere = !page.final_url.is_empty() && url_key(&page.final_url) != url_key(url);
    let payload = PageSlice {
        ok: true,
        provider: page.provider,
        url,
        url_source: source.as_str(),
        title: &page.title,
        total_chars: total,
        offset: start,
        truncated,
        next_offset: truncated.then_some(end),
        from_this_turn_copy: cached,
        text: &text,
        notices: &notices,
        final_url: elsewhere.then_some(page.final_url.as_str()),
    };
    ToolOutcome::Ok(wrap_web_result(
        page.provider,
        &dumps_capped_ordered(&payload, FETCH_JSON_CHARS),
        &[("kind", Some("page"))],
    ))
}

impl WebTool {
    /// `web_fetch`.
    pub(super) async fn fetch(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let (size, size_notice) = match fetch_chars(args) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let start = match offset(args) {
            Ok(v) => v,
            Err(refused) => return refused,
        };
        let requested = match args.get("url") {
            Some(Value::String(s)) => s.as_str(),
            _ => "",
        };
        let config = self.shared.budget.config(&ctx.app).await;
        let checked = fetch_url::check(
            requested,
            &self.shared.operator_message,
            &self.shared.run.seen(),
            &config.preferred_sources,
        );
        let (url, source) = match checked {
            Check::Allowed { url, source } => (url, source),
            Check::Refused(reason) => {
                tracing::info!(
                    "Web fetch refused: the address is not one the operator or a search gave"
                );
                return ToolOutcome::json_ordered(&NotOk::new("url_not_allowed", reason));
            }
        };
        let mut notices: Vec<String> = size_notice.into_iter().collect();
        if let Some(page) = self.shared.run.page(&url) {
            return slice(&page, &url, source, (size, start), &notices, true);
        }
        let candidates = self.fetch_candidates(ctx, &config).await;
        if candidates.is_empty() {
            tracing::info!("Web fetch refused before dispatch: no fetch provider has a key");
            return not_configured();
        }
        if let Some(refusal) = self.shared.budget.refusal(&ctx.app).await {
            tracing::info!("Web fetch refused before dispatch: {}", refusal.error);
            return ToolOutcome::json_ordered(&refusal);
        }
        let outcome = match self.read_page(ctx, &candidates, &url, &mut notices).await {
            Ok(Some(o)) => o,
            Ok(None) => return not_configured(),
            Err(busy_answer) => return busy_answer,
        };
        tracing::info!(
            "web_fetch: provider={} source={} ok={} chars={} turn={}/{}",
            outcome.provider,
            source.as_str(),
            outcome.ok,
            outcome.text.chars().count(),
            self.shared.budget.used(),
            config.max_calls_per_turn
        );
        if !outcome.ok {
            return self.fetch_failed(&url, &outcome, &config, &notices);
        }
        self.shared.budget.record(&ctx.app).await;
        let page = Page {
            provider: outcome.provider,
            text: bounded_text(&outcome.text),
            title: outcome.title,
            final_url: outcome.final_url,
        };
        self.shared.run.keep(&url, &page);
        slice(&page, &url, source, (size, start), &notices, false)
    }

    /// The fetch providers that hold a key, the configured one first
    /// (`_fetch_candidates`).
    async fn fetch_candidates(
        &self,
        ctx: &ToolCtx,
        config: &Config,
    ) -> Vec<(&'static str, Secret)> {
        let mut order = vec![config.fetch_provider];
        order.extend(
            FETCH_PROVIDERS
                .iter()
                .copied()
                .filter(|p| *p != config.fetch_provider),
        );
        let mut found = Vec::new();
        for name in order {
            if let Some(key) = stored_key(&ctx.app, name).await {
                found.push((name, key));
            }
        }
        found
    }

    /// Ask each candidate in turn until one reads the page. A second
    /// provider is a second paid request, so the budget is asked again
    /// first; every request that went out and failed counts against the
    /// turn. `Err` is a busy gate's answer (nothing falls back); `Ok(None)`
    /// only when there was no candidate.
    async fn read_page(
        &self,
        ctx: &ToolCtx,
        candidates: &[(&'static str, Secret)],
        url: &str,
        notices: &mut Vec<String>,
    ) -> Result<Option<Outcome>, ToolOutcome> {
        let mut last: Option<Outcome> = None;
        for (index, &(name, ref key)) in candidates.iter().enumerate() {
            if index > 0 && self.shared.budget.refusal(&ctx.app).await.is_some() {
                notices.push(OTHER_NOT_TRIED.into());
                break;
            }
            let o = keyed::fetch_with(&self.shared.net, name, url, key).await;
            mark_used(&ctx.app, &o).await;
            if o.busy {
                return Err(busy(&o, notices.as_slice()));
            }
            if o.ok {
                return Ok(Some(self.after_redirect(o)));
            }
            self.shared.budget.record_attempt();
            if let Some((next, _)) = candidates.get(index + 1) {
                notices.push(format!("{} {} was tried instead.", o.error, label(next)));
            }
            last = Some(o);
        }
        Ok(last)
    }

    /// A page that ended up on an address that is not fetched is not read
    /// (desktop). The request went out, so it counts against the turn.
    fn after_redirect(&self, page: Outcome) -> Outcome {
        if page.final_url.is_empty() {
            return page;
        }
        match redirect_refusal(&page.final_url) {
            None => page,
            Some(reason) => {
                tracing::warn!("A fetched page redirected to an address that is not fetched");
                self.shared.budget.record_attempt();
                Outcome::failed(
                    page.provider,
                    format!(
                        "The page redirected away from the address asked for. {}",
                        reason
                    ),
                )
            }
        }
    }

    /// A read that failed, with the next step when the page was on a
    /// preferred source: that source is dropped for the rest of the run.
    fn fetch_failed(
        &self,
        url: &str,
        outcome: &Outcome,
        config: &Config,
        notices: &[String],
    ) -> ToolOutcome {
        let failed = config
            .preferred_sources
            .iter()
            .find(|d| host_matches(url, d))
            .cloned();
        let next_step = failed.as_ref().map(|failed| {
            self.shared.run.fail_source(failed);
            let remaining = self.shared.run.live_sources(&config.preferred_sources);
            if remaining.is_empty() {
                format!(
                    "{} could not be read and no other preferred source is left. Search the open web with topic=\"general\" and say that the preferred sources were not available.",
                    failed
                )
            } else {
                format!(
                    "{} could not be read, so move to {}. Search with topic=\"fundamentals\" (or open a page of it you already have) and say that the first choice was not available.",
                    failed,
                    or_list(&remaining)
                )
            }
        });
        let payload = FetchFailed {
            ok: false,
            error: "provider_failed",
            provider: outcome.provider,
            message: &outcome.error,
            notices,
            preferred_source_failed: failed.as_deref(),
            next_step,
        };
        failure(outcome.provider, &payload)
    }
}
