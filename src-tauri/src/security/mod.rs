//! Key material, encryption and hashing.
//!
//! # Where keys live
//!
//! A random 256-bit data key (AES-256-GCM) and a random 256-bit pepper
//! (Argon2 pepper and HMAC key for the API-key index) are created on first
//! run.
//!
//! * **Keychain mode** (default): both live in the OS keychain under the
//!   service `com.openalgo.desktop`, accounts `data-key` and
//!   `api-key-pepper`. They are loaded at startup; nothing secret is on disk.
//! * **Password mode** (fallback): when the machine has no usable keychain
//!   (headless Linux, some Raspberry Pi images) both keys are wrapped with a
//!   key derived from the trader's OpenAlgo password (Argon2id) and stored in
//!   `vault.json` (0600). Until the trader signs in after a start the keys are
//!   not in memory: broker sessions cannot be resumed and `/api/v1` answers
//!   "Invalid openalgo apikey". Password reset by TOTP cannot open the vault
//!   either, because the TOTP secret is itself encrypted with the data key;
//!   in this mode only "reset account" recovers a forgotten password. The UI
//!   reads `key_mode` from `/auth/session-status` and says so.
//!
//! # Migration from `secrets.dat`
//!
//! Older builds kept both keys in `secrets.dat`, XOR-ed with a constant. On
//! first start the keys are read once, moved to the keychain (or held in
//! memory until the first sign-in creates the vault), every stored ciphertext
//! is re-encrypted with associated data, and the file is deleted.

pub mod crypto;
pub mod fsperm;
pub mod hashing;
pub mod keystore;
pub mod legacy;
pub mod secret;
pub mod totp;

use crate::error::{AppError, Result};
use base64::Engine;
use crypto::{Aad, DataCipher};
use keystore::{KeyStore, KeyStoreError, DATA_KEY, PEPPER};
use parking_lot::{Mutex, RwLock};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use secret::{Secret, SecretBytes};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
pub const VAULT_FILE: &str = "vault.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    Keychain,
    Password,
}

struct Keys {
    cipher: DataCipher,
    pepper: SecretBytes,
    raw_key: SecretBytes,
}

impl Keys {
    fn new(key: SecretBytes, pepper: SecretBytes) -> Result<Self> {
        Ok(Self {
            cipher: DataCipher::new(key.expose())?,
            pepper,
            raw_key: key,
        })
    }

    fn random() -> Result<Self> {
        let mut pepper = vec![0u8; hashing::PEPPER_SIZE];
        rand::rngs::OsRng.fill_bytes(&mut pepper);
        Self::new(crypto::generate_key(), SecretBytes::new(pepper))
    }
}

#[derive(Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    kdf: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

pub struct SecurityManager {
    mode: KeyMode,
    keys: RwLock<Option<Arc<Keys>>>,
    store: Arc<dyn KeyStore>,
    data_dir: PathBuf,
    /// True when keys came from `secrets.dat` and stored rows still need
    /// re-encryption with associated data.
    legacy_pending: Mutex<bool>,
}

impl std::fmt::Debug for SecurityManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityManager")
            .field("mode", &self.mode)
            .field("unlocked", &self.is_unlocked())
            .finish()
    }
}

impl SecurityManager {
    /// Load or create key material for `data_dir` using `store`.
    pub fn open(data_dir: &Path, store: Arc<dyn KeyStore>) -> Result<Self> {
        let vault_exists = data_dir.join(VAULT_FILE).exists();
        let legacy = legacy::read(data_dir)?;

        let keychain = if vault_exists {
            None
        } else {
            match (store.get(DATA_KEY), store.get(PEPPER)) {
                (Ok(k), Ok(p)) => Some((k, p)),
                (Err(KeyStoreError::Unavailable(e)), _)
                | (_, Err(KeyStoreError::Unavailable(e))) => {
                    tracing::warn!("No usable keychain, using password-derived key: {}", e);
                    None
                }
                (Err(e), _) | (_, Err(e)) => return Err(AppError::Keychain(e.to_string())),
            }
        };

        let mgr = |mode, keys: Option<Keys>, legacy_pending| Self {
            mode,
            keys: RwLock::new(keys.map(Arc::new)),
            store: store.clone(),
            data_dir: data_dir.to_path_buf(),
            legacy_pending: Mutex::new(legacy_pending),
        };

        match keychain {
            Some((Some(k), Some(p))) => {
                let keys = Keys::new(decode_b64(&k)?, decode_b64(&p)?)?;
                // Keys already moved on an earlier run that was interrupted
                // before the file was removed: still re-encrypt and delete.
                Ok(mgr(KeyMode::Keychain, Some(keys), legacy.is_some()))
            }
            Some(_) => {
                let (keys, from_legacy) = match legacy {
                    Some((k, p)) => (Keys::new(k, p)?, true),
                    None => (Keys::random()?, false),
                };
                store
                    .set(DATA_KEY, &B64.encode(keys.raw_key.expose()))
                    .and_then(|_| store.set(PEPPER, &B64.encode(keys.pepper.expose())))
                    .map_err(|e| AppError::Keychain(e.to_string()))?;
                // Some secret services accept a write and return nothing on
                // read; refuse to continue with keys we could not read back.
                match store.get(DATA_KEY) {
                    Ok(Some(v)) if v == B64.encode(keys.raw_key.expose()) => {}
                    _ => {
                        return Err(AppError::Keychain(
                            "keychain did not keep the data key".into(),
                        ))
                    }
                }
                tracing::info!("Data key stored in the {}", store.kind());
                Ok(mgr(KeyMode::Keychain, Some(keys), from_legacy))
            }
            None => {
                // Password mode. Legacy keys stay in memory until the first
                // sign-in wraps them into the vault.
                let keys = match legacy {
                    Some((k, p)) => Some(Keys::new(k, p)?),
                    None => None,
                };
                let pending = keys.is_some();
                Ok(mgr(KeyMode::Password, keys, pending))
            }
        }
    }

    /// Random in-memory keys, keychain mode. For tests and the in-process
    /// HTTP harness.
    pub fn for_tests() -> Self {
        let keys = Keys::random().ok().map(Arc::new);
        Self {
            mode: KeyMode::Keychain,
            keys: RwLock::new(keys),
            store: Arc::new(keystore::MemoryKeyStore::new()),
            data_dir: std::env::temp_dir(),
            legacy_pending: Mutex::new(false),
        }
    }

    pub fn mode(&self) -> KeyMode {
        self.mode
    }

    pub fn is_unlocked(&self) -> bool {
        self.keys.read().is_some()
    }

    fn keys(&self) -> Result<Arc<Keys>> {
        self.keys.read().clone().ok_or(AppError::Locked)
    }

    fn vault_path(&self) -> PathBuf {
        self.data_dir.join(VAULT_FILE)
    }

    pub fn has_vault(&self) -> bool {
        self.vault_path().exists()
    }

    /// Password mode: called at setup (no vault yet) and on every sign-in.
    /// Creates the vault when missing, otherwise opens it. Returns false when
    /// the password does not open the vault. Keychain mode: always true.
    pub fn unlock_with_password(&self, password: &str) -> Result<bool> {
        if self.mode == KeyMode::Keychain {
            return Ok(true);
        }
        if !self.has_vault() {
            let keys = match self.keys.read().clone() {
                Some(k) => k,
                None => Arc::new(Keys::random()?),
            };
            self.write_vault(&keys, password)?;
            *self.keys.write() = Some(keys);
            return Ok(true);
        }
        let raw = std::fs::read(self.vault_path())?;
        let vault: VaultFile = serde_json::from_slice(&raw)
            .map_err(|_| AppError::Encryption("vault file is damaged".into()))?;
        let salt = B64
            .decode(&vault.salt)
            .map_err(|_| AppError::Encryption("vault file is damaged".into()))?;
        let kek = hashing::derive_kek(password, &salt)?;
        let cipher = DataCipher::new(&kek)?;
        let plain =
            match cipher.decrypt_bytes(&vault.ciphertext, &vault.nonce, &Aad::fixed("vault")) {
                Ok(p) => p,
                Err(_) => return Ok(false),
            };
        let bytes = plain.expose();
        if bytes.len() != crypto::KEY_SIZE + hashing::PEPPER_SIZE {
            return Err(AppError::Encryption("vault file is damaged".into()));
        }
        let keys = Keys::new(
            SecretBytes::new(bytes[..crypto::KEY_SIZE].to_vec()),
            SecretBytes::new(bytes[crypto::KEY_SIZE..].to_vec()),
        )?;
        *self.keys.write() = Some(Arc::new(keys));
        Ok(true)
    }

    /// Password mode: re-wrap the vault after a password change.
    pub fn rewrap(&self, new_password: &str) -> Result<()> {
        if self.mode == KeyMode::Keychain {
            return Ok(());
        }
        let keys = self.keys()?;
        self.write_vault(&keys, new_password)
    }

    fn write_vault(&self, keys: &Keys, password: &str) -> Result<()> {
        let salt: [u8; 16] = rand::Rng::gen(&mut rand::rngs::OsRng);
        let kek = hashing::derive_kek(password, &salt)?;
        let cipher = DataCipher::new(&kek)?;
        let mut blob = keys.raw_key.expose().to_vec();
        blob.extend_from_slice(keys.pepper.expose());
        let (ct, nonce) = cipher.encrypt(&blob, &Aad::fixed("vault"))?;
        zeroize::Zeroize::zeroize(&mut blob);
        let vault = VaultFile {
            version: 1,
            kdf: "argon2id-m19456-t2-p1".into(),
            salt: B64.encode(salt),
            nonce,
            ciphertext: ct,
        };
        fsperm::write_private_file(&self.vault_path(), &serde_json::to_vec(&vault)?)?;
        Ok(())
    }

    /// Replace both keys with new random ones (account reset). Every existing
    /// ciphertext and password hash becomes unusable, which is the point.
    pub fn rotate(&self) -> Result<()> {
        let keys = Keys::random()?;
        match self.mode {
            KeyMode::Keychain => {
                self.store
                    .set(DATA_KEY, &B64.encode(keys.raw_key.expose()))
                    .and_then(|_| self.store.set(PEPPER, &B64.encode(keys.pepper.expose())))
                    .map_err(|e| AppError::Keychain(e.to_string()))?;
            }
            KeyMode::Password => {
                let p = self.vault_path();
                if p.exists() {
                    std::fs::remove_file(p)?;
                }
            }
        }
        *self.keys.write() = Some(Arc::new(keys));
        *self.legacy_pending.lock() = false;
        legacy::remove(&self.data_dir)?;
        Ok(())
    }

    /// Whether stored rows still need the one-time re-encryption with AAD.
    pub fn legacy_migration_pending(&self) -> bool {
        *self.legacy_pending.lock()
    }

    /// Called after the re-encryption migration succeeded.
    pub fn finish_legacy_migration(&self) -> Result<()> {
        // In password mode the file can only go once the vault exists,
        // otherwise a restart before sign-in would lose the keys.
        if self.mode == KeyMode::Password && !self.has_vault() {
            return Ok(());
        }
        legacy::remove(&self.data_dir)?;
        *self.legacy_pending.lock() = false;
        Ok(())
    }

    // ---------------------------------------------------------------- crypto

    pub fn encrypt(&self, plaintext: &str, aad: &Aad) -> Result<(String, String)> {
        self.keys()?.cipher.encrypt(plaintext.as_bytes(), aad)
    }

    pub fn decrypt(&self, ct: &str, nonce: &str, aad: &Aad) -> Result<Secret> {
        self.keys()?.cipher.decrypt(ct, nonce, aad)
    }

    /// Pre-AAD ciphertexts, migration only.
    pub fn decrypt_legacy(&self, ct: &str, nonce: &str) -> Result<Secret> {
        self.keys()?.cipher.decrypt_legacy(ct, nonce)
    }

    pub fn hash_password(&self, password: &str) -> Result<String> {
        hashing::hash_password(self.keys()?.pepper.expose(), password)
    }

    pub fn verify_password(&self, password: &str, hash: &str) -> Result<bool> {
        hashing::verify_password(self.keys()?.pepper.expose(), password, hash)
    }

    pub fn api_key_lookup(&self, api_key: &str) -> Result<String> {
        Ok(hashing::lookup_hmac(self.keys()?.pepper.expose(), api_key))
    }
}

fn decode_b64(s: &str) -> Result<SecretBytes> {
    B64.decode(s.trim())
        .map(SecretBytes::new)
        .map_err(|_| AppError::Keychain("stored key has an unknown format".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use keystore::{MemoryKeyStore, UnavailableKeyStore};

    #[test]
    fn keychain_mode_creates_and_reloads_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
        let a = SecurityManager::open(dir.path(), store.clone()).unwrap();
        assert_eq!(a.mode(), KeyMode::Keychain);
        let aad = Aad::new("t", "c", "1");
        let (ct, n) = a.encrypt("hello", &aad).unwrap();
        let h = a.hash_password("Pw@12345").unwrap();

        let b = SecurityManager::open(dir.path(), store).unwrap();
        assert_eq!(b.decrypt(&ct, &n, &aad).unwrap().expose(), "hello");
        assert!(b.verify_password("Pw@12345", &h).unwrap());
        assert_eq!(
            a.api_key_lookup("k").unwrap(),
            b.api_key_lookup("k").unwrap()
        );
        assert!(!dir.path().join(VAULT_FILE).exists());
    }

    #[test]
    fn legacy_file_is_migrated_into_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        legacy::write_for_test(dir.path(), &[3u8; 32], &[4u8; 32]);
        let store: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
        let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
        assert!(m.legacy_migration_pending());
        assert_eq!(store.get(DATA_KEY).unwrap().unwrap(), B64.encode([3u8; 32]));
        // A ciphertext written by the old build decrypts through the legacy path.
        let old = DataCipher::new(&[3u8; 32]).unwrap();
        let (ct, n) = old.encrypt_legacy("broker-token");
        assert_eq!(m.decrypt_legacy(&ct, &n).unwrap().expose(), "broker-token");
        m.finish_legacy_migration().unwrap();
        assert!(!legacy::path(dir.path()).exists());
        assert!(!m.legacy_migration_pending());
    }

    #[test]
    fn password_mode_is_locked_until_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn KeyStore> = Arc::new(UnavailableKeyStore);
        let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
        assert_eq!(m.mode(), KeyMode::Password);
        assert!(!m.is_unlocked());
        assert!(matches!(
            m.encrypt("x", &Aad::fixed("x")),
            Err(AppError::Locked)
        ));
        // Setup creates the vault.
        assert!(m.unlock_with_password("First@123").unwrap());
        let (ct, n) = m.encrypt("x", &Aad::fixed("x")).unwrap();
        assert!(dir.path().join(VAULT_FILE).exists());

        // Next start: locked; wrong password does not open; right one does.
        let m2 = SecurityManager::open(dir.path(), store).unwrap();
        assert!(!m2.is_unlocked());
        assert!(!m2.unlock_with_password("Wrong@123").unwrap());
        assert!(m2.unlock_with_password("First@123").unwrap());
        assert_eq!(m2.decrypt(&ct, &n, &Aad::fixed("x")).unwrap().expose(), "x");

        // Password change re-wraps.
        m2.rewrap("Second@123").unwrap();
        let m3 = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(!m3.unlock_with_password("First@123").unwrap());
        assert!(m3.unlock_with_password("Second@123").unwrap());
    }

    #[test]
    fn password_mode_keeps_legacy_keys_until_vault_exists() {
        let dir = tempfile::tempdir().unwrap();
        legacy::write_for_test(dir.path(), &[5u8; 32], &[6u8; 32]);
        let m = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(m.is_unlocked());
        m.finish_legacy_migration().unwrap();
        assert!(
            legacy::path(dir.path()).exists(),
            "kept until the vault exists"
        );
        m.unlock_with_password("Pw@12345").unwrap();
        m.finish_legacy_migration().unwrap();
        assert!(!legacy::path(dir.path()).exists());
        let m2 = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(m2.unlock_with_password("Pw@12345").unwrap());
        let old = DataCipher::new(&[5u8; 32]).unwrap();
        let (ct, n) = old.encrypt(b"v", &Aad::fixed("x")).unwrap();
        assert_eq!(m2.decrypt(&ct, &n, &Aad::fixed("x")).unwrap().expose(), "v");
    }

    #[test]
    fn rotate_invalidates_old_ciphertexts() {
        let dir = tempfile::tempdir().unwrap();
        let m = SecurityManager::open(dir.path(), Arc::new(MemoryKeyStore::new())).unwrap();
        let aad = Aad::fixed("x");
        let (ct, n) = m.encrypt("v", &aad).unwrap();
        m.rotate().unwrap();
        assert!(m.decrypt(&ct, &n, &aad).is_err());
    }
}
