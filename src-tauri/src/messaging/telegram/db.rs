//! Telegram tables, with the web's names and columns (`database/telegram_db.py`):
//! `telegram_users`, `bot_config`, `command_logs`, `notification_queue`,
//! `user_preferences`. They live in `openalgo.db`, as on the web.
//!
//! The bot token and each linked user's API key are sealed with the app's
//! data key (`messaging::sealed`) instead of the web's Fernet key. Foreign
//! keys are not declared: the web logs commands from users who have not
//! linked yet, which its unenforced foreign keys allowed.

use crate::error::Result;
use crate::messaging::format::http_date;
use crate::messaging::sealed;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

/// Most rows the never-drained notification queue keeps.
pub const NOTIFICATION_QUEUE_CAP: i64 = 1000;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS telegram_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            telegram_id INTEGER NOT NULL UNIQUE,
            openalgo_username VARCHAR(255) NOT NULL,
            encrypted_api_key TEXT,
            host_url VARCHAR(500),
            first_name VARCHAR(255),
            last_name VARCHAR(255),
            telegram_username VARCHAR(255),
            broker VARCHAR(50) DEFAULT 'default',
            is_active BOOLEAN DEFAULT 1,
            notifications_enabled BOOLEAN DEFAULT 1,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            last_command_at DATETIME
         );
         CREATE INDEX IF NOT EXISTS ix_telegram_users_telegram_id ON telegram_users(telegram_id);
         CREATE INDEX IF NOT EXISTS ix_telegram_users_openalgo_username ON telegram_users(openalgo_username);
         CREATE TABLE IF NOT EXISTS bot_config (
            id INTEGER PRIMARY KEY,
            token TEXT,
            is_active BOOLEAN DEFAULT 0,
            bot_username VARCHAR(255),
            max_message_length INTEGER DEFAULT 4096,
            rate_limit_per_minute INTEGER DEFAULT 30,
            broadcast_enabled BOOLEAN DEFAULT 1,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         INSERT OR IGNORE INTO bot_config (id) VALUES (1);
         CREATE TABLE IF NOT EXISTS command_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            telegram_id INTEGER NOT NULL,
            command VARCHAR(100) NOT NULL,
            chat_id INTEGER,
            parameters TEXT,
            executed_at DATETIME DEFAULT CURRENT_TIMESTAMP
         );
         CREATE INDEX IF NOT EXISTS ix_command_logs_telegram_id ON command_logs(telegram_id);
         CREATE INDEX IF NOT EXISTS ix_command_logs_executed_at ON command_logs(executed_at);
         CREATE TABLE IF NOT EXISTS notification_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            telegram_id INTEGER NOT NULL,
            message TEXT NOT NULL,
            priority INTEGER DEFAULT 5,
            status VARCHAR(20) DEFAULT 'pending',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            sent_at DATETIME,
            error_message TEXT
         );
         CREATE INDEX IF NOT EXISTS ix_notification_queue_status ON notification_queue(status);
         CREATE TABLE IF NOT EXISTS user_preferences (
            telegram_id INTEGER PRIMARY KEY,
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

pub fn db_time(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn token_aad() -> Aad {
    Aad::new("bot_config", "token", "1")
}

fn api_key_aad(telegram_id: i64) -> Aad {
    Aad::new(
        "telegram_users",
        "encrypted_api_key",
        &telegram_id.to_string(),
    )
}

/// The `bot_config` row, token opened.
#[derive(Clone)]
pub struct BotConfig {
    pub token: Option<Secret>,
    pub is_active: bool,
    pub bot_username: Option<String>,
    pub max_message_length: i64,
    pub rate_limit_per_minute: i64,
    pub broadcast_enabled: bool,
}

impl std::fmt::Debug for BotConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotConfig")
            .field("has_token", &self.token.is_some())
            .field("is_active", &self.is_active)
            .field("bot_username", &self.bot_username)
            .finish()
    }
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            token: None,
            is_active: false,
            bot_username: None,
            max_message_length: 4096,
            rate_limit_per_minute: 30,
            broadcast_enabled: true,
        }
    }
}

pub fn get_bot_config(conn: &Connection, security: &SecurityManager) -> Result<BotConfig> {
    let row = conn
        .query_row(
            "SELECT token, is_active, bot_username, max_message_length,
                    rate_limit_per_minute, broadcast_enabled FROM bot_config WHERE id = 1",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<bool>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<bool>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((token, active, username, mml, rate, bcast)) = row else {
        return Ok(BotConfig::default());
    };
    let token = match token.filter(|t| !t.is_empty()) {
        None => None,
        Some(t) => match sealed::open(security, &token_aad(), &t) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("Could not open the stored Telegram bot token: {}", e);
                None
            }
        },
    };
    Ok(BotConfig {
        token,
        is_active: active.unwrap_or(false),
        bot_username: username,
        max_message_length: mml.unwrap_or(4096),
        rate_limit_per_minute: rate.unwrap_or(30),
        broadcast_enabled: bcast.unwrap_or(true),
    })
}

/// Fields of a `bot_config` update; `None` leaves a column alone.
#[derive(Default)]
pub struct ConfigUpdate {
    /// `Some(None)` clears the token.
    pub token: Option<Option<String>>,
    pub is_active: Option<bool>,
    pub bot_username: Option<String>,
    pub broadcast_enabled: Option<bool>,
    pub rate_limit_per_minute: Option<i64>,
}

/// Web `update_bot_config`: writes nothing when every field already matches.
pub fn update_bot_config(
    conn: &Connection,
    security: &SecurityManager,
    u: ConfigUpdate,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO bot_config (id) VALUES (1)", [])?;
    let cur = get_bot_config(conn, security)?;
    let mut changed = false;
    if let Some(tok) = u.token {
        let same = match (&tok, &cur.token) {
            (Some(new), Some(old)) => new.as_str() == old.expose(),
            (None, None) => true,
            _ => false,
        };
        if !same {
            let stored = match tok.filter(|t| !t.is_empty()) {
                Some(t) => Some(sealed::seal(security, &token_aad(), &t)?),
                None => None,
            };
            conn.execute(
                "UPDATE bot_config SET token = ?1 WHERE id = 1",
                params![stored],
            )?;
            changed = true;
        }
    }
    if let Some(a) = u.is_active.filter(|a| *a != cur.is_active) {
        conn.execute(
            "UPDATE bot_config SET is_active = ?1 WHERE id = 1",
            params![a],
        )?;
        changed = true;
    }
    if let Some(n) = u
        .bot_username
        .filter(|n| Some(n) != cur.bot_username.as_ref())
    {
        conn.execute(
            "UPDATE bot_config SET bot_username = ?1 WHERE id = 1",
            params![n],
        )?;
        changed = true;
    }
    if let Some(b) = u.broadcast_enabled.filter(|b| *b != cur.broadcast_enabled) {
        conn.execute(
            "UPDATE bot_config SET broadcast_enabled = ?1 WHERE id = 1",
            params![b],
        )?;
        changed = true;
    }
    if let Some(r) = u
        .rate_limit_per_minute
        .filter(|r| *r != cur.rate_limit_per_minute)
    {
        conn.execute(
            "UPDATE bot_config SET rate_limit_per_minute = ?1 WHERE id = 1",
            params![r],
        )?;
        changed = true;
    }
    if changed {
        conn.execute(
            "UPDATE bot_config SET updated_at = ?1 WHERE id = 1",
            params![db_time(now)],
        )?;
    }
    Ok(())
}

/// A `telegram_users` row (without the key).
#[derive(Debug, Clone)]
pub struct TgUser {
    pub id: i64,
    pub telegram_id: i64,
    pub openalgo_username: String,
    pub host_url: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub telegram_username: Option<String>,
    pub broker: Option<String>,
    pub is_active: bool,
    pub notifications_enabled: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub last_command_at: Option<String>,
}

const USER_COLS: &str = "id, telegram_id, openalgo_username, host_url, first_name, last_name,
    telegram_username, broker, is_active, notifications_enabled, created_at, updated_at,
    last_command_at";

fn user_from(r: &Row) -> rusqlite::Result<TgUser> {
    Ok(TgUser {
        id: r.get(0)?,
        telegram_id: r.get(1)?,
        openalgo_username: r.get(2)?,
        host_url: r.get(3)?,
        first_name: r.get(4)?,
        last_name: r.get(5)?,
        telegram_username: r.get(6)?,
        broker: r.get(7)?,
        is_active: r.get::<_, Option<bool>>(8)?.unwrap_or(true),
        notifications_enabled: r.get::<_, Option<bool>>(9)?.unwrap_or(true),
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        last_command_at: r.get(12)?,
    })
}

impl TgUser {
    /// `get_all_telegram_users` row.
    pub fn list_json(&self) -> Value {
        json!({
            "id": self.id,
            "telegram_id": self.telegram_id,
            "openalgo_username": self.openalgo_username,
            "first_name": self.first_name,
            "last_name": self.last_name,
            "telegram_username": self.telegram_username,
            "broker": self.broker,
            "notifications_enabled": self.notifications_enabled,
            "created_at": http_date(self.created_at.as_deref()),
            "last_command_at": http_date(self.last_command_at.as_deref()),
        })
    }

    /// `get_telegram_user_by_username` row.
    pub fn full_json(&self) -> Value {
        json!({
            "id": self.id,
            "telegram_id": self.telegram_id,
            "openalgo_username": self.openalgo_username,
            "first_name": self.first_name,
            "last_name": self.last_name,
            "telegram_username": self.telegram_username,
            "broker": self.broker,
            "is_active": self.is_active,
            "notifications_enabled": self.notifications_enabled,
            "created_at": http_date(self.created_at.as_deref()),
            "updated_at": http_date(self.updated_at.as_deref()),
            "last_command_at": http_date(self.last_command_at.as_deref()),
        })
    }
}

pub fn get_user(conn: &Connection, telegram_id: i64) -> Result<Option<TgUser>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM telegram_users WHERE telegram_id = ?1 AND is_active = 1",
                USER_COLS
            ),
            params![telegram_id],
            user_from,
        )
        .optional()?)
}

pub fn get_user_by_username(conn: &Connection, username: &str) -> Result<Option<TgUser>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM telegram_users WHERE openalgo_username = ?1 AND is_active = 1
                 ORDER BY id LIMIT 1",
                USER_COLS
            ),
            params![username],
            user_from,
        )
        .optional()?)
}

/// Optional filters of `get_all_telegram_users`.
#[derive(Debug, Default, Clone)]
pub struct UserFilter {
    pub broker: Option<String>,
    pub notifications_enabled: Option<bool>,
}

pub fn all_users(conn: &Connection, f: &UserFilter) -> Result<Vec<TgUser>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM telegram_users WHERE is_active = 1
           AND (?1 IS NULL OR broker = ?1)
           AND (?2 IS NULL OR notifications_enabled = ?2)
         ORDER BY id",
        USER_COLS
    ))?;
    let rows = stmt
        .query_map(params![f.broker, f.notifications_enabled], user_from)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A linking from `/link`.
pub struct Link<'a> {
    pub telegram_id: i64,
    pub username: &'a str,
    pub api_key: Option<&'a str>,
    pub host_url: Option<&'a str>,
    pub first_name: &'a str,
    pub last_name: &'a str,
    pub telegram_username: &'a str,
    pub broker: &'a str,
}

pub fn create_or_update_user(
    conn: &Connection,
    security: &SecurityManager,
    l: &Link,
    now: DateTime<Utc>,
) -> Result<()> {
    let key = match l.api_key {
        Some(k) if !k.is_empty() => Some(sealed::seal(security, &api_key_aad(l.telegram_id), k)?),
        _ => None,
    };
    let ts = db_time(now);
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM telegram_users WHERE telegram_id = ?1",
            params![l.telegram_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        conn.execute(
            "UPDATE telegram_users SET openalgo_username = ?2,
                encrypted_api_key = COALESCE(?3, encrypted_api_key),
                host_url = COALESCE(NULLIF(?4, ''), host_url),
                first_name = ?5, last_name = ?6, telegram_username = ?7, broker = ?8,
                is_active = 1, updated_at = ?9
             WHERE telegram_id = ?1",
            params![
                l.telegram_id,
                l.username,
                key,
                l.host_url.unwrap_or(""),
                l.first_name,
                l.last_name,
                l.telegram_username,
                l.broker,
                ts
            ],
        )?;
    } else {
        conn.execute(
            "INSERT INTO telegram_users (telegram_id, openalgo_username, encrypted_api_key,
                host_url, first_name, last_name, telegram_username, broker, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                l.telegram_id,
                l.username,
                key,
                l.host_url,
                l.first_name,
                l.last_name,
                l.telegram_username,
                l.broker,
                ts
            ],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO user_preferences (telegram_id, created_at, updated_at)
             VALUES (?1, ?2, ?2)",
            params![l.telegram_id, ts],
        )?;
    }
    Ok(())
}

/// Soft delete (web `delete_telegram_user`). False when there is no row.
pub fn delete_user(conn: &Connection, telegram_id: i64, now: DateTime<Utc>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE telegram_users SET is_active = 0, updated_at = ?2 WHERE telegram_id = ?1",
        params![telegram_id, db_time(now)],
    )?;
    Ok(n > 0)
}

/// A linked user's SDK credentials (`get_user_credentials`).
pub struct Credentials {
    pub username: String,
    pub api_key: Option<Secret>,
    pub host_url: Option<String>,
    pub broker: Option<String>,
}

pub fn user_credentials(
    conn: &Connection,
    security: &SecurityManager,
    telegram_id: i64,
) -> Result<Option<Credentials>> {
    let row = conn
        .query_row(
            "SELECT openalgo_username, encrypted_api_key, host_url, broker FROM telegram_users
             WHERE telegram_id = ?1 AND is_active = 1",
            params![telegram_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((username, key, host_url, broker)) = row else {
        return Ok(None);
    };
    let api_key = match key.filter(|k| !k.is_empty()) {
        None => None,
        Some(k) => match sealed::open(security, &api_key_aad(telegram_id), &k) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("Could not open a linked Telegram user's API key: {}", e);
                None
            }
        },
    };
    Ok(Some(Credentials {
        username,
        api_key,
        host_url,
        broker,
    }))
}

pub fn log_command(
    conn: &Connection,
    telegram_id: i64,
    command: &str,
    chat_id: Option<i64>,
    now: DateTime<Utc>,
) -> Result<()> {
    let ts = db_time(now);
    conn.execute(
        "INSERT INTO command_logs (telegram_id, command, chat_id, parameters, executed_at)
         VALUES (?1, ?2, ?3, NULL, ?4)",
        params![telegram_id, command, chat_id, ts],
    )?;
    conn.execute(
        "UPDATE telegram_users SET last_command_at = ?2 WHERE telegram_id = ?1",
        params![telegram_id, ts],
    )?;
    Ok(())
}

/// `get_command_stats(days)`.
pub struct CommandStats {
    pub total_commands: i64,
    /// Ordered by count, highest first.
    pub commands_by_type: Vec<(String, i64)>,
    pub active_users: i64,
    pub top_users: Vec<(Option<String>, i64)>,
    pub period_days: i64,
}

pub fn command_stats(conn: &Connection, days: i64, now: DateTime<Utc>) -> Result<CommandStats> {
    let since = db_time(now - Duration::days(days));
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM command_logs WHERE executed_at >= ?1",
        params![since],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT command, COUNT(id) AS c FROM command_logs WHERE executed_at >= ?1
         GROUP BY command ORDER BY c DESC, command",
    )?;
    let by_type = stmt
        .query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let active: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT telegram_id) FROM command_logs WHERE executed_at >= ?1",
        params![since],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT u.telegram_username, COUNT(c.id) AS n FROM telegram_users u
         JOIN command_logs c ON c.telegram_id = u.telegram_id
         WHERE c.executed_at >= ?1 GROUP BY u.telegram_username ORDER BY n DESC LIMIT 10",
    )?;
    let top = stmt
        .query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(CommandStats {
        total_commands: total,
        commands_by_type: by_type,
        active_users: active,
        top_users: top,
        period_days: days,
    })
}

/// Queue a message whose delivery failed (web `add_notification`). The web
/// never drains this table; here it is capped so it cannot grow forever.
pub fn add_notification(
    conn: &Connection,
    telegram_id: i64,
    message: &str,
    priority: i64,
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO notification_queue (telegram_id, message, priority, status, created_at)
         VALUES (?1, ?2, ?3, 'pending', ?4)",
        params![telegram_id, message, priority, db_time(now)],
    )?;
    conn.execute(
        "DELETE FROM notification_queue WHERE id NOT IN
           (SELECT id FROM notification_queue ORDER BY id DESC LIMIT ?1)",
        params![NOTIFICATION_QUEUE_CAP],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap(); // idempotent
        c
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, 10, 0, 0).unwrap()
    }

    #[test]
    fn config_token_is_sealed_and_unchanged_writes_are_skipped() {
        let c = conn();
        let sec = SecurityManager::for_tests();
        update_bot_config(
            &c,
            &sec,
            ConfigUpdate {
                token: Some(Some("123:ABC".into())),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
        let raw: String = c
            .query_row("SELECT token FROM bot_config WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert!(!raw.contains("123:ABC"));
        let cfg = get_bot_config(&c, &sec).unwrap();
        assert_eq!(cfg.token.unwrap().expose(), "123:ABC");
        assert!(!format!("{:?}", get_bot_config(&c, &sec).unwrap()).contains("ABC"));
        // Same token: no write.
        update_bot_config(
            &c,
            &sec,
            ConfigUpdate {
                token: Some(Some("123:ABC".into())),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
        let again: String = c
            .query_row("SELECT token FROM bot_config WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw, again);
    }

    #[test]
    fn users_link_unlink_stats_and_queue_cap() {
        let c = conn();
        let sec = SecurityManager::for_tests();
        // Commands from an unlinked user are logged (no foreign key).
        log_command(&c, 42, "start", Some(42), now()).unwrap();
        create_or_update_user(
            &c,
            &sec,
            &Link {
                telegram_id: 42,
                username: "trader",
                api_key: Some("k"),
                host_url: Some("http://127.0.0.1:5000"),
                first_name: "A",
                last_name: "",
                telegram_username: "a",
                broker: "zerodha",
            },
            now(),
        )
        .unwrap();
        assert_eq!(
            get_user(&c, 42).unwrap().unwrap().openalgo_username,
            "trader"
        );
        let creds = user_credentials(&c, &sec, 42).unwrap().unwrap();
        assert_eq!(creds.api_key.unwrap().expose(), "k");
        log_command(&c, 42, "help", Some(42), now()).unwrap();
        log_command(&c, 42, "help", Some(42), now()).unwrap();
        let s = command_stats(&c, 7, now()).unwrap();
        assert_eq!(s.total_commands, 3);
        assert_eq!(s.commands_by_type[0], ("help".to_string(), 2));
        assert_eq!(s.active_users, 1);
        assert!(delete_user(&c, 42, now()).unwrap());
        assert!(get_user(&c, 42).unwrap().is_none());
        assert!(!delete_user(&c, 7, now()).unwrap());
        for i in 0..(NOTIFICATION_QUEUE_CAP + 5) {
            add_notification(&c, 42, &format!("m{}", i), 8, now()).unwrap();
        }
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM notification_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, NOTIFICATION_QUEUE_CAP);
    }
}
