//! The external data servers the Agent may reach over MCP (the web's
//! `services/agent/mcp/registry.py`).
//!
//! Adding a server is one entry in [`registered`]: the toolkit, the prompt
//! section, the settings page and the connection test all read it. The key
//! is what the model passes as `server`, so keep it short, lower case and
//! stable.
//!
//! A spec carries no credential, by design, and no per-tool argument rules:
//! the server's own `inputSchema` is the contract, read live and cached.

use crate::agent::contract::tool::{Surface, Surfaces};
use std::time::Duration;

/// The one transport implemented: JSON-RPC 2.0 over HTTP POST, answered
/// with JSON or a short `text/event-stream`. The older HTTP+SSE transport is
/// deprecated, and stdio would start a process per server.
pub const TRANSPORT_STREAMABLE_HTTP: &str = "streamable-http";

/// One external MCP server.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerSpec {
    /// What the model passes as `server`.
    pub key: String,
    /// A short name a trader reads on the settings page and in an error.
    pub title: String,
    /// The streamable-HTTP endpoint; https only.
    pub url: String,
    /// One or two sentences for the settings page.
    pub description: String,
    /// What the model should reach for this server for (the prompt).
    pub use_for: String,
    /// Data limits the model must state or respect (the prompt).
    pub caveats: String,
    /// Who runs the server, named in a failure so a trader knows the fault
    /// is not in their own setup.
    pub provider: String,
    pub transport: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// One tool call's budget, re-initialisation and retry included.
    pub call_deadline: Duration,
    /// Extra attempts after a timeout, a failed connection or a 5xx.
    pub max_retries: u32,
    /// Consecutive failed calls that pause the server.
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
    pub tools_cache_ttl: Duration,
    /// How long a failed tool-list fetch is remembered.
    pub negative_cache_ttl: Duration,
    /// When not empty, the only tools offered.
    pub allow_tools: Vec<String>,
    /// Tools never offered.
    pub deny_tools: Vec<String>,
    pub surfaces: Surfaces,
}

impl McpServerSpec {
    /// A spec with the web's defaults for everything after `use_for`.
    pub fn new(key: &str, title: &str, url: &str, description: &str, use_for: &str) -> Self {
        McpServerSpec {
            key: key.into(),
            title: title.into(),
            url: url.into(),
            description: description.into(),
            use_for: use_for.into(),
            caveats: String::new(),
            provider: String::new(),
            transport: TRANSPORT_STREAMABLE_HTTP.into(),
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(10),
            call_deadline: Duration::from_secs(25),
            max_retries: 1,
            breaker_threshold: 3,
            breaker_cooldown: Duration::from_secs(60),
            tools_cache_ttl: Duration::from_secs(3600),
            negative_cache_ttl: Duration::from_secs(30),
            allow_tools: Vec::new(),
            deny_tools: Vec::new(),
            surfaces: Surfaces::ALL,
        }
    }

    /// The web's `__post_init__` checks: a spec that would fail at call
    /// time is refused when the registry is read instead.
    pub fn check(&self) -> Result<(), String> {
        let key_ok = self.key.len() >= 2
            && self.key.len() <= 32
            && self.key.starts_with(|c: char| c.is_ascii_lowercase())
            && self
                .key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !key_ok {
            return Err(format!(
                "MCP server key {:?} must be 2 to 32 lower-case letters, digits or underscores, starting with a letter",
                self.key
            ));
        }
        let https = reqwest_agent::Url::parse(&self.url)
            .is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()));
        if !https {
            return Err(format!(
                "MCP server {:?} needs an https URL, got {:?}",
                self.key, self.url
            ));
        }
        if self.transport != TRANSPORT_STREAMABLE_HTTP {
            return Err(format!(
                "MCP server {:?} uses unknown transport {:?}",
                self.key, self.transport
            ));
        }
        let durations = [
            self.connect_timeout,
            self.read_timeout,
            self.call_deadline,
            self.breaker_cooldown,
            self.tools_cache_ttl,
            self.negative_cache_ttl,
        ];
        if durations.iter().any(Duration::is_zero) {
            return Err(format!(
                "MCP server {:?}: every timeout must be positive",
                self.key
            ));
        }
        if self.breaker_threshold < 1 {
            return Err(format!(
                "MCP server {:?}: breaker_threshold must be >= 1",
                self.key
            ));
        }
        if let Some(both) = self
            .allow_tools
            .iter()
            .find(|t| self.deny_tools.contains(t))
        {
            return Err(format!(
                "MCP server {:?} both allows and denies: {}",
                self.key, both
            ));
        }
        if ![Surface::Chat, Surface::Chart, Surface::Voice]
            .iter()
            .any(|s| self.surfaces.contains(*s))
        {
            return Err(format!("MCP server {:?} names no surface", self.key));
        }
        Ok(())
    }

    /// Whether the allowlist and denylist let a tool through: deny wins, an
    /// empty allowlist lets everything else through.
    pub fn offers(&self, tool: &str) -> bool {
        !self.deny_tools.iter().any(|t| t == tool)
            && (self.allow_tools.is_empty() || self.allow_tools.iter().any(|t| t == tool))
    }
}

/// Every server the Agent may reach, in the order the prompt and the
/// settings page list them.
///
/// Empty, as on the web: NSE's two MCP servers (end-of-day bhavcopy and the
/// live cash-market snapshot) were the first entries and were removed on
/// 2026-10-09, because in repeated live checks both stalled for 10 to 20
/// seconds and then timed out. With nothing registered the toolkit is not
/// built, the prompt says nothing about it and the settings page hides the
/// section. Before a server run by a third party is added here, read
/// `agent::mcp::taint`: every argument sent is built from the trader's own
/// words.
pub fn registered() -> Vec<McpServerSpec> {
    Vec::new()
}
