//! Argon2id password hashing

use crate::error::{AppError, Result};
use argon2::{
    password_hash::{
        rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2, Params, Version,
};

const PEPPER_SIZE: usize = 32;

/// Hashing manager using Argon2id
pub struct HashingManager {
    pepper: Vec<u8>,
}

impl HashingManager {
    /// Create new hashing manager with pepper
    pub fn new(pepper: &[u8]) -> Self {
        Self {
            pepper: pepper.to_vec(),
        }
    }

    /// Generate a new random pepper
    pub fn generate_pepper() -> Vec<u8> {
        use rand::RngCore;
        let mut pepper = vec![0u8; PEPPER_SIZE];
        OsRng.fill_bytes(&mut pepper);
        pepper
    }

    /// Hash a password with Argon2id
    pub fn hash_password(&self, password: &str) -> Result<String> {
        // Combine password with pepper
        let peppered = self.pepper_password(password);

        // Use Argon2id with secure parameters
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            Params::new(
                19456,  // m_cost (19 MiB)
                2,      // t_cost (2 iterations)
                1,      // p_cost (1 thread)
                None,   // output length (default 32)
            )
            .map_err(|e| AppError::Internal(format!("Invalid Argon2 params: {}", e)))?,
        );

        let salt = SaltString::generate(&mut OsRng);

        let hash = argon2
            .hash_password(peppered.as_bytes(), &salt)
            .map_err(|e| AppError::Internal(format!("Password hashing failed: {}", e)))?;

        Ok(hash.to_string())
    }

    /// Verify a password against a hash
    pub fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        let peppered = self.pepper_password(password);

        let parsed_hash = PasswordHash::new(hash)
            .map_err(|e| AppError::Internal(format!("Invalid password hash format: {}", e)))?;

        let argon2 = Argon2::default();

        match argon2.verify_password(peppered.as_bytes(), &parsed_hash) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(AppError::Internal(format!("Password verification failed: {}", e))),
        }
    }

    /// Combine password with pepper
    fn pepper_password(&self, password: &str) -> String {
        use base64::Engine;
        let pepper_b64 = base64::engine::general_purpose::STANDARD.encode(&self.pepper);
        format!("{}{}", password, pepper_b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify() {
        let pepper = HashingManager::generate_pepper();
        let manager = HashingManager::new(&pepper);

        let password = "my_secure_password123!";
        let hash = manager.hash_password(password).unwrap();

        assert!(manager.verify_password(password, &hash).unwrap());
        assert!(!manager.verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_different_hashes() {
        let pepper = HashingManager::generate_pepper();
        let manager = HashingManager::new(&pepper);

        let password = "same_password";
        let hash1 = manager.hash_password(password).unwrap();
        let hash2 = manager.hash_password(password).unwrap();

        // Same password should produce different hashes due to random salts
        assert_ne!(hash1, hash2);

        // But both should verify correctly
        assert!(manager.verify_password(password, &hash1).unwrap());
        assert!(manager.verify_password(password, &hash2).unwrap());
    }
}
