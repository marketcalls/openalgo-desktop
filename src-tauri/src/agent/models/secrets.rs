//! The Agent's keys in `ag_secret` (the web's `agent_db` secret functions):
//! provider keys (`provider:{kind}`), per-model overrides (`model:{id}`),
//! and the rows other parts store here (`websearch:{provider}`,
//! `oauth:chatgpt`, `voice:openai`).
//!
//! Each value is sealed with the app's data key (KEY-01,
//! `messaging::sealed`), bound to its row by name, so a ciphertext copied
//! to another name does not open. This is the only module that decrypts
//! one. Nothing here returns a value to be shown: a key is described by a
//! presence flag and a fingerprint. A fresh start or an account reset drops
//! every row ([`wipe`]), since the key they were sealed with is gone.
//!
//! Failures are logged with the secret's name and the error's kind only:
//! an error's text can quote what it failed on.

use crate::db::sqlite::key_vault::Witness;
use crate::error::Result;
use crate::messaging::sealed;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const PROVIDER_PREFIX: &str = "provider:";
pub const MODEL_PREFIX: &str = "model:";

/// The key shared by every model of a provider kind.
pub fn provider_secret_name(kind: &str) -> String {
    format!("{}{}", PROVIDER_PREFIX, kind)
}

/// A model's own key, which wins over the shared one.
pub fn model_secret_name(id: i64) -> String {
    format!("{}{}", MODEL_PREFIX, id)
}

fn aad(name: &str) -> Aad {
    Aad::new("ag_secret", "ciphertext", name)
}

/// A display-safe identifier: the last four characters and the first twelve
/// hex digits of the SHA-256 (`...abcd sha256:0123456789ab`), or `...????`
/// for a value of eight characters or fewer, exactly as the web shows it.
pub fn fingerprint(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "...????".into();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    let digest = hex::encode(Sha256::digest(value.as_bytes()));
    format!("...{} sha256:{}", tail, &digest[..12])
}

/// One stored secret, described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretInfo {
    pub name: String,
    pub fingerprint: String,
    pub has_value: bool,
    /// When it was last handed to a provider (stored UTC text).
    pub last_used_at: Option<String>,
}

impl SecretInfo {
    /// The web's `secret_to_dict`.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "fingerprint": self.fingerprint,
            "has_value": self.has_value,
            "last_used_at": self
                .last_used_at
                .as_deref()
                .map(crate::agent::conversations::store::iso)
                .unwrap_or(Value::Null),
        })
    }
}

fn info_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<SecretInfo> {
    let ciphertext: String = r.get(1)?;
    Ok(SecretInfo {
        name: r.get(0)?,
        fingerprint: r.get(2)?,
        has_value: !ciphertext.is_empty(),
        last_used_at: r.get(3)?,
    })
}

/// Store or replace a secret, writing nothing when it holds the same value
/// already (compared after decrypting: sealing is not deterministic). A
/// replaced key has never been used, so its last-used time is cleared.
/// Errors are the web's sentences.
pub fn set(
    conn: &Connection,
    security: &SecurityManager,
    name: &str,
    value: &str,
) -> std::result::Result<(), &'static str> {
    if name.is_empty() {
        return Err("A secret name is required");
    }
    if value.is_empty() {
        return Err("A secret value is required");
    }
    if get(conn, security, name).is_some_and(|stored| stored.expose() == value) {
        return Ok(());
    }
    let Ok(sealed) = sealed::seal(security, &aad(name), value) else {
        tracing::error!("Could not encrypt Agent secret {}", name);
        return Err("Could not encrypt the secret");
    };
    conn.execute(
        "INSERT INTO ag_secret (name, ciphertext, fingerprint, last_used_at)
         VALUES (?1, ?2, ?3, NULL)
         ON CONFLICT(name) DO UPDATE SET ciphertext = excluded.ciphertext,
             fingerprint = excluded.fingerprint, last_used_at = NULL",
        params![name, sealed, fingerprint(value)],
    )
    .map(|_| ())
    .map_err(|e| {
        tracing::error!(
            "Could not store Agent secret {}: {}",
            name,
            crate::agent::models::error_kind(&e)
        );
        "Could not store the secret"
    })
}

/// A secret's plaintext, or `None` when none is stored or it cannot be
/// opened (keys locked, a row sealed by keys a fresh start replaced).
/// A read, never a use: [`mark_used`] records a use.
pub fn get(conn: &Connection, security: &SecurityManager, name: &str) -> Option<Secret> {
    let stored: Option<String> = match conn
        .query_row(
            "SELECT ciphertext FROM ag_secret WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                "Could not read Agent secret {}: {}",
                name,
                crate::agent::models::error_kind(&e)
            );
            return None;
        }
    };
    let stored = stored.filter(|s| !s.is_empty())?;
    match sealed::open(security, &aad(name), &stored) {
        Ok(secret) => Some(secret),
        Err(_) => {
            tracing::error!("Could not open Agent secret {}", name);
            None
        }
    }
}

/// The stored secrets among `names`, described, in one query.
pub fn describe(conn: &Connection, names: &[String]) -> Result<HashMap<String, SecretInfo>> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    let marks = vec!["?"; names.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT name, ciphertext, fingerprint, last_used_at FROM ag_secret WHERE name IN ({})",
        marks
    ))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(names.iter()), info_of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(|i| (i.name.clone(), i)).collect())
}

/// Every stored secret, described, by name (the web's `list_secrets`).
pub fn list(conn: &Connection) -> Result<Vec<SecretInfo>> {
    let mut stmt = conn.prepare(
        "SELECT name, ciphertext, fingerprint, last_used_at FROM ag_secret ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], info_of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Record that a secret was handed to a provider. Best effort: a failure
/// is logged and costs nothing else.
pub fn mark_used(conn: &Connection, name: &str, now: chrono::DateTime<chrono::Utc>) {
    if let Err(e) = conn.execute(
        "UPDATE ag_secret SET last_used_at = ?1 WHERE name = ?2",
        params![crate::agent::conversations::store::stamp(now), name],
    ) {
        tracing::warn!(
            "Could not mark Agent secret {} as used: {}",
            name,
            crate::agent::models::error_kind(&e)
        );
    }
}

/// Remove one secret; true when a row went.
pub fn delete(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM ag_secret WHERE name = ?1", params![name])? > 0)
}

/// The first sealed secret, as proof of the data key
/// (`key_vault::witness`).
pub fn witness(conn: &Connection) -> Result<Option<Witness>> {
    let first: Option<(String, String)> = conn
        .query_row(
            "SELECT name, ciphertext FROM ag_secret WHERE ciphertext <> '' ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(first.and_then(|(name, stored)| {
        sealed::parts(&stored).map(|(nonce, ct)| Witness {
            ciphertext: ct.to_string(),
            nonce: nonce.to_string(),
            aad: aad(&name),
        })
    }))
}

/// A fresh start or an account reset: every secret goes with the key that
/// sealed it. A model that ran on one of them (or on the ChatGPT sign-in)
/// can no longer pass its test, so its test result and default status go
/// too, and the setup gate asks for a key again rather than letting the
/// first message fail. A keyless model (a local server) keeps both.
pub fn wipe(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "UPDATE ag_provider_model
            SET last_tested_at = NULL, last_test_ok = NULL, last_test_error = NULL,
                is_default = 0
          WHERE model_name LIKE 'chatgpt/%'
             OR EXISTS (SELECT 1 FROM ag_secret s
                         WHERE s.name = 'model:' || ag_provider_model.id
                            OR s.name = 'provider:' || ag_provider_model.provider_kind);
         DELETE FROM ag_secret;",
    )?;
    Ok(())
}
