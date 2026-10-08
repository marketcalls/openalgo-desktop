//! MCP persistence: access tokens and settings in the main database
//! (migration `077_mcp`), and the audit trail in `logs.db` (bounded to the
//! newest [`AUDIT_MAX_ROWS`] rows, like the web's trimmed `log/mcp.jsonl`).
//!
//! A token is `oamcp_` plus 64 hex characters (256 random bits). Only its
//! SHA-256 is stored: the token is shown once, when it is created. A fast
//! hash is enough for a random 256-bit secret (no dictionary to try), and it
//! keeps verification independent of the keychain.

use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TOKEN_PREFIX: &str = "oamcp_";

/// Audit rows kept (web `_AUDIT_MAX_LINES`).
pub const AUDIT_MAX_ROWS: i64 = 5000;

/// Main database: tokens and settings.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mcp_tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            scope TEXT NOT NULL CHECK (scope IN ('read', 'read_write')),
            token_hash TEXT NOT NULL UNIQUE,
            token_prefix TEXT NOT NULL,
            created_at TEXT NOT NULL,
            last_used_at TEXT,
            revoked_at TEXT
        );
        CREATE TABLE IF NOT EXISTS mcp_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            http_enabled INTEGER NOT NULL DEFAULT 0,
            public_url TEXT NOT NULL DEFAULT '',
            require_approval INTEGER NOT NULL DEFAULT 0,
            write_scope_enabled INTEGER NOT NULL DEFAULT 1
        );
        INSERT OR IGNORE INTO mcp_settings (id) VALUES (1);",
    )?;
    Ok(())
}

/// `logs.db`: the audit trail (idempotent; run on every open).
pub fn migrate_audit(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mcp_audit (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            jti TEXT,
            client_id TEXT NOT NULL DEFAULT '',
            tool TEXT NOT NULL DEFAULT '',
            scope TEXT NOT NULL DEFAULT '',
            params_hash TEXT NOT NULL DEFAULT '',
            duration_ms INTEGER NOT NULL DEFAULT 0,
            outcome TEXT NOT NULL,
            request_ip TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_mcp_audit_tool ON mcp_audit(tool);",
    )?;
    Ok(())
}

// ----------------------------------------------------------------------
// Tokens
// ----------------------------------------------------------------------

/// What a token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    Read,
    ReadWrite,
}

impl TokenScope {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenScope::Read => "read",
            TokenScope::ReadWrite => "read_write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(TokenScope::Read),
            "read_write" => Some(TokenScope::ReadWrite),
            _ => None,
        }
    }

    /// The OAuth-style scopes it grants.
    pub fn grants(self) -> Vec<super::Scope> {
        use super::Scope::*;
        match self {
            TokenScope::Read => vec![ReadMarket, ReadAccount],
            TokenScope::ReadWrite => vec![ReadMarket, ReadAccount, WriteOrders],
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenRow {
    pub id: i64,
    pub name: String,
    pub scope: TokenScope,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl TokenRow {
    /// The audit trail's token id (the web logs the JWT `jti`).
    pub fn jti(&self) -> String {
        format!("mcp-token-{}", self.id)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "scope": self.scope.as_str(),
            "token_prefix": self.prefix,
            "created_at": self.created_at,
            "last_used_at": self.last_used_at,
        })
    }
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn generate() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{}{}", TOKEN_PREFIX, hex::encode(b))
}

fn ts(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Create a token; returns the row and the token (shown once).
pub fn create_token(
    conn: &Connection,
    name: &str,
    scope: TokenScope,
    now: DateTime<Utc>,
) -> Result<(TokenRow, String)> {
    let token = generate();
    let prefix: String = token.chars().take(TOKEN_PREFIX.len() + 6).collect();
    conn.execute(
        "INSERT INTO mcp_tokens (name, scope, token_hash, token_prefix, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, scope.as_str(), hash_token(&token), prefix, ts(now)],
    )?;
    let row = TokenRow {
        id: conn.last_insert_rowid(),
        name: name.to_string(),
        scope,
        prefix,
        created_at: ts(now),
        last_used_at: None,
    };
    Ok((row, token))
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<TokenRow> {
    let scope: String = r.get(2)?;
    Ok(TokenRow {
        id: r.get(0)?,
        name: r.get(1)?,
        scope: TokenScope::parse(&scope).unwrap_or(TokenScope::Read),
        prefix: r.get(3)?,
        created_at: r.get(4)?,
        last_used_at: r.get(5)?,
    })
}

/// The live (not revoked) token with this value. Candidates are found by
/// the token's non-secret display prefix, and the stored hash is compared
/// in constant time.
pub fn find_token(conn: &Connection, token: &str) -> Result<Option<TokenRow>> {
    use subtle::ConstantTimeEq;
    if !token.starts_with(TOKEN_PREFIX) || token.len() != TOKEN_PREFIX.len() + 64 {
        return Ok(None);
    }
    let prefix: String = token.chars().take(TOKEN_PREFIX.len() + 6).collect();
    let presented = hash_token(token);
    let mut st = conn.prepare(
        "SELECT id, name, scope, token_prefix, created_at, last_used_at, token_hash FROM mcp_tokens
         WHERE token_prefix = ?1 AND revoked_at IS NULL",
    )?;
    let candidates = st
        .query_map([prefix], |r| Ok((row(r)?, r.get::<_, String>(6)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut found = None;
    for (t, stored) in candidates {
        if bool::from(stored.as_bytes().ct_eq(presented.as_bytes())) {
            found = Some(t);
        }
    }
    Ok(found)
}

/// Whether the token with this id is still live (open event streams check
/// this on every keepalive).
pub fn token_is_live(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM mcp_tokens WHERE id = ?1 AND revoked_at IS NULL",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn touch_token(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "UPDATE mcp_tokens SET last_used_at = ?1 WHERE id = ?2",
        params![ts(now), id],
    )?;
    Ok(())
}

/// Live tokens, newest first.
pub fn list_tokens(conn: &Connection) -> Result<Vec<TokenRow>> {
    let mut st = conn.prepare(
        "SELECT id, name, scope, token_prefix, created_at, last_used_at FROM mcp_tokens
         WHERE revoked_at IS NULL ORDER BY id DESC",
    )?;
    let rows = st
        .query_map([], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Revoke one token; false when no live token has that id.
pub fn revoke_token(conn: &Connection, id: i64, now: DateTime<Utc>) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE mcp_tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        params![ts(now), id],
    )? > 0)
}

/// Revoke every live token; returns how many.
pub fn revoke_all(conn: &Connection, now: DateTime<Utc>) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE mcp_tokens SET revoked_at = ?1 WHERE revoked_at IS NULL",
        [ts(now)],
    )?)
}

// ----------------------------------------------------------------------
// Settings
// ----------------------------------------------------------------------

/// The admin page's settings (web `_mcp_settings_payload`). `http_enabled`
/// is "Remote MCP": `/mcp` reachable from other machines. Loopback clients
/// (the stdio bridge, local HTTP clients) are always served.
/// `write_scope_enabled` off is the kill switch's state: no tool that places,
/// modifies or cancels orders runs, whatever the token's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub http_enabled: bool,
    pub public_url: String,
    pub require_approval: bool,
    pub write_scope_enabled: bool,
}

impl Settings {
    pub fn to_json(&self) -> Value {
        json!({
            "http_enabled": self.http_enabled,
            "public_url": self.public_url,
            "mcp_url": if self.public_url.is_empty() { String::new() } else { format!("{}/mcp", self.public_url) },
            "require_approval": self.require_approval,
            "write_scope_enabled": self.write_scope_enabled,
        })
    }
}

pub fn settings(conn: &Connection) -> Result<Settings> {
    Ok(conn
        .query_row(
            "SELECT http_enabled, public_url, require_approval, write_scope_enabled FROM mcp_settings WHERE id = 1",
            [],
            |r| {
                Ok(Settings {
                    http_enabled: r.get::<_, i64>(0)? != 0,
                    public_url: r.get(1)?,
                    require_approval: r.get::<_, i64>(2)? != 0,
                    write_scope_enabled: r.get::<_, i64>(3)? != 0,
                })
            },
        )
        .optional()?
        .unwrap_or(Settings {
            http_enabled: false,
            public_url: String::new(),
            require_approval: false,
            write_scope_enabled: true,
        }))
}

pub fn save_settings(conn: &Connection, s: &Settings) -> Result<()> {
    conn.execute(
        "INSERT INTO mcp_settings (id, http_enabled, public_url, require_approval, write_scope_enabled)
         VALUES (1, ?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET http_enabled = ?1, public_url = ?2,
             require_approval = ?3, write_scope_enabled = ?4",
        params![s.http_enabled, s.public_url, s.require_approval, s.write_scope_enabled],
    )?;
    Ok(())
}

// ----------------------------------------------------------------------
// Audit
// ----------------------------------------------------------------------

/// One audited call (web `_audit_log` entry; never the arguments, only
/// their hash).
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub ts: String,
    pub jti: Option<String>,
    pub client_id: String,
    pub tool: String,
    pub scope: String,
    pub params_hash: String,
    pub duration_ms: i64,
    pub outcome: String,
    pub request_ip: String,
}

impl AuditEntry {
    pub fn to_json(&self) -> Value {
        json!({
            "ts": self.ts,
            "jti": self.jti,
            "client_id": self.client_id,
            "tool": self.tool,
            "scope": self.scope,
            "params_hash": self.params_hash,
            "duration_ms": self.duration_ms,
            "outcome": self.outcome,
            "request_ip": self.request_ip,
        })
    }
}

/// Append one entry and trim to [`AUDIT_MAX_ROWS`].
pub fn audit(conn: &Connection, e: &AuditEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO mcp_audit (ts, jti, client_id, tool, scope, params_hash, duration_ms, outcome, request_ip)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            e.ts,
            e.jti,
            e.client_id,
            e.tool,
            e.scope,
            e.params_hash,
            e.duration_ms,
            e.outcome,
            e.request_ip
        ],
    )?;
    let id = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM mcp_audit WHERE id <= ?1",
        [id - AUDIT_MAX_ROWS],
    )?;
    Ok(())
}

/// Filters of `GET /admin/api/mcp/audit`.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub limit: i64,
    pub tool: String,
    pub scope: String,
    pub outcome: String,
}

/// Newest matching entries, returned oldest first, with the scan count and
/// the number of entries kept (web `api_mcp_audit`).
pub fn audit_tail(conn: &Connection, q: &AuditQuery) -> Result<(Vec<AuditEntry>, i64, i64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM mcp_audit", [], |r| r.get(0))?;
    let mut st = conn.prepare(
        "SELECT id, ts, jti, client_id, tool, scope, params_hash, duration_ms, outcome, request_ip
         FROM mcp_audit
         WHERE (?1 = '' OR instr(lower(tool), lower(?1)) > 0)
           AND (?2 = '' OR scope = ?2)
           AND (?3 = '' OR outcome = ?3)
         ORDER BY id DESC LIMIT ?4",
    )?;
    let rows = st
        .query_map(params![q.tool, q.scope, q.outcome, q.limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                AuditEntry {
                    ts: r.get(1)?,
                    jti: r.get(2)?,
                    client_id: r.get(3)?,
                    tool: r.get(4)?,
                    scope: r.get(5)?,
                    params_hash: r.get(6)?,
                    duration_ms: r.get(7)?,
                    outcome: r.get(8)?,
                    request_ip: r.get(9)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let scanned = match rows.last() {
        Some((oldest, _)) if rows.len() as i64 == q.limit => conn.query_row(
            "SELECT COUNT(*) FROM mcp_audit WHERE id >= ?1",
            [oldest],
            |r| r.get(0),
        )?,
        _ => total,
    };
    let mut out: Vec<AuditEntry> = rows.into_iter().map(|(_, e)| e).collect();
    out.reverse();
    Ok((out, scanned, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        migrate_audit(&c).unwrap();
        migrate_audit(&c).unwrap();
        c
    }

    #[test]
    fn tokens_are_stored_hashed_and_revocable() {
        let c = db();
        let now = Utc::now();
        let (row, token) = create_token(&c, "Claude", TokenScope::Read, now).unwrap();
        assert!(token.starts_with(TOKEN_PREFIX) && token.len() == TOKEN_PREFIX.len() + 64);
        let stored: String = c
            .query_row("SELECT token_hash FROM mcp_tokens", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, token);
        assert_eq!(find_token(&c, &token).unwrap().unwrap().id, row.id);
        assert!(find_token(&c, "oamcp_nope").unwrap().is_none());
        assert!(revoke_token(&c, row.id, now).unwrap());
        assert!(find_token(&c, &token).unwrap().is_none());
        assert!(!revoke_token(&c, row.id, now).unwrap());
        create_token(&c, "a", TokenScope::ReadWrite, now).unwrap();
        create_token(&c, "b", TokenScope::Read, now).unwrap();
        assert_eq!(revoke_all(&c, now).unwrap(), 2);
        assert!(list_tokens(&c).unwrap().is_empty());
    }

    #[test]
    fn settings_default_and_survive_a_second_migration() {
        let c = db();
        let s = settings(&c).unwrap();
        assert!(!s.http_enabled && s.write_scope_enabled);
        save_settings(
            &c,
            &Settings {
                write_scope_enabled: false,
                ..s
            },
        )
        .unwrap();
        migrate(&c).unwrap();
        assert!(!settings(&c).unwrap().write_scope_enabled);
    }

    #[test]
    fn audit_is_bounded_and_filtered() {
        let c = db();
        let e = |tool: &str, outcome: &str| AuditEntry {
            ts: "t".into(),
            jti: Some("mcp-token-1".into()),
            client_id: "c".into(),
            tool: tool.into(),
            scope: "read:market".into(),
            params_hash: "h".into(),
            duration_ms: 1,
            outcome: outcome.into(),
            request_ip: "127.0.0.1".into(),
        };
        for k in 0..(AUDIT_MAX_ROWS + 10) {
            audit(
                &c,
                &e(
                    if k % 2 == 0 { "get_quote" } else { "get_funds" },
                    "success",
                ),
            )
            .unwrap();
        }
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM mcp_audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, AUDIT_MAX_ROWS);
        let (rows, scanned, total) = audit_tail(
            &c,
            &AuditQuery {
                limit: 3,
                tool: "QUOTE".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.tool == "get_quote"));
        assert_eq!(total, AUDIT_MAX_ROWS);
        assert!((3..=6).contains(&scanned));
    }
}
