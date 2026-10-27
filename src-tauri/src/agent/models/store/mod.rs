//! `ag_provider_model` rows and the rules the registry keeps (the web's
//! `agent_db` model functions): one row per model the trader added, several
//! providers at once, exactly one enabled and tested default.
//!
//! Each write runs in one IMMEDIATE transaction, so there is never a
//! committed state with two defaults. Each function answers as the web's
//! store does, `Ok` or the sentence the route shows; a database failure is
//! logged and answered with the web's generic sentence.

use super::secrets::{model_secret_name, provider_secret_name, SecretInfo};
use crate::agent::conversations::store::{iso, stamp};
use crate::agent::providers::{BASE_URL_KINDS, PROVIDER_KINDS};
use crate::agent::settings::schema::REASONING_EFFORTS;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};
use std::collections::HashMap;

/// One registered model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub id: i64,
    pub provider_kind: String,
    pub model_name: String,
    pub display_name: String,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub supports_reasoning: bool,
    pub default_reasoning_effort: String,
    pub supports_vision: bool,
    pub tools_unreliable: bool,
    pub last_tested_at: Option<String>,
    pub last_test_ok: Option<bool>,
    pub last_test_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, provider_kind, model_name, display_name, base_url, enabled, \
     is_default, supports_reasoning, default_reasoning_effort, supports_vision, \
     tools_unreliable, last_tested_at, last_test_ok, last_test_error, created_at, updated_at";

impl ModelRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<ModelRow> {
        Ok(ModelRow {
            id: r.get(0)?,
            provider_kind: r.get(1)?,
            model_name: r.get(2)?,
            display_name: r.get(3)?,
            base_url: r.get(4)?,
            enabled: r.get(5)?,
            is_default: r.get(6)?,
            supports_reasoning: r.get(7)?,
            default_reasoning_effort: r.get(8)?,
            supports_vision: r.get(9)?,
            tools_unreliable: r.get(10)?,
            last_tested_at: r.get(11)?,
            last_test_ok: r.get(12)?,
            last_test_error: r.get(13)?,
            created_at: r.get(14)?,
            updated_at: r.get(15)?,
        })
    }

    /// The secret names that can answer for this row, own key first.
    pub fn secret_names(&self) -> [String; 2] {
        [
            model_secret_name(self.id),
            provider_secret_name(&self.provider_kind),
        ]
    }

    /// The web's `provider_model_to_dict`: the key described by whichever
    /// secret answers (the model's own wins), never shown.
    pub fn to_json(&self, secrets: &HashMap<String, SecretInfo>) -> Value {
        let [own, shared] = self.secret_names();
        let secret = secrets
            .get(&own)
            .or_else(|| secrets.get(&shared))
            .filter(|s| s.has_value);
        json!({
            "id": self.id,
            "provider_kind": self.provider_kind,
            "model_name": self.model_name,
            "display_name": self.display_name,
            "base_url": self.base_url,
            "enabled": self.enabled,
            "is_default": self.is_default,
            "supports_reasoning": self.supports_reasoning,
            "default_reasoning_effort": self.default_reasoning_effort,
            "supports_vision": self.supports_vision,
            "tools_unreliable": self.tools_unreliable,
            "last_tested_at": self.last_tested_at.as_deref().map(iso).unwrap_or(Value::Null),
            "last_test_ok": self.last_test_ok,
            "last_test_error": self.last_test_error,
            "has_api_key": secret.is_some(),
            "api_key_fingerprint": secret.map(|s| s.fingerprint.clone()),
            "api_key_source": secret.map(|s| s.name.clone()),
            "created_at": iso(&self.created_at),
            "updated_at": iso(&self.updated_at),
        })
    }
}

/// The store's answer: the result, or the sentence to show.
pub type Answer<T> = std::result::Result<T, String>;

pub const NOT_FOUND: &str = "Model not found";

/// A database failure as the web's generic sentence, logged once here.
fn failed<T>(what: &'static str, e: impl std::fmt::Display) -> Answer<T> {
    tracing::error!("Agent model store: {} failed: {}", what, e);
    Err(what_sentence(what).to_string())
}

fn what_sentence(what: &'static str) -> &'static str {
    match what {
        "create" => "Could not register the model",
        "update" => "Could not update the model",
        "delete" => "Could not delete the model",
        "default" => "Could not set the default model",
        "test" => "Could not record the test result",
        _ => "Could not read the models",
    }
}

/// An empty or blank base URL is no base URL (two spellings of "none"
/// would defeat the duplicate check).
pub fn normalize_stored_base_url(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The web's `_validate_model_config`.
fn validate(
    kind: &str,
    name: &str,
    display: &str,
    base_url: Option<&str>,
    effort: &str,
) -> Answer<()> {
    if !PROVIDER_KINDS.contains(&kind) {
        return Err(format!("Unknown provider kind '{}'", kind));
    }
    if name.trim().is_empty() {
        return Err("A model name is required".into());
    }
    if display.trim().is_empty() {
        return Err("A display name is required".into());
    }
    if !REASONING_EFFORTS.contains(&effort) {
        return Err(format!("Unknown reasoning effort '{}'", effort));
    }
    if BASE_URL_KINDS.contains(&kind) && base_url.is_none() {
        return Err(format!("A base URL is required for {}", kind));
    }
    Ok(())
}

fn duplicate_exists(
    conn: &Connection,
    kind: &str,
    name: &str,
    base_url: Option<&str>,
    exclude: Option<i64>,
) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM ag_provider_model
            WHERE provider_kind = ?1 AND model_name = ?2 AND base_url IS ?3
              AND id IS NOT ?4)",
        params![kind, name, base_url, exclude],
        |r| r.get(0),
    )
}

pub fn get(conn: &Connection, id: i64) -> crate::error::Result<Option<ModelRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM ag_provider_model WHERE id = ?1", COLUMNS),
            params![id],
            ModelRow::from_row,
        )
        .optional()?)
}

/// The enabled default, or none (the setup gate's case).
pub fn default_model(conn: &Connection) -> crate::error::Result<Option<ModelRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM ag_provider_model WHERE is_default = 1 AND enabled = 1
                 ORDER BY id LIMIT 1",
                COLUMNS
            ),
            [],
            ModelRow::from_row,
        )
        .optional()?)
}

/// Every model, the default first, then oldest first.
pub fn list(conn: &Connection, enabled_only: bool) -> crate::error::Result<Vec<ModelRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM ag_provider_model {} ORDER BY is_default DESC, id ASC",
        COLUMNS,
        if enabled_only {
            "WHERE enabled = 1"
        } else {
            ""
        }
    ))?;
    let rows = stmt
        .query_map([], ModelRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The setup gate: an enabled default whose last test passed.
pub fn is_configured(conn: &Connection) -> crate::error::Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM ag_provider_model
            WHERE is_default = 1 AND enabled = 1 AND last_test_ok = 1)",
        [],
        |r| r.get(0),
    )?)
}

/// What a new row carries (the key is not part of it).
#[derive(Debug, Clone, Default)]
pub struct NewModel {
    pub provider_kind: String,
    pub model_name: String,
    pub display_name: String,
    pub base_url: Option<String>,
    pub default_reasoning_effort: String,
    pub enabled: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub supports_vision: Option<bool>,
    pub tools_unreliable: Option<bool>,
}

/// Register a model (the web's `create_model`). A new model is never the
/// default: it has not been tested.
pub fn create(conn: &Connection, m: &NewModel, now: DateTime<Utc>) -> Answer<ModelRow> {
    let kind = m.provider_kind.trim();
    let name = m.model_name.trim();
    let display = m.display_name.trim();
    let base_url = normalize_stored_base_url(m.base_url.as_deref());
    let effort = m.default_reasoning_effort.trim();
    let effort = if effort.is_empty() { "off" } else { effort };
    validate(kind, name, display, base_url.as_deref(), effort)?;
    let run = || -> rusqlite::Result<Answer<i64>> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        if duplicate_exists(&tx, kind, name, base_url.as_deref(), None)? {
            return Ok(Err(format!(
                "{} is already registered for this provider",
                display
            )));
        }
        tx.execute(
            "INSERT INTO ag_provider_model (provider_kind, model_name, display_name, base_url,
                 enabled, is_default, supports_reasoning, default_reasoning_effort,
                 supports_vision, tools_unreliable, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                kind,
                name,
                display,
                base_url,
                m.enabled.unwrap_or(true),
                m.supports_reasoning.unwrap_or(false),
                effort,
                m.supports_vision.unwrap_or(false),
                m.tools_unreliable.unwrap_or(false),
                stamp(now),
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Ok(id))
    };
    match run() {
        Ok(Ok(id)) => match get(conn, id) {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(what_sentence("create").into()),
            Err(e) => failed("create", e),
        },
        Ok(Err(sentence)) => Err(sentence),
        Err(e) => failed("create", e),
    }
}

/// Clear every default but `keep` (none when `None`). Inside a transaction.
fn clear_other_defaults(conn: &Connection, keep: Option<i64>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ag_provider_model SET is_default = 0 WHERE is_default = 1 AND id IS NOT ?1",
        params![keep],
    )?;
    Ok(())
}

/// Hand the default to the oldest other enabled model that passed its test;
/// none leaves the Agent unconfigured until one is tested. Inside a
/// transaction.
fn promote_replacement(conn: &Connection, excluded: i64) -> rusqlite::Result<()> {
    let next: Option<i64> = conn
        .query_row(
            "SELECT id FROM ag_provider_model
              WHERE id <> ?1 AND enabled = 1 AND last_test_ok = 1
              ORDER BY id LIMIT 1",
            params![excluded],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = next {
        clear_other_defaults(conn, Some(id))?;
        conn.execute(
            "UPDATE ag_provider_model SET is_default = 1 WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(())
}

/// Remove a model with its own key override and its default status (the
/// web's `delete_model`). The `model:{id}` key goes too: SQLite reuses a
/// deleted highest id, and a key left behind would become the next model's.
pub fn delete(conn: &Connection, id: i64) -> Answer<()> {
    let run = || -> rusqlite::Result<Answer<()>> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        let was_default: Option<bool> = tx
            .query_row(
                "SELECT is_default FROM ag_provider_model WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(was_default) = was_default else {
            return Ok(Err(NOT_FOUND.into()));
        };
        tx.execute(
            "DELETE FROM ag_secret WHERE name = ?1",
            params![model_secret_name(id)],
        )?;
        tx.execute("DELETE FROM ag_provider_model WHERE id = ?1", params![id])?;
        if was_default {
            promote_replacement(&tx, id)?;
        }
        tx.commit()?;
        Ok(Ok(()))
    };
    run().unwrap_or_else(|e| failed("delete", e))
}

/// Make a model the default, clearing every other (the web's
/// `set_default_model`): it must be enabled and have passed its test.
pub fn set_default(conn: &Connection, id: i64) -> Answer<()> {
    let run = || -> rusqlite::Result<Answer<()>> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        let row: Option<(bool, Option<bool>)> = tx
            .query_row(
                "SELECT enabled, last_test_ok FROM ag_provider_model WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            None => return Ok(Err(NOT_FOUND.into())),
            Some((false, _)) => {
                return Ok(Err("Enable the model before making it the default".into()))
            }
            Some((true, ok)) if ok != Some(true) => {
                return Ok(Err(
                    "Test the model's credentials before making it the default".into(),
                ))
            }
            _ => {}
        }
        clear_other_defaults(&tx, Some(id))?;
        tx.execute(
            "UPDATE ag_provider_model SET is_default = 1 WHERE id = ?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    };
    run().unwrap_or_else(|e| failed("default", e))
}

/// Store a credential test's outcome (the web's `record_model_test`). A
/// pass clears the error and makes an enabled model the default when no
/// other enabled model holds it (the first model a trader tests is the one
/// they meant to use); a failure keeps the provider's own message and takes
/// the default away (another tested model gets it).
pub fn record_test(
    conn: &Connection,
    id: i64,
    ok: bool,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> Answer<()> {
    let message = (!ok).then(|| {
        error
            .filter(|e| !e.is_empty())
            .unwrap_or("The provider rejected the request")
            .to_string()
    });
    let run = || -> rusqlite::Result<Answer<()>> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        let row: Option<(bool, bool)> = tx
            .query_row(
                "SELECT enabled, is_default FROM ag_provider_model WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((enabled, is_default)) = row else {
            return Ok(Err(NOT_FOUND.into()));
        };
        tx.execute(
            "UPDATE ag_provider_model
                SET last_tested_at = ?1, last_test_ok = ?2, last_test_error = ?3
              WHERE id = ?4",
            params![stamp(now), ok, message, id],
        )?;
        if ok && enabled {
            let other_default: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM ag_provider_model
                    WHERE is_default = 1 AND enabled = 1 AND id <> ?1)",
                params![id],
                |r| r.get(0),
            )?;
            if !other_default {
                clear_other_defaults(&tx, Some(id))?;
                tx.execute(
                    "UPDATE ag_provider_model SET is_default = 1 WHERE id = ?1",
                    params![id],
                )?;
            }
        } else if !ok && is_default {
            tx.execute(
                "UPDATE ag_provider_model SET is_default = 0 WHERE id = ?1",
                params![id],
            )?;
            promote_replacement(&tx, id)?;
        }
        tx.commit()?;
        Ok(Ok(()))
    };
    run().unwrap_or_else(|e| failed("test", e))
}

mod update;
pub use update::{update, ModelChanges};
