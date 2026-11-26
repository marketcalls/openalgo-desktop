//! A provider's test from the panel (the web's
//! `probe_websearch_provider`): one real query through the very calls the
//! tools make (Perplexity on the research path, since it returns no
//! links). A provider that also reads pages (Firecrawl, TinyFish) reads
//! one too, since one key answers both tools and a pass has to mean both
//! work. A key just typed can be tested before it is saved; it is never
//! stored, logged or returned. A test is not counted against the daily cap:
//! the cap bounds the model, not the trader checking their settings.

use super::providers::{self, Outcome};
use super::settings::{self, Config};
use super::{duckduckgo, keyed, provider_spec, ProviderSpec, KEYED_LINK_PROVIDERS, PERPLEXITY};
use crate::security::Secret;
use crate::state::AppState;
use std::sync::Arc;
use std::time::Instant;

/// A real question with a stable answer, cheap on every provider.
pub const PROBE_QUERY: &str = "who wrote Hamlet";
pub const PROBE_MAX_RESULTS: usize = 3;
/// The page a fetch test reads: a stable, tiny page that exists to be
/// fetched (`WEBFETCH_PROBE_URL`).
pub const FETCH_PROBE_URL: &str = "https://example.com";

/// What one test produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub ok: bool,
    pub provider: &'static str,
    pub message: String,
    pub latency_ms: u64,
    pub result_count: usize,
}

/// Test a provider; `typed` replaces the stored key for this test only.
/// An unknown provider is the web's refusal sentence.
pub async fn probe(
    app: &Arc<AppState>,
    provider: &str,
    typed: Option<Secret>,
) -> Result<Probe, String> {
    let spec = provider_spec(provider)?;
    let key = match typed.filter(|k| !k.expose().trim().is_empty()) {
        Some(k) => Some(Secret::new(k.expose().trim())),
        None if spec.needs_key => {
            let id = spec.id;
            app.blocking(move |c| Ok(settings::stored_key(c, id)))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    if spec.needs_key && key.is_none() {
        return Ok(Probe {
            ok: false,
            provider: spec.id,
            message: format!(
                "No {} key is stored, so there is nothing to test.",
                spec.label
            ),
            latency_ms: 0,
            result_count: 0,
        });
    }
    let model = app
        .blocking(|c| Ok(Config::read(&*c.sqlite.conn()?).perplexity_model))
        .await
        .unwrap_or_else(|_| super::DEFAULT_PERPLEXITY_MODEL.into());
    let net = providers::net();
    let key = key.unwrap_or_default();
    let started = Instant::now();
    let outcome: Outcome = if spec.id == PERPLEXITY {
        providers::perplexity(&net, PROBE_QUERY, &model, &key).await
    } else if KEYED_LINK_PROVIDERS.contains(&spec.id) {
        keyed::search_with(&net, spec.id, PROBE_QUERY, PROBE_MAX_RESULTS, &key).await
    } else {
        duckduckgo::search(&net, PROBE_QUERY, PROBE_MAX_RESULTS).await
    };
    let count = if spec.id == PERPLEXITY {
        outcome.citations.len()
    } else {
        outcome.results.len()
    };
    let fetched = if spec.fetch && outcome.ok && count > 0 {
        Some(keyed::fetch_with(&net, spec.id, FETCH_PROBE_URL, &key).await)
    } else {
        None
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    if outcome.key_used || fetched.as_ref().is_some_and(|f| f.key_used) {
        let id = spec.id;
        let _ = app
            .blocking(move |c| {
                settings::mark_used(c, id);
                Ok(())
            })
            .await;
    }
    let (ok, message) = verdict(spec, &outcome, count, fetched.as_ref());
    Ok(Probe {
        ok,
        provider: spec.id,
        message,
        latency_ms,
        result_count: count,
    })
}

/// Whether a test passed, and the sentence that says so.
fn verdict(
    spec: &ProviderSpec,
    outcome: &Outcome,
    count: usize,
    fetched: Option<&Outcome>,
) -> (bool, String) {
    if !outcome.ok {
        let message = if outcome.error.is_empty() {
            format!("{} did not answer.", spec.label)
        } else {
            outcome.error.clone()
        };
        return (false, message);
    }
    if spec.result_kind == "links" && count == 0 {
        return (
            false,
            format!(
                "{} answered but returned no results. It is most likely rate limiting this server; try again in a minute.",
                spec.label
            ),
        );
    }
    let noun = if spec.result_kind == "answer" {
        "citation"
    } else {
        "result"
    };
    let plural = if count == 1 { "" } else { "s" };
    let answered = format!("{} answered with {} {}{}.", spec.label, count, noun, plural);
    match fetched {
        Some(f) if !f.ok => (
            false,
            format!("Search works, but reading a page failed. {}", f.error),
        ),
        Some(_) => (true, format!("{} Reading a page worked too.", answered)),
        None => (true, answered),
    }
}
