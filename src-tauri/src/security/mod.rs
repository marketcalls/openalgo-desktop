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
//! * **A keychain that cannot be opened at startup** (locked, access denied)
//!   also starts in password mode, with no vault and no keys. Only account
//!   setup creates keys: a sign-in there is told to unlock the keychain and
//!   restart (`AppError::KeychainUnavailable`), and never writes a vault
//!   that would shadow the keys still in the keychain.
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
/// The vault for a new password while a password change is under way.
pub const VAULT_NEXT_FILE: &str = "vault.next.json";

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
    /// Serialises changes to the vault files (password change, the
    /// clean-up after a sign-in, account reset).
    vault_lock: Mutex<()>,
    /// Test fault: the rename that completes a password change fails.
    #[cfg(test)]
    fail_promote: std::sync::atomic::AtomicBool,
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
            vault_lock: Mutex::new(()),
            #[cfg(test)]
            fail_promote: Default::default(),
        };

        match keychain {
            Some((Some(k), Some(p))) => {
                let keys = Keys::new(decode_b64(&k)?, decode_b64(&p)?)?;
                // Keys already moved on an earlier run that was interrupted
                // before the file was removed: still re-encrypt and delete.
                Ok(mgr(KeyMode::Keychain, Some(keys), legacy.is_some()))
            }
            Some((k, p)) => {
                let (keys, from_legacy) = match legacy {
                    Some((k, p)) => (Keys::new(k, p)?, true),
                    // One entry of the pair is gone (deleted by hand, a sync
                    // fault). Its partner still belongs to this install's
                    // data, so it is left exactly as it is and the app does
                    // not start on new keys (security review SEC-04).
                    None if k.is_some() || p.is_some() => {
                        let missing = if k.is_none() { DATA_KEY } else { PEPPER };
                        tracing::error!(
                            "The keychain holds only one of OpenAlgo's two keys ({} is missing)",
                            missing
                        );
                        return Err(AppError::Keychain(format!(
                            "the keychain entry {} is missing",
                            missing
                        )));
                    }
                    None => (Keys::random()?, false),
                };
                write_key_pair(store.as_ref(), &keys)?;
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
            vault_lock: Mutex::new(()),
            #[cfg(test)]
            fail_promote: Default::default(),
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

    /// Whether this install cannot be signed in to until its keychain is
    /// back: password mode with neither a vault nor keys in memory. On an
    /// install that has an account this means the keys are in a keychain
    /// that could not be opened at startup (locked, or access denied).
    pub fn keys_unreachable(&self) -> bool {
        self.mode == KeyMode::Password && !self.has_vault() && !self.is_unlocked()
    }

    /// Password mode: called at setup and on every sign-in. Opens the vault
    /// with the password; returns false when it does not open. Keychain
    /// mode: always true.
    ///
    /// With no vault, keys already in memory (moved from `secrets.dat`) are
    /// wrapped into a new one. New random keys are made only when
    /// `allow_create` is true, which only account setup passes: a sign-in
    /// on an install whose keys are elsewhere (a keychain that was locked or
    /// denied at startup) must never replace them, so it gets
    /// `AppError::KeychainUnavailable` and leaves nothing on disk.
    pub fn unlock_with_password(&self, password: &str, allow_create: bool) -> Result<bool> {
        if self.mode == KeyMode::Keychain {
            return Ok(true);
        }
        if !self.has_vault() {
            let keys = match self.keys.read().clone() {
                Some(k) => k,
                None if allow_create => Arc::new(Keys::random()?),
                None => return Err(AppError::KeychainUnavailable),
            };
            self.write_vault(&self.vault_path(), &keys, password)?;
            *self.keys.write() = Some(keys);
            return Ok(true);
        }
        // The vault, then one a password change left half done (stopped
        // before its last step): whichever opens with this password. The
        // caller checks the password against the stored hash and then calls
        // `settle_vault`, so the file that matches the hash is kept.
        let mut keys = open_vault_file(&self.vault_path(), password)?;
        if keys.is_none() && self.next_vault_path().exists() {
            keys = match open_vault_file(&self.next_vault_path(), password) {
                Ok(k) => k,
                Err(e) => {
                    tracing::warn!("Ignoring an unfinished password change: {}", e);
                    None
                }
            };
        }
        match keys {
            Some(k) => {
                *self.keys.write() = Some(Arc::new(k));
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn next_vault_path(&self) -> PathBuf {
        self.data_dir.join(VAULT_NEXT_FILE)
    }

    /// Password mode: change the password the vault is wrapped with, in step
    /// with `commit` (which stores the new password hash). Every point a
    /// fault or a power cut can stop it at leaves an account that signs in
    /// (security review SEC-02):
    ///
    /// 1. The vault for the new password is written beside the current one
    ///    (`vault.next.json`, synced). A failure here changes nothing.
    /// 2. `commit` stores the new hash. If it fails, the new file is removed.
    /// 3. The new file replaces the vault. If that fails, `rollback` puts the
    ///    old hash back.
    ///
    /// A stop between the steps leaves both files: sign-in opens whichever
    /// one the typed password opens and keeps the one matching the stored
    /// hash (`settle_vault`). Keychain mode has no vault: only `commit`
    /// runs. Changes are serialised.
    pub fn change_vault_password(
        &self,
        new_password: &str,
        commit: impl FnOnce() -> Result<()>,
        rollback: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let _change = self.vault_lock.lock();
        if self.mode == KeyMode::Keychain {
            return commit();
        }
        let keys = self.keys()?;
        let next = self.next_vault_path();
        self.write_vault(&next, &keys, new_password)?;
        if let Err(e) = commit() {
            let _ = std::fs::remove_file(&next);
            return Err(e);
        }
        if let Err(e) = self.promote_next_vault() {
            match rollback() {
                Ok(()) => {
                    let _ = std::fs::remove_file(&next);
                }
                Err(r) => tracing::error!(
                    "Password change could not be finished or undone ({}); both vault files are kept and either password signs in: {}",
                    r,
                    e
                ),
            }
            return Err(e);
        }
        Ok(())
    }

    fn promote_next_vault(&self) -> Result<()> {
        #[cfg(test)]
        if self.fail_promote.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::Io(std::io::Error::other("test fault")));
        }
        std::fs::rename(self.next_vault_path(), self.vault_path())?;
        fsperm::sync_dir(&self.data_dir);
        Ok(())
    }

    /// Password mode, after a sign-in whose password matched the stored
    /// hash: finish or discard a password change that stopped half way.
    /// The vault this password opens is the one that matches the hash, so
    /// it is the one kept. A no-op when no change was left unfinished.
    pub fn settle_vault(&self, password: &str) -> Result<()> {
        if self.mode == KeyMode::Keychain {
            return Ok(());
        }
        let _change = self.vault_lock.lock();
        let next = self.next_vault_path();
        if !next.exists() {
            return Ok(());
        }
        if open_vault_file(&self.vault_path(), password)?.is_some() {
            // The change never reached the stored hash.
            std::fs::remove_file(&next)?;
            tracing::info!("Discarded an unfinished password change");
        } else if open_vault_file(&next, password)?.is_some() {
            self.promote_next_vault()?;
            tracing::info!("Finished an interrupted password change");
        }
        Ok(())
    }

    /// Test fault: the last step of a password change fails.
    #[cfg(test)]
    pub fn fail_vault_promote(&self, on: bool) {
        self.fail_promote
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test: the state a stop right after step 1 of a password change
    /// leaves (the new vault written, nothing else).
    #[cfg(test)]
    pub fn stage_vault_for_test(&self, new_password: &str) -> Result<()> {
        let keys = self.keys()?;
        self.write_vault(&self.next_vault_path(), &keys, new_password)
    }

    fn write_vault(&self, path: &Path, keys: &Keys, password: &str) -> Result<()> {
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
        fsperm::write_private_file(path, &serde_json::to_vec(&vault)?)?;
        fsperm::sync_dir(&self.data_dir);
        Ok(())
    }

    /// Replace both keys with new random ones (account reset). Every existing
    /// ciphertext and password hash becomes unusable, which is the point.
    pub fn rotate(&self) -> Result<()> {
        let keys = Keys::random()?;
        match self.mode {
            KeyMode::Keychain => {
                if let Err(e) = write_key_pair(self.store.as_ref(), &keys) {
                    // The data key is written last, so a refused write leaves
                    // it as the one in memory; put the old pair back whole
                    // (best effort) so the pepper matches it again.
                    if let Some(old) = self.keys.read().clone() {
                        let _ = write_key_pair(self.store.as_ref(), &old);
                    }
                    return Err(e);
                }
            }
            KeyMode::Password => {
                let _change = self.vault_lock.lock();
                for p in [self.vault_path(), self.next_vault_path()] {
                    if p.exists() {
                        std::fs::remove_file(p)?;
                    }
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

/// The keys in the vault file at `path`, or None when `password` does not
/// open it.
fn open_vault_file(path: &Path, password: &str) -> Result<Option<Keys>> {
    let damaged = || AppError::Encryption("vault file is damaged".into());
    let raw = std::fs::read(path)?;
    let vault: VaultFile = serde_json::from_slice(&raw).map_err(|_| damaged())?;
    let salt = B64.decode(&vault.salt).map_err(|_| damaged())?;
    let kek = hashing::derive_kek(password, &salt)?;
    let cipher = DataCipher::new(&kek)?;
    let plain = match cipher.decrypt_bytes(&vault.ciphertext, &vault.nonce, &Aad::fixed("vault"))
    {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let bytes = plain.expose();
    if bytes.len() != crypto::KEY_SIZE + hashing::PEPPER_SIZE {
        return Err(damaged());
    }
    Ok(Some(Keys::new(
        SecretBytes::new(bytes[..crypto::KEY_SIZE].to_vec()),
        SecretBytes::new(bytes[crypto::KEY_SIZE..].to_vec()),
    )?))
}

/// Store both keys in the keychain: the pepper first and the data key last,
/// so a refused write leaves the data key that encrypts the stored secrets
/// as it was (a refused pepper write changes nothing). Both are read back:
/// some secret services accept a write and return nothing on read.
fn write_key_pair(store: &dyn KeyStore, keys: &Keys) -> Result<()> {
    let pepper = B64.encode(keys.pepper.expose());
    let data_key = B64.encode(keys.raw_key.expose());
    store
        .set(PEPPER, &pepper)
        .and_then(|_| store.set(DATA_KEY, &data_key))
        .map_err(|e| AppError::Keychain(e.to_string()))?;
    for (name, want) in [(PEPPER, &pepper), (DATA_KEY, &data_key)] {
        match store.get(name) {
            Ok(Some(v)) if &v == want => {}
            _ => {
                return Err(AppError::Keychain(format!(
                    "the keychain did not keep {}",
                    name
                )))
            }
        }
    }
    Ok(())
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
        assert!(m.unlock_with_password("First@123", true).unwrap());
        let (ct, n) = m.encrypt("x", &Aad::fixed("x")).unwrap();
        assert!(dir.path().join(VAULT_FILE).exists());

        // Next start: locked; wrong password does not open; right one does.
        let m2 = SecurityManager::open(dir.path(), store).unwrap();
        assert!(!m2.is_unlocked());
        assert!(!m2.unlock_with_password("Wrong@123", false).unwrap());
        assert!(m2.unlock_with_password("First@123", false).unwrap());
        assert_eq!(m2.decrypt(&ct, &n, &Aad::fixed("x")).unwrap().expose(), "x");

        // Password change re-wraps.
        m2.change_vault_password("Second@123", || Ok(()), || Ok(()))
            .unwrap();
        assert!(!dir.path().join(VAULT_NEXT_FILE).exists());
        let m3 = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(!m3.unlock_with_password("First@123", false).unwrap());
        assert!(m3.unlock_with_password("Second@123", false).unwrap());
    }

    /// SEC-03: only account setup makes new keys; a sign-in with neither a
    /// vault nor keys in memory is told the keychain is unavailable.
    #[test]
    fn only_setup_creates_a_vault() {
        let dir = tempfile::tempdir().unwrap();
        let m = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(m.keys_unreachable());
        assert!(matches!(
            m.unlock_with_password("Pw@12345", false),
            Err(AppError::KeychainUnavailable)
        ));
        assert!(!dir.path().join(VAULT_FILE).exists());
        assert!(m.unlock_with_password("Pw@12345", true).unwrap());
        assert!(dir.path().join(VAULT_FILE).exists());
        assert!(!m.keys_unreachable());
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
        m.unlock_with_password("Pw@12345", false).unwrap();
        m.finish_legacy_migration().unwrap();
        assert!(!legacy::path(dir.path()).exists());
        let m2 = SecurityManager::open(dir.path(), Arc::new(UnavailableKeyStore)).unwrap();
        assert!(m2.unlock_with_password("Pw@12345", false).unwrap());
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

    // ------------------------------------- SEC-04 the keychain key pair

    use keystore::FaultyKeyStore;

    /// A populated keychain install: keys stored, one secret encrypted.
    fn populated() -> (tempfile::TempDir, Arc<FaultyKeyStore>, (String, String)) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultyKeyStore::default());
        let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
        let ct = m.encrypt("broker-secret", &Aad::fixed("x")).unwrap();
        (dir, store, ct)
    }

    /// SEC-04: with one of the two entries gone, the app refuses to start
    /// and leaves the other one exactly as it was.
    #[test]
    fn a_partial_key_pair_is_refused_and_kept() {
        for (gone, kept) in [(PEPPER, DATA_KEY), (DATA_KEY, PEPPER)] {
            let (dir, store, _) = populated();
            let survivor = store.get(kept).unwrap().unwrap();
            store.delete(gone).unwrap();
            let r = SecurityManager::open(dir.path(), store.clone());
            assert!(matches!(r, Err(AppError::Keychain(_))), "{} gone", gone);
            assert_eq!(store.get(kept).unwrap().unwrap(), survivor, "{} gone", gone);
            assert_eq!(store.get(gone).unwrap(), None, "{} recreated", gone);
            assert!(!dir.path().join(VAULT_FILE).exists());
        }
        // An empty keychain on an empty folder still starts with new keys.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(FaultyKeyStore::default());
        let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
        assert!(m.is_unlocked());
        assert!(store.get(DATA_KEY).unwrap().is_some() && store.get(PEPPER).unwrap().is_some());
    }

    /// A secret service that accepts the pepper and returns nothing for it.
    struct ForgetsPepper(MemoryKeyStore);

    impl KeyStore for ForgetsPepper {
        fn get(&self, name: &str) -> std::result::Result<Option<String>, KeyStoreError> {
            if name == PEPPER {
                return Ok(None);
            }
            self.0.get(name)
        }
        fn set(&self, name: &str, value: &str) -> std::result::Result<(), KeyStoreError> {
            self.0.set(name, value)
        }
        fn delete(&self, name: &str) -> std::result::Result<(), KeyStoreError> {
            self.0.delete(name)
        }
        fn kind(&self) -> &'static str {
            "forgetful"
        }
    }

    /// SEC-04: both keys are read back after they are written.
    #[test]
    fn both_keys_are_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let r = SecurityManager::open(dir.path(), Arc::new(ForgetsPepper(MemoryKeyStore::new())));
        assert!(matches!(r, Err(AppError::Keychain(_))));
    }

    /// SEC-04: a keychain that refuses a write part way through a rotation
    /// keeps the data key the stored secrets are encrypted with.
    #[test]
    fn a_refused_write_during_rotation_keeps_the_data_key() {
        for allowed in [0usize, 1] {
            let (dir, store, (ct, n)) = populated();
            let data_key = store.get(DATA_KEY).unwrap().unwrap();
            let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
            *store.writes_left.lock() = Some(allowed);
            assert!(m.rotate().is_err(), "{} writes allowed", allowed);
            *store.writes_left.lock() = None;
            assert_eq!(
                store.get(DATA_KEY).unwrap().unwrap(),
                data_key,
                "{} writes allowed",
                allowed
            );
            let again = SecurityManager::open(dir.path(), store.clone()).unwrap();
            assert_eq!(
                again.decrypt(&ct, &n, &Aad::fixed("x")).unwrap().expose(),
                "broker-secret"
            );
        }
    }

    /// SEC-03: a keychain that is locked at startup leaves the stored pair
    /// untouched and starts with no keys.
    #[test]
    fn a_locked_keychain_leaves_the_pair_alone() {
        let (dir, store, (ct, n)) = populated();
        let pair = (store.get(DATA_KEY).unwrap(), store.get(PEPPER).unwrap());
        store.set_unavailable(true);
        let m = SecurityManager::open(dir.path(), store.clone()).unwrap();
        assert!(m.keys_unreachable());
        store.set_unavailable(false);
        assert_eq!(
            (store.get(DATA_KEY).unwrap(), store.get(PEPPER).unwrap()),
            pair
        );
        let m = SecurityManager::open(dir.path(), store).unwrap();
        assert_eq!(
            m.decrypt(&ct, &n, &Aad::fixed("x")).unwrap().expose(),
            "broker-secret"
        );
    }
}
