//! WhatsApp tables with the web's names and columns (`database/whatsapp_db.py`):
//! `whatsapp_config` (the singleton row with the sealed session snapshot),
//! `whatsapp_users`, `whatsapp_command_logs`, `whatsapp_notification_queue`,
//! `whatsapp_user_preferences`, in `openalgo.db`.
//!
//! `session_blob` holds `v1:<nonce>:<ciphertext>` of the base64 session
//! snapshot, bound to its row. A refresh or a logout clear is conditional on
//! the stored ciphertext, as on the web: AES-GCM output differs on every
//! encryption, so equal ciphertext means nobody wrote the column since.

use crate::error::{AppError, Result};
use crate::messaging::format::http_date;
use crate::messaging::sealed;
use crate::messaging::telegram::db::db_time;
use crate::security::crypto::Aad;
use crate::security::SecurityManager;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Map, Value};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS whatsapp_config (
            id INTEGER PRIMARY KEY,
            session_blob BLOB,
            own_jid VARCHAR(120),
            own_phone VARCHAR(32),
            bot_username VARCHAR(255),
            owner_user_id INTEGER,
            owner_username VARCHAR(255),
            is_paired BOOLEAN DEFAULT 0,
            is_active BOOLEAN DEFAULT 0,
            paired_at DATETIME,
            max_message_length INTEGER DEFAULT 4096,
            rate_limit_per_minute INTEGER DEFAULT 30,
            broadcast_enabled BOOLEAN DEFAULT 1,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         INSERT OR IGNORE INTO whatsapp_config (id) VALUES (1);
         CREATE TABLE IF NOT EXISTS whatsapp_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            whatsapp_jid VARCHAR(120) NOT NULL UNIQUE,
            phone_number VARCHAR(32) NOT NULL,
            openalgo_username VARCHAR(255) NOT NULL,
            encrypted_api_key TEXT,
            host_url VARCHAR(500),
            display_name VARCHAR(255),
            broker VARCHAR(50) DEFAULT 'default',
            is_active BOOLEAN DEFAULT 1,
            notifications_enabled BOOLEAN DEFAULT 1,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            last_command_at DATETIME
         );
         CREATE INDEX IF NOT EXISTS ix_whatsapp_users_phone_number ON whatsapp_users(phone_number);
         CREATE INDEX IF NOT EXISTS ix_whatsapp_users_openalgo_username ON whatsapp_users(openalgo_username);
         CREATE TABLE IF NOT EXISTS whatsapp_command_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            whatsapp_jid VARCHAR(120) NOT NULL,
            command VARCHAR(100) NOT NULL,
            parameters TEXT,
            executed_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         CREATE INDEX IF NOT EXISTS ix_whatsapp_command_logs_whatsapp_jid ON whatsapp_command_logs(whatsapp_jid);
         CREATE TABLE IF NOT EXISTS whatsapp_notification_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            whatsapp_jid VARCHAR(120) NOT NULL,
            message TEXT NOT NULL,
            media_path TEXT,
            media_kind VARCHAR(20),
            priority INTEGER DEFAULT 5,
            status VARCHAR(20) DEFAULT 'pending',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            sent_at DATETIME,
            error_message TEXT
         );
         CREATE INDEX IF NOT EXISTS ix_whatsapp_notification_queue_status ON whatsapp_notification_queue(status);
         CREATE TABLE IF NOT EXISTS whatsapp_user_preferences (
            whatsapp_jid VARCHAR(120) PRIMARY KEY,
            order_notifications BOOLEAN DEFAULT 1,
            trade_notifications BOOLEAN DEFAULT 1,
            pnl_notifications BOOLEAN DEFAULT 1,
            daily_summary BOOLEAN DEFAULT 1,
            summary_time VARCHAR(10) DEFAULT '18:00',
            language VARCHAR(10) DEFAULT 'en',
            timezone VARCHAR(50) DEFAULT 'Asia/Kolkata',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );",
    )?;
    Ok(())
}

fn blob_aad() -> Aad {
    Aad::new("whatsapp_config", "session_blob", "1")
}

/// The config row (no session).
#[derive(Debug, Clone, Default)]
pub struct WaConfig {
    pub is_paired: bool,
    pub is_active: bool,
    pub own_jid: Option<String>,
    pub own_phone: Option<String>,
    pub bot_username: Option<String>,
    pub owner_user_id: Option<i64>,
    pub owner_username: Option<String>,
    pub paired_at: Option<String>,
    pub max_message_length: i64,
    pub rate_limit_per_minute: i64,
    pub broadcast_enabled: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl WaConfig {
    /// Web `get_bot_config()` as `jsonify` writes it.
    pub fn to_json(&self) -> Map<String, Value> {
        let v = json!({
            "is_paired": self.is_paired,
            "is_active": self.is_active,
            "own_jid": self.own_jid,
            "own_phone": self.own_phone,
            "bot_username": self.bot_username,
            "owner_user_id": self.owner_user_id,
            "owner_username": self.owner_username,
            "paired_at": http_date(self.paired_at.as_deref()),
            "max_message_length": self.max_message_length,
            "rate_limit_per_minute": self.rate_limit_per_minute,
            "broadcast_enabled": self.broadcast_enabled,
            "created_at": http_date(self.created_at.as_deref()),
            "updated_at": http_date(self.updated_at.as_deref()),
        });
        match v {
            Value::Object(m) => m,
            _ => Map::new(),
        }
    }
}

pub fn get_config(conn: &Connection) -> Result<WaConfig> {
    let row = conn
        .query_row(
            "SELECT is_paired, is_active, own_jid, own_phone, bot_username, owner_user_id,
                    owner_username, paired_at, max_message_length, rate_limit_per_minute,
                    broadcast_enabled, created_at, updated_at
             FROM whatsapp_config WHERE id = 1",
            [],
            |r| {
                Ok(WaConfig {
                    is_paired: r.get::<_, Option<bool>>(0)?.unwrap_or(false),
                    is_active: r.get::<_, Option<bool>>(1)?.unwrap_or(false),
                    own_jid: r.get(2)?,
                    own_phone: r.get(3)?,
                    bot_username: r.get(4)?,
                    owner_user_id: r.get(5)?,
                    owner_username: r.get(6)?,
                    paired_at: r.get(7)?,
                    max_message_length: r.get::<_, Option<i64>>(8)?.unwrap_or(4096),
                    rate_limit_per_minute: r.get::<_, Option<i64>>(9)?.unwrap_or(30),
                    broadcast_enabled: r.get::<_, Option<bool>>(10)?.unwrap_or(true),
                    created_at: r.get(11)?,
                    updated_at: r.get(12)?,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or(WaConfig {
        max_message_length: 4096,
        rate_limit_per_minute: 30,
        broadcast_enabled: true,
        ..Default::default()
    }))
}

/// Web `update_bot_config`: only the safe fields; the rate limit clamped to
/// 1..=120 and ignored when it is not a number.
pub fn update_config(conn: &Connection, updates: &Map<String, Value>, now: DateTime<Utc>) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO whatsapp_config (id) VALUES (1)", [])?;
    for (k, v) in updates {
        match k.as_str() {
            "is_active" | "broadcast_enabled" => {
                let b = crate::messaging::format::truthy(Some(v));
                conn.execute(
                    &format!("UPDATE whatsapp_config SET {} = ?1 WHERE id = 1", k),
                    params![b],
                )?;
            }
            "rate_limit_per_minute" => {
                if let Some(n) = crate::messaging::format::py_int(v) {
                    conn.execute(
                        "UPDATE whatsapp_config SET rate_limit_per_minute = ?1 WHERE id = 1",
                        params![n.clamp(1, 120)],
                    )?;
                }
            }
            "max_message_length" => {
                if let Some(n) = crate::messaging::format::py_int(v) {
                    conn.execute(
                        "UPDATE whatsapp_config SET max_message_length = ?1 WHERE id = 1",
                        params![n],
                    )?;
                }
            }
            _ => {}
        }
    }
    conn.execute(
        "UPDATE whatsapp_config SET updated_at = ?1 WHERE id = 1",
        params![db_time(now)],
    )?;
    Ok(())
}

pub fn set_active(conn: &Connection, active: bool) -> Result<()> {
    conn.execute(
        "UPDATE whatsapp_config SET is_active = ?1 WHERE id = 1",
        params![active],
    )?;
    Ok(())
}

/// Seal a snapshot for the column.
pub fn seal_snapshot(security: &SecurityManager, snapshot: &[u8]) -> Result<String> {
    sealed::seal(security, &blob_aad(), &B64.encode(snapshot))
}

/// Open the column back to snapshot bytes.
pub fn open_snapshot(security: &SecurityManager, stored: &str) -> Result<Vec<u8>> {
    let b64 = sealed::open(security, &blob_aad(), stored)?;
    B64.decode(b64.expose())
        .map_err(|_| AppError::Encryption("session snapshot is not valid".into()))
}

/// Who paired the device.
pub struct Owner<'a> {
    pub own_jid: Option<&'a str>,
    pub own_phone: Option<&'a str>,
    pub owner_user_id: Option<i64>,
    pub owner_username: Option<&'a str>,
}

/// Web `save_session_blob`: store the session and mark the device paired.
/// Returns the stored ciphertext.
pub fn save_session(
    conn: &Connection,
    security: &SecurityManager,
    snapshot: &[u8],
    owner: &Owner,
    now: DateTime<Utc>,
) -> Result<String> {
    let sealed = seal_snapshot(security, snapshot)?;
    conn.execute("INSERT OR IGNORE INTO whatsapp_config (id) VALUES (1)", [])?;
    conn.execute(
        "UPDATE whatsapp_config SET session_blob = ?1,
            own_jid = COALESCE(?2, own_jid), own_phone = COALESCE(?3, own_phone),
            owner_user_id = COALESCE(?4, owner_user_id),
            owner_username = COALESCE(?5, owner_username),
            is_paired = 1, paired_at = ?6, updated_at = ?6
         WHERE id = 1",
        params![
            sealed,
            owner.own_jid,
            owner.own_phone,
            owner.owner_user_id,
            owner.owner_username,
            db_time(now)
        ],
    )?;
    Ok(sealed)
}

/// The stored session: (snapshot, ciphertext), or `None` when unpaired.
pub fn load_session(conn: &Connection, security: &SecurityManager) -> Result<Option<(Vec<u8>, String)>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT CAST(session_blob AS TEXT) FROM whatsapp_config WHERE id = 1 AND is_paired = 1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    match stored.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => Ok(Some((open_snapshot(security, &s)?, s))),
    }
}

/// Web `refresh_session_blob`: replace the session of a device that is still
/// paired with the session `expected` (ciphertext). Returns the new
/// ciphertext, or `None` when refused.
pub fn refresh_session(
    conn: &Connection,
    security: &SecurityManager,
    snapshot: &[u8],
    expected: &str,
) -> Result<Option<String>> {
    let sealed = seal_snapshot(security, snapshot)?;
    let n = conn.execute(
        "UPDATE whatsapp_config SET session_blob = ?1
         WHERE id = 1 AND is_paired = 1 AND CAST(session_blob AS TEXT) = ?2",
        params![sealed, expected],
    )?;
    Ok((n == 1).then_some(sealed))
}

const UNPAIRED: &str = "session_blob = NULL, own_jid = NULL, own_phone = NULL, bot_username = NULL,
    owner_user_id = NULL, owner_username = NULL, is_paired = 0, is_active = 0, paired_at = NULL";

/// Web `clear_session_blob` (unlink).
pub fn clear_session(conn: &Connection) -> Result<bool> {
    let n = conn.execute(
        &format!("UPDATE whatsapp_config SET {} WHERE id = 1", UNPAIRED),
        [],
    )?;
    Ok(n > 0)
}

/// Web `clear_rejected_session`: forget a logged-out session unless a newer
/// pairing replaced it.
pub fn clear_rejected(conn: &Connection, expected: &str) -> Result<bool> {
    let n = conn.execute(
        &format!(
            "UPDATE whatsapp_config SET {} WHERE id = 1 AND CAST(session_blob AS TEXT) = ?1",
            UNPAIRED
        ),
        params![expected],
    )?;
    Ok(n == 1)
}

/// Record the device's own number if not known yet.
pub fn persist_owner_identity(conn: &Connection, jid: &str, phone: &str) -> Result<()> {
    conn.execute(
        "UPDATE whatsapp_config SET own_jid = COALESCE(own_jid, ?1),
            own_phone = COALESCE(own_phone, NULLIF(?2, '')) WHERE id = 1",
        params![jid, phone],
    )?;
    Ok(())
}

/// A `whatsapp_users` row.
#[derive(Debug, Clone)]
pub struct WaUser {
    pub id: i64,
    pub whatsapp_jid: String,
    pub phone_number: String,
    pub openalgo_username: String,
    pub display_name: Option<String>,
    pub broker: Option<String>,
    pub notifications_enabled: bool,
    pub created_at: Option<String>,
    pub last_command_at: Option<String>,
}

impl WaUser {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "whatsapp_jid": self.whatsapp_jid,
            "phone_number": self.phone_number,
            "openalgo_username": self.openalgo_username,
            "display_name": self.display_name,
            "broker": self.broker,
            "notifications_enabled": self.notifications_enabled,
            "created_at": http_date(self.created_at.as_deref()),
            "last_command_at": http_date(self.last_command_at.as_deref()),
        })
    }
}

const USER_COLS: &str = "id, whatsapp_jid, phone_number, openalgo_username, display_name, broker,
    notifications_enabled, created_at, last_command_at";

fn user_from(r: &Row) -> rusqlite::Result<WaUser> {
    Ok(WaUser {
        id: r.get(0)?,
        whatsapp_jid: r.get(1)?,
        phone_number: r.get(2)?,
        openalgo_username: r.get(3)?,
        display_name: r.get(4)?,
        broker: r.get(5)?,
        notifications_enabled: r.get::<_, Option<bool>>(6)?.unwrap_or(true),
        created_at: r.get(7)?,
        last_command_at: r.get(8)?,
    })
}

pub fn get_user_by_username(conn: &Connection, username: &str) -> Result<Option<WaUser>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM whatsapp_users WHERE openalgo_username = ?1 AND is_active = 1
                 ORDER BY id LIMIT 1",
                USER_COLS
            ),
            params![username],
            user_from,
        )
        .optional()?)
}

pub fn all_users(conn: &Connection, broker: Option<&str>, notifications: Option<bool>) -> Result<Vec<WaUser>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM whatsapp_users WHERE is_active = 1
           AND (?1 IS NULL OR broker = ?1) AND (?2 IS NULL OR notifications_enabled = ?2)
         ORDER BY id",
        USER_COLS
    ))?;
    let rows = stmt
        .query_map(params![broker, notifications], user_from)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn create_or_update_user(
    conn: &Connection,
    jid: &str,
    phone: &str,
    username: &str,
    display_name: &str,
    broker: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    let ts = db_time(now);
    conn.execute(
        "INSERT INTO whatsapp_users (whatsapp_jid, phone_number, openalgo_username, display_name,
            broker, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT(whatsapp_jid) DO UPDATE SET phone_number = ?2, openalgo_username = ?3,
            display_name = COALESCE(NULLIF(?4, ''), display_name), broker = ?5, is_active = 1,
            updated_at = ?6",
        params![jid, phone, username, display_name, broker, ts],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO whatsapp_user_preferences (whatsapp_jid, created_at, updated_at)
         VALUES (?1, ?2, ?2)",
        params![jid, ts],
    )?;
    Ok(())
}

/// Soft delete; false when there is no such row.
pub fn delete_user(conn: &Connection, jid: &str, now: DateTime<Utc>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE whatsapp_users SET is_active = 0, updated_at = ?2 WHERE whatsapp_jid = ?1",
        params![jid, db_time(now)],
    )?;
    Ok(n > 0)
}

pub fn log_command(conn: &Connection, jid: &str, command: &str, args: &[String], now: DateTime<Utc>) -> Result<()> {
    let ts = db_time(now);
    let params_json = if args.is_empty() {
        None
    } else {
        Some(json!({"args": args}).to_string())
    };
    conn.execute(
        "INSERT INTO whatsapp_command_logs (whatsapp_jid, command, parameters, executed_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![jid, command, params_json, ts],
    )?;
    conn.execute(
        "UPDATE whatsapp_users SET last_command_at = ?2 WHERE whatsapp_jid = ?1",
        params![jid, ts],
    )?;
    Ok(())
}

/// Web `get_command_stats(days)`: `{total_commands, by_command, days}`.
pub fn command_stats(conn: &Connection, days: i64, now: DateTime<Utc>) -> Result<Value> {
    let since = db_time(now - Duration::days(days));
    let mut stmt = conn.prepare(
        "SELECT command, COUNT(*) FROM whatsapp_command_logs WHERE executed_at >= ?1
         GROUP BY command ORDER BY MIN(id)",
    )?;
    let rows = stmt
        .query_map(params![since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total: i64 = rows.iter().map(|(_, n)| n).sum();
    let mut by = Map::new();
    for (c, n) in rows {
        by.insert(c, json!(n));
    }
    Ok(json!({"total_commands": total, "by_command": by, "days": days}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 10, 0, 0).unwrap()
    }

    #[test]
    fn session_blob_is_sealed_and_conditional() {
        let c = conn();
        let sec = SecurityManager::for_tests();
        assert!(load_session(&c, &sec).unwrap().is_none());
        let owner = Owner {
            own_jid: Some("919876543210@s.whatsapp.net"),
            own_phone: Some("919876543210"),
            owner_user_id: Some(1),
            owner_username: Some("trader"),
        };
        let ct = save_session(&c, &sec, b"SNAPSHOT-1", &owner, now()).unwrap();
        let raw: String = c
            .query_row("SELECT CAST(session_blob AS TEXT) FROM whatsapp_config", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("SNAPSHOT"));
        let (snap, stored) = load_session(&c, &sec).unwrap().unwrap();
        assert_eq!(snap, b"SNAPSHOT-1");
        assert_eq!(stored, ct);
        let cfg = get_config(&c).unwrap();
        assert!(cfg.is_paired);
        assert_eq!(cfg.owner_username.as_deref(), Some("trader"));
        // A refresh over a stale ciphertext is refused.
        assert!(refresh_session(&c, &sec, b"S2", "v1:stale").unwrap().is_none());
        let ct2 = refresh_session(&c, &sec, b"S2", &ct).unwrap().unwrap();
        assert_eq!(load_session(&c, &sec).unwrap().unwrap().0, b"S2");
        // A logout of the old session does not clear the new one.
        assert!(!clear_rejected(&c, &ct).unwrap());
        assert!(clear_rejected(&c, &ct2).unwrap());
        assert!(load_session(&c, &sec).unwrap().is_none());
        assert!(!get_config(&c).unwrap().is_paired);
        assert!(get_config(&c).unwrap().owner_username.is_none());
    }

    #[test]
    fn config_users_and_stats() {
        let c = conn();
        let mut u = Map::new();
        u.insert("rate_limit_per_minute".into(), json!(500));
        u.insert("broadcast_enabled".into(), json!(false));
        u.insert("session_blob".into(), json!("nope"));
        update_config(&c, &u, now()).unwrap();
        let cfg = get_config(&c).unwrap();
        assert_eq!(cfg.rate_limit_per_minute, 120);
        assert!(!cfg.broadcast_enabled);
        create_or_update_user(&c, "91@s.whatsapp.net", "91", "trader", "T", "zerodha", now()).unwrap();
        assert_eq!(all_users(&c, None, None).unwrap().len(), 1);
        assert!(get_user_by_username(&c, "trader").unwrap().is_some());
        log_command(&c, "91@s.whatsapp.net", "help", &[], now()).unwrap();
        log_command(&c, "91@s.whatsapp.net", "help", &[], now()).unwrap();
        let s = command_stats(&c, 7, now()).unwrap();
        assert_eq!(s, json!({"total_commands": 2, "by_command": {"help": 2}, "days": 7}));
        assert!(delete_user(&c, "91@s.whatsapp.net", now()).unwrap());
        assert!(all_users(&c, None, None).unwrap().is_empty());
    }
}
