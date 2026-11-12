//! Where each alert channel's settings are kept (web
//! `database/alert_channels_db.py`).
//!
//! One row per channel the trader has changed on the /alerts page: whether
//! the channel is on, which event groups it receives, and for a webhook
//! channel (Slack, Discord) the webhook address. A channel with no row
//! uses its defaults (`settings`), so an install that never opens /alerts
//! keeps exactly the alerts it had, and no seed rows are ever written.
//!
//! **A webhook address is a secret.** Anybody holding a Slack or Discord
//! webhook URL can post into that channel, so the column holds it sealed
//! with the data key (`messaging::sealed`, bound by associated data to this
//! table, column and channel), it is never returned to a page, and it is
//! dropped with every other encrypted value by a fresh start
//! (`key_vault::drop_encrypted_values`) and an account reset.

use crate::error::Result;
use crate::messaging::sealed;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

/// Migration `102_alert_channels`: the table, with the web's columns. A new
/// table, so there is nothing to carry over: Telegram and WhatsApp keep
/// alerting from their defaults until the trader changes them (the web's
/// `upgrade/migrate_alert_channels.py` writes no rows either).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alert_channels (
            channel VARCHAR(20) NOT NULL PRIMARY KEY,
            enabled BOOLEAN NOT NULL DEFAULT 1,
            categories TEXT NOT NULL DEFAULT '',
            webhook_url TEXT,
            updated_at DATETIME NOT NULL
         );",
    )?;
    Ok(())
}

/// The associated data binding a sealed webhook address to its channel.
pub fn webhook_aad(channel: &str) -> Aad {
    Aad::new("alert_channels", "webhook_url", channel)
}

/// A channel's saved settings, without the webhook address itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredChannel {
    pub enabled: bool,
    /// The event groups it receives, as saved.
    pub categories: Vec<String>,
    /// Whether a webhook address is stored.
    pub webhook_saved: bool,
}

/// Naive UTC with microseconds, as the web stores `updated_at`.
fn stamp(now: DateTime<Utc>) -> String {
    now.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

fn split(categories: &str) -> Vec<String> {
    categories
        .split(',')
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

/// A channel's saved settings, or `None` when it has never been changed.
/// An error is a database that could not be read; the caller decides
/// whether to fall back to the defaults.
pub fn load(conn: &Connection, channel: &str) -> Result<Option<StoredChannel>> {
    Ok(conn
        .query_row(
            "SELECT enabled, categories, webhook_url FROM alert_channels WHERE channel = ?1",
            [channel],
            |r| {
                Ok(StoredChannel {
                    enabled: r.get::<_, i64>(0)? != 0,
                    categories: split(&r.get::<_, String>(1)?),
                    webhook_saved: r
                        .get::<_, Option<String>>(2)?
                        .is_some_and(|u| !u.is_empty()),
                })
            },
        )
        .optional()?)
}

/// Save whether a channel is on and which groups it receives. A saved
/// webhook address is kept as it is.
pub fn save_channel(
    conn: &Connection,
    channel: &str,
    enabled: bool,
    categories: &[&str],
    now: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO alert_channels (channel, enabled, categories, webhook_url, updated_at)
         VALUES (?1, ?2, ?3, NULL, ?4)
         ON CONFLICT(channel) DO UPDATE SET
             enabled = excluded.enabled,
             categories = excluded.categories,
             updated_at = excluded.updated_at",
        params![channel, enabled, categories.join(","), stamp(now)],
    )?;
    Ok(())
}

/// Store a channel's webhook address, sealed, and set it on or off. A
/// channel saved for the first time starts with `default_categories`; an
/// existing choice of groups is kept.
pub fn save_webhook(
    conn: &Connection,
    security: &SecurityManager,
    channel: &str,
    url: &str,
    enabled: bool,
    default_categories: &[&str],
    now: DateTime<Utc>,
) -> Result<()> {
    let sealed = sealed::seal(security, &webhook_aad(channel), url)?;
    conn.execute(
        "INSERT INTO alert_channels (channel, enabled, categories, webhook_url, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(channel) DO UPDATE SET
             enabled = excluded.enabled,
             webhook_url = excluded.webhook_url,
             updated_at = excluded.updated_at",
        params![
            channel,
            enabled,
            default_categories.join(","),
            sealed,
            stamp(now)
        ],
    )?;
    Ok(())
}

/// Forget a channel's webhook address and turn the channel off.
pub fn clear_webhook(conn: &Connection, channel: &str, now: DateTime<Utc>) -> Result<()> {
    conn.execute(
        "UPDATE alert_channels SET webhook_url = NULL, enabled = 0, updated_at = ?2
         WHERE channel = ?1",
        params![channel, stamp(now)],
    )?;
    Ok(())
}

/// A channel's webhook address, opened, or `None` when none is usable. An
/// address that no longer opens (keys replaced by a fresh start, a row
/// copied from another install) reads as not saved: the trader is asked to
/// save it again rather than shown an error they cannot act on. An error is
/// a database that could not be read.
pub fn read_webhook_url(
    conn: &Connection,
    security: &SecurityManager,
    channel: &str,
) -> Result<Option<Secret>> {
    let stored: Option<Option<String>> = conn
        .query_row(
            "SELECT webhook_url FROM alert_channels WHERE channel = ?1",
            [channel],
            |r| r.get(0),
        )
        .optional()?;
    let Some(stored) = stored.flatten().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    match sealed::open(security, &webhook_aad(channel), &stored) {
        Ok(url) => Ok(Some(url)),
        Err(_) => {
            tracing::info!("The saved {} webhook no longer opens", channel);
            Ok(None)
        }
    }
}

/// Every saved webhook address dropped, and its channel switched off: a
/// fresh start or an account reset, after which the key it was sealed with
/// is gone.
pub fn drop_secrets(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE alert_channels SET webhook_url = NULL, enabled = 0
         WHERE webhook_url IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// The first sealed webhook address, as proof of the data key
/// (`key_vault::witness`); `None` when none is saved.
pub fn witness(conn: &Connection) -> Result<Option<crate::db::sqlite::key_vault::Witness>> {
    let first: Option<(String, String)> = conn
        .query_row(
            "SELECT channel, webhook_url FROM alert_channels
             WHERE webhook_url IS NOT NULL AND webhook_url <> '' ORDER BY channel LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(first.and_then(|(channel, stored)| {
        sealed::parts(&stored).map(|(nonce, ct)| crate::db::sqlite::key_vault::Witness {
            ciphertext: ct.to_string(),
            nonce: nonce.to_string(),
            aad: webhook_aad(&channel),
        })
    }))
}

#[cfg(test)]
mod tests;
