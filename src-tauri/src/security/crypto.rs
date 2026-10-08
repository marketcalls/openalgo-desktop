//! AES-256-GCM with associated data.
//!
//! Every stored ciphertext is bound to the table, column and row it lives in
//! (`Aad`). Copying a ciphertext/nonce pair into another column or another
//! row makes decryption fail instead of silently returning the wrong secret.

use crate::error::{AppError, Result};
use crate::security::secret::{Secret, SecretBytes};
use aes_gcm::{
    aead::{Aead, KeyInit, OsRng, Payload},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use rand::Rng;

pub const NONCE_SIZE: usize = 12;
pub const KEY_SIZE: usize = 32;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Associated data identifying where a ciphertext lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aad(String);

impl Aad {
    pub fn new(table: &str, column: &str, row: &str) -> Self {
        Aad(format!("openalgo:v1:{}:{}:{}", table, column, row))
    }

    /// Fixed associated data for non-row payloads such as the key vault.
    pub fn fixed(label: &str) -> Self {
        Aad(format!("openalgo:v1:{}", label))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

pub fn generate_key() -> SecretBytes {
    let key: [u8; KEY_SIZE] = OsRng.gen();
    SecretBytes::new(key.to_vec())
}

pub struct DataCipher {
    cipher: Aes256Gcm,
}

impl DataCipher {
    pub fn new(key: &[u8]) -> Result<Self> {
        if key.len() != KEY_SIZE {
            return Err(AppError::Encryption(format!(
                "invalid key size {}",
                key.len()
            )));
        }
        let cipher =
            Aes256Gcm::new_from_slice(key).map_err(|e| AppError::Encryption(e.to_string()))?;
        Ok(Self { cipher })
    }

    /// Encrypt; returns (ciphertext_b64, nonce_b64).
    pub fn encrypt(&self, plaintext: &[u8], aad: &Aad) -> Result<(String, String)> {
        let nonce_bytes: [u8; NONCE_SIZE] = OsRng.gen();
        let ct = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| AppError::Encryption(e.to_string()))?;
        Ok((B64.encode(ct), B64.encode(nonce_bytes)))
    }

    pub fn decrypt_bytes(&self, ct_b64: &str, nonce_b64: &str, aad: &Aad) -> Result<SecretBytes> {
        let ct = B64
            .decode(ct_b64)
            .map_err(|_| AppError::Encryption("invalid ciphertext encoding".into()))?;
        let nonce = B64
            .decode(nonce_b64)
            .map_err(|_| AppError::Encryption("invalid nonce encoding".into()))?;
        if nonce.len() != NONCE_SIZE {
            return Err(AppError::Encryption("invalid nonce size".into()));
        }
        let pt = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| AppError::Encryption("decryption failed".into()))?;
        Ok(SecretBytes::new(pt))
    }

    pub fn decrypt(&self, ct_b64: &str, nonce_b64: &str, aad: &Aad) -> Result<Secret> {
        let bytes = self.decrypt_bytes(ct_b64, nonce_b64, aad)?;
        let s = std::str::from_utf8(bytes.expose())
            .map_err(|_| AppError::Encryption("plaintext is not UTF-8".into()))?;
        Ok(Secret::new(s))
    }

    /// Decrypt a ciphertext written before associated data existed. Used only
    /// by the one-time migration that re-encrypts old rows with AAD.
    pub fn decrypt_legacy(&self, ct_b64: &str, nonce_b64: &str) -> Result<Secret> {
        let ct = B64
            .decode(ct_b64)
            .map_err(|_| AppError::Encryption("invalid ciphertext encoding".into()))?;
        let nonce = B64
            .decode(nonce_b64)
            .map_err(|_| AppError::Encryption("invalid nonce encoding".into()))?;
        if nonce.len() != NONCE_SIZE {
            return Err(AppError::Encryption("invalid nonce size".into()));
        }
        let pt = self
            .cipher
            .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
            .map_err(|_| AppError::Encryption("decryption failed".into()))?;
        String::from_utf8(pt)
            .map(Secret::new)
            .map_err(|_| AppError::Encryption("plaintext is not UTF-8".into()))
    }

    /// Encrypt without AAD. Test helper that reproduces pre-AAD ciphertexts.
    #[cfg(test)]
    pub fn encrypt_legacy(&self, plaintext: &str) -> (String, String) {
        let nonce_bytes: [u8; NONCE_SIZE] = OsRng.gen();
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
            .unwrap();
        (B64.encode(ct), B64.encode(nonce_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> DataCipher {
        DataCipher::new(generate_key().expose()).unwrap()
    }

    #[test]
    fn round_trip_with_aad() {
        let c = cipher();
        let aad = Aad::new("auth", "auth_token", "zerodha");
        let (ct, n) = c.encrypt(b"token-123", &aad).unwrap();
        assert_eq!(c.decrypt(&ct, &n, &aad).unwrap().expose(), "token-123");
    }

    #[test]
    fn column_swap_is_rejected() {
        let c = cipher();
        let key_aad = Aad::new("broker_credentials", "api_key", "zerodha");
        let secret_aad = Aad::new("broker_credentials", "api_secret", "zerodha");
        let (ct, n) = c.encrypt(b"the-secret", &secret_aad).unwrap();
        assert!(c.decrypt(&ct, &n, &key_aad).is_err());
    }

    #[test]
    fn row_swap_is_rejected() {
        let c = cipher();
        let (ct, n) = c
            .encrypt(b"tok", &Aad::new("auth", "auth_token", "fyers"))
            .unwrap();
        assert!(c
            .decrypt(&ct, &n, &Aad::new("auth", "auth_token", "zerodha"))
            .is_err());
    }

    #[test]
    fn wrong_key_is_rejected() {
        let a = cipher();
        let b = cipher();
        let aad = Aad::fixed("x");
        let (ct, n) = a.encrypt(b"v", &aad).unwrap();
        assert!(b.decrypt(&ct, &n, &aad).is_err());
    }

    #[test]
    fn fresh_nonce_per_call() {
        let c = cipher();
        let aad = Aad::fixed("x");
        let (c1, n1) = c.encrypt(b"same", &aad).unwrap();
        let (c2, n2) = c.encrypt(b"same", &aad).unwrap();
        assert_ne!(n1, n2);
        assert_ne!(c1, c2);
    }

    #[test]
    fn legacy_round_trip() {
        let c = cipher();
        let (ct, n) = c.encrypt_legacy("old");
        assert_eq!(c.decrypt_legacy(&ct, &n).unwrap().expose(), "old");
        // A legacy ciphertext does not decrypt under any AAD.
        assert!(c.decrypt(&ct, &n, &Aad::fixed("x")).is_err());
    }

    #[test]
    fn empty_and_unicode() {
        let c = cipher();
        let aad = Aad::fixed("x");
        for s in ["", "Token \u{20B9} \u{0928}\u{092E}"] {
            let (ct, n) = c.encrypt(s.as_bytes(), &aad).unwrap();
            assert_eq!(c.decrypt(&ct, &n, &aad).unwrap().expose(), s);
        }
    }
}
