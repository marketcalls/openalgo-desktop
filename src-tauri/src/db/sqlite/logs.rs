//! Logs database (`logs.db`), isolated from the main database like the web's
//! `logs.db`. Holds `order_logs` (live API calls) and `analyzer_logs`
//! (sandbox mode API calls) with the web's column names.
//!
//! Request bodies are stored without the `apikey` field.

use crate::error::Result;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

pub struct LogsDb {
    pool: Pool<SqliteConnectionManager>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS migrations (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS order_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_type TEXT NOT NULL,
    request_data TEXT NOT NULL,
    response_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_order_logs_created ON order_logs(created_at);
CREATE TABLE IF NOT EXISTS analyzer_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    api_type TEXT NOT NULL,
    request_data TEXT NOT NULL,
    response_data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
CREATE INDEX IF NOT EXISTS idx_analyzer_logs_created ON analyzer_logs(created_at);
";

/// Remove `apikey` (and anything else credential-shaped) from a request body.
pub fn redact(mut v: Value) -> Value {
    if let Value::Object(map) = &mut v {
        for k in [
            "apikey",
            "api_key",
            "password",
            "totp",
            "auth_code",
            "request_token",
        ] {
            map.remove(k);
        }
    }
    v
}

impl LogsDb {
    pub fn new(path: &Path) -> Result<Self> {
        let pool = super::open_pool(path, 4, super::Durability::LOGS)?;
        pool.get()?.execute_batch(SCHEMA)?;
        super::monitor::migrate(&*pool.get()?)?;
        crate::mcp::store::migrate_audit(&*pool.get()?)?;
        Ok(Self { pool })
    }

    /// Check out a pooled connection (monitoring tables). Drop it before any
    /// network await.
    pub fn conn(&self) -> Result<super::DbConn> {
        Ok(self.pool.get()?)
    }

    /// Open and idle pooled connections (health monitor).
    pub fn pool_state(&self) -> (u32, u32) {
        let st = self.pool.state();
        (st.connections, st.idle_connections)
    }

    /// Copy sandbox logs written by earlier builds into the main database, once.
    pub fn import_from_main(&self, main: &Connection) -> Result<()> {
        let conn = self.pool.get()?;
        super::monitor::import_from_main(&conn, main)?;
        let done: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM migrations WHERE name = '001_import_main_analyzer_logs')",
            [],
            |r| r.get(0),
        )?;
        if done {
            return Ok(());
        }
        let mut stmt = main.prepare(
            "SELECT api_type, request_data, response_data, created_at FROM analyzer_logs ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        conn.execute_batch("BEGIN")?;
        for (t, req, resp, at) in rows {
            let req = serde_json::from_str::<Value>(&req)
                .map(|v| redact(v).to_string())
                .unwrap_or(req);
            conn.execute(
                "INSERT INTO analyzer_logs (api_type, request_data, response_data, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![t, req, resp, at],
            )?;
        }
        conn.execute(
            "INSERT INTO migrations (name) VALUES ('001_import_main_analyzer_logs')",
            [],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn insert_order_log(
        &self,
        api_type: &str,
        request: &Value,
        response: &Value,
    ) -> Result<()> {
        self.pool.get()?.execute(
            "INSERT INTO order_logs (api_type, request_data, response_data) VALUES (?1, ?2, ?3)",
            params![
                api_type,
                redact(request.clone()).to_string(),
                response.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn insert_analyzer_log(
        &self,
        api_type: &str,
        request: &Value,
        response: &Value,
    ) -> Result<()> {
        self.pool.get()?.execute(
            "INSERT INTO analyzer_logs (api_type, request_data, response_data) VALUES (?1, ?2, ?3)",
            params![
                api_type,
                redact(request.clone()).to_string(),
                response.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn count_analyzer_logs(&self) -> Result<i64> {
        Ok(self
            .pool
            .get()?
            .query_row("SELECT COUNT(*) FROM analyzer_logs", [], |r| r.get(0))?)
    }

    pub fn count_order_logs(&self) -> Result<i64> {
        Ok(self
            .pool
            .get()?
            .query_row("SELECT COUNT(*) FROM order_logs", [], |r| r.get(0))?)
    }

    /// Latest order log rows (newest first), for tests and the logs page.
    pub fn recent_order_logs(&self, limit: i64) -> Result<Vec<(String, String, String)>> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT api_type, request_data, response_data FROM order_logs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn logs_never_store_the_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = LogsDb::new(&dir.path().join("logs.db")).unwrap();
        db.insert_order_log(
            "placeorder",
            &json!({"apikey": "secret-key", "symbol": "SBIN"}),
            &json!({"status": "success"}),
        )
        .unwrap();
        let rows = db.recent_order_logs(1).unwrap();
        assert!(!rows[0].1.contains("secret-key"));
        assert!(rows[0].1.contains("SBIN"));
        assert_eq!(db.count_order_logs().unwrap(), 1);
    }
}
