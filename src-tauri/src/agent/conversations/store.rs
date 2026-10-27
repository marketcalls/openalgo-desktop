//! The Agent's tables in `openalgo.db` (migration `097_agent_core`) and
//! their reads and writes (the web's `database/agent_db.py` conversation
//! and message parts, plus the run history agno kept in its own store).
//!
//! - `ag_conversation` and `ag_message`: what the sidebar lists and the
//!   page renders, column for column as the web.
//! - `ag_run`: each run's own messages as the model saw them (the user's
//!   input, the answers, the tool calls and results), replayed into later
//!   turns (agno's session store on the web).
//! - `ag_paused_run`: a run waiting for the trader's approval: its pending
//!   calls, its expiry, and the state a resume claims under one statement.
//!
//! Timestamps are UTC, stored `YYYY-MM-DD HH:MM:SS.ffffff` (sortable) and
//! shown with an explicit offset (D-039).

use crate::error::Result;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";

pub(crate) fn stamp(now: DateTime<Utc>) -> String {
    now.format(STORED).to_string()
}

/// A stored time as the web's `_iso`: ISO 8601 with `+00:00`, microseconds
/// only when there are some.
pub(crate) fn iso(stored: &str) -> Value {
    match NaiveDateTime::parse_from_str(stored, STORED) {
        Ok(t) if t.and_utc().timestamp_subsec_micros() == 0 => {
            Value::String(t.format("%Y-%m-%dT%H:%M:%S+00:00").to_string())
        }
        Ok(t) => Value::String(t.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()),
        Err(_) => Value::Null,
    }
}

/// Migration `097_agent_core`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ag_conversation (
            id INTEGER PRIMARY KEY,
            user_id TEXT NOT NULL,
            title TEXT,
            surface TEXT NOT NULL DEFAULT 'chat',
            agno_session_id TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_ag_conversation_user_updated
            ON ag_conversation (user_id, updated_at);
        CREATE INDEX IF NOT EXISTS ix_ag_conversation_agno_session_id
            ON ag_conversation (agno_session_id);
        CREATE TABLE IF NOT EXISTS ag_message (
            id INTEGER PRIMARY KEY,
            conversation_id INTEGER NOT NULL
                REFERENCES ag_conversation (id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            tools TEXT,
            notices TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_ag_message_conversation_created
            ON ag_message (conversation_id, created_at);
        CREATE TABLE IF NOT EXISTS ag_run (
            run_id TEXT PRIMARY KEY,
            conversation_id INTEGER NOT NULL
                REFERENCES ag_conversation (id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            status TEXT NOT NULL,
            messages TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_ag_run_session_created
            ON ag_run (session_id, created_at);
        CREATE TABLE IF NOT EXISTS ag_paused_run (
            run_id TEXT PRIMARY KEY,
            conversation_id INTEGER NOT NULL
                REFERENCES ag_conversation (id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            state TEXT NOT NULL,
            pending TEXT NOT NULL,
            paused_at TEXT NOT NULL,
            expires_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

/// One conversation row.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub id: i64,
    pub user_id: String,
    pub title: Option<String>,
    pub surface: String,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Conversation {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Conversation> {
        Ok(Conversation {
            id: r.get(0)?,
            user_id: r.get(1)?,
            title: r.get(2)?,
            surface: r.get(3)?,
            session_id: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    }

    /// The web's `conversation_to_dict`.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "user_id": self.user_id,
            "title": self.title,
            "surface": self.surface,
            "agno_session_id": self.session_id,
            "created_at": iso(&self.created_at),
            "updated_at": iso(&self.updated_at),
        })
    }
}

const CONVERSATION_COLUMNS: &str =
    "id, user_id, title, surface, agno_session_id, created_at, updated_at";

/// One message row.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub tools: Value,
    pub notices: Value,
    pub created_at: String,
}

impl Message {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Message> {
        let json_list = |s: Option<String>| -> Value {
            s.and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .filter(|v| !v.is_null())
                .unwrap_or_else(|| Value::Array(Vec::new()))
        };
        Ok(Message {
            id: r.get(0)?,
            conversation_id: r.get(1)?,
            role: r.get(2)?,
            content: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            tools: json_list(r.get(4)?),
            notices: json_list(r.get(5)?),
            created_at: r.get(6)?,
        })
    }

    /// The web's `message_to_dict`.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "conversation_id": self.conversation_id,
            "role": self.role,
            "content": self.content,
            "tools": self.tools,
            "notices": self.notices,
            "created_at": iso(&self.created_at),
        })
    }
}

const MESSAGE_COLUMNS: &str = "id, conversation_id, role, content, tools, notices, created_at";

pub fn create_conversation(
    conn: &Connection,
    user: &str,
    title: Option<&str>,
    surface: &str,
    now: DateTime<Utc>,
) -> Result<Conversation> {
    let ts = stamp(now);
    conn.execute(
        "INSERT INTO ag_conversation (user_id, title, surface, agno_session_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, NULL, ?4, ?4)",
        params![user, title.filter(|t| !t.is_empty()), surface, ts],
    )?;
    let id = conn.last_insert_rowid();
    Ok(Conversation {
        id,
        user_id: user.into(),
        title: title.filter(|t| !t.is_empty()).map(str::to_string),
        surface: surface.into(),
        session_id: None,
        created_at: ts.clone(),
        updated_at: ts,
    })
}

/// One conversation, scoped to its owner (another's reads as absent).
pub fn get_conversation(conn: &Connection, id: i64, user: &str) -> Result<Option<Conversation>> {
    let sql = format!(
        "SELECT {} FROM ag_conversation WHERE id = ?1 AND user_id = ?2",
        CONVERSATION_COLUMNS
    );
    Ok(conn
        .query_row(&sql, params![id, user], Conversation::from_row)
        .optional()?)
}

/// A user's conversations, most recently updated first, on any of
/// `surfaces` (all when empty).
pub fn list_conversations(
    conn: &Connection,
    user: &str,
    surfaces: &[String],
    limit: i64,
) -> Result<Vec<Conversation>> {
    let surface_list = serde_json::to_string(surfaces)?;
    let sql = format!(
        "SELECT {} FROM ag_conversation
         WHERE user_id = ?1
           AND (?2 = '[]' OR surface IN (SELECT value FROM json_each(?2)))
         ORDER BY updated_at DESC, id DESC LIMIT ?3",
        CONVERSATION_COLUMNS
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(params![user, surface_list, limit], Conversation::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Rename a conversation or bind its session; `None` leaves a field alone.
pub fn update_conversation(
    conn: &Connection,
    id: i64,
    user: &str,
    title: Option<&str>,
    session_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Option<Conversation>> {
    let n = conn.execute(
        "UPDATE ag_conversation
         SET title = COALESCE(?3, title),
             agno_session_id = COALESCE(?4, agno_session_id),
             updated_at = ?5
         WHERE id = ?1 AND user_id = ?2",
        params![id, user, title, session_id, stamp(now)],
    )?;
    if n == 0 {
        return Ok(None);
    }
    get_conversation(conn, id, user)
}

/// Delete a conversation with its messages, runs and paused runs; false
/// when it is not the user's.
pub fn delete_conversation(conn: &Connection, id: i64, user: &str) -> Result<bool> {
    let tx = crate::db::sqlite::write_transaction(conn)?;
    let owned: Option<i64> = tx
        .query_row(
            "SELECT id FROM ag_conversation WHERE id = ?1 AND user_id = ?2",
            params![id, user],
            |r| r.get(0),
        )
        .optional()?;
    if owned.is_none() {
        return Ok(false);
    }
    tx.execute("DELETE FROM ag_message WHERE conversation_id = ?1", [id])?;
    tx.execute("DELETE FROM ag_run WHERE conversation_id = ?1", [id])?;
    tx.execute("DELETE FROM ag_paused_run WHERE conversation_id = ?1", [id])?;
    tx.execute("DELETE FROM ag_conversation WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(true)
}

/// Append one message and move its conversation to the top, in one
/// transaction.
pub fn add_message(
    conn: &Connection,
    conversation_id: i64,
    role: &str,
    content: &str,
    tools: Option<&Value>,
    notices: Option<&Value>,
    now: DateTime<Utc>,
) -> Result<Message> {
    let ts = stamp(now);
    let as_text = |v: Option<&Value>| v.map(Value::to_string);
    let tx = crate::db::sqlite::write_transaction(conn)?;
    tx.execute(
        "INSERT INTO ag_message (conversation_id, role, content, tools, notices, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            conversation_id,
            role,
            content,
            as_text(tools),
            as_text(notices),
            ts
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "UPDATE ag_conversation SET updated_at = ?2 WHERE id = ?1",
        params![conversation_id, ts],
    )?;
    tx.commit()?;
    Ok(Message {
        id,
        conversation_id,
        role: role.into(),
        content: content.into(),
        tools: tools.cloned().unwrap_or_else(|| json!([])),
        notices: notices.cloned().unwrap_or_else(|| json!([])),
        created_at: ts,
    })
}

/// A conversation's newest `limit` messages, oldest first.
pub fn list_messages(conn: &Connection, conversation_id: i64, limit: i64) -> Result<Vec<Message>> {
    let sql = format!(
        "SELECT {} FROM ag_message WHERE conversation_id = ?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
        MESSAGE_COLUMNS
    );
    let mut st = conn.prepare(&sql)?;
    let mut rows = st
        .query_map(params![conversation_id, limit], Message::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows)
}

/// The newest message of a role (the resume's original question).
pub fn last_message_of_role(
    conn: &Connection,
    conversation_id: i64,
    role: &str,
) -> Result<Option<Message>> {
    let sql = format!(
        "SELECT {} FROM ag_message WHERE conversation_id = ?1 AND role = ?2
         ORDER BY id DESC LIMIT 1",
        MESSAGE_COLUMNS
    );
    Ok(conn
        .query_row(&sql, params![conversation_id, role], Message::from_row)
        .optional()?)
}

/// Remove one message and everything after it (by id), returning the
/// removed rows, oldest first.
pub fn truncate_from(
    conn: &Connection,
    conversation_id: i64,
    message_id: i64,
) -> Result<Vec<Message>> {
    let tx = crate::db::sqlite::write_transaction(conn)?;
    let removed = {
        let sql = format!(
            "SELECT {} FROM ag_message WHERE conversation_id = ?1 AND id >= ?2 ORDER BY id ASC",
            MESSAGE_COLUMNS
        );
        let mut st = tx.prepare(&sql)?;
        let rows = st
            .query_map(params![conversation_id, message_id], Message::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    tx.execute(
        "DELETE FROM ag_message WHERE conversation_id = ?1 AND id >= ?2",
        params![conversation_id, message_id],
    )?;
    tx.commit()?;
    Ok(removed)
}

/// Start recording a run.
pub fn insert_run(
    conn: &Connection,
    run_id: &str,
    conversation_id: i64,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let ts = stamp(now);
    conn.execute(
        "INSERT INTO ag_run (run_id, conversation_id, session_id, status, messages, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'running', '[]', ?4, ?4)
         ON CONFLICT (run_id) DO UPDATE SET status = 'running', updated_at = ?4",
        params![run_id, conversation_id, session_id, ts],
    )?;
    Ok(())
}

/// Save a run's messages and status (`running`, `paused`, `completed`,
/// `cancelled`, `error`).
pub fn save_run(
    conn: &Connection,
    run_id: &str,
    status: &str,
    messages: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE ag_run SET status = ?2, messages = ?3, updated_at = ?4 WHERE run_id = ?1",
        params![run_id, status, messages, stamp(now)],
    )?;
    Ok(())
}

/// One run's saved messages (a paused run being resumed).
pub fn run_messages(conn: &Connection, run_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT messages FROM ag_run WHERE run_id = ?1",
            [run_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// The messages of the session's last `n` completed runs, oldest first
/// (a paused, cancelled or failed run is not history, as on the web).
pub fn history(conn: &Connection, session_id: &str, n: i64) -> Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT messages FROM ag_run WHERE session_id = ?1 AND status = 'completed'
         ORDER BY created_at DESC, rowid DESC LIMIT ?2",
    )?;
    let mut rows = st
        .query_map(params![session_id, n], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows)
}

/// Forget runs (an edit truncated the turns they answered); how many went.
pub fn delete_runs(conn: &Connection, run_ids: &[String]) -> Result<usize> {
    let mut n = 0;
    for id in run_ids {
        n += conn.execute("DELETE FROM ag_run WHERE run_id = ?1", [id])?;
        conn.execute("DELETE FROM ag_paused_run WHERE run_id = ?1", [id])?;
    }
    Ok(n)
}

/// Runs a crash or quit left `running`: they never finished, so they are
/// not history.
pub fn close_interrupted_runs(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE ag_run SET status = 'error', updated_at = ?1 WHERE status = 'running'",
        [stamp(now)],
    )?)
}

/// A paused run as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct PausedRow {
    pub run_id: String,
    pub conversation_id: i64,
    pub session_id: String,
    pub user_id: String,
    pub pending: String,
    pub paused_at: String,
    pub expires_at: String,
}

/// Record (or record again) a run that paused for approval.
pub fn save_paused(conn: &Connection, p: &PausedRow) -> Result<()> {
    conn.execute(
        "INSERT INTO ag_paused_run (run_id, conversation_id, session_id, user_id, state, pending, paused_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, 'paused', ?5, ?6, ?7)
         ON CONFLICT (run_id) DO UPDATE SET state = 'paused', pending = ?5, paused_at = ?6, expires_at = ?7",
        params![
            p.run_id,
            p.conversation_id,
            p.session_id,
            p.user_id,
            p.pending,
            p.paused_at,
            p.expires_at
        ],
    )?;
    Ok(())
}

/// What a resume found.
#[derive(Debug, Clone, PartialEq)]
pub enum Claim {
    /// This resume holds the run; no other can.
    Claimed(PausedRow),
    /// The approval waited past its expiry.
    Expired,
    /// Another resume already took it, or it finished.
    AlreadyAnswered,
    /// No such paused run for this user.
    NotFound,
}

/// Claim a paused run for one resume, under one statement: the check and
/// the claim cannot be split between two approvals (the web resumed a run
/// approved from two tabs twice).
pub fn claim_paused(
    conn: &Connection,
    run_id: &str,
    user: &str,
    now: DateTime<Utc>,
) -> Result<Claim> {
    let ts = stamp(now);
    let tx = crate::db::sqlite::write_transaction(conn)?;
    let claimed = tx.execute(
        "UPDATE ag_paused_run SET state = 'resuming'
         WHERE run_id = ?1 AND user_id = ?2 AND state = 'paused' AND expires_at > ?3",
        params![run_id, user, ts],
    )?;
    let row = tx
        .query_row(
            "SELECT run_id, conversation_id, session_id, user_id, pending, paused_at, expires_at, state
             FROM ag_paused_run WHERE run_id = ?1 AND user_id = ?2",
            params![run_id, user],
            |r| {
                Ok((
                    PausedRow {
                        run_id: r.get(0)?,
                        conversation_id: r.get(1)?,
                        session_id: r.get(2)?,
                        user_id: r.get(3)?,
                        pending: r.get(4)?,
                        paused_at: r.get(5)?,
                        expires_at: r.get(6)?,
                    },
                    r.get::<_, String>(7)?,
                ))
            },
        )
        .optional()?;
    let claim = if claimed == 1 {
        row.map(|(p, _)| Claim::Claimed(p))
            .unwrap_or(Claim::NotFound)
    } else {
        match row {
            None => Claim::NotFound,
            // Still `paused` but not claimable: its time ran out.
            Some((_, state)) if state == "paused" => {
                tx.execute(
                    "UPDATE ag_paused_run SET state = 'expired' WHERE run_id = ?1",
                    [run_id],
                )?;
                Claim::Expired
            }
            Some((_, state)) if state == "expired" => Claim::Expired,
            Some(_) => Claim::AlreadyAnswered,
        }
    };
    tx.commit()?;
    Ok(claim)
}

/// The resume of a claimed run is over (it finished, failed or paused
/// again, which [`save_paused`] records).
pub fn release_paused(conn: &Connection, run_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE ag_paused_run SET state = 'done' WHERE run_id = ?1 AND state = 'resuming'",
        [run_id],
    )?;
    Ok(())
}

/// Paused runs past their expiry, marked expired; their ids.
pub fn expire_paused(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<String>> {
    let ts = stamp(now);
    let mut st = conn
        .prepare("SELECT run_id FROM ag_paused_run WHERE state = 'paused' AND expires_at <= ?1")?;
    let ids = st
        .query_map([&ts], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in &ids {
        conn.execute(
            "UPDATE ag_paused_run SET state = 'expired' WHERE run_id = ?1 AND state = 'paused'",
            [id],
        )?;
    }
    Ok(ids)
}

#[cfg(test)]
mod tests;
