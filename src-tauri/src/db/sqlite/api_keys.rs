//! API key management with Argon2 hashing and AES-256-GCM encryption

use crate::error::{AppError, Result};
use crate::security::SecurityManager;
use rusqlite::{params, Connection};
use super::models::{ApiKey, ApiKeyInfo};

/// Generate a random 64-character hex API key
pub fn generate_api_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Mask API key for display (show first 8 and last 4 chars)
fn mask_api_key(key: &str) -> String {
    if key.len() <= 12 {
        "*".repeat(key.len())
    } else {
        format!("{}...{}", &key[..8], &key[key.len()-4..])
    }
}

/// Create a new API key and store it securely
///
/// The API key is:
/// 1. Hashed with Argon2id (for validation without decryption)
/// 2. Encrypted with AES-256-GCM (for potential recovery/display)
///
/// Returns the plaintext key (only shown once to user)
pub fn create_api_key(
    conn: &Connection,
    name: &str,
    permissions: &str,
    security: &SecurityManager,
) -> Result<(i64, String)> {
    // Check if name already exists
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM api_keys WHERE name = ?1)",
        params![name],
        |row| row.get(0),
    )?;

    if exists {
        return Err(AppError::Validation(format!("API key with name '{}' already exists", name)));
    }

    // Generate new API key
    let api_key = generate_api_key();

    // Hash the key with Argon2id (for validation)
    let key_hash = security.hash_password(&api_key)?;

    // Encrypt the key with AES-256-GCM (for potential recovery)
    let (encrypted_key, nonce) = security.encrypt(&api_key)?;

    // Store in database
    conn.execute(
        r#"
        INSERT INTO api_keys (name, key_hash, encrypted_key, nonce, permissions)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![name, key_hash, encrypted_key, nonce, permissions],
    )?;

    let id = conn.last_insert_rowid();

    tracing::info!("Created API key '{}' with id {}", name, id);

    Ok((id, api_key))
}

/// Validate an API key and return the associated name/user if valid
///
/// Uses Argon2 verification against stored hash
pub fn validate_api_key(
    conn: &Connection,
    api_key: &str,
    security: &SecurityManager,
) -> Result<ApiKey> {
    // Get all API keys and check against each hash
    // This is necessary because Argon2 uses random salts
    let mut stmt = conn.prepare(
        r#"
        SELECT id, name, key_hash, encrypted_key, nonce, permissions, created_at, last_used_at
        FROM api_keys
        "#
    )?;

    let keys: Vec<ApiKey> = stmt.query_map([], |row| {
        Ok(ApiKey {
            id: row.get(0)?,
            name: row.get(1)?,
            key_hash: row.get(2)?,
            encrypted_key: row.get(3)?,
            nonce: row.get(4)?,
            permissions: row.get(5)?,
            created_at: row.get(6)?,
            last_used_at: row.get(7)?,
        })
    })?.filter_map(|r| r.ok()).collect();

    // Check each key's hash
    for key in keys {
        if security.verify_password(api_key, &key.key_hash)? {
            // Update last_used_at
            let _ = conn.execute(
                "UPDATE api_keys SET last_used_at = datetime('now') WHERE id = ?1",
                params![key.id],
            );

            tracing::debug!("API key '{}' validated successfully", key.name);
            return Ok(key);
        }
    }

    Err(AppError::Auth("Invalid API key".to_string()))
}

/// List all API keys (with masked key values)
pub fn list_api_keys(
    conn: &Connection,
    security: &SecurityManager,
) -> Result<Vec<ApiKeyInfo>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, name, encrypted_key, nonce, permissions, created_at, last_used_at
        FROM api_keys
        ORDER BY created_at DESC
        "#
    )?;

    let keys: Vec<ApiKeyInfo> = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let encrypted_key: String = row.get(2)?;
        let nonce: String = row.get(3)?;
        let permissions: String = row.get(4)?;
        let created_at: String = row.get(5)?;
        let last_used_at: Option<String> = row.get(6)?;

        Ok((id, name, encrypted_key, nonce, permissions, created_at, last_used_at))
    })?.filter_map(|r| r.ok())
    .filter_map(|(id, name, encrypted_key, nonce, permissions, created_at, last_used_at)| {
        // Decrypt the key to get masked version
        let key_masked = match security.decrypt(&encrypted_key, &nonce) {
            Ok(decrypted) => mask_api_key(&decrypted),
            Err(_) => "****...****".to_string(),
        };

        Some(ApiKeyInfo {
            id,
            name,
            key_masked,
            permissions,
            created_at,
            last_used_at,
        })
    })
    .collect();

    Ok(keys)
}

/// Get API key by name
pub fn get_api_key_by_name(
    conn: &Connection,
    name: &str,
) -> Result<Option<ApiKey>> {
    let result = conn.query_row(
        r#"
        SELECT id, name, key_hash, encrypted_key, nonce, permissions, created_at, last_used_at
        FROM api_keys
        WHERE name = ?1
        "#,
        params![name],
        |row| {
            Ok(ApiKey {
                id: row.get(0)?,
                name: row.get(1)?,
                key_hash: row.get(2)?,
                encrypted_key: row.get(3)?,
                nonce: row.get(4)?,
                permissions: row.get(5)?,
                created_at: row.get(6)?,
                last_used_at: row.get(7)?,
            })
        },
    );

    match result {
        Ok(key) => Ok(Some(key)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete API key by name
pub fn delete_api_key(conn: &Connection, name: &str) -> Result<bool> {
    let rows_affected = conn.execute(
        "DELETE FROM api_keys WHERE name = ?1",
        params![name],
    )?;

    if rows_affected > 0 {
        tracing::info!("Deleted API key '{}'", name);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Delete API key by ID
pub fn delete_api_key_by_id(conn: &Connection, id: i64) -> Result<bool> {
    let rows_affected = conn.execute(
        "DELETE FROM api_keys WHERE id = ?1",
        params![id],
    )?;

    if rows_affected > 0 {
        tracing::info!("Deleted API key with id {}", id);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Count total API keys
pub fn count_api_keys(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM api_keys",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Get the first API key (decrypted) - for single-user desktop app
/// Returns the full decrypted API key for display to the user
pub fn get_first_api_key_decrypted(
    conn: &Connection,
    security: &SecurityManager,
) -> Result<Option<String>> {
    let result = conn.query_row(
        r#"
        SELECT encrypted_key, nonce
        FROM api_keys
        ORDER BY created_at ASC
        LIMIT 1
        "#,
        [],
        |row| {
            let encrypted_key: String = row.get(0)?;
            let nonce: String = row.get(1)?;
            Ok((encrypted_key, nonce))
        },
    );

    match result {
        Ok((encrypted_key, nonce)) => {
            let decrypted = security.decrypt(&encrypted_key, &nonce)?;
            Ok(Some(decrypted))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check if any API keys exist
pub fn has_api_key(conn: &Connection) -> Result<bool> {
    let count = count_api_keys(conn)?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_db() -> (Connection, SecurityManager) {
        let conn = Connection::open_in_memory().unwrap();

        // Create api_keys table
        conn.execute(
            r#"
            CREATE TABLE api_keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                key_hash TEXT NOT NULL,
                encrypted_key TEXT NOT NULL,
                nonce TEXT NOT NULL,
                permissions TEXT NOT NULL DEFAULT 'read',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_used_at TEXT
            )
            "#,
            [],
        ).unwrap();

        // Create SecurityManager
        let temp_dir = tempdir().unwrap();
        let config_dir = temp_dir.path().to_path_buf();
        let security = SecurityManager::new_for_testing(config_dir).unwrap();

        (conn, security)
    }

    #[test]
    fn test_generate_api_key() {
        let key = generate_api_key();
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_mask_api_key() {
        let key = "abcdef0123456789abcdef0123456789";
        let masked = mask_api_key(key);
        assert!(masked.starts_with("abcdef01"));
        assert!(masked.ends_with("6789"));
        assert!(masked.contains("..."));
    }

    #[test]
    fn test_create_and_validate_api_key() {
        let (conn, security) = create_test_db();

        // Create API key
        let (id, api_key) = create_api_key(&conn, "test-key", "read,write", &security).unwrap();
        assert!(id > 0);
        assert_eq!(api_key.len(), 64);

        // Validate API key
        let validated = validate_api_key(&conn, &api_key, &security).unwrap();
        assert_eq!(validated.name, "test-key");
        assert_eq!(validated.permissions, "read,write");
    }

    #[test]
    fn test_invalid_api_key() {
        let (conn, security) = create_test_db();

        // Create API key
        let _ = create_api_key(&conn, "test-key", "read", &security).unwrap();

        // Try to validate with wrong key
        let result = validate_api_key(&conn, "wrong_key_1234567890123456789012345678901234567890", &security);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_api_keys() {
        let (conn, security) = create_test_db();

        // Create multiple API keys
        create_api_key(&conn, "key1", "read", &security).unwrap();
        create_api_key(&conn, "key2", "read,write", &security).unwrap();

        // List keys
        let keys = list_api_keys(&conn, &security).unwrap();
        assert_eq!(keys.len(), 2);

        // Keys should be masked
        for key in &keys {
            assert!(key.key_masked.contains("..."));
        }
    }

    #[test]
    fn test_delete_api_key() {
        let (conn, security) = create_test_db();

        // Create and then delete
        create_api_key(&conn, "to-delete", "read", &security).unwrap();

        let deleted = delete_api_key(&conn, "to-delete").unwrap();
        assert!(deleted);

        // Should be gone
        let count = count_api_keys(&conn).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_duplicate_name_rejected() {
        let (conn, security) = create_test_db();

        // Create first key
        create_api_key(&conn, "unique-name", "read", &security).unwrap();

        // Try to create duplicate
        let result = create_api_key(&conn, "unique-name", "write", &security);
        assert!(result.is_err());
    }
}
