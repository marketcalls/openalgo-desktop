//! External data servers reached over the Model Context Protocol (the
//! web's `services/agent/mcp`). The Agent is the client here; the desktop's
//! own MCP server for outside assistants is `crate::mcp`.
//!
//! - [`registry`]: the servers (none today, as on the web).
//! - [`client`]: one server's client: session, retry, deadline, circuit
//!   breaker and tool-list cache, over [`transport`] and [`wire`].
//! - [`errors`]: the failures and their trader sentences.
//! - [`taint`]: the arguments sent, built from the trader's own words.
//! - [`prompt`]: the EXTERNAL DATA prompt section.
//!
//! The model reaches them through two tools (`agent::tools::mcp`).
//! [`ExternalData`], owned by the Agent, holds the registry read at
//! startup, one client per server (bounded by the registry) and the cache
//! of long answers being paged.

pub mod answers;
pub mod client;
pub mod errors;
pub mod prompt;
pub mod registry;
pub mod taint;
pub mod transport;
pub mod wire;

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

use crate::agent::contract::tool::Surface;
use client::{McpClient, ServerCheck};
use parking_lot::{Mutex, RwLock};
use registry::McpServerSpec;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use transport::{Clock, HttpTransport, SystemClock, Transport};

/// How long a full answer is kept for paging (the web's
/// `RESULT_CACHE_TTL_S`), and the most characters kept at once
/// (`RESULT_CACHE_CHARS`): bounded by size, since one answer may run to two
/// million characters.
pub const RESULT_CACHE_TTL: Duration = Duration::from_secs(300);
pub const RESULT_CACHE_CHARS: usize = 1_000_000;

/// Makes the transport for a server (the real HTTP client; a test's fake).
pub type Connector = Arc<dyn Fn(&McpServerSpec) -> Option<Arc<dyn Transport>> + Send + Sync>;

/// Full answers kept for paging: oldest out first, and by expiry.
#[derive(Default)]
struct Answers {
    entries: HashMap<String, (Arc<String>, Duration)>,
    order: VecDeque<String>,
    chars: usize,
}

impl Answers {
    fn get(&mut self, key: &str, now: Duration) -> Option<Arc<String>> {
        self.expire(now);
        self.entries.get(key).map(|(text, _)| text.clone())
    }

    fn put(&mut self, key: String, text: Arc<String>, now: Duration) {
        let size = text.chars().count();
        // An answer larger than the whole cache is not kept; a later page
        // of it calls the server again.
        if size > RESULT_CACHE_CHARS {
            return;
        }
        self.expire(now);
        self.remove(&key);
        while self.chars + size > RESULT_CACHE_CHARS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((t, _)) = self.entries.remove(&oldest) {
                self.chars -= t.chars().count();
            }
        }
        self.chars += size;
        self.order.push_back(key.clone());
        self.entries.insert(key, (text, now + RESULT_CACHE_TTL));
    }

    fn remove(&mut self, key: &str) {
        if let Some((t, _)) = self.entries.remove(key) {
            self.chars -= t.chars().count();
            self.order.retain(|k| k != key);
        }
    }

    fn expire(&mut self, now: Duration) {
        while let Some(front) = self.order.front() {
            match self.entries.get(front) {
                Some((_, until)) if *until > now => break,
                _ => {
                    let key = self.order.pop_front().unwrap_or_default();
                    if let Some((t, _)) = self.entries.remove(&key) {
                        self.chars -= t.chars().count();
                    }
                }
            }
        }
    }
}

/// The registered servers, their clients and the answers being paged.
pub struct ExternalData {
    servers: RwLock<Arc<Vec<Arc<McpServerSpec>>>>,
    clients: Mutex<HashMap<String, Arc<McpClient>>>,
    answers: Mutex<Answers>,
    connector: RwLock<Connector>,
    clock: RwLock<Arc<dyn Clock>>,
}

impl Default for ExternalData {
    fn default() -> Self {
        Self::new(registry::registered())
    }
}

fn http(spec: &McpServerSpec) -> Option<Arc<dyn Transport>> {
    match HttpTransport::new(spec.connect_timeout, spec.read_timeout) {
        Ok(t) => Some(Arc::new(t)),
        Err(e) => {
            tracing::error!("External data server {} cannot be reached: {}", spec.key, e);
            None
        }
    }
}

impl ExternalData {
    /// The servers given, a malformed spec refused (logged) as the web
    /// refuses one at import.
    pub fn new(specs: Vec<McpServerSpec>) -> Self {
        ExternalData {
            servers: RwLock::new(Arc::new(Self::checked(specs))),
            clients: Mutex::new(HashMap::new()),
            answers: Mutex::new(Answers::default()),
            connector: RwLock::new(Arc::new(http) as Connector),
            clock: RwLock::new(Arc::new(SystemClock::default()) as Arc<dyn Clock>),
        }
    }

    fn checked(specs: Vec<McpServerSpec>) -> Vec<Arc<McpServerSpec>> {
        specs
            .into_iter()
            .filter(|s| match s.check() {
                Ok(()) => true,
                Err(why) => {
                    tracing::error!("External data server not registered: {}", why);
                    false
                }
            })
            .map(Arc::new)
            .collect()
    }

    /// Serve other servers through another transport and clock (a test's).
    #[cfg(test)]
    pub fn install(&self, specs: Vec<McpServerSpec>, connector: Connector, clock: Arc<dyn Clock>) {
        *self.servers.write() = Arc::new(Self::checked(specs));
        *self.connector.write() = connector;
        *self.clock.write() = clock;
        self.clients.lock().clear();
        *self.answers.lock() = Answers::default();
    }

    /// Every registered server, in registry order.
    pub fn servers(&self) -> Arc<Vec<Arc<McpServerSpec>>> {
        self.servers.read().clone()
    }

    /// The servers offered on one surface, in registry order.
    pub fn for_surface(&self, surface: Surface) -> Vec<Arc<McpServerSpec>> {
        self.servers()
            .iter()
            .filter(|s| s.surfaces.contains(surface))
            .cloned()
            .collect()
    }

    /// A server by key, trimmed and matched case-insensitively.
    pub fn server_by_key(&self, key: &str) -> Option<Arc<McpServerSpec>> {
        let wanted = key.trim().to_lowercase();
        self.servers().iter().find(|s| s.key == wanted).cloned()
    }

    /// The shared client of a registered server, built on first use and
    /// again if the registry's spec changed.
    pub fn client(&self, spec: &Arc<McpServerSpec>) -> Option<Arc<McpClient>> {
        let mut clients = self.clients.lock();
        if let Some(c) = clients
            .get(&spec.key)
            .filter(|c| Arc::ptr_eq(&c.spec, spec))
        {
            return Some(c.clone());
        }
        let connect = self.connector.read().clone();
        let transport = connect.as_ref()(spec)?;
        let client = Arc::new(McpClient::new(
            spec.clone(),
            transport,
            self.clock.read().clone(),
        ));
        clients.insert(spec.key.clone(), client.clone());
        Some(client)
    }

    /// Test the connection to the servers named (every one when `None`),
    /// one after another in registry order. Never fails.
    pub async fn check_servers(&self, keys: Option<Vec<String>>) -> Vec<ServerCheck> {
        let wanted: Option<Vec<String>> =
            keys.map(|k| k.iter().map(|x| x.trim().to_lowercase()).collect());
        let mut results = Vec::new();
        for spec in self.servers().iter() {
            if wanted.as_ref().is_some_and(|w| !w.contains(&spec.key)) {
                continue;
            }
            let check = match self.client(spec) {
                Some(client) => client.check().await,
                None => ServerCheck {
                    key: spec.key.clone(),
                    title: spec.title.clone(),
                    ok: false,
                    latency_ms: 0,
                    tool_count: 0,
                    message: format!(
                        "The test for {} could not be run. Try again in a moment.",
                        spec.title
                    ),
                    server_name: String::new(),
                },
            };
            tracing::info!(
                "MCP connection test {}: ok={} latency_ms={} tools={}",
                spec.key,
                check.ok,
                check.latency_ms,
                check.tool_count
            );
            results.push(check);
        }
        results
    }

    /// A full answer kept for paging.
    pub fn answer(&self, key: &str) -> Option<Arc<String>> {
        let now = self.clock.read().now();
        self.answers.lock().get(key, now)
    }

    /// Keep a full answer for paging.
    pub fn keep_answer(&self, key: String, text: Arc<String>) {
        let now = self.clock.read().now();
        self.answers.lock().put(key, text, now);
    }
}
