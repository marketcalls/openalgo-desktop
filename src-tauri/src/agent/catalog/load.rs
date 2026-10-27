//! The catalogue from the bundled snapshot, and from a newer price table
//! fetched at startup (the web's `_read_cost_entries`,
//! `_read_models_by_provider`, `_read_providers`, and LiteLLM's own
//! `add_known_models` for a fetched table).
//!
//! An entry's fields are read the way the web reads them: a limit that is
//! not a positive whole number, a price that is not a number and a flag that
//! is neither yes nor no are unknown, never zero.

use super::{Catalog, Entry};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

const SNAPSHOT: &str = include_str!("snapshot.json");

/// The documentation entries LiteLLM's table carries; never models.
const RESERVED: &[&str] = &["sample_spec", "fallback_generalizations"];

/// LiteLLM's integrity rule for a fetched table: at least this many
/// models, and at least half of what its bundled table holds.
const MIN_MODEL_COUNT: usize = 50;

/// The recorded snapshot, before any refresh.
struct Snapshot {
    backup_model_count: usize,
    chat_providers: Vec<String>,
    provider_ids: HashSet<String>,
    /// Per offered provider: its listed names and the `litellm_provider`
    /// values whose new models LiteLLM folds into its list.
    models: Vec<(String, Vec<String>, Vec<String>)>,
    entries: HashMap<String, Entry>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn read_snapshot() -> Option<Snapshot> {
    let v: Value = serde_json::from_str(SNAPSHOT).ok()?;
    let chat_providers = strings(v.get("chat_providers"));
    let models = chat_providers
        .iter()
        .map(|p| {
            let m = &v["models"][p];
            (
                p.clone(),
                strings(m.get("listed")),
                strings(m.get("sources")),
            )
        })
        .collect();
    Some(Snapshot {
        backup_model_count: v["backup_model_count"].as_u64().unwrap_or(0) as usize,
        chat_providers,
        provider_ids: strings(v.get("provider_ids")).into_iter().collect(),
        models,
        entries: v
            .get("entries")
            .and_then(Value::as_object)
            .map(entries_of)
            .unwrap_or_default(),
    })
}

/// The catalogue as shipped. A snapshot that cannot be read costs the
/// suggestions, never the Agent: the catalogue is then empty and models are
/// added by name.
pub fn bundled() -> Catalog {
    match read_snapshot() {
        Some(s) => build(&s, s.entries.clone(), &[]),
        None => {
            tracing::error!("The bundled model catalogue could not be read; it is empty");
            Catalog::default()
        }
    }
}

fn build(
    s: &Snapshot,
    entries: HashMap<String, Entry>,
    extra: &[(String, Vec<String>)],
) -> Catalog {
    let mut entries = entries;
    let mut providers: Vec<(String, Vec<String>)> = s
        .models
        .iter()
        .map(|(p, listed, _)| {
            let mut names = listed.clone();
            if let Some((_, more)) = extra.iter().find(|(q, _)| q == p) {
                names.extend(more.iter().cloned());
            }
            (p.clone(), names)
        })
        .collect();
    // The plan models the table lacks (the web registers them on every
    // build too).
    crate::agent::chatgpt::models::register(&mut entries, &mut providers);
    let mut provider_ids = s.provider_ids.clone();
    provider_ids.extend(entries.values().map(|e| e.provider.clone()));
    provider_ids.remove("");
    Catalog {
        entries,
        provider_ids,
        providers,
    }
}

/// The catalogue with a fetched price table in place of the bundled one,
/// or why the table was refused (it is then not used). Kept to the same
/// providers as the snapshot; a model the table adds for an offered
/// provider joins its list, as LiteLLM's `add_known_models` adds it.
pub fn with_fetched(body: &[u8]) -> Result<Catalog, String> {
    let s = read_snapshot().ok_or("the bundled catalogue could not be read")?;
    let table: Map<String, Value> = match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => m,
        Ok(_) => return Err("not a JSON object".into()),
        Err(_) => return Err("not JSON".into()),
    };
    let count = table
        .iter()
        .filter(|(k, v)| !RESERVED.contains(&k.as_str()) && v.is_object())
        .count();
    if count < MIN_MODEL_COUNT || count * 2 < s.backup_model_count {
        return Err(format!(
            "it holds {} models against {} bundled",
            count, s.backup_model_count
        ));
    }
    let all = entries_of(&expand_aliases(table));
    let feeding: HashSet<&str> = s
        .models
        .iter()
        .flat_map(|(_, _, sources)| sources.iter().map(String::as_str))
        .collect();
    let offered: HashSet<&str> = s.chat_providers.iter().map(String::as_str).collect();
    let extra: Vec<(String, Vec<String>)> = s
        .models
        .iter()
        .map(|(p, _, sources)| {
            let mut added: Vec<String> = all
                .iter()
                .filter(|(k, e)| {
                    sources.contains(&e.provider)
                        && !s.entries.contains_key(*k)
                        && !k.starts_with("ft:")
                })
                .map(|(k, _)| k.clone())
                .collect();
            added.sort();
            (p.clone(), added)
        })
        .collect();
    let listed: HashSet<&String> = s.models.iter().flat_map(|(_, l, _)| l.iter()).collect();
    let kept = all
        .into_iter()
        .filter(|(k, e)| {
            offered.contains(e.provider.as_str())
                || feeding.contains(e.provider.as_str())
                || listed.contains(k)
        })
        .collect();
    Ok(build(&s, kept, &extra))
}

/// LiteLLM's alias expansion: an entry's `aliases` become entries of their
/// own, never over an existing one.
fn expand_aliases(mut table: Map<String, Value>) -> Map<String, Value> {
    let mut added: Vec<(String, Value)> = Vec::new();
    for entry in table.values() {
        let Some(aliases) = entry.get("aliases").and_then(Value::as_array) else {
            continue;
        };
        for alias in aliases.iter().filter_map(Value::as_str) {
            if !table.contains_key(alias) && !added.iter().any(|(a, _)| a == alias) {
                added.push((alias.to_string(), entry.clone()));
            }
        }
    }
    table.extend(added);
    table
}

fn entries_of(table: &Map<String, Value>) -> HashMap<String, Entry> {
    table
        .iter()
        .filter_map(|(k, v)| {
            let key = k.trim();
            if key.is_empty() || RESERVED.contains(&key) {
                return None;
            }
            v.as_object().map(|o| (key.to_string(), entry_of(o)))
        })
        .collect()
}

fn entry_of(o: &Map<String, Value>) -> Entry {
    Entry {
        provider: clean(o.get("litellm_provider")),
        mode: Some(clean(o.get("mode"))).filter(|m| !m.is_empty()),
        max_input_tokens: as_int(o.get("max_input_tokens")).or_else(|| as_int(o.get("max_tokens"))),
        max_output_tokens: as_int(o.get("max_output_tokens")),
        input_cost: as_float(o.get("input_cost_per_token")),
        output_cost: as_float(o.get("output_cost_per_token")),
        cache_read_cost: as_float(o.get("cache_read_input_token_cost")),
        supports_function_calling: as_bool(o.get("supports_function_calling")),
        supports_vision: as_bool(o.get("supports_vision")),
        supports_reasoning: as_bool(o.get("supports_reasoning")),
    }
}

/// A trimmed identifier, or empty when the value is not a string.
fn clean(v: Option<&Value>) -> String {
    v.and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// A number (or numeric text), never a boolean.
fn as_float(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A positive whole limit (Python's `int()` truncates a float).
fn as_int(v: Option<&Value>) -> Option<u64> {
    let n = match v? {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))?,
        Value::String(s) => s.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    (n > 0).then_some(n as u64)
}

/// Yes or no, and unknown kept apart from no.
fn as_bool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        _ => None,
    }
}
