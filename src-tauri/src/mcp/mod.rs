//! The native MCP (Model Context Protocol) server: the web's `mcp/mcpserver.py`
//! tools, served two ways.
//!
//! * **HTTP** at `POST /mcp` and `GET /mcp` on the app's own server
//!   ([`http`]), at parity with the web's `blueprints/mcp_http.py`: JSON-RPC
//!   2.0 (`initialize`, `ping`, `tools/list`, `tools/call`), a bearer token
//!   with scopes, 401 / 403 challenges, per-token rate limits, an audit table
//!   and a kill switch.
//! * **stdio**, as `openalgo-desktop mcp --url <app> --token <token>`
//!   ([`stdio`]), for Claude Desktop, Claude Code, Cursor and the like.
//!
//! # One process writes the databases
//!
//! The stdio process never opens the data directory. It is a thin MCP server
//! (rmcp over stdin and stdout) that forwards `tools/list` and `tools/call` to
//! the running app's `/mcp` over loopback with an MCP token, so every call
//! runs in the app, through the same registry, scope checks, rate limits,
//! kill switch and audit as an HTTP client, and the app stays the only
//! process that writes `openalgo.db`, `logs.db`, `sandbox.db` and the
//! Historify store. Opening the databases read-only from a second process was
//! rejected: it could not place orders (most of what the tools are for), it
//! would need its own broker session, symbol master and sandbox engine, and
//! it would read the keychain from a process the trader never signed in to.
//! When the app is not running the stdio server still starts and answers
//! every call with a message saying to open OpenAlgo Desktop.
//!
//! # Tools call the services in process
//!
//! The web's tools call `/api/v1` through the Python SDK, and under its
//! gthread worker the HTTP transport serves those SDK calls in process
//! (`_InProcessWsgi`). The desktop does the same without a socket: a tool
//! builds the SDK's payload and hands it to the `/api/v1` handler stack
//! ([`dispatch`]), so validation, analyzer (sandbox) routing, Semi-Auto
//! queuing, events and response shapes are exactly those of `/api/v1`. The
//! research tools compute the `openalgo.ta` indicators in Rust ([`ta`]).

pub mod admin;
pub mod catalog;
pub mod dispatch;
pub mod envelope;
pub mod http;
pub mod research;
pub mod schema;
pub mod stdio;
pub mod store;
pub mod ta;
pub mod tools;

use serde_json::{json, Value};

/// OAuth-style scopes, as the web's `utils/mcp_tool_registry.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    ReadMarket,
    ReadAccount,
    WriteOrders,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::ReadMarket => "read:market",
            Scope::ReadAccount => "read:account",
            Scope::WriteOrders => "write:orders",
        }
    }
}

/// Output risk: selects the trust-boundary instruction around a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    BrokerStructured,
    ExternalText,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::BrokerStructured => "broker_structured",
            Risk::ExternalText => "external_text",
        }
    }
}

/// One tool: the web's registration metadata and signature.
#[derive(Debug)]
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub toolset: &'static str,
    pub scope: Scope,
    pub risk: Risk,
    /// Changes broker state or sends something outward.
    pub write: bool,
    pub destructive: bool,
    pub open_world: bool,
    pub description: &'static str,
    pub params: &'static [schema::Param],
}

impl ToolDef {
    /// The `tools/list` descriptor, as the web's HTTP transport builds it
    /// (`_tool_descriptor`): name, description, input schema, annotations.
    pub fn descriptor(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": schema::input_schema(self.name, self.params),
            "annotations": {
                "title": self.title,
                "readOnlyHint": !self.write,
                "destructiveHint": self.destructive,
                "idempotentHint": !self.write,
                "openWorldHint": self.open_world,
            },
        })
    }
}

/// The tool with this name.
pub fn tool(name: &str) -> Option<&'static ToolDef> {
    catalog::TOOLS.iter().find(|t| t.name == name)
}

/// Tool names callable under any of `scopes`, sorted (web
/// `list_tools_for_scopes`).
pub fn tools_for_scopes(scopes: &[Scope]) -> Vec<&'static ToolDef> {
    let mut v: Vec<_> = catalog::TOOLS
        .iter()
        .filter(|t| scopes.contains(&t.scope))
        .collect();
    v.sort_by_key(|t| t.name);
    v
}

pub use http::McpRuntime;
