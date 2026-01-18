//! Auth token storage

use crate::error::Result;
use crate::security::SecurityManager;
use rusqlite::Connection;

/// Store encrypted auth token
pub fn store_auth_token(
    conn: &Connection,
    broker_id: &str,
    auth_token: &str,
    feed_token: Option<&str>,
    security: &SecurityManager,
) -> Result<()> {
    let (encrypted_auth, auth_nonce) = security.encrypt(auth_token)?;

    // Encrypt feed_token with its own nonce (if present)
    let (encrypted_feed, feed_nonce) = match feed_token {
        Some(ft) => {
            let (enc, nonce) = security.encrypt(ft)?;
            (Some(enc), Some(nonce))
        }
        None => (None, None),
    };

    conn.execute(
        "INSERT INTO auth (broker_id, auth_token_encrypted, feed_token_encrypted, auth_token_nonce, feed_token_nonce)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(broker_id) DO UPDATE SET
           auth_token_encrypted = excluded.auth_token_encrypted,
           feed_token_encrypted = excluded.feed_token_encrypted,
           auth_token_nonce = excluded.auth_token_nonce,
           feed_token_nonce = excluded.feed_token_nonce,
           updated_at = datetime('now')",
        rusqlite::params![broker_id, encrypted_auth, encrypted_feed, auth_nonce, feed_nonce],
    )?;

    Ok(())
}

/// Get decrypted auth token
pub fn get_auth_token(
    conn: &Connection,
    broker_id: &str,
    security: &SecurityManager,
) -> Result<Option<(String, Option<String>)>> {
    let result = conn.query_row(
        "SELECT auth_token_encrypted, feed_token_encrypted, auth_token_nonce, feed_token_nonce
         FROM auth WHERE broker_id = ?",
        [broker_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        },
    );

    match result {
        Ok((encrypted_auth, encrypted_feed, auth_nonce, feed_nonce)) => {
            // Decrypt auth_token with its nonce
            let auth_token = security.decrypt(&encrypted_auth, &auth_nonce)?;

            // Decrypt feed_token with its own nonce (if present)
            let feed_token = match (encrypted_feed, feed_nonce) {
                (Some(enc), Some(nonce)) => Some(security.decrypt(&enc, &nonce)?),
                _ => None,
            };

            Ok(Some((auth_token, feed_token)))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete auth token for a specific broker
pub fn delete_auth_token(conn: &Connection, broker_id: &str) -> Result<()> {
    conn.execute("DELETE FROM auth WHERE broker_id = ?", [broker_id])?;
    Ok(())
}

/// Clear all auth tokens (used by auto-logout)
pub fn clear_all_auth_tokens(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM auth", [])?;
    Ok(())
}
