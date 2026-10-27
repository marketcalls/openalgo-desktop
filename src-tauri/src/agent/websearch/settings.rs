//! The web search configuration (the web search block of the web's
//! `services/agent/settings.py`): what `GET /agent/api/websearch` shows,
//! what `PUT` changes, the provider keys, and the daily counter.
//!
//! Stored values are read leniently (an unknown provider reads as
//! DuckDuckGo, a tunable is clamped, a broken counter is zero for today);
//! submitted values are refused out of range, never clamped, and all are
//! checked before anything is written. A key never travels in the settings
//! payload and is never shown: a flag and a fingerprint stand for it.
//!
//! D-019: the web read the counter, added one and wrote it back, so two
//! searches at once could lose a count. Here one SQL statement does it.

use super::{
    keyed_spec, provider_spec, secret_name, today, ProviderSpec, DEFAULT_DAILY_CAP,
    DEFAULT_MAX_CALLS_PER_TURN, DEFAULT_PERPLEXITY_MODEL, DEFAULT_PROVIDER, KEY_DAILY_CAP,
    KEY_MAX_CALLS_PER_TURN, KEY_PERPLEXITY_MODEL, KEY_PROVIDER, KEY_USAGE, MAX_CALLS_PER_TURN,
    MAX_DAILY_CAP, MAX_KEY_CHARS, MAX_PERPLEXITY_MODEL_CHARS, PROVIDERS,
};
use crate::agent::models::secrets::{self, SecretInfo};
use crate::agent::settings::store;
use crate::security::Secret;
use crate::state::AppState;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// The keys `PUT /agent/api/websearch` accepts.
pub const UPDATABLE: &[&str] = &[
    "provider",
    "perplexity_model",
    "max_calls_per_turn",
    "daily_cap",
];

/// The settings as the tools read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub provider: &'static str,
    pub perplexity_model: String,
    pub max_calls_per_turn: i64,
    pub daily_cap: i64,
}

fn bounded(rows: &HashMap<String, String>, key: &str, default: i64, max: i64) -> i64 {
    let Some(raw) = rows.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return default;
    };
    match raw.parse::<i64>() {
        Ok(v) => v.clamp(0, max),
        Err(_) => {
            tracing::warn!("Agent setting {} is not an integer; using {}", key, default);
            default
        }
    }
}

impl Config {
    /// From the stored rows, leniently.
    pub fn from_rows(rows: &HashMap<String, String>) -> Config {
        let raw = rows
            .get(KEY_PROVIDER)
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_default();
        let provider = match PROVIDERS.iter().find(|p| p.id == raw) {
            Some(p) => p.id,
            None => {
                if !raw.is_empty() {
                    tracing::warn!(
                        "Agent setting {} holds an unknown provider; using {}",
                        KEY_PROVIDER,
                        DEFAULT_PROVIDER
                    );
                }
                DEFAULT_PROVIDER
            }
        };
        let model = rows
            .get(KEY_PERPLEXITY_MODEL)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_PERPLEXITY_MODEL);
        Config {
            provider,
            perplexity_model: model.to_string(),
            max_calls_per_turn: bounded(
                rows,
                KEY_MAX_CALLS_PER_TURN,
                DEFAULT_MAX_CALLS_PER_TURN,
                MAX_CALLS_PER_TURN,
            ),
            daily_cap: bounded(rows, KEY_DAILY_CAP, DEFAULT_DAILY_CAP, MAX_DAILY_CAP),
        }
    }

    /// Read fresh; a store that cannot be read gives the defaults.
    pub fn read(conn: &Connection) -> Config {
        Config::from_rows(&rows_of(conn))
    }
}

fn rows_of(conn: &Connection) -> HashMap<String, String> {
    store::read_all(conn).unwrap_or_else(|e| {
        tracing::error!("Could not read the web search settings: {}", e);
        HashMap::new()
    })
}

/// Today's count from the stored counter: an earlier day, a missing row or
/// one that is not the counter reads as zero (the cap resets itself).
pub fn usage_from(raw: Option<&str>, today: &str) -> i64 {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return 0;
    };
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(raw) else {
        tracing::warn!(
            "Agent setting {} is not JSON; today's usage reads as zero",
            KEY_USAGE
        );
        return 0;
    };
    if m.get("date").and_then(Value::as_str) != Some(today) {
        return 0;
    }
    let count = match m.get("count") {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    count.unwrap_or(0).max(0)
}

/// Today's searches so far.
pub fn usage(conn: &Connection, today: &str) -> i64 {
    let rows = rows_of(conn);
    usage_from(rows.get(KEY_USAGE).map(String::as_str), today)
}

/// Count one search against today, in one statement: a counter from
/// another day, or one that cannot be read, starts again at one. Returns
/// today's count.
pub fn count_one(conn: &Connection, today: &str) -> crate::error::Result<i64> {
    let n = conn.query_row(
        "INSERT INTO ag_setting (key, value) VALUES (?1, json_object('date', ?2, 'count', 1))
         ON CONFLICT(key) DO UPDATE SET value = json_object('date', ?2, 'count',
             CASE WHEN json_valid(ag_setting.value)
                   AND json_type(ag_setting.value) = 'object'
                   AND json_extract(ag_setting.value, '$.date') = ?2
                  THEN MAX(0, CAST(COALESCE(json_extract(ag_setting.value, '$.count'), 0) AS INTEGER)) + 1
                  ELSE 1 END)
         RETURNING json_extract(value, '$.count')",
        params![KEY_USAGE, today],
        |r| r.get::<_, i64>(0),
    )?;
    Ok(n)
}

/// What each field reverts to (`get_websearch_defaults`).
pub fn defaults() -> Value {
    json!({
        "provider": DEFAULT_PROVIDER,
        "perplexity_model": DEFAULT_PERPLEXITY_MODEL,
        "max_calls_per_turn": DEFAULT_MAX_CALLS_PER_TURN,
        "daily_cap": DEFAULT_DAILY_CAP,
    })
}

/// The stored keys, described (only those holding a value).
fn stored_keys(conn: &Connection) -> HashMap<String, SecretInfo> {
    let names: Vec<String> = PROVIDERS
        .iter()
        .filter(|p| p.needs_key)
        .map(|p| secret_name(p.id))
        .collect();
    match secrets::describe(conn, &names) {
        Ok(m) => m.into_iter().filter(|(_, s)| s.has_value).collect(),
        Err(e) => {
            tracing::error!("Could not read the stored web search keys: {}", e);
            HashMap::new()
        }
    }
}

fn provider_entry(spec: &ProviderSpec, stored: &HashMap<String, SecretInfo>) -> Value {
    let secret = spec
        .needs_key
        .then(|| stored.get(&secret_name(spec.id)))
        .flatten()
        .map(SecretInfo::to_json);
    let mut entry = spec.to_json();
    if let Value::Object(m) = &mut entry {
        m.insert("has_api_key".into(), json!(secret.is_some()));
        m.insert(
            "api_key_fingerprint".into(),
            secret
                .as_ref()
                .map(|s| s["fingerprint"].clone())
                .unwrap_or(Value::Null),
        );
        m.insert(
            "api_key_last_used_at".into(),
            secret
                .as_ref()
                .map(|s| s["last_used_at"].clone())
                .unwrap_or(Value::Null),
        );
        m.insert("ready".into(), json!(!spec.needs_key || secret.is_some()));
    }
    entry
}

/// The configuration, every key described and none shown
/// (`get_websearch_config`).
pub fn config(app: &AppState) -> crate::error::Result<Value> {
    let conn = app.sqlite.conn()?;
    let rows = rows_of(&conn);
    let c = Config::from_rows(&rows);
    let day = today(app.now());
    let count = usage_from(rows.get(KEY_USAGE).map(String::as_str), &day);
    let stored = stored_keys(&conn);
    Ok(json!({
        "provider": c.provider,
        "perplexity_model": c.perplexity_model,
        "max_calls_per_turn": c.max_calls_per_turn,
        "daily_cap": c.daily_cap,
        "usage": {"date": day, "count": count, "remaining": (c.daily_cap - count).max(0)},
        "providers": PROVIDERS.iter().map(|p| provider_entry(p, &stored)).collect::<Vec<_>>(),
    }))
}

/// A submitted tunable: a whole number in range, refused otherwise.
fn tunable(name: &str, raw: &Value, max: i64) -> Result<i64, String> {
    let whole = || format!("{} must be a whole number", name);
    let value = match raw {
        Value::Number(n) if n.is_i64() => n.as_i64().ok_or_else(whole)?,
        Value::Bool(_) | Value::Null | Value::Array(_) | Value::Object(_) => return Err(whole()),
        other => crate::agent::settings::pyrepr::py_str(other)
            .trim()
            .parse::<i64>()
            .map_err(|_| whole())?,
    };
    if !(0..=max).contains(&value) {
        return Err(format!("{} must be between 0 and {}", name, max));
    }
    Ok(value)
}

/// A submitted Perplexity model: blank restores the default; otherwise a
/// plain printable ASCII id of at most 120 characters.
fn perplexity_model(raw: &Value) -> Result<String, String> {
    let text = match raw {
        Value::Null => String::new(),
        v if !crate::agent::models::truthy(v) => String::new(),
        v => crate::agent::settings::pyrepr::py_str(v).trim().to_string(),
    };
    if text.is_empty() {
        return Ok(DEFAULT_PERPLEXITY_MODEL.into());
    }
    if text.chars().count() > MAX_PERPLEXITY_MODEL_CHARS {
        return Err(format!(
            "perplexity_model must be at most {} characters",
            MAX_PERPLEXITY_MODEL_CHARS
        ));
    }
    if !text.chars().all(|c| c.is_ascii_graphic()) {
        return Err("perplexity_model must be a plain model id with no spaces".into());
    }
    Ok(text)
}

/// The rows a partial update writes, every value checked first
/// (`update_websearch`'s validation). The error is the web's sentence.
pub fn prepare(values: &Map<String, Value>) -> Result<Vec<(&'static str, String)>, String> {
    let mut unknown: Vec<&str> = values
        .keys()
        .map(String::as_str)
        .filter(|k| !UPDATABLE.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!(
            "Unknown web search setting(s): {}",
            unknown.join(", ")
        ));
    }
    let mut rows = Vec::new();
    if let Some(v) = values.get("provider") {
        let raw = crate::agent::settings::pyrepr::py_str(v);
        rows.push((
            KEY_PROVIDER,
            provider_spec(if v.is_null() { "" } else { &raw })?
                .id
                .to_string(),
        ));
    }
    if let Some(v) = values.get("perplexity_model") {
        rows.push((KEY_PERPLEXITY_MODEL, perplexity_model(v)?));
    }
    if let Some(v) = values.get("max_calls_per_turn") {
        let n = tunable("max_calls_per_turn", v, MAX_CALLS_PER_TURN)?;
        rows.push((KEY_MAX_CALLS_PER_TURN, n.to_string()));
    }
    if let Some(v) = values.get("daily_cap") {
        rows.push((
            KEY_DAILY_CAP,
            tunable("daily_cap", v, MAX_DAILY_CAP)?.to_string(),
        ));
    }
    if rows.is_empty() {
        return Err("Nothing to update".into());
    }
    Ok(rows)
}

/// Why a write was refused: the trader's mistake (400) or the store (500).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    Invalid(String),
    Store(String),
}

/// Write a partial update, all or nothing.
pub fn update(app: &AppState, values: &Map<String, Value>) -> Result<(), WriteError> {
    let rows = prepare(values).map_err(WriteError::Invalid)?;
    let conn = app.sqlite.conn().map_err(|e| store_failed(&rows, &e))?;
    store::write_all(&conn, &rows).map_err(|e| store_failed(&rows, &e))?;
    let mut keys: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    keys.sort();
    tracing::info!("Web search configuration updated: {}", keys.join(", "));
    Ok(())
}

fn store_failed(rows: &[(&str, String)], e: &dyn std::fmt::Display) -> WriteError {
    let mut keys: Vec<&str> = rows.iter().map(|(k, _)| *k).collect();
    keys.sort();
    tracing::error!(
        "Failed to persist the web search configuration ({}): {}",
        keys.join(", "),
        e
    );
    WriteError::Store(format!(
        "Could not persist web search setting(s): {}",
        keys.join(", ")
    ))
}

/// A submitted key, checked without quoting it.
pub fn checked_key(spec: &ProviderSpec, raw: Option<&Value>) -> Result<Secret, String> {
    let Some(Value::String(s)) = raw else {
        return Err(format!("An api_key is required for {}", spec.label));
    };
    let key = s.trim();
    if key.is_empty() {
        return Err(format!("An api_key is required for {}", spec.label));
    }
    if key.chars().count() > MAX_KEY_CHARS {
        return Err(format!(
            "api_key must be at most {} characters",
            MAX_KEY_CHARS
        ));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("api_key must not contain spaces or control characters".into());
    }
    Ok(Secret::new(key))
}

/// Store a provider's key (an unchanged one writes nothing).
pub fn set_key(app: &AppState, provider: &str, raw: Option<&Value>) -> Result<(), WriteError> {
    let spec = keyed_spec(provider).map_err(WriteError::Invalid)?;
    let key = checked_key(spec, raw).map_err(WriteError::Invalid)?;
    let failed = || {
        tracing::error!("Could not store the {} web search key", spec.label);
        WriteError::Store(format!("Could not store the {} key", spec.label))
    };
    let conn = app.sqlite.conn().map_err(|_| failed())?;
    secrets::set(&conn, &app.security, &secret_name(spec.id), key.expose())
        .map_err(|_| failed())?;
    tracing::info!("Web search key stored for {}", spec.id);
    Ok(())
}

/// Remove a provider's key; clearing an absent one succeeds.
pub fn clear_key(app: &AppState, provider: &str) -> Result<(), WriteError> {
    let spec = keyed_spec(provider).map_err(WriteError::Invalid)?;
    let conn = app.sqlite.conn().map_err(|e| {
        tracing::error!("Could not clear the {} web search key: {}", spec.label, e);
        WriteError::Store(format!("Could not clear the {} key", spec.label))
    })?;
    secrets::delete(&conn, &secret_name(spec.id)).map_err(|e| {
        tracing::error!("Could not clear the {} web search key: {}", spec.label, e);
        WriteError::Store(format!("Could not clear the {} key", spec.label))
    })?;
    tracing::info!("Web search key cleared for {}", spec.id);
    Ok(())
}

/// A provider's stored key, opened at the moment of use; `None` when none
/// is stored or it cannot be opened (logged by provider, never quoted).
pub fn stored_key(app: &AppState, provider: &str) -> Option<Secret> {
    let conn = app.sqlite.conn().ok()?;
    secrets::get(&conn, &app.security, &secret_name(provider))
        .filter(|k| !k.expose().trim().is_empty())
}

/// Record that a provider's key was handed to it.
pub fn mark_used(app: &AppState, provider: &str) {
    if let Ok(conn) = app.sqlite.conn() {
        secrets::mark_used(&conn, &secret_name(provider), app.now());
    }
}
