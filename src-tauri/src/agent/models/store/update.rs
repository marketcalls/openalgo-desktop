//! Editing a registered model (the web's `update_model`).
//!
//! Only the allowlisted fields change. The default, the test result and the
//! identity each have their own path: asking for one here is refused with a
//! sentence rather than ignored, because a 200 for a change that did not
//! happen reads as success. A new base URL clears the test result and the
//! default (the credentials were tested against the old endpoint), and a
//! default that is disabled or moved hands the default on.

use super::{
    duplicate_exists, failed, get, normalize_stored_base_url, promote_replacement, validate,
    Answer, ModelRow, NOT_FOUND,
};
use crate::agent::conversations::store::stamp;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::Value;

/// What one edit asks for. `None` leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct ModelChanges {
    pub display_name: Option<String>,
    /// `Some(None)` removes the base URL.
    pub base_url: Option<Option<String>>,
    pub default_reasoning_effort: Option<String>,
    pub enabled: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub supports_vision: Option<bool>,
    pub tools_unreliable: Option<bool>,
    /// The request named `is_default` (refused: the default has its action).
    pub is_default: bool,
    /// The identity the request named, as sent (refused when it differs).
    pub provider_kind: Option<Value>,
    pub model_name: Option<Value>,
}

impl ModelChanges {
    /// Nothing asked for (the route answers "Nothing to update").
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.base_url.is_none()
            && self.default_reasoning_effort.is_none()
            && self.enabled.is_none()
            && self.supports_reasoning.is_none()
            && self.supports_vision.is_none()
            && self.tools_unreliable.is_none()
            && !self.is_default
            && self.provider_kind.is_none()
            && self.model_name.is_none()
    }

    /// The identity refusal: a value that is present, not null and not the
    /// row's own.
    fn changes_identity(&self, row: &ModelRow) -> bool {
        let differs = |asked: &Option<Value>, current: &str| match asked {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => s != current,
            Some(_) => true,
        };
        differs(&self.provider_kind, &row.provider_kind)
            || differs(&self.model_name, &row.model_name)
    }
}

/// The row after the edit, validated, before it is written.
struct Edited {
    row: ModelRow,
    endpoint_changed: bool,
    lost_default: bool,
}

fn apply(row: &ModelRow, c: &ModelChanges) -> Answer<Edited> {
    let mut next = row.clone();
    if let Some(url) = &c.base_url {
        next.base_url = normalize_stored_base_url(url.as_deref());
    }
    if let Some(effort) = &c.default_reasoning_effort {
        next.default_reasoning_effort = effort.clone();
    }
    if let Some(name) = &c.display_name {
        next.display_name = name.trim().to_string();
    }
    validate(
        &next.provider_kind,
        &next.model_name,
        &next.display_name,
        next.base_url.as_deref(),
        &next.default_reasoning_effort,
    )?;
    for (flag, value) in [
        (&mut next.enabled, c.enabled),
        (&mut next.supports_reasoning, c.supports_reasoning),
        (&mut next.supports_vision, c.supports_vision),
        (&mut next.tools_unreliable, c.tools_unreliable),
    ] {
        if let Some(v) = value {
            *flag = v;
        }
    }
    let endpoint_changed = next.base_url != row.base_url;
    if endpoint_changed {
        next.last_tested_at = None;
        next.last_test_ok = None;
        next.last_test_error = None;
    }
    let lost_default = row.is_default && (endpoint_changed || !next.enabled);
    if lost_default {
        next.is_default = false;
    }
    Ok(Edited {
        row: next,
        endpoint_changed,
        lost_default,
    })
}

/// Edit a model; the edited row, or the sentence to show.
pub fn update(
    conn: &Connection,
    id: i64,
    c: &ModelChanges,
    now: DateTime<Utc>,
) -> Answer<ModelRow> {
    let run = || -> rusqlite::Result<Answer<()>> {
        let tx = crate::db::sqlite::write_transaction(conn)?;
        let row = match get(&tx, id) {
            Ok(Some(row)) => row,
            Ok(None) => return Ok(Err(NOT_FOUND.into())),
            Err(e) => return Ok(failed("update", e)),
        };
        if c.is_default {
            return Ok(Err(
                "Use the default action to change which model is the default".into(),
            ));
        }
        if c.changes_identity(&row) {
            return Ok(Err("A model's provider and model name cannot be changed. \
                 Register the other model instead."
                .into()));
        }
        let edited = match apply(&row, c) {
            Ok(e) => e,
            Err(sentence) => return Ok(Err(sentence)),
        };
        let next = &edited.row;
        if edited.endpoint_changed
            && duplicate_exists(
                &tx,
                &next.provider_kind,
                &next.model_name,
                next.base_url.as_deref(),
                Some(id),
            )?
        {
            return Ok(Err(format!(
                "{} is already registered at that endpoint",
                next.display_name
            )));
        }
        write(&tx, next, now)?;
        if edited.lost_default {
            promote_replacement(&tx, id)?;
        }
        tx.commit()?;
        Ok(Ok(()))
    };
    match run() {
        Ok(Ok(())) => match get(conn, id) {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(NOT_FOUND.into()),
            Err(e) => failed("update", e),
        },
        Ok(Err(sentence)) => Err(sentence),
        Err(e) => failed("update", e),
    }
}

fn write(conn: &Connection, r: &ModelRow, now: DateTime<Utc>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE ag_provider_model
            SET display_name = ?1, base_url = ?2, enabled = ?3, is_default = ?4,
                supports_reasoning = ?5, default_reasoning_effort = ?6,
                supports_vision = ?7, tools_unreliable = ?8, last_tested_at = ?9,
                last_test_ok = ?10, last_test_error = ?11, updated_at = ?12
          WHERE id = ?13",
        params![
            r.display_name,
            r.base_url,
            r.enabled,
            r.is_default,
            r.supports_reasoning,
            r.default_reasoning_effort,
            r.supports_vision,
            r.tools_unreliable,
            r.last_tested_at,
            r.last_test_ok,
            r.last_test_error,
            stamp(now),
            r.id,
        ],
    )?;
    Ok(())
}
