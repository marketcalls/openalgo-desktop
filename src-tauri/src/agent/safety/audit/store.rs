//! `ag_audit` in `logs.db` (logs migration `014_agent_audit`): the web's
//! `AgAudit` table (`database/agent_db.py`), append-only.
//!
//! No foreign key: a trade record outlives the conversation it was typed
//! into, so deleting a conversation leaves these rows where they are.
//! Desktop addition (D-009): `attempt_id`, the attempt row a result row
//! answers, so two calls of one tool in one run can be told apart.

use crate::agent::conversations::store::iso;
use crate::error::Result;
use rusqlite::{params, Connection, Row};
use serde_json::{json, Value};

/// The migration's name in `logs.db`'s `migrations` table.
pub const MIGRATION: &str = "014_agent_audit";

/// Rows a read returns when it names no limit (the web's `list_audit`).
pub const DEFAULT_LIMIT: i64 = 200;
/// The most rows one read returns.
pub const MAX_LIMIT: i64 = 1000;

/// Logs migration `014_agent_audit` (idempotent; runs on every open).
pub fn migrate(conn: &Connection) -> Result<()> {
    let applied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migrations WHERE name = ?1)",
        [MIGRATION],
        |r| r.get(0),
    )?;
    if applied {
        return Ok(());
    }
    let tx = crate::db::sqlite::write_transaction(conn)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ag_audit (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            phase TEXT NOT NULL,
            tool TEXT NOT NULL,
            conversation_id INTEGER,
            run_id TEXT,
            attempt_id INTEGER,
            args TEXT,
            risk_verdict TEXT,
            ok INTEGER,
            response TEXT,
            order_ids TEXT
        );
        CREATE INDEX IF NOT EXISTS ix_ag_audit_conversation_ts ON ag_audit (conversation_id, ts);
        CREATE INDEX IF NOT EXISTS ix_ag_audit_run_ts ON ag_audit (run_id, ts);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO migrations (name) VALUES (?1)",
        [MIGRATION],
    )?;
    tx.commit()?;
    Ok(())
}

/// One row as written (already redacted and bounded by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct NewRow {
    pub ts: String,
    pub phase: &'static str,
    pub tool: String,
    pub conversation_id: Option<i64>,
    pub run_id: Option<String>,
    pub attempt_id: Option<i64>,
    pub args: Option<Value>,
    pub risk_verdict: Option<String>,
    pub ok: Option<bool>,
    pub response: Option<Value>,
    pub order_ids: Option<Vec<String>>,
}

/// Append one row; its id.
pub fn insert(conn: &Connection, row: &NewRow) -> Result<i64> {
    let text = |v: &Option<Value>| v.as_ref().map(Value::to_string);
    conn.execute(
        "INSERT INTO ag_audit (ts, phase, tool, conversation_id, run_id, attempt_id, args,
                               risk_verdict, ok, response, order_ids)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            row.ts,
            row.phase,
            row.tool,
            row.conversation_id,
            row.run_id,
            row.attempt_id,
            text(&row.args),
            row.risk_verdict,
            row.ok,
            text(&row.response),
            row.order_ids.as_ref().map(|ids| json!(ids).to_string()),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// What a read narrows to (the web's `list_audit` filters).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    pub conversation_id: Option<i64>,
    pub run_id: Option<String>,
    pub tool: Option<String>,
    pub limit: i64,
}

/// The trail, newest first, as the web's `audit_to_dict` renders a row
/// (plus `attempt_id`).
pub fn list(conn: &Connection, f: &Filter) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "SELECT id, ts, phase, tool, conversation_id, run_id, attempt_id, args, risk_verdict,
                ok, response, order_ids
         FROM ag_audit
         WHERE (?1 IS NULL OR conversation_id = ?1)
           AND (?2 IS NULL OR run_id = ?2)
           AND (?3 IS NULL OR tool = ?3)
         ORDER BY ts DESC, id DESC
         LIMIT ?4",
    )?;
    let rows = st
        .query_map(
            params![
                f.conversation_id,
                f.run_id,
                f.tool,
                f.limit.clamp(1, MAX_LIMIT)
            ],
            to_dict,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn to_dict(r: &Row<'_>) -> rusqlite::Result<Value> {
    let parsed = |i: usize| -> rusqlite::Result<Value> {
        Ok(r.get::<_, Option<String>>(i)?
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null))
    };
    let ts: String = r.get(1)?;
    let order_ids = match parsed(11)? {
        Value::Null => json!([]),
        ids => ids,
    };
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "ts": iso(&ts),
        "phase": r.get::<_, String>(2)?,
        "tool": r.get::<_, String>(3)?,
        "conversation_id": r.get::<_, Option<i64>>(4)?,
        "run_id": r.get::<_, Option<String>>(5)?,
        "attempt_id": r.get::<_, Option<i64>>(6)?,
        "args": parsed(7)?,
        "risk_verdict": r.get::<_, Option<String>>(8)?,
        "ok": r.get::<_, Option<bool>>(9)?,
        "response": parsed(10)?,
        "order_ids": order_ids,
    }))
}
