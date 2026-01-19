//! File-based secure storage (replaces OS keychain)
//!
//! Stores master key and pepper in a local file with basic obfuscation.
//! This avoids OS keychain password prompts while still providing encryption.

use crate::error::{AppError, Result};
use base64::Engine;
use std::fs;
use std::path::PathBuf;

const SECRETS_FILE: &str = "secrets.dat";

/// File-based storage for app secrets
pub struct FileStorage {
    config_dir: PathBuf,
}

impl FileStorage {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    /// Get or create app secrets (master key + pepper)
    /// Stored in a local file instead of OS keychain
    pub fn get_or_create_secrets(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        let secrets_path = self.config_dir.join(SECRETS_FILE);

        if secrets_path.exists() {
            // Read existing secrets
            let data = fs::read(&secrets_path)
                .map_err(|e| AppError::Config(format!("Failed to read secrets: {}", e)))?;

            self.decode_secrets(&data)
        } else {
            // Generate new secrets
            use rand::RngCore;

            let mut master_key = vec![0u8; 32];
            let mut pepper = vec![0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut master_key);
            rand::rngs::OsRng.fill_bytes(&mut pepper);

            // Save to file
            let data = self.encode_secrets(&master_key, &pepper);

            // Ensure config directory exists
            if let Some(parent) = secrets_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| AppError::Config(format!("Failed to create config dir: {}", e)))?;
            }

            fs::write(&secrets_path, &data)
                .map_err(|e| AppError::Config(format!("Failed to write secrets: {}", e)))?;

            Ok((master_key, pepper))
        }
    }

    /// Encode secrets for storage (basic obfuscation)
    fn encode_secrets(&self, master_key: &[u8], pepper: &[u8]) -> Vec<u8> {
        // Simple format: base64(xor(master_key, obfuscation_key)) + ":" + base64(xor(pepper, obfuscation_key))
        let obfuscation_key = self.get_obfuscation_key();

        let obfuscated_master: Vec<u8> = master_key
            .iter()
            .zip(obfuscation_key.iter().cycle())
            .map(|(a, b)| a ^ b)
            .collect();

        let obfuscated_pepper: Vec<u8> = pepper
            .iter()
            .zip(obfuscation_key.iter().cycle())
            .map(|(a, b)| a ^ b)
            .collect();

        let encoded = format!(
            "{}:{}",
            base64::engine::general_purpose::STANDARD.encode(&obfuscated_master),
            base64::engine::general_purpose::STANDARD.encode(&obfuscated_pepper)
        );

        encoded.into_bytes()
    }

    /// Decode secrets from storage
    fn decode_secrets(&self, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let data_str = String::from_utf8(data.to_vec())
            .map_err(|e| AppError::Config(format!("Invalid secrets format: {}", e)))?;

        let parts: Vec<&str> = data_str.split(':').collect();
        if parts.len() != 2 {
            return Err(AppError::Config("Invalid secrets format".to_string()));
        }

        let obfuscation_key = self.get_obfuscation_key();

        let obfuscated_master = base64::engine::general_purpose::STANDARD
            .decode(parts[0])
            .map_err(|e| AppError::Config(format!("Failed to decode master key: {}", e)))?;

        let obfuscated_pepper = base64::engine::general_purpose::STANDARD
            .decode(parts[1])
            .map_err(|e| AppError::Config(format!("Failed to decode pepper: {}", e)))?;

        let master_key: Vec<u8> = obfuscated_master
            .iter()
            .zip(obfuscation_key.iter().cycle())
            .map(|(a, b)| a ^ b)
            .collect();

        let pepper: Vec<u8> = obfuscated_pepper
            .iter()
            .zip(obfuscation_key.iter().cycle())
            .map(|(a, b)| a ^ b)
            .collect();

        Ok((master_key, pepper))
    }

    /// Get obfuscation key (derived from app identifier)
    fn get_obfuscation_key(&self) -> Vec<u8> {
        // Use a fixed app-specific key for obfuscation
        // This provides basic protection without requiring user interaction
        let app_key = b"OpenAlgo-Desktop-v1.0-SecretKey!";
        app_key.to_vec()
    }
}
