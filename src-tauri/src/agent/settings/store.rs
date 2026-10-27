//! `ag_setting` rows (migration `098_agent_config`): a key and its text, the
//! web's `agent_db.get_all_settings` and `set_setting`.
//!
//! No cache: the rows are a few dozen, read by primary key order in one
//! statement, and the safety switches must be read fresh anyway (the web
//! caches and has to invalidate on every write path, failures included).

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

/// Every stored row; a row holding NULL reads as absent (its default).
pub fn read_all(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM ag_setting WHERE value IS NOT NULL")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}

/// Write several rows in one transaction, all or none. A row that already
/// holds its text is not rewritten (the web's no-op rule: `openalgo.db` is
/// shared, and an unchanged save must not take the write lock per key).
pub fn write_all(conn: &Connection, rows: &[(&str, String)]) -> Result<()> {
    let tx = crate::db::sqlite::write_transaction(conn)?;
    for (key, text) in rows {
        let stored: Option<Option<String>> = tx
            .query_row(
                "SELECT value FROM ag_setting WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        if stored.as_ref().and_then(|v| v.as_deref()) == Some(text.as_str()) {
            continue;
        }
        tx.execute(
            "INSERT INTO ag_setting (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, text],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Remove a row so its default applies again (the web's `delete_setting`,
/// which only its tests call).
#[cfg(test)]
pub fn delete(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM ag_setting WHERE key = ?1", params![key])? > 0)
}
