//! One-column AES-256-GCM storage for the messaging secrets (bot token,
//! linked users' API keys, the WhatsApp session snapshot).
//!
//! The web keeps each of these in a single Fernet column; here the column
//! holds `v1:<nonce>:<ciphertext>` from the app's data key, with associated
//! data binding it to its table, column and row, so the web's column names
//! and shapes are kept.

use crate::error::{AppError, Result};
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};

const PREFIX: &str = "v1:";

pub fn seal(security: &SecurityManager, aad: &Aad, plaintext: &str) -> Result<String> {
    let (ct, nonce) = security.encrypt(plaintext, aad)?;
    Ok(format!("{}{}:{}", PREFIX, nonce, ct))
}

pub fn open(security: &SecurityManager, aad: &Aad, stored: &str) -> Result<Secret> {
    let rest = stored
        .strip_prefix(PREFIX)
        .ok_or_else(|| AppError::Encryption("unknown sealed format".into()))?;
    let (nonce, ct) = rest
        .split_once(':')
        .ok_or_else(|| AppError::Encryption("unknown sealed format".into()))?;
    security.decrypt(ct, nonce, aad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_binding() {
        let sec = SecurityManager::for_tests();
        let aad = Aad::new("bot_config", "token", "1");
        let s = seal(&sec, &aad, "123:ABC").unwrap();
        assert!(s.starts_with("v1:"));
        assert!(!s.contains("123:ABC"));
        assert_eq!(open(&sec, &aad, &s).unwrap().expose(), "123:ABC");
        // Moved to another row it no longer opens.
        assert!(open(&sec, &Aad::new("bot_config", "token", "2"), &s).is_err());
        assert!(open(&sec, &aad, "plain").is_err());
    }
}
