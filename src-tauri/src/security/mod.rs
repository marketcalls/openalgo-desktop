//! Security module for encryption, hashing, and keychain access

mod keychain;
mod encryption;
mod hashing;

use crate::error::Result;

/// Security manager combining all security features
pub struct SecurityManager {
    keychain: keychain::KeychainManager,
    encryption: encryption::EncryptionManager,
    hashing: hashing::HashingManager,
}

impl SecurityManager {
    /// Create new security manager
    ///
    /// Uses a single keychain entry for both master key and pepper
    /// to minimize password prompts on macOS/Windows/Linux
    pub fn new() -> Result<Self> {
        let keychain = keychain::KeychainManager::new();

        // Get or create both secrets in a single keychain access
        // This reduces password prompts from 3-4 to just 1
        let (master_key, pepper) = keychain.get_or_create_secrets()?;

        let encryption = encryption::EncryptionManager::new(&master_key)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test security manager with mock keychain behavior
    /// Note: This test uses the real keychain if available, otherwise it will fail
    /// In CI environments, you may need to skip these tests
    #[test]
    fn test_encryption_round_trip() {
        // Use the encryption manager directly to avoid keychain dependency in tests
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        let plaintext = "test_auth_token_12345";
        let (ciphertext, nonce) = manager.encrypt(plaintext).unwrap();
        let decrypted = manager.decrypt(&ciphertext, &nonce).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn test_encryption_different_nonces() {
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        let plaintext = "same_text";
        let (cipher1, nonce1) = manager.encrypt(plaintext).unwrap();
        let (cipher2, nonce2) = manager.encrypt(plaintext).unwrap();

        // Same plaintext should produce different ciphertexts due to random nonces
        assert_ne!(cipher1, cipher2);
        assert_ne!(nonce1, nonce2);

        // But both should decrypt correctly
        assert_eq!(plaintext, manager.decrypt(&cipher1, &nonce1).unwrap());
        assert_eq!(plaintext, manager.decrypt(&cipher2, &nonce2).unwrap());
    }

    #[test]
    fn test_wrong_nonce_fails() {
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        let plaintext = "sensitive_data";
        let (ciphertext, _correct_nonce) = manager.encrypt(plaintext).unwrap();
        let (_other_cipher, wrong_nonce) = manager.encrypt("other").unwrap();

        // Decryption with wrong nonce should fail
        let result = manager.decrypt(&ciphertext, &wrong_nonce);
        assert!(result.is_err());
    }

    #[test]
    fn test_password_hash_verify() {
        let pepper = hashing::HashingManager::generate_pepper();
        let manager = hashing::HashingManager::new(&pepper);

        let password = "my_secure_password!123";
        let hash = manager.hash_password(password).unwrap();

        // Correct password should verify
        assert!(manager.verify_password(password, &hash).unwrap());

        // Wrong password should not verify
        assert!(!manager.verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_password_hash_unique_salts() {
        let pepper = hashing::HashingManager::generate_pepper();
        let manager = hashing::HashingManager::new(&pepper);

        let password = "same_password";
        let hash1 = manager.hash_password(password).unwrap();
        let hash2 = manager.hash_password(password).unwrap();

        // Same password should produce different hashes (due to random salts)
        assert_ne!(hash1, hash2);

        // But both should verify
        assert!(manager.verify_password(password, &hash1).unwrap());
        assert!(manager.verify_password(password, &hash2).unwrap());
    }

    #[test]
    fn test_encryption_empty_string() {
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        let plaintext = "";
        let (ciphertext, nonce) = manager.encrypt(plaintext).unwrap();
        let decrypted = manager.decrypt(&ciphertext, &nonce).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn test_encryption_unicode() {
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        let plaintext = "Token with unicode: \u{1F4B0} \u{1F3C6}";
        let (ciphertext, nonce) = manager.encrypt(plaintext).unwrap();
        let decrypted = manager.decrypt(&ciphertext, &nonce).unwrap();

        assert_eq!(plaintext, decrypted);
    }

    #[test]
    fn test_encryption_long_text() {
        let key = encryption::EncryptionManager::generate_key();
        let manager = encryption::EncryptionManager::new(&key).unwrap();

        // Create a long token (1KB)
        let plaintext = "x".repeat(1024);
        let (ciphertext, nonce) = manager.encrypt(&plaintext).unwrap();
        let decrypted = manager.decrypt(&ciphertext, &nonce).unwrap();

        assert_eq!(plaintext, decrypted);
    }
}
