//! Models the ChatGPT plan serves that the bundled catalogue does not list
//! (the web's `chatgpt_models.py`), merged in whenever the catalogue is
//! built: from the bundled snapshot and from a fetched table alike, so a
//! run on a stored row and the config page see the same models.
//!
//! The web's three rules hold here (its `CLAUDE.md`, "The ChatGPT
//! subscription model list is ours to maintain"):
//!
//! - **Never a price.** A plan turn has no per-token price; an entry with
//!   none keeps its cost unknown and its usage reported as the plan's.
//! - **Never over an existing entry, and the provider decides, not the
//!   name.** Most of these names are also OpenAI API models; a bare-name
//!   check would skip every model this exists to add. Only an entry whose
//!   provider is `chatgpt` counts.
//! - **Quietly.** A catalogue of another shape costs these models, never
//!   the Agent.
//!
//! The entry carries `mode: responses`: without it the web's LiteLLM routed
//! the model to chat completions and met a Cloudflare page instead.

use crate::agent::catalog::Entry;
use std::collections::HashMap;

/// The plan's provider id in the catalogue.
pub const PROVIDER: &str = "chatgpt";

/// Bare name and context window: models measured against a real plan
/// (the web's `SUPPLEMENTAL`). The rest LiteLLM 1.104.0 already lists.
pub const SUPPLEMENTAL: &[(&str, u64)] = &[("gpt-6-astra", 922_000)];

/// The output limit every entry shares (the web's `_BASE`).
const MAX_OUTPUT_TOKENS: u64 = 128_000;

/// One supplemental entry: capabilities and limits, no cost.
pub fn entry(max_input_tokens: u64) -> Entry {
    Entry {
        provider: PROVIDER.into(),
        mode: Some("responses".into()),
        max_input_tokens: Some(max_input_tokens),
        max_output_tokens: Some(MAX_OUTPUT_TOKENS),
        input_cost: None,
        output_cost: None,
        cache_read_cost: None,
        supports_function_calling: Some(true),
        supports_vision: Some(true),
        supports_reasoning: Some(true),
    }
}

fn is_plan_entry(entries: &HashMap<String, Entry>, key: &str) -> bool {
    entries.get(key).is_some_and(|e| e.provider == PROVIDER)
}

/// Add the missing models, in place, and list them under the plan's
/// provider. Idempotent; an entry the catalogue already has as the plan's
/// is left as it is. Returns the ids added.
pub fn register(
    entries: &mut HashMap<String, Entry>,
    providers: &mut Vec<(String, Vec<String>)>,
) -> Vec<String> {
    let mut added = Vec::new();
    for (name, max_input) in SUPPLEMENTAL {
        let id = format!("{}/{}", PROVIDER, name);
        if is_plan_entry(entries, &id) || is_plan_entry(entries, name) {
            continue;
        }
        entries.insert(id.clone(), entry(*max_input));
        if !providers.iter().any(|(p, _)| p == PROVIDER) {
            providers.push((PROVIDER.into(), Vec::new()));
        }
        if let Some((_, listed)) = providers.iter_mut().find(|(p, _)| p == PROVIDER) {
            if !listed.contains(&id) {
                listed.push(id.clone());
            }
        }
        added.push(id);
    }
    if !added.is_empty() {
        tracing::info!(
            "Registered {} ChatGPT plan model(s) the catalogue does not list: {}",
            added.len(),
            added.join(", ")
        );
    }
    added
}
