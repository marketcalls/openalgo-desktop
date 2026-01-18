//! OS Keychain integration using the keyring crate

use crate::error::{AppError, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

const SERVICE: &str = "openalgo-desktop";

/// Keychain manager for secure credential storage
pub struct KeychainManager;

impl KeychainManager {
    pub fn new() -> Self {
        Self
    }

    /// Get master encryption key from keychain
    pub fn get_master_key(&self) -> Result<Option<Vec<u8>>> {
        let entry = Entry::new(SERVICE, "master-key")
            .map_err(|e| AppError::Keychain(e))?;

        match entry.get_password() {
            Ok(key) => {
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &key,
                ).map_err(|e| AppError::Encryption(e.to_string()))?;
                Ok(Some(bytes))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::Keychain(e)),
        }
    }

    /// Store master encryption key in keychain
    pub fn store_master_key(&self, key: &[u8]) -> Result<()> {
        let entry = Entry::new(SERVICE, "master-key")
            .map_err(|e| AppError::Keychain(e))?;

        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            key,
        );

        entry.set_password(&encoded)
            .map_err(|e| AppError::Keychain(e))?;

        Ok(())
    }

    /// Get password pepper from keychain
    pub fn get_pepper(&self) -> Result<Option<Vec<u8>>> {
        let entry = Entry::new(SERVICE, "pepper")
            .map_err(|e| AppError::Keychain(e))?;

        match entry.get_password() {
            Ok(pepper) => {
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &pepper,
                ).map_err(|e| AppError::Encryption(e.to_string()))?;
                Ok(Some(bytes))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::Keychain(e)),
        }
    }

    /// Store password pepper in keychain
    pub fn store_pepper(&self, pepper: &[u8]) -> Result<()> {
        let entry = Entry::new(SERVICE, "pepper")
            .map_err(|e| AppError::Keychain(e))?;

        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            pepper,
        );

        entry.set_password(&encoded)
            .map_err(|e| AppError::Keychain(e))?;

        Ok(())
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
