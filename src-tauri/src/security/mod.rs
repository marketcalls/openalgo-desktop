//! Security module for encryption, hashing, and keychain access

mod keychain;
mod encryption;
mod hashing;

use crate::error::{AppError, Result};

/// Security manager combining all security features
pub struct SecurityManager {
    keychain: keychain::KeychainManager,
    encryption: encryption::EncryptionManager,
    hashing: hashing::HashingManager,
}

impl SecurityManager {
    /// Create new security manager
    pub fn new() -> Result<Self> {
        let keychain = keychain::KeychainManager::new();

        // Get or create master encryption key from keychain
        let master_key = match keychain.get_master_key()? {
            Some(key) => key,
            None => {
                let key = encryption::EncryptionManager::generate_key();
                keychain.store_master_key(&key)?;
                key
            }
        };

        let encryption = encryption::EncryptionManager::new(&master_key)?;

        // Get or create password pepper from keychain
        let pepper = match keychain.get_pepper()? {
            Some(p) => p,
            None => {
                let p = hashing::HashingManager::generate_pepper();
                keychain.store_pepper(&p)?;
                p
            }
        };

        let hashing = hashing::HashingManager::new(&pepper);

        Ok(Self {
            keychain,
            encryption,
            hashing,
        })
    }

    // ========== Encryption ==========

    /// Encrypt data
    pub fn encrypt(&self, plaintext: &str) -> Result<(String, String)> {
        self.encryption.encrypt(plaintext)
    }

    /// Decrypt data
    pub fn decrypt(&self, ciphertext: &str, nonce: &str) -> Result<String> {
        self.encryption.decrypt(ciphertext, nonce)
    }

    // ========== Hashing ==========

    /// Hash a password
    pub fn hash_password(&self, password: &str) -> Result<String> {
        self.hashing.hash_password(password)
    }

    /// Verify a password against a hash
    pub fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        self.hashing.verify_password(password, hash)
    }

    // ========== Keychain (Broker Credentials) ==========

    /// Store broker credentials in OS keychain
    pub fn store_broker_credentials(
        &self,
        broker_id: &str,
        api_key: &str,
        api_secret: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<()> {
        self.keychain.store_broker_credentials(broker_id, api_key, api_secret, client_id)
    }

    /// Get broker credentials from OS keychain
    pub fn get_broker_credentials(
        &self,
        broker_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        self.keychain.get_broker_credentials(broker_id)
    }

    /// Delete broker credentials from OS keychain
    pub fn delete_broker_credentials(&self, broker_id: &str) -> Result<()> {
        self.keychain.delete_broker_credentials(broker_id)
    }
}
