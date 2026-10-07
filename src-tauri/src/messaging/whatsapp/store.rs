//! The WhatsApp session store: whatsapp-rust's storage `Backend` kept in
//! memory, exported and imported as one snapshot.
//!
//! The crate's own SQLite store links a second libsqlite3 (Cargo forbids two
//! crates with the same `links` key next to the app's bundled rusqlite), and
//! a table per Signal record would put private keys on disk in the clear.
//! Instead the whole session lives here, and the bot service writes a
//! snapshot back into `whatsapp_config.session_blob`, sealed with the app's
//! data key, after logins, every few minutes when it changed, and at stop:
//! the desktop form of the web's `wars.export_session()` / `from_bytes()`.
//!
//! Transient retry state (sent-message copies, base keys) is not exported.

use bytes::Bytes;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use whatsapp_rust::wacore::appstate::hash::HashState;
use whatsapp_rust::wacore::appstate::processor::AppStateMutationMAC;
use whatsapp_rust::wacore::store::error::{Result, StoreError};
use whatsapp_rust::wacore::store::traits::*;
use whatsapp_rust::wacore::store::Device;

/// Snapshot format version.
const SNAPSHOT_VERSION: u32 = 1;
/// Bound on retained sent-message copies (retry support only).
const MAX_SENT_MESSAGES: usize = 4096;

#[derive(Default)]
struct State {
    identities: HashMap<String, [u8; 32]>,
    sessions: HashMap<String, Bytes>,
    prekeys: HashMap<u32, (Bytes, bool)>,
    signed_prekeys: HashMap<u32, Vec<u8>>,
    sender_keys: HashMap<String, Vec<u8>>,
    sync_keys: HashMap<Vec<u8>, AppStateSyncKey>,
    latest_sync_key_id: Option<Vec<u8>>,
    versions: HashMap<String, HashState>,
    mutation_macs: HashMap<(String, Vec<u8>), Vec<u8>>,
    sender_key_devices: HashMap<String, HashMap<String, bool>>,
    lid_mappings: HashMap<String, LidPnMappingEntry>,
    pn_to_lid: HashMap<String, String>,
    base_keys: HashMap<(String, String), Vec<u8>>,
    device_lists: HashMap<String, DeviceListRecord>,
    group_metadata: HashMap<String, Vec<u8>>,
    tc_tokens: HashMap<String, TcTokenEntry>,
    sent_messages: HashMap<(String, String), (Vec<u8>, i64)>,
    msg_secrets: HashMap<(String, String, String), (Vec<u8>, i64, i64)>,
    device: Option<Device>,
}

/// What a snapshot holds (maps flattened to lists: JSON keys are strings).
#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    version: u32,
    identities: Vec<(String, Vec<u8>)>,
    sessions: Vec<(String, Vec<u8>)>,
    prekeys: Vec<(u32, Vec<u8>, bool)>,
    signed_prekeys: Vec<(u32, Vec<u8>)>,
    sender_keys: Vec<(String, Vec<u8>)>,
    sync_keys: Vec<(Vec<u8>, AppStateSyncKey)>,
    latest_sync_key_id: Option<Vec<u8>>,
    versions: Vec<(String, HashState)>,
    mutation_macs: Vec<(String, Vec<u8>, Vec<u8>)>,
    sender_key_devices: Vec<(String, Vec<(String, bool)>)>,
    lid_mappings: Vec<LidPnMappingEntry>,
    device_lists: Vec<DeviceListRecord>,
    group_metadata: Vec<(String, Vec<u8>)>,
    tc_tokens: Vec<(String, TcTokenEntry)>,
    msg_secrets: Vec<(String, String, String, Vec<u8>, i64, i64)>,
    device: Option<Device>,
}

fn ser_err(e: impl std::error::Error + Send + Sync + 'static) -> StoreError {
    StoreError::Serialization(Box::new(e))
}

/// The in-memory backend; `version()` moves on every write.
pub struct SnapshotStore {
    state: Mutex<State>,
    version: AtomicU64,
    next_device_id: AtomicI32,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            version: AtomicU64::new(0),
            next_device_id: AtomicI32::new(1),
        }
    }

    fn touch(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    /// Changes so far; the saver compares it with the version it last wrote.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Whether a paired device record is present (pairing finished).
    pub fn has_paired_device(&self) -> bool {
        self.state
            .lock()
            .device
            .as_ref()
            .map(|d| d.pn.is_some())
            .unwrap_or(false)
    }

    /// The whole session as compressed bytes (`wars.export_session`).
    pub fn export(&self) -> Result<Vec<u8>> {
        let snap = {
            let s = self.state.lock();
            Snapshot {
                version: SNAPSHOT_VERSION,
                identities: s
                    .identities
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_vec()))
                    .collect(),
                sessions: s
                    .sessions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_vec()))
                    .collect(),
                prekeys: s
                    .prekeys
                    .iter()
                    .map(|(k, (r, u))| (*k, r.to_vec(), *u))
                    .collect(),
                signed_prekeys: s
                    .signed_prekeys
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect(),
                sender_keys: s
                    .sender_keys
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                sync_keys: s
                    .sync_keys
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                latest_sync_key_id: s.latest_sync_key_id.clone(),
                versions: s
                    .versions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                mutation_macs: s
                    .mutation_macs
                    .iter()
                    .map(|((n, i), v)| (n.clone(), i.clone(), v.clone()))
                    .collect(),
                sender_key_devices: s
                    .sender_key_devices
                    .iter()
                    .map(|(g, m)| (g.clone(), m.iter().map(|(d, h)| (d.clone(), *h)).collect()))
                    .collect(),
                lid_mappings: s.lid_mappings.values().cloned().collect(),
                device_lists: s.device_lists.values().cloned().collect(),
                group_metadata: s
                    .group_metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                tc_tokens: s
                    .tc_tokens
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                msg_secrets: s
                    .msg_secrets
                    .iter()
                    .map(|((c, se, m), (sec, e, t))| {
                        (c.clone(), se.clone(), m.clone(), sec.clone(), *e, *t)
                    })
                    .collect(),
                device: s.device.clone(),
            }
        };
        let json = serde_json::to_vec(&snap).map_err(ser_err)?;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&json)?;
        Ok(enc.finish()?)
    }

    /// Rebuild a store from `export()` bytes (`wars.WhatsApp.from_bytes`).
    pub fn import(bytes: &[u8]) -> Result<Self> {
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .take(512 * 1024 * 1024)
            .read_to_end(&mut json)?;
        let snap: Snapshot = serde_json::from_slice(&json).map_err(ser_err)?;
        if snap.version != SNAPSHOT_VERSION {
            return Err(StoreError::Validation(
                "unknown session snapshot version".into(),
            ));
        }
        let mut s = State::default();
        for (k, v) in snap.identities {
            let key: [u8; 32] = v
                .try_into()
                .map_err(|_| StoreError::Validation("identity key length".into()))?;
            s.identities.insert(k, key);
        }
        s.sessions = snap
            .sessions
            .into_iter()
            .map(|(k, v)| (k, Bytes::from(v)))
            .collect();
        s.prekeys = snap
            .prekeys
            .into_iter()
            .map(|(k, r, u)| (k, (Bytes::from(r), u)))
            .collect();
        s.signed_prekeys = snap.signed_prekeys.into_iter().collect();
        s.sender_keys = snap.sender_keys.into_iter().collect();
        s.sync_keys = snap.sync_keys.into_iter().collect();
        s.latest_sync_key_id = snap.latest_sync_key_id;
        s.versions = snap.versions.into_iter().collect();
        s.mutation_macs = snap
            .mutation_macs
            .into_iter()
            .map(|(n, i, v)| ((n, i), v))
            .collect();
        s.sender_key_devices = snap
            .sender_key_devices
            .into_iter()
            .map(|(g, m)| (g, m.into_iter().collect()))
            .collect();
        for e in snap.lid_mappings {
            s.pn_to_lid.insert(e.phone_number.clone(), e.lid.clone());
            s.lid_mappings.insert(e.lid.clone(), e);
        }
        s.device_lists = snap
            .device_lists
            .into_iter()
            .map(|r| (r.user.clone(), r))
            .collect();
        s.group_metadata = snap.group_metadata.into_iter().collect();
        s.tc_tokens = snap.tc_tokens.into_iter().collect();
        s.msg_secrets = snap
            .msg_secrets
            .into_iter()
            .map(|(c, se, m, sec, e, t)| ((c, se, m), (sec, e, t)))
            .collect();
        s.device = snap.device.map(|mut d| {
            // Runtime-only fields, not serialized (as the crate's SQLite store
            // rebuilds them).
            d.device_props =
                std::sync::Arc::new(whatsapp_rust::wacore::store::device::DEVICE_PROPS.clone());
            d.client_profile = whatsapp_rust::ClientProfile::web();
            d
        });
        Ok(Self {
            state: Mutex::new(s),
            version: AtomicU64::new(0),
            next_device_id: AtomicI32::new(2),
        })
    }
}

#[async_trait::async_trait]
impl SignalStore for SnapshotStore {
    async fn put_identity(&self, address: &str, key: [u8; 32]) -> Result<()> {
        self.state
            .lock()
            .identities
            .insert(address.to_string(), key);
        self.touch();
        Ok(())
    }
    async fn load_identity(&self, address: &str) -> Result<Option<[u8; 32]>> {
        Ok(self.state.lock().identities.get(address).copied())
    }
    async fn delete_identity(&self, address: &str) -> Result<()> {
        self.state.lock().identities.remove(address);
        self.touch();
        Ok(())
    }
    async fn get_session(&self, address: &str) -> Result<Option<Bytes>> {
        Ok(self.state.lock().sessions.get(address).cloned())
    }
    async fn put_session(&self, address: &str, session: &[u8]) -> Result<()> {
        self.state
            .lock()
            .sessions
            .insert(address.to_string(), Bytes::copy_from_slice(session));
        self.touch();
        Ok(())
    }
    async fn has_session(&self, address: &str) -> Result<bool> {
        Ok(self.state.lock().sessions.contains_key(address))
    }
    async fn has_signal_state_for_user(&self, user: &str) -> Result<bool> {
        fn matches(addr: &str, user: &str) -> bool {
            addr.strip_prefix(user)
                .is_some_and(|rest| rest.starts_with('@') || rest.starts_with(':'))
        }
        let s = self.state.lock();
        Ok(s.sessions.keys().any(|k| matches(k, user))
            || s.identities.keys().any(|k| matches(k, user)))
    }
    async fn delete_session(&self, address: &str) -> Result<()> {
        self.state.lock().sessions.remove(address);
        self.touch();
        Ok(())
    }
    async fn store_prekey(&self, id: u32, record: &[u8], uploaded: bool) -> Result<()> {
        self.state
            .lock()
            .prekeys
            .insert(id, (Bytes::copy_from_slice(record), uploaded));
        self.touch();
        Ok(())
    }
    async fn store_prekeys_batch(&self, keys: &[(u32, Bytes)], uploaded: bool) -> Result<()> {
        let mut s = self.state.lock();
        for (id, r) in keys {
            s.prekeys.insert(*id, (r.clone(), uploaded));
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn load_prekey(&self, id: u32) -> Result<Option<Bytes>> {
        Ok(self.state.lock().prekeys.get(&id).map(|e| e.0.clone()))
    }
    async fn mark_prekeys_uploaded(&self, ids: &[u32]) -> Result<()> {
        let mut s = self.state.lock();
        for id in ids {
            if let Some(e) = s.prekeys.get_mut(id) {
                e.1 = true;
            }
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn remove_prekey(&self, id: u32) -> Result<()> {
        self.state.lock().prekeys.remove(&id);
        self.touch();
        Ok(())
    }
    async fn get_max_prekey_id(&self) -> Result<u32> {
        Ok(self.state.lock().prekeys.keys().copied().max().unwrap_or(0))
    }
    async fn store_signed_prekey(&self, id: u32, record: &[u8]) -> Result<()> {
        self.state.lock().signed_prekeys.insert(id, record.to_vec());
        self.touch();
        Ok(())
    }
    async fn load_signed_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        Ok(self.state.lock().signed_prekeys.get(&id).cloned())
    }
    async fn load_all_signed_prekeys(&self) -> Result<Vec<(u32, Vec<u8>)>> {
        Ok(self
            .state
            .lock()
            .signed_prekeys
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect())
    }
    async fn remove_signed_prekey(&self, id: u32) -> Result<()> {
        self.state.lock().signed_prekeys.remove(&id);
        self.touch();
        Ok(())
    }
    async fn put_sender_key(&self, address: &str, record: &[u8]) -> Result<()> {
        self.state
            .lock()
            .sender_keys
            .insert(address.to_string(), record.to_vec());
        self.touch();
        Ok(())
    }
    async fn get_sender_key(&self, address: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.state.lock().sender_keys.get(address).cloned())
    }
    async fn delete_sender_key(&self, address: &str) -> Result<()> {
        self.state.lock().sender_keys.remove(address);
        self.touch();
        Ok(())
    }
}

#[async_trait::async_trait]
impl AppSyncStore for SnapshotStore {
    async fn get_sync_key(&self, key_id: &[u8]) -> Result<Option<AppStateSyncKey>> {
        Ok(self.state.lock().sync_keys.get(key_id).cloned())
    }
    async fn set_sync_key(&self, key_id: &[u8], key: AppStateSyncKey) -> Result<()> {
        let mut s = self.state.lock();
        s.sync_keys.insert(key_id.to_vec(), key);
        s.latest_sync_key_id = Some(key_id.to_vec());
        drop(s);
        self.touch();
        Ok(())
    }
    async fn get_version(&self, name: &str) -> Result<HashState> {
        Ok(self
            .state
            .lock()
            .versions
            .get(name)
            .cloned()
            .unwrap_or_default())
    }
    async fn set_version(&self, name: &str, state: HashState) -> Result<()> {
        self.state.lock().versions.insert(name.to_string(), state);
        self.touch();
        Ok(())
    }
    async fn put_mutation_macs(
        &self,
        name: &str,
        _version: u64,
        mutations: &[AppStateMutationMAC],
    ) -> Result<()> {
        let mut s = self.state.lock();
        for m in mutations {
            s.mutation_macs
                .insert((name.to_string(), m.index_mac.clone()), m.value_mac.clone());
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn get_mutation_mac(&self, name: &str, index_mac: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .state
            .lock()
            .mutation_macs
            .get(&(name.to_string(), index_mac.to_vec()))
            .cloned())
    }
    async fn delete_mutation_macs(&self, name: &str, index_macs: &[Vec<u8>]) -> Result<()> {
        let mut s = self.state.lock();
        for im in index_macs {
            s.mutation_macs.remove(&(name.to_string(), im.clone()));
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn clear_mutation_macs(&self, name: &str) -> Result<()> {
        self.state
            .lock()
            .mutation_macs
            .retain(|(n, _), _| n != name);
        self.touch();
        Ok(())
    }
    async fn get_latest_sync_key_id(&self) -> Result<Option<Vec<u8>>> {
        Ok(self.state.lock().latest_sync_key_id.clone())
    }
}

#[async_trait::async_trait]
impl ProtocolStore for SnapshotStore {
    async fn get_sender_key_devices(&self, group_jid: &str) -> Result<Vec<(String, bool)>> {
        Ok(self
            .state
            .lock()
            .sender_key_devices
            .get(group_jid)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect())
            .unwrap_or_default())
    }
    async fn set_sender_key_status(&self, group_jid: &str, entries: &[(&str, bool)]) -> Result<()> {
        let mut s = self.state.lock();
        let m = s
            .sender_key_devices
            .entry(group_jid.to_string())
            .or_default();
        for (d, h) in entries {
            m.insert(d.to_string(), *h);
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn clear_sender_key_devices(&self, group_jid: &str) -> Result<()> {
        self.state.lock().sender_key_devices.remove(group_jid);
        self.touch();
        Ok(())
    }
    async fn delete_sender_key_device_rows(&self, device_jids: &[&str]) -> Result<()> {
        let mut s = self.state.lock();
        for m in s.sender_key_devices.values_mut() {
            m.retain(|j, _| !device_jids.contains(&j.as_str()));
        }
        drop(s);
        self.touch();
        Ok(())
    }
    async fn clear_all_sender_key_devices(&self) -> Result<()> {
        self.state.lock().sender_key_devices.clear();
        self.touch();
        Ok(())
    }
    async fn get_lid_mapping(&self, lid: &str) -> Result<Option<LidPnMappingEntry>> {
        Ok(self.state.lock().lid_mappings.get(lid).cloned())
    }
    async fn get_pn_mapping(&self, phone: &str) -> Result<Option<LidPnMappingEntry>> {
        let s = self.state.lock();
        Ok(s.pn_to_lid
            .get(phone)
            .and_then(|l| s.lid_mappings.get(l))
            .cloned())
    }
    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> Result<()> {
        let mut s = self.state.lock();
        if let Some(old) = s
            .lid_mappings
            .get(&entry.lid)
            .filter(|o| o.phone_number != entry.phone_number)
            .map(|o| o.phone_number.clone())
        {
            s.pn_to_lid.remove(&old);
        }
        s.pn_to_lid
            .insert(entry.phone_number.clone(), entry.lid.clone());
        s.lid_mappings.insert(entry.lid.clone(), entry.clone());
        drop(s);
        self.touch();
        Ok(())
    }
    async fn get_all_lid_mappings(&self) -> Result<Vec<LidPnMappingEntry>> {
        Ok(self.state.lock().lid_mappings.values().cloned().collect())
    }
    async fn save_base_key(&self, address: &str, message_id: &str, base_key: &[u8]) -> Result<()> {
        self.state.lock().base_keys.insert(
            (address.to_string(), message_id.to_string()),
            base_key.to_vec(),
        );
        Ok(())
    }
    async fn has_same_base_key(
        &self,
        address: &str,
        message_id: &str,
        current: &[u8],
    ) -> Result<bool> {
        Ok(self
            .state
            .lock()
            .base_keys
            .get(&(address.to_string(), message_id.to_string()))
            .is_some_and(|b| b == current))
    }
    async fn delete_base_key(&self, address: &str, message_id: &str) -> Result<()> {
        self.state
            .lock()
            .base_keys
            .remove(&(address.to_string(), message_id.to_string()));
        Ok(())
    }
    async fn update_device_list(&self, record: DeviceListRecord) -> Result<()> {
        self.state
            .lock()
            .device_lists
            .insert(record.user.clone(), record);
        self.touch();
        Ok(())
    }
    async fn get_devices(&self, user: &str) -> Result<Option<DeviceListRecord>> {
        Ok(self.state.lock().device_lists.get(user).cloned())
    }
    async fn delete_devices(&self, user: &str) -> Result<()> {
        self.state.lock().device_lists.remove(user);
        self.touch();
        Ok(())
    }
    async fn get_group_metadata(&self, group_jid: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.state.lock().group_metadata.get(group_jid).cloned())
    }
    async fn put_group_metadata(&self, group_jid: &str, blob: &[u8]) -> Result<()> {
        self.state
            .lock()
            .group_metadata
            .insert(group_jid.to_string(), blob.to_vec());
        self.touch();
        Ok(())
    }
    async fn delete_group_metadata(&self, group_jid: &str) -> Result<()> {
        self.state.lock().group_metadata.remove(group_jid);
        self.touch();
        Ok(())
    }
    async fn get_tc_token(&self, jid: &str) -> Result<Option<TcTokenEntry>> {
        Ok(self.state.lock().tc_tokens.get(jid).cloned())
    }
    async fn put_tc_token(&self, jid: &str, entry: &TcTokenEntry) -> Result<()> {
        self.state
            .lock()
            .tc_tokens
            .insert(jid.to_string(), entry.clone());
        self.touch();
        Ok(())
    }
    async fn delete_tc_token(&self, jid: &str) -> Result<()> {
        self.state.lock().tc_tokens.remove(jid);
        self.touch();
        Ok(())
    }
    async fn get_all_tc_token_jids(&self) -> Result<Vec<String>> {
        Ok(self.state.lock().tc_tokens.keys().cloned().collect())
    }
    async fn delete_expired_tc_tokens(&self, cutoff: i64) -> Result<u32> {
        let mut s = self.state.lock();
        let before = s.tc_tokens.len();
        s.tc_tokens.retain(|_, e| e.token_timestamp >= cutoff);
        let n = (before - s.tc_tokens.len()) as u32;
        drop(s);
        if n > 0 {
            self.touch();
        }
        Ok(n)
    }
    async fn store_sent_message(
        &self,
        chat_jid: &str,
        message_id: &str,
        payload: &[u8],
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut s = self.state.lock();
        if s.sent_messages.len() >= MAX_SENT_MESSAGES {
            let target = MAX_SENT_MESSAGES * 3 / 4;
            let drop_n = s.sent_messages.len().saturating_sub(target);
            let mut by_age: Vec<_> = s
                .sent_messages
                .iter()
                .map(|(k, e)| (e.1, k.clone()))
                .collect();
            by_age.sort_unstable_by_key(|(t, _)| *t);
            for (_, k) in by_age.into_iter().take(drop_n) {
                s.sent_messages.remove(&k);
            }
        }
        s.sent_messages.insert(
            (chat_jid.to_string(), message_id.to_string()),
            (payload.to_vec(), now),
        );
        Ok(())
    }
    async fn take_sent_message(&self, chat_jid: &str, message_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .state
            .lock()
            .sent_messages
            .remove(&(chat_jid.to_string(), message_id.to_string()))
            .map(|e| e.0))
    }
    async fn delete_expired_sent_messages(&self, cutoff: i64) -> Result<u32> {
        let mut s = self.state.lock();
        let before = s.sent_messages.len();
        s.sent_messages.retain(|_, e| e.1 >= cutoff);
        Ok((before - s.sent_messages.len()) as u32)
    }
}

#[async_trait::async_trait]
impl MsgSecretStore for SnapshotStore {
    async fn put_msg_secrets(&self, entries: Vec<MsgSecretEntry>) -> Result<usize> {
        let n = entries.len();
        let mut s = self.state.lock();
        for e in entries {
            let key = (e.chat, e.sender, e.msg_id);
            let (exp, ts) = match s.msg_secrets.get(&key) {
                Some((_, ex, t)) => (
                    merge_msg_secret_expiry(*ex, e.expires_at),
                    merge_msg_secret_message_ts(*t, e.message_ts),
                ),
                None => (e.expires_at, e.message_ts),
            };
            s.msg_secrets.insert(key, (e.secret, exp, ts));
        }
        drop(s);
        self.touch();
        Ok(n)
    }
    async fn get_msg_secret(
        &self,
        chat: &str,
        sender: &str,
        msg_id: &str,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .get_msg_secret_with_ts(chat, sender, msg_id)
            .await?
            .map(|(s, _)| s))
    }
    async fn get_msg_secret_with_ts(
        &self,
        chat: &str,
        sender: &str,
        msg_id: &str,
    ) -> Result<Option<(Vec<u8>, i64)>> {
        Ok(self
            .state
            .lock()
            .msg_secrets
            .get(&(chat.to_string(), sender.to_string(), msg_id.to_string()))
            .map(|(s, _, t)| (s.clone(), *t)))
    }
    async fn delete_expired_msg_secrets(&self, cutoff: i64) -> Result<u32> {
        let mut s = self.state.lock();
        let before = s.msg_secrets.len();
        s.msg_secrets.retain(|_, (_, e, _)| *e == 0 || *e > cutoff);
        let n = (before - s.msg_secrets.len()) as u32;
        drop(s);
        if n > 0 {
            self.touch();
        }
        Ok(n)
    }
}

#[async_trait::async_trait]
impl DeviceStore for SnapshotStore {
    async fn save(&self, device: &Device) -> Result<()> {
        self.state.lock().device = Some(device.clone());
        self.touch();
        Ok(())
    }
    async fn load(&self) -> Result<Option<Device>> {
        Ok(self.state.lock().device.clone())
    }
    async fn exists(&self) -> Result<bool> {
        Ok(self.state.lock().device.is_some())
    }
    async fn create(&self) -> Result<i32> {
        let id = self.next_device_id.fetch_add(1, Ordering::Relaxed);
        let mut s = self.state.lock();
        if s.device.is_none() {
            s.device = Some(Device::new());
        }
        drop(s);
        self.touch();
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_backend<T: Backend>() {}

    #[test]
    fn implements_backend() {
        is_backend::<SnapshotStore>();
    }

    #[tokio::test]
    async fn export_import_round_trip_keeps_the_session() {
        let s = SnapshotStore::new();
        let v0 = s.version();
        s.create().await.unwrap();
        s.put_identity("1@s.whatsapp.net", [7u8; 32]).await.unwrap();
        s.put_session("1@s.whatsapp.net", b"sess").await.unwrap();
        s.store_prekey(5, b"pk", true).await.unwrap();
        s.put_lid_mapping(&LidPnMappingEntry {
            lid: "100".into(),
            phone_number: "91".into(),
            created_at: 1,
            updated_at: 1,
            learning_source: "usync".into(),
        })
        .await
        .unwrap();
        s.put_mutation_macs(
            "regular",
            1,
            &[AppStateMutationMAC {
                index_mac: vec![1],
                value_mac: vec![2],
            }],
        )
        .await
        .unwrap();
        s.put_msg_secret("c", "s", "m", &[3u8; 32]).await.unwrap();
        assert!(s.version() > v0);
        let dev_before = s.load().await.unwrap().unwrap();

        let blob = s.export().unwrap();
        let r = SnapshotStore::import(&blob).unwrap();
        assert_eq!(
            r.load_identity("1@s.whatsapp.net").await.unwrap(),
            Some([7u8; 32])
        );
        assert_eq!(
            r.get_session("1@s.whatsapp.net").await.unwrap().unwrap(),
            Bytes::from_static(b"sess")
        );
        assert_eq!(
            r.load_prekey(5).await.unwrap().unwrap(),
            Bytes::from_static(b"pk")
        );
        assert_eq!(r.get_pn_mapping("91").await.unwrap().unwrap().lid, "100");
        assert_eq!(
            r.get_mutation_mac("regular", &[1]).await.unwrap(),
            Some(vec![2])
        );
        assert_eq!(
            r.get_msg_secret("c", "s", "m").await.unwrap(),
            Some(vec![3u8; 32])
        );
        let dev_after = r.load().await.unwrap().unwrap();
        assert_eq!(dev_before.registration_id, dev_after.registration_id);
        assert_eq!(
            dev_before.identity_key.public_key.serialize(),
            dev_after.identity_key.public_key.serialize()
        );
        assert!(!r.has_paired_device());
        assert!(SnapshotStore::import(b"not a snapshot").is_err());
    }
}
