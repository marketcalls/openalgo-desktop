//! The Agent's settings in `ag_setting` (the web's
//! `services/agent/settings.py`): the trading switch, the risk limits the
//! order guard reads, the system prompt override, the default reasoning
//! effort, the voice block and the external data switch.
//!
//! - `schema`: every key, its kind, default and bounds, in the web's order.
//! - `parse`: strict on write, lenient on read; stored text and the JSON
//!   the settings screen reads.
//! - `decimal`, `pyrepr`: Python's decimal and value printing, so stored
//!   values and refusals read exactly as the web's.
//! - `store`: the rows.
//! - `kill_switch`: the switch's two halves, the setting and the file.
//! - `policy`: the run settings the chat engine reads ([`RunPolicy`]).
//!
//! This module never reads the environment: every setting lives in the
//! database and changes from the app (no `.env`). A read that fails gives
//! the defaults, which are the safe end of every switch.
//!
//! [`RunPolicy`]: crate::agent::contract::policy::RunPolicy

pub mod decimal;
pub mod kill_switch;
pub mod parse;
pub mod policy;
pub mod pyrepr;
pub mod schema;
pub mod store;

#[cfg(test)]
mod tests;

use crate::state::AppState;
use parse::{default_of, parse, render, serialise, Typed};
use schema::{Field, FIELDS};
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// Every setting read at one moment, typed.
#[derive(Debug, Clone)]
pub struct Snapshot {
    values: Vec<(&'static Field, Typed)>,
}

impl Snapshot {
    /// The settings now, read fresh. A database that cannot be read gives
    /// the defaults (trading off), logged, as the web's `_load_all`.
    pub fn read(app: &AppState) -> Snapshot {
        let rows = app
            .sqlite
            .conn()
            .and_then(|c| store::read_all(&c))
            .unwrap_or_else(|e| {
                tracing::error!(
                    "Agent settings could not be read; using the defaults: {}",
                    e
                );
                HashMap::new()
            });
        Snapshot::from_rows(&rows, &app.data_dir)
    }

    /// Typed values from stored rows; a missing or unusable row is its
    /// default.
    pub fn from_rows(rows: &HashMap<String, String>, data_dir: &Path) -> Snapshot {
        Snapshot {
            values: FIELDS
                .iter()
                .map(|f| {
                    let default = default_of(f, data_dir);
                    let raw = rows
                        .get(f.key)
                        .map(|t| Value::String(t.clone()))
                        .unwrap_or(Value::Null);
                    let value = parse(f, &raw, &default, false).unwrap_or(default);
                    (f, value)
                })
                .collect(),
        }
    }

    fn get(&self, key: &str) -> Option<&Typed> {
        self.values
            .iter()
            .find(|(f, _)| f.key == key)
            .map(|(_, v)| v)
    }

    /// A switch (`trading_enabled`, `kill_switch`); false for any other key.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Typed::Bool(true)))
    }

    /// A whole-number limit (`max_orders_per_session`); 0 for any other key.
    pub fn whole(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(Typed::Int(i)) => *i,
            _ => 0,
        }
    }

    /// A money or percentage limit as stored (`"500000.00"`).
    pub fn decimal(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Typed::Decimal(d)) => Some(d.to_string()),
            _ => None,
        }
    }

    /// A list limit (`allowed_exchanges`), upper-cased tokens.
    pub fn tokens(&self, key: &str) -> BTreeSet<String> {
        match self.get(key) {
            Some(Typed::Set(s)) => s.clone(),
            _ => BTreeSet::new(),
        }
    }

    /// A text or choice setting (`default_reasoning_effort`).
    pub fn text(&self, key: &str) -> &str {
        match self.get(key) {
            Some(Typed::Text(t)) => t,
            _ => "",
        }
    }

    /// The trader's replacement system prompt; blank is none.
    pub fn system_prompt_override(&self) -> Option<String> {
        let text = self.text(schema::SYSTEM_PROMPT).trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    /// Every setting as `GET /agent/api/settings` shows it (the web's
    /// `get_all`): decimals as strings, lists sorted, the override `null`
    /// when unset.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        for (f, v) in &self.values {
            out.insert(f.key.to_string(), render(v));
        }
        out.insert(
            schema::SYSTEM_PROMPT.to_string(),
            self.system_prompt_override()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        Value::Object(out)
    }
}

/// The shipped defaults as the settings screen reads them (the web's
/// `_json_safe_settings(get_setting_defaults())`): the override is `""`.
pub fn defaults_json(data_dir: &Path) -> Value {
    Value::Object(
        FIELDS
            .iter()
            .map(|f| (f.key.to_string(), render(&default_of(f, data_dir))))
            .collect(),
    )
}

/// Why a settings write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// A key or a value the trader can correct (400, the sentence as is).
    Invalid(String),
    /// The database refused the write (500).
    Store,
}

/// Validate the values, the order the web prepares them: an unknown key is
/// refused first, then each value strictly, before anything is written.
fn prepare(
    values: &Map<String, Value>,
    data_dir: &Path,
) -> Result<Vec<(&'static str, String)>, UpdateError> {
    let mut unknown: Vec<&str> = values
        .keys()
        .map(String::as_str)
        .filter(|k| schema::field_of(k).is_none())
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        unknown.dedup();
        return Err(UpdateError::Invalid(format!(
            "Unknown agent setting(s): {}",
            unknown.join(", ")
        )));
    }
    values
        .iter()
        .filter_map(|(k, raw)| schema::field_of(k).map(|f| (f, raw)))
        .map(|(f, raw)| {
            let default = default_of(f, data_dir);
            parse(f, raw, &default, true)
                .map(|v| (f.key, serialise(&v)))
                .map_err(UpdateError::Invalid)
        })
        .collect()
}

/// A partial update (`PUT /agent/api/settings`, the web's `update`): every
/// value validated before any write, all written in one transaction, and
/// every setting answered fresh after it.
pub fn update(app: &AppState, values: &Map<String, Value>) -> Result<Value, UpdateError> {
    let prepared = prepare(values, &app.data_dir)?;
    let written = app
        .sqlite
        .conn()
        .and_then(|c| store::write_all(&c, &prepared));
    if let Err(e) = written {
        tracing::error!("Agent settings could not be saved: {}", e);
        return Err(UpdateError::Store);
    }
    let mut keys: Vec<&str> = prepared.iter().map(|(k, _)| *k).collect();
    keys.sort_unstable();
    tracing::info!("Agent settings updated: {}", keys.join(", "));
    Ok(Snapshot::read(app).to_json())
}

/// `GET /agent/api/settings`: every setting, read fresh, and the shipped
/// defaults beside them.
pub fn settings_answer(app: &AppState) -> Value {
    crate::agent::models::success(serde_json::json!({
        "data": Snapshot::read(app).to_json(),
        "defaults": defaults_json(&app.data_dir),
    }))
}

/// `PUT /agent/api/settings`.
pub fn update_answer(
    app: &AppState,
    body: &Map<String, Value>,
) -> Result<Value, crate::agent::models::Refused> {
    use crate::agent::models::Refused;
    if body.is_empty() {
        return Err(Refused::bad_request("Nothing to update"));
    }
    match update(app, body) {
        Ok(data) => Ok(crate::agent::models::success(
            serde_json::json!({ "data": data }),
        )),
        Err(UpdateError::Invalid(message)) => Err(Refused::bad_request(message)),
        Err(UpdateError::Store) => Err(Refused::new(500, "Could not save the agent settings")),
    }
}

/// `GET /agent/api/kill-switch` (desktop): the switch's two halves.
pub fn kill_switch_answer(app: &AppState) -> Value {
    crate::agent::models::success(serde_json::json!({
        "data": kill_switch::state(app).to_json(),
    }))
}

/// `PUT /agent/api/kill-switch` (desktop) with `{"engaged": bool}`: the
/// Trading safety panel's switch, which sets the setting and the file
/// together (decision 2).
pub fn set_kill_switch_answer(
    app: &AppState,
    body: &Map<String, Value>,
) -> Result<Value, crate::agent::models::Refused> {
    use crate::agent::models::Refused;
    let Some(Value::Bool(engaged)) = body.get("engaged") else {
        return Err(Refused::bad_request("engaged must be true or false"));
    };
    match kill_switch::set(app, *engaged) {
        Ok(state) => Ok(crate::agent::models::success(
            serde_json::json!({ "data": state.to_json() }),
        )),
        Err(e) => {
            tracing::error!("The Agent kill switch could not be saved: {}", e);
            Err(Refused::new(500, "Could not save the kill switch"))
        }
    }
}
