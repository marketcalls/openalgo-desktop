//! Stored broker session (web `auth` table equivalent).
//!
//! One row per broker. Tokens are AES-256-GCM encrypted with AAD bound to the
//! broker id. A logout or the 03:00 IST boundary revokes the row
//! (`is_revoked = 1`) and blanks the ciphertexts, so "expired" can be told
//! apart from "never connected".

use crate::error::Result;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct StoredBrokerSession {
    pub broker_id: String,
    pub auth_token: Secret,
    pub feed_token: Option<Secret>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub authenticated_at: DateTime<Utc>,
}

fn aad(column: &str, broker: &str) -> Aad {
    Aad::new("auth", column, broker)
}

pub fn upsert(
    conn: &Connection,
    security: &SecurityManager,
    s: &StoredBrokerSession,
) -> Result<()> {
    let (auth_ct, auth_nonce) =
        security.encrypt(s.auth_token.expose(), &aad("auth_token", &s.broker_id))?;
    let (feed_ct, feed_nonce) = match &s.feed_token {
        Some(f) => {
            let (c, n) = security.encrypt(f.expose(), &aad("feed_token", &s.broker_id))?;
            (Some(c), Some(n))
        }
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO auth (broker_id, auth_token_encrypted, auth_token_nonce, feed_token_encrypted,
             feed_token_nonce, user_id, user_name, authenticated_at, is_revoked)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
         ON CONFLICT(broker_id) DO UPDATE SET
             auth_token_encrypted = excluded.auth_token_encrypted,
             auth_token_nonce = excluded.auth_token_nonce,
             feed_token_encrypted = excluded.feed_token_encrypted,
             feed_token_nonce = excluded.feed_token_nonce,
             user_id = excluded.user_id,
             user_name = excluded.user_name,
             authenticated_at = excluded.authenticated_at,
             is_revoked = 0,
             updated_at = datetime('now')",
        params![
            s.broker_id,
            auth_ct,
            auth_nonce,
            feed_ct,
            feed_nonce,
            s.user_id,
            s.user_name,
            s.authenticated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

/// The most recent non-revoked session, decrypted. Rows that no longer
/// decrypt (key rotated, tampered) are revoked and skipped.
pub fn latest_active(
    conn: &Connection,
    security: &SecurityManager,
) -> Result<Option<StoredBrokerSession>> {
    type Raw = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let row: Option<Raw> = conn
        .query_row(
            "SELECT broker_id, auth_token_encrypted, auth_token_nonce, feed_token_encrypted,
                    feed_token_nonce, user_id, user_name, authenticated_at
             FROM auth WHERE is_revoked = 0 AND auth_token_encrypted != ''
             ORDER BY authenticated_at DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((broker, ct, nonce, fct, fnonce, user_id, user_name, at)) = row else {
        return Ok(None);
    };
    let authenticated_at = at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let Some(authenticated_at) = authenticated_at else {
        revoke(conn, &broker)?;
        return Ok(None);
    };
    let auth_token = match security.decrypt(&ct, &nonce, &aad("auth_token", &broker)) {
        Ok(t) => t,
        Err(crate::error::AppError::Locked) => return Err(crate::error::AppError::Locked),
        Err(_) => {
            tracing::warn!(
                "Stored broker session for {} could not be read; revoking",
                broker
            );
            revoke(conn, &broker)?;
            return Ok(None);
        }
    };
    let feed_token = match (fct, fnonce) {
        (Some(c), Some(n)) if !c.is_empty() => {
            security.decrypt(&c, &n, &aad("feed_token", &broker)).ok()
        }
        _ => None,
    };
    Ok(Some(StoredBrokerSession {
        broker_id: broker,
        auth_token,
        feed_token,
        user_id,
        user_name,
        authenticated_at,
    }))
}

pub fn revoke(conn: &Connection, broker_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE auth SET is_revoked = 1, auth_token_encrypted = '', auth_token_nonce = '',
            feed_token_encrypted = NULL, feed_token_nonce = NULL, updated_at = datetime('now')
         WHERE broker_id = ?1",
        [broker_id],
    )?;
    Ok(())
}

pub fn revoke_all(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE auth SET is_revoked = 1, auth_token_encrypted = '', auth_token_nonce = '',
            feed_token_encrypted = NULL, feed_token_nonce = NULL, updated_at = datetime('now')
         WHERE is_revoked = 0",
        [],
    )?)
}

/// Revoke the stored sessions authenticated before `boundary` (the daily
/// boundary, SES-02), except those of brokers `keep` names (continuous
/// crypto sessions, SES-01). A row whose time cannot be read is revoked.
/// Returns how many were revoked.
pub fn revoke_before(
    conn: &Connection,
    boundary: DateTime<Utc>,
    keep: impl Fn(&str) -> bool,
) -> Result<usize> {
    let rows: Vec<(String, Option<String>)> = conn
        .prepare("SELECT broker_id, authenticated_at FROM auth WHERE is_revoked = 0")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut n = 0;
    for (broker, at) in rows {
        let before = at
            .and_then(|a| DateTime::parse_from_rfc3339(&a).ok())
            .is_none_or(|a| a.with_timezone(&Utc) < boundary);
        if before && !keep(&broker) {
            revoke(conn, &broker)?;
            n += 1;
        }
    }
    Ok(n)
}

/// The broker account id of the last session with `broker_id`, revoked or
/// not (the account a new sign-in is expected to belong to).
pub fn last_user_id(conn: &Connection, broker_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT user_id FROM auth WHERE broker_id = ?1",
            [broker_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|u| !u.trim().is_empty()))
}

/// Forget the account an ended session with `broker_id` was for, so the
/// next sign-in may be for another account (the trader saved that
/// broker's settings again). A live session keeps its account.
pub fn forget_account(conn: &Connection, broker_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE auth SET user_id = NULL WHERE broker_id = ?1 AND is_revoked = 1",
        [broker_id],
    )?;
    Ok(())
}

/// Whether a row exists but is revoked (for "expired" vs "never connected").
pub fn has_revoked(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM auth WHERE is_revoked = 1", [], |r| {
        r.get(0)
    })?;
    Ok(n > 0)
}

pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM auth", params![])?;
    Ok(())
}
