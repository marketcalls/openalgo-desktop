//! Migration `098_agent_config`: the model registry, its keys and the
//! Agent's settings (the web's `ag_provider_model`, `ag_secret` and
//! `ag_setting` in `database/agent_db.py`, created by its
//! `upgrade/migrate_agent.py`, with the voice rows its
//! `migrate_agent_voice.py` seeds).
//!
//! Column for column as the web, with two desktop differences: timestamps
//! are UTC text (`YYYY-MM-DD HH:MM:SS.ffffff`, as migration 097), and
//! `ag_secret.ciphertext` holds the KEY-01 vault's sealed value
//! (`v1:<nonce>:<ciphertext>`, bound to the secret's name) instead of a
//! Fernet token. The duplicate check stays in code: SQLite treats NULLs as
//! distinct, so the UNIQUE constraint misses a model with no base URL.

use crate::agent::settings::parse::{default_of, serialise};
use crate::agent::settings::schema::{field_of, VOICE_KEYS};
use crate::error::Result;
use rusqlite::{params, Connection};

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ag_provider_model (
            id INTEGER PRIMARY KEY,
            provider_kind TEXT NOT NULL,
            model_name TEXT NOT NULL,
            display_name TEXT NOT NULL,
            base_url TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            is_default INTEGER NOT NULL DEFAULT 0,
            supports_reasoning INTEGER NOT NULL DEFAULT 0,
            default_reasoning_effort TEXT NOT NULL DEFAULT 'off',
            supports_vision INTEGER NOT NULL DEFAULT 0,
            tools_unreliable INTEGER NOT NULL DEFAULT 0,
            last_tested_at TEXT,
            last_test_ok INTEGER,
            last_test_error TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            CONSTRAINT uq_ag_provider_model_identity
                UNIQUE (provider_kind, model_name, base_url)
        );
        CREATE INDEX IF NOT EXISTS ix_ag_provider_model_enabled
            ON ag_provider_model (enabled);
        CREATE TABLE IF NOT EXISTS ag_secret (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            ciphertext TEXT NOT NULL,
            fingerprint TEXT NOT NULL DEFAULT '',
            last_used_at TEXT
        );
        CREATE INDEX IF NOT EXISTS ix_ag_secret_name ON ag_secret (name);
        CREATE TABLE IF NOT EXISTS ag_setting (
            key TEXT PRIMARY KEY,
            value TEXT
        );",
    )?;
    seed_voice(conn)
}

/// The voice block as rows, each only where no row exists: a row the
/// trader wrote, even one holding nothing, is theirs (the web seeds
/// "guarded on absence of the row, never on its value").
fn seed_voice(conn: &Connection) -> Result<()> {
    // The voice defaults place nothing in the data folder, so any folder
    // renders them.
    let nowhere = std::path::Path::new("");
    for key in VOICE_KEYS {
        if let Some(field) = field_of(key) {
            conn.execute(
                "INSERT OR IGNORE INTO ag_setting (key, value) VALUES (?1, ?2)",
                params![key, serialise(&default_of(field, nowhere))],
            )?;
        }
    }
    Ok(())
}
