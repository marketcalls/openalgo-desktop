//! OS Keychain integration using the keyring crate

use crate::error::{AppError, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

const SERVICE: &str = "openalgo-desktop";

/// Combined secrets stored in a single keychain entry
#[derive(Serialize, Deserialize)]
struct AppSecrets {
    master_key: String,  // base64 encoded
    pepper: String,      // base64 encoded
}

/// Keychain manager for secure credential storage
pub struct KeychainManager;

impl KeychainManager {
    pub fn new() -> Self {
        Self
    }

    /// Get or create app secrets (master key + pepper) from keychain
    /// This uses a single keychain entry to minimize password prompts
    pub fn get_or_create_secrets(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        let entry = Entry::new(SERVICE, "app-secrets")
            .map_err(|e| AppError::Keychain(e))?;

        match entry.get_password() {
            Ok(json) => {
                // Secrets exist, decode them
                let secrets: AppSecrets = serde_json::from_str(&json)
                    .map_err(|e| AppError::Serialization(e))?;

                let master_key = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &secrets.master_key,
                ).map_err(|e| AppError::Encryption(e.to_string()))?;

                let pepper = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &secrets.pepper,
                ).map_err(|e| AppError::Encryption(e.to_string()))?;

                Ok((master_key, pepper))
            }
            Err(keyring::Error::NoEntry) => {
                // First run - generate and store new secrets
                use rand::RngCore;

                let mut master_key = vec![0u8; 32];
                let mut pepper = vec![0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut master_key);
                rand::rngs::OsRng.fill_bytes(&mut pepper);

                let secrets = AppSecrets {
                    master_key: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &master_key,
                    ),
                    pepper: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &pepper,
                    ),
                };

                let json = serde_json::to_string(&secrets)
                    .map_err(|e| AppError::Serialization(e))?;

                entry.set_password(&json)
                    .map_err(|e| AppError::Keychain(e))?;

                Ok((master_key, pepper))
            }
            Err(e) => Err(AppError::Keychain(e)),
        }
    }

    /// Store broker credentials
    pub fn store_broker_credentials(
        &self,
        broker_id: &str,
        api_key: &str,
        api_secret: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct BrokerCreds {
            api_key: String,
            api_secret: Option<String>,
            client_id: Option<String>,
        }

        let creds = BrokerCreds {
            api_key: api_key.to_string(),
            api_secret: api_secret.map(|s| s.to_string()),
            client_id: client_id.map(|s| s.to_string()),
        };

        let json = serde_json::to_string(&creds)
            .map_err(|e| AppError::Serialization(e))?;

        let entry = Entry::new(SERVICE, &format!("broker-{}", broker_id))
            .map_err(|e| AppError::Keychain(e))?;

        entry.set_password(&json)
            .map_err(|e| AppError::Keychain(e))?;

        Ok(())
    }

    /// Get broker credentials
    pub fn get_broker_credentials(
        &self,
        broker_id: &str,
    ) -> Result<Option<(String, Option<String>, Option<String>)>> {
        let entry = Entry::new(SERVICE, &format!("broker-{}", broker_id))
            .map_err(|e| AppError::Keychain(e))?;

        match entry.get_password() {
            Ok(json) => {
                #[derive(Deserialize)]
                struct BrokerCreds {
                    api_key: String,
                    api_secret: Option<String>,
                    client_id: Option<String>,
                }

                let creds: BrokerCreds = serde_json::from_str(&json)
                    .map_err(|e| AppError::Serialization(e))?;

                Ok(Some((creds.api_key, creds.api_secret, creds.client_id)))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::Keychain(e)),
        }
    }

    /// Delete broker credentials
    pub fn delete_broker_credentials(&self, broker_id: &str) -> Result<()> {
        let entry = Entry::new(SERVICE, &format!("broker-{}", broker_id))
            .map_err(|e| AppError::Keychain(e))?;

        match entry.delete_password() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()), // Already deleted
            Err(e) => Err(AppError::Keychain(e)),
        }
    }
}

impl Default for KeychainManager {
    fn default() -> Self {
        Self::new()
    }
}
