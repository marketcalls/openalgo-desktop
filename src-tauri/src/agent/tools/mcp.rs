//! External data over MCP: one toolkit for every registered server (the
//! web's `services/agent/tools/mcp.py`, toolkit `mcp`).
//!
//! Two tools, whatever the number of servers: `list_mcp_tools` returns a
//! catalogue (or one tool in full), and `call_mcp_tool` checks the server
//! and the tool against the registry and the live list, builds the
//! arguments through the taint boundary (`agent::mcp::taint`, a desktop
//! fix of the web's D-021) and calls it. The arguments are not checked
//! against the server's schema: the server is the authority, and its
//! refusal comes back as a tool error the model can act on.
//!
//! Building the toolkit does no network work. Everything a server returns,
//! its own error text included, is third-party text and is wrapped
//! `trust="third-party"`. The tools are read-only and need no approval; no
//! OpenAlgo key or broker credential ever goes to a server.
//!
//! A long answer is paged ([`paging`]); the full text is kept five minutes
//! so page two needs no second call, and page one always calls, so a live
//! snapshot is never served from memory.

mod call;
mod catalogue;
mod paging;

use super::lane_specs::web_or_log;
use crate::agent::contract::json::dumps_capped_ordered;
use crate::agent::contract::tool::{
    AgentTool, Capability, Surfaces, ToolCtx, ToolGroup, ToolOutcome, ToolSpec, MAX_RESULT_CHARS,
};
use crate::agent::mcp::client::McpClient;
use crate::agent::mcp::errors::{self, Kind, McpError};
use crate::agent::mcp::registry::McpServerSpec;
use crate::agent::prompts::untrusted::wrap_tool_result;
use crate::agent::tools::service::py_repr;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Ceiling on the arguments the model may send, serialised as the web
/// measures them.
pub const MAX_ARGUMENT_CHARS: usize = 4000;

const TRUST: &str = "third-party";

/// The toolkit (web `ToolkitSpec` `mcp`): every surface, each server
/// narrowing its own; only when the run's external data switch is on.
pub const GROUP: ToolGroup = ToolGroup {
    key: "mcp",
    surfaces: Surfaces::ALL,
    requires: &[Capability::Mcp],
    order: 85,
    description: "Read external market data from the registered MCP servers.",
    build,
};

fn build(ctx: &ToolCtx) -> Vec<Arc<dyn AgentTool>> {
    let servers = ctx.app.agent.mcp().for_surface(ctx.surface);
    [
        ("list_mcp_tools", Which::List),
        ("call_mcp_tool", Which::Call),
    ]
    .into_iter()
    .filter_map(|(name, kind)| {
        let mut spec = web_or_log(GROUP.key, name)?;
        spec.description = Cow::Owned(described(&spec.description, &servers));
        Some(Arc::new(McpTool {
            spec,
            kind,
            servers: servers.clone(),
        }) as Arc<dyn AgentTool>)
    })
    .collect()
}

/// A tool's description with the servers offered here named in it: a
/// model picks a tool by its description, and a generic one lost every
/// exchange statistic to web search in the web's live testing.
fn described(base: &str, servers: &[Arc<McpServerSpec>]) -> String {
    if servers.is_empty() {
        return base.to_string();
    }
    let catalogue: Vec<String> = servers
        .iter()
        .map(|s| {
            format!(
                "{} ({}): {}",
                s.key,
                s.title,
                s.use_for.trim_end_matches(['.', ' '])
            )
        })
        .collect();
    format!(
        "{}\n\nServers offered here: {}. Prefer these to web search for any figure they publish.",
        base,
        catalogue.join("; ")
    )
}

#[derive(Clone, Copy)]
enum Which {
    List,
    Call,
}

struct McpTool {
    spec: ToolSpec,
    kind: Which,
    servers: Vec<Arc<McpServerSpec>>,
}

#[derive(Serialize)]
struct Row {
    name: String,
    summary: String,
    arguments: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required: Option<Vec<String>>,
}

#[derive(Serialize)]
struct Listing<'a> {
    ok: bool,
    server: &'a str,
    title: &'a str,
    tool_count: usize,
    tools: Vec<Row>,
    note: &'static str,
}

#[derive(Serialize)]
struct OneTool {
    name: String,
    description: String,
    input_schema: catalogue::CompactSchema,
}

#[derive(Serialize)]
struct Described<'a> {
    ok: bool,
    server: &'a str,
    title: &'a str,
    tool: OneTool,
}

#[derive(Serialize)]
struct Answer<'a> {
    ok: bool,
    server: &'a str,
    tool: &'a str,
    page: usize,
    pages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<String>,
    content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
}

#[derive(Serialize)]
struct Unavailable<'a> {
    ok: bool,
    server: &'a str,
    error: &'static str,
    message: String,
    next_step: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<u64>,
}

#[derive(Serialize)]
struct ToolFailed<'a> {
    ok: bool,
    server: &'a str,
    tool: &'a str,
    error: &'static str,
    message: String,
    server_message: String,
    next_step: &'static str,
}

/// A payload labelled as data, within the shared cap.
fn wrapped<T: Serialize>(tool: &str, payload: &T, labels: &[(&str, Option<&str>)]) -> ToolOutcome {
    ToolOutcome::Ok(wrap_tool_result(
        tool,
        &dumps_capped_ordered(payload, MAX_RESULT_CHARS),
        labels,
    ))
}

/// A server that could not be reached or understood: the client's trader
/// sentence, never a status code. Our own words, so no trust label.
fn failure(tool: &str, spec: &McpServerSpec, e: &McpError, mcp_tool: Option<&str>) -> ToolOutcome {
    let payload = Unavailable {
        ok: false,
        server: &spec.key,
        error: e.code(),
        message: e.message.clone(),
        next_step: "Tell the operator this source is unavailable right now and answer from the broker tools where you can. Do not call this server again this turn.",
        retry_after_seconds: match e.kind {
            Kind::Paused(after) => Some(after.as_secs() + 1),
            _ => None,
        },
    };
    wrapped(
        tool,
        &payload,
        &[("server", Some(spec.key.as_str())), ("mcp_tool", mcp_tool)],
    )
}

/// A tool the server ran and reported as failed: its own words kept (they
/// usually name the bad argument), as third-party text.
fn tool_failed(spec: &McpServerSpec, name: &str, e: &McpError, text: &str) -> ToolOutcome {
    let payload = ToolFailed {
        ok: false,
        server: &spec.key,
        tool: name,
        error: e.code(),
        message: e.message.clone(),
        server_message: text.chars().take(2000).collect(),
        next_step: "Read server_message. If it names a bad argument, fix it once (list_mcp_tools shows the arguments); otherwise report it to the operator.",
    };
    wrapped(
        "call_mcp_tool",
        &payload,
        &[
            ("server", Some(spec.key.as_str())),
            ("mcp_tool", Some(name)),
            ("trust", Some(TRUST)),
        ],
    )
}

/// Python's `str()` of an argument as sent.
fn as_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Python's `json.dumps` length with its default separators (`, ` and
/// `: `), which the web's argument cap measures.
fn python_dumps_len(v: &Value) -> usize {
    match v {
        Value::Array(items) => {
            2 + items.iter().map(python_dumps_len).sum::<usize>()
                + 2 * items.len().saturating_sub(1)
        }
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(k, v)| python_dumps_len(&Value::String(k.clone())) + 2 + python_dumps_len(v))
                .sum::<usize>()
                + 2 * map.len().saturating_sub(1)
        }
        other => other.to_string().chars().count(),
    }
}

impl McpTool {
    /// The `server` argument among this surface's servers.
    fn server(&self, raw: Option<&Value>) -> Result<Arc<McpServerSpec>, ToolOutcome> {
        let given = as_text(raw);
        let wanted = given.trim().to_lowercase();
        if let Some(s) = self.servers.iter().find(|s| s.key == wanted) {
            return Ok(s.clone());
        }
        let keys: Vec<&str> = self.servers.iter().map(|s| s.key.as_str()).collect();
        let shown: String = given.chars().take(60).collect();
        Err(ToolOutcome::invalid_argument(
            "server",
            &format!(
                "{} is not an external data server here.",
                py_repr(&Value::String(shown))
            ),
            Some(&format!(
                "Use one of: {}.",
                if keys.is_empty() {
                    "none".to_string()
                } else {
                    keys.join(", ")
                }
            )),
        ))
    }

    /// A transport for the server, or its failure as the model reads it.
    fn client(
        &self,
        ctx: &ToolCtx,
        spec: &Arc<McpServerSpec>,
        mcp_tool: Option<&str>,
    ) -> Result<Arc<McpClient>, ToolOutcome> {
        ctx.app.agent.mcp().client(spec).ok_or_else(|| {
            let e = errors::unreachable(spec, "no transport");
            failure(self.spec.name, spec, &e, mcp_tool)
        })
    }

    async fn list(&self, ctx: &ToolCtx, args: &Map<String, Value>) -> ToolOutcome {
        let tool = "list_mcp_tools";
        let spec = match self.server(args.get("server")) {
            Ok(s) => s,
            Err(r) => return r,
        };
        let wanted = as_text(args.get("tool")).trim().to_string();
        let client = match self.client(ctx, &spec, None) {
            Ok(c) => c,
            Err(r) => return r,
        };
        let tools = match client.list_tools(false).await {
            Ok(t) => t,
            Err(e) => return failure(tool, &spec, &e, None),
        };
        let name_of = |t: &Map<String, Value>| {
            t.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let labels = [("server", Some(spec.key.as_str())), ("trust", Some(TRUST))];
        if wanted.is_empty() {
            let rows: Vec<Row> = tools
                .iter()
                .map(|t| Row {
                    name: name_of(t),
                    summary: catalogue::summary(t.get("description")),
                    arguments: catalogue::argument_types(t.get("inputSchema")),
                    required: catalogue::compact_schema(t.get("inputSchema")).required,
                })
                .collect();
            let payload = Listing {
                ok: true,
                server: &spec.key,
                title: &spec.title,
                tool_count: rows.len(),
                tools: rows,
                note: "Call list_mcp_tools again with tool set to one name for its full description and argument details.",
            };
            return wrapped(tool, &payload, &labels);
        }
        let Some(found) = tools.iter().find(|t| name_of(*t) == wanted) else {
            let mut names: Vec<String> = tools.iter().map(name_of).collect();
            names.sort();
            let shown: String = wanted.chars().take(80).collect();
            return ToolOutcome::invalid_argument(
                "tool",
                &format!(
                    "{} offers no tool named {}.",
                    spec.key,
                    py_repr(&Value::String(shown))
                ),
                Some(&format!("Pick one of: {}.", names.join(", "))),
            );
        };
        let payload = Described {
            ok: true,
            server: &spec.key,
            title: &spec.title,
            tool: OneTool {
                name: name_of(found),
                description: catalogue::description(found.get("description")),
                input_schema: catalogue::compact_schema(found.get("inputSchema")),
            },
        };
        wrapped(tool, &payload, &labels)
    }
}

#[async_trait]
impl AgentTool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx, args: Map<String, Value>) -> ToolOutcome {
        match self.kind {
            Which::List => self.list(ctx, &args).await,
            Which::Call => call::call(self, ctx, &args).await,
        }
    }
}
