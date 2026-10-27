//! The voice settings and the voice key (the web's voice block of
//! `services/agent/settings.py`: `get_voice_config`, `update_voice`,
//! `set_voice_key`, `clear_voice_key`, `voice_key`).
//!
//! The eight settings are ordinary `ag_setting` rows (the config lane's
//! schema, seeded by migration 098); this module reads them as one block,
//! validates the two spoken values and the provider before any write, and
//! keeps the key write only: it is sealed in `ag_secret` under the app's
//! data key and described by presence and fingerprint, never returned.
//! Everything here reads the database: callers run it off the runtime.

use super::confirm::normalise_agent_name;
use super::providers::{voice_provider_spec, VoiceProviderSpec, VOICE_PROVIDERS};
use crate::agent::models::{secrets, success, text_field, truthy};
use crate::agent::reply::Refusal;
use crate::agent::settings::parse::{default_of, render};
use crate::agent::settings::schema::{self, TRADING_ENABLED};
use crate::agent::settings::{self, Snapshot, UpdateError};
use crate::security::Secret;
use crate::state::AppState;
use serde_json::{json, Map, Value};

pub const ENABLED: &str = "voice_enabled";
pub const PROVIDER: &str = "voice_provider";
pub const MODEL: &str = "voice_model";
pub const SPEAKER: &str = "voice_speaker";
pub const AGENT_NAME: &str = "voice_agent_name";
pub const TRADING: &str = "voice_trading_enabled";
pub const CONFIRM_WINDOW: &str = "voice_confirm_window_seconds";
pub const IDLE_TIMEOUT: &str = "voice_idle_timeout_seconds";

/// The eight keys, in the web's `get_voice_defaults` order.
pub const KEYS: [&str; 8] = [
    ENABLED,
    PROVIDER,
    MODEL,
    SPEAKER,
    AGENT_NAME,
    TRADING,
    CONFIRM_WINDOW,
    IDLE_TIMEOUT,
];

/// The shortest text accepted as an OpenAI key.
pub const MIN_KEY_CHARS: usize = 20;

pub const NOT_A_KEY: &str = "That does not look like an OpenAI API key.";

/// The voice block read at one moment.
#[derive(Debug, Clone)]
pub struct VoiceSettings {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub speaker: String,
    pub agent_name: String,
    /// The spoken surface's own switch, before the master switch.
    pub trading: bool,
    pub confirm_window_seconds: i64,
    pub idle_timeout_seconds: i64,
    /// The Agent's master trading switch (`trading_enabled`).
    pub trading_master: bool,
}

impl VoiceSettings {
    pub fn of(s: &Snapshot) -> VoiceSettings {
        VoiceSettings {
            enabled: s.flag(ENABLED),
            provider: s.text(PROVIDER).to_string(),
            model: s.text(MODEL).to_string(),
            speaker: s.text(SPEAKER).to_string(),
            agent_name: s.text(AGENT_NAME).to_string(),
            trading: s.flag(TRADING),
            confirm_window_seconds: s.whole(CONFIRM_WINDOW),
            idle_timeout_seconds: s.whole(IDLE_TIMEOUT),
            trading_master: s.flag(TRADING_ENABLED),
        }
    }

    /// Read fresh. An unreadable database gives the defaults: voice off and
    /// no spoken trading (`Snapshot::read`).
    pub fn read(app: &AppState) -> VoiceSettings {
        VoiceSettings::of(&Snapshot::read(app))
    }

    /// Whether a spoken request can reach the order tools: both switches.
    pub fn trading_effective(&self) -> bool {
        self.trading_master && self.trading
    }

    /// The configured provider, or the refusal naming what is accepted.
    pub fn spec(&self) -> Result<&'static VoiceProviderSpec, String> {
        voice_provider_spec(&self.provider)
    }
}

/// The shipped voice block (the web's `get_voice_defaults`).
pub fn defaults_json(app: &AppState) -> Value {
    Value::Object(
        KEYS.iter()
            .filter_map(|k| schema::field_of(k))
            .map(|f| (f.key.to_string(), render(&default_of(f, &app.data_dir))))
            .collect(),
    )
}

/// Presence and fingerprint of the stored key, never its value; a row that
/// cannot be read reads as absent.
fn key_row(app: &AppState, name: &str) -> Value {
    let absent = json!({"has_value": false, "fingerprint": null, "last_used_at": null});
    let found = app
        .sqlite
        .conn()
        .and_then(|c| secrets::describe(&c, &[name.to_string()]));
    match found {
        Ok(rows) => rows.get(name).map_or(absent, |info| {
            let row = info.to_json();
            json!({
                "has_value": row["has_value"],
                "fingerprint": row["fingerprint"],
                "last_used_at": row["last_used_at"],
            })
        }),
        Err(e) => {
            tracing::error!("Could not read the voice key row: {}", e);
            absent
        }
    }
}

/// The whole voice configuration (the web's `get_voice_config`): the eight
/// settings, both trading readings, the provider's speakers and models, the
/// providers, the defaults and the key's description. No key value.
pub fn config_json(app: &AppState) -> Result<Value, String> {
    let snapshot = Snapshot::read(app);
    let v = VoiceSettings::of(&snapshot);
    let spec = v.spec()?;
    let stored = snapshot.to_json();
    let mut out = Map::new();
    for key in KEYS {
        out.insert(key.to_string(), stored[key].clone());
    }
    out.insert("trading_enabled_master".into(), json!(v.trading_master));
    out.insert("trading_effective".into(), json!(v.trading_effective()));
    out.insert("speakers".into(), json!(spec.speakers));
    out.insert("known_models".into(), json!(spec.known_models));
    out.insert(
        "providers".into(),
        Value::Array(
            VOICE_PROVIDERS
                .iter()
                .map(|p| json!({"id": p.id, "label": p.label, "default_model": p.default_model}))
                .collect(),
        ),
    );
    out.insert("defaults".into(), defaults_json(app));
    out.insert("key".into(), key_row(app, spec.secret_name));
    Ok(Value::Object(out))
}

/// `{status, data}` for the voice routes, or the web's 500.
fn answered(app: &AppState, extra: Option<(&str, &str)>, failure: &str) -> Result<Value, Refusal> {
    let data = config_json(app).map_err(|e| {
        tracing::error!("Could not read the voice configuration: {}", e);
        Refusal::new(500, failure, None)
    })?;
    let mut payload = json!({ "data": data });
    if let Some((k, v)) = extra {
        payload[k] = json!(v);
    }
    Ok(success(payload))
}

/// `GET /agent/api/voice`.
pub fn voice_answer(app: &AppState) -> Result<Value, Refusal> {
    answered(app, None, "Could not read the voice configuration")
}

/// The validated rows of a partial update, in the web's order of checks:
/// unknown keys, the name, the provider, the speaker, the model; the
/// switches and the two windows are checked by the settings writer.
fn pending_of(app: &AppState, values: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut unknown: Vec<&str> = values
        .keys()
        .map(String::as_str)
        .filter(|k| !KEYS.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(format!("Unknown voice settings: {}", unknown.join(", ")));
    }
    let current = VoiceSettings::read(app);
    let mut pending = Map::new();
    if let Some(raw) = values.get(AGENT_NAME) {
        let name = match raw {
            Value::Null => "Vega".to_string(),
            Value::String(s) if s.is_empty() => "Vega".to_string(),
            other if truthy(other) => crate::agent::settings::pyrepr::py_str(other),
            _ => String::new(),
        };
        pending.insert(AGENT_NAME.into(), json!(normalise_agent_name(&name)?));
    }
    let provider = match values.get(PROVIDER) {
        Some(_) => Some(text_field(values, PROVIDER)).filter(|p| !p.is_empty()),
        None => Some(current.provider.clone()).filter(|p| !p.is_empty()),
    };
    let spec = voice_provider_spec(provider.as_deref().unwrap_or("openai"))?;
    if values.contains_key(PROVIDER) {
        pending.insert(PROVIDER.into(), json!(spec.id));
    }
    if values.contains_key(SPEAKER) {
        let speaker = text_field(values, SPEAKER).to_lowercase();
        if !spec.speakers.contains(&speaker.as_str()) {
            return Err(format!(
                "Voice must be one of: {}",
                spec.speakers.join(", ")
            ));
        }
        pending.insert(SPEAKER.into(), json!(speaker));
    }
    if values.contains_key(MODEL) {
        let model = text_field(values, MODEL);
        if model.is_empty() {
            return Err("The voice model cannot be empty".into());
        }
        pending.insert(MODEL.into(), json!(model));
    }
    for key in [ENABLED, TRADING, CONFIRM_WINDOW, IDLE_TIMEOUT] {
        if let Some(v) = values.get(key) {
            pending.insert(key.into(), v.clone());
        }
    }
    Ok(pending)
}

/// `PUT /agent/api/voice`: every value checked before anything is written;
/// never a key (that is its own route).
pub fn update_answer(app: &AppState, body: &Map<String, Value>) -> Result<Value, Refusal> {
    if body.is_empty() {
        return Err(Refusal::bad_request("Nothing to update"));
    }
    let pending = pending_of(app, body).map_err(Refusal::bad_request)?;
    if !pending.is_empty() {
        match settings::update(app, &pending) {
            Ok(_) => {}
            Err(UpdateError::Invalid(m)) => return Err(Refusal::bad_request(m)),
            Err(UpdateError::Store) => {
                return Err(Refusal::new(
                    500,
                    "Could not save the voice configuration",
                    None,
                ))
            }
        }
    }
    answered(app, None, "Could not save the voice configuration")
}

/// The configured provider's key row name (`voice:openai`).
fn secret_name(app: &AppState) -> Result<&'static str, String> {
    VoiceSettings::read(app).spec().map(|s| s.secret_name)
}

/// `PUT /agent/api/voice/key` with `{api_key}`: blank and short keys are
/// refused (clearing is the DELETE). The key is never logged or returned.
pub fn set_key_answer(app: &AppState, body: &Map<String, Value>) -> Result<Value, Refusal> {
    let key = Secret::new(text_field(body, "api_key"));
    if key.expose().chars().count() < MIN_KEY_CHARS {
        return Err(Refusal::bad_request(NOT_A_KEY));
    }
    let name = secret_name(app).map_err(Refusal::bad_request)?;
    let stored = app
        .sqlite
        .conn()
        .map_err(|_| "no connection")
        .and_then(|c| secrets::set(&c, &app.security, name, key.expose()));
    if stored.is_err() {
        tracing::error!("Could not store the voice key");
        return Err(Refusal::new(500, "Could not store the voice key", None));
    }
    tracing::info!("Voice key stored");
    answered(
        app,
        Some(("message", "Voice key stored")),
        "Could not store the voice key",
    )
}

/// `DELETE /agent/api/voice/key`: idempotent; the settings stay.
pub fn clear_key_answer(app: &AppState) -> Result<Value, Refusal> {
    let refused = || Refusal::new(500, "Could not clear the voice key", None);
    let name = secret_name(app).map_err(|_| refused())?;
    let removed = app.sqlite.conn().and_then(|c| secrets::delete(&c, name));
    if let Err(e) = removed {
        tracing::error!("Could not remove the voice key: {}", e);
        return Err(refused());
    }
    tracing::info!("Voice key removed");
    answered(
        app,
        Some(("message", "Voice key cleared")),
        "Could not clear the voice key",
    )
}

/// The stored key, for minting a session only; never shown or logged.
pub fn voice_key(app: &AppState, spec: &VoiceProviderSpec) -> Option<Secret> {
    let conn = app
        .sqlite
        .conn()
        .inspect_err(|e| tracing::error!("Could not read the voice key: {}", e))
        .ok()?;
    secrets::get(&conn, &app.security, spec.secret_name).filter(|k| !k.expose().is_empty())
}

/// Record that the key minted a session (its "last used" on the panel).
pub fn mark_key_used(app: &AppState, spec: &VoiceProviderSpec) {
    match app.sqlite.conn() {
        Ok(c) => secrets::mark_used(&c, spec.secret_name, app.now()),
        Err(e) => tracing::warn!("Could not record voice key usage: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One place for the key names: the config lane's schema seeds the
    /// same eight, in the same order.
    #[test]
    fn the_block_is_the_schemas_voice_keys() {
        assert_eq!(KEYS, schema::VOICE_KEYS);
    }

    /// The web's "an install with no row still gets a timeout": a missing
    /// row must not read as "never hang up", the expensive direction; every
    /// switch reads as its safe end.
    #[test]
    fn missing_rows_read_as_the_shipped_safe_values() {
        let dir = std::env::temp_dir();
        let v = VoiceSettings::of(&Snapshot::from_rows(&Default::default(), &dir));
        assert_eq!(
            (v.idle_timeout_seconds, v.confirm_window_seconds),
            (180, 30)
        );
        assert!(!v.enabled && !v.trading && !v.trading_master && !v.trading_effective());
        assert_eq!(
            (v.provider.as_str(), v.agent_name.as_str()),
            ("openai", "Vega")
        );
        let rows = [
            (IDLE_TIMEOUT, "9999"),
            (TRADING, "yes"),
            (TRADING_ENABLED, "nope"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let v = VoiceSettings::of(&Snapshot::from_rows(&rows, &dir));
        assert_eq!(
            v.idle_timeout_seconds, 3600,
            "a stored value past the bound is clamped"
        );
        assert!(
            v.trading && !v.trading_effective(),
            "voice alone never trades"
        );
    }
}
