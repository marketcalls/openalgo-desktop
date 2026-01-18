//! AES-256-GCM encryption

use crate::error::{AppError, Result};
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use rand::RngCore;

const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;

/// Encryption manager using AES-256-GCM
pub struct EncryptionManager {
    cipher: Aes256Gcm,
}

impl EncryptionManager {
    /// Create new encryption manager with provided key
    pub fn new(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_SIZE {
            return Err(AppError::Encryption(format!(
                "Invalid key size: expected {}, got {}",
                KEY_SIZE,
                key.len()
            )));
        }

        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| AppError::Encryption(e.to_string()))?;

        Ok(Self { cipher })
    }

    /// Generate a new random encryption key (used in tests)
    #[allow(dead_code)]
    pub fn generate_key() -> Vec<u8> {
        let mut key = vec![0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        key
    }

    /// Generate a new random nonce
    fn generate_nonce() -> [u8; NONCE_SIZE] {
        let mut nonce = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce);
        nonce
    }

    /// Encrypt plaintext, returns (ciphertext_base64, nonce_base64)
    pub fn encrypt(&self, plaintext: &str) -> Result<(String, String)> {
        let nonce_bytes = Self::generate_nonce();
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| AppError::Encryption(e.to_string()))?;

        let ciphertext_b64 = base64::engine::general_purpose::STANDARD.encode(&ciphertext);
        let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(&nonce_bytes);

        Ok((ciphertext_b64, nonce_b64))
    }

    /// Decrypt ciphertext
    pub fn decrypt(&self, ciphertext_b64: &str, nonce_b64: &str) -> Result<String> {
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(ciphertext_b64)
            .map_err(|e| AppError::Encryption(format!("Invalid ciphertext base64: {}", e)))?;

        let nonce_bytes = base64::engine::general_purpose::STANDARD
            .decode(nonce_b64)
            .map_err(|e| AppError::Encryption(format!("Invalid nonce base64: {}", e)))?;

        if nonce_bytes.len() != NONCE_SIZE {
            return Err(AppError::Encryption(format!(
                "Invalid nonce size: expected {}, got {}",
                NONCE_SIZE,
                nonce_bytes.len()
            )));
        }

        let nonce = Nonce::from_slice(&nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|e| AppError::Encryption(format!("Decryption failed: {}", e)))?;

        String::from_utf8(plaintext)
            .map_err(|e| AppError::Encryption(format!("Invalid UTF-8 in plaintext: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() {
        let key = EncryptionManager::generate_key();
        let manager = EncryptionManager::new(&key).unwrap();

        let plaintext = "Hello, World!";
        let (ciphertext, nonce) = manager.encrypt(plaintext).unwrap();
        let decrypted = manager.decrypt(&ciphertext, &nonce).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn test_different_nonces() {
        let key = EncryptionManager::generate_key();
        let manager = EncryptionManager::new(&key).unwrap();

        let plaintext = "Same text";
        let (ciphertext1, nonce1) = manager.encrypt(plaintext).unwrap();
        let (ciphertext2, nonce2) = manager.encrypt(plaintext).unwrap();

        // Same plaintext should produce different ciphertexts due to random nonces
        assert_ne!(ciphertext1, ciphertext2);
        assert_ne!(nonce1, nonce2);
    }
}
