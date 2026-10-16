//! Chart preferences saved through `/api/v1/chart` (web
//! `database/chart_prefs_db.py`): one text value per user and key.
//!
//! The table is `chart_prefs`, not the web's `chart_preferences`: that name
//! already holds an older per-symbol table here, which migration
//! `010_chart_preferences` created and nothing reads; it is left alone.

use crate::error::Result;
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

/// Migration `087_chart_prefs`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chart_prefs (
            user_id TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (user_id, key)
         );",
    )?;
    Ok(())
}

/// Every saved preference of `user`, by key.
pub fn read_all(conn: &Connection, user: &str) -> Result<BTreeMap<String, Option<String>>> {
    let mut stmt = conn.prepare("SELECT key, value FROM chart_prefs WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Save `prefs` for `user` in one transaction: a key already saved takes the
/// new value, a new key is added, other keys are kept.
pub fn save(conn: &mut Connection, user: &str, prefs: &[(String, Option<String>)]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO chart_prefs (user_id, key, value, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(user_id, key) DO UPDATE SET
                value = excluded.value, updated_at = excluded.updated_at",
        )?;
        for (key, value) in prefs {
            stmt.execute(params![user, key, value])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_kept_per_key_and_replaced_by_a_later_save() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let first = vec![
            ("tv_theme".to_string(), Some("dark".to_string())),
            ("layout".to_string(), None),
        ];
        save(&mut conn, "trader", &first).unwrap();
        save(
            &mut conn,
            "trader",
            &[("tv_theme".to_string(), Some("light".to_string()))],
        )
        .unwrap();
        save(
            &mut conn,
            "other",
            &[("x".to_string(), Some("1".to_string()))],
        )
        .unwrap();
        let got = read_all(&conn, "trader").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["tv_theme"].as_deref(), Some("light"));
        assert_eq!(got["layout"], None);
    }
}
