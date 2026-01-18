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
    let (encrypted_auth, nonce) = security.encrypt(auth_token)?;
    let encrypted_feed = feed_token
        .map(|ft| security.encrypt(ft))
        .transpose()?
        .map(|(enc, _)| enc);

    conn.execute(
        "INSERT INTO auth (broker_id, auth_token_encrypted, feed_token_encrypted, nonce)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(broker_id) DO UPDATE SET
           auth_token_encrypted = excluded.auth_token_encrypted,
           feed_token_encrypted = excluded.feed_token_encrypted,
           nonce = excluded.nonce,
           updated_at = datetime('now')",
        rusqlite::params![broker_id, encrypted_auth, encrypted_feed, nonce],
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
        "SELECT auth_token_encrypted, feed_token_encrypted, nonce FROM auth WHERE broker_id = ?",
        [broker_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    );

    match result {
        Ok((encrypted_auth, encrypted_feed, nonce)) => {
            let auth_token = security.decrypt(&encrypted_auth, &nonce)?;
            let feed_token = encrypted_feed
                .map(|enc| security.decrypt(&enc, &nonce))
                .transpose()?;
            Ok(Some((auth_token, feed_token)))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete auth token
pub fn delete_auth_token(conn: &Connection, broker_id: &str) -> Result<()> {
    conn.execute("DELETE FROM auth WHERE broker_id = ?", [broker_id])?;
    Ok(())
}
