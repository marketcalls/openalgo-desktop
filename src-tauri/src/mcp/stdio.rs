//! `openalgo-desktop mcp --url <app>`: the stdio transport.
//!
//! The MCP token comes from the `OPENALGO_MCP_TOKEN` environment variable,
//! which the AI client sets from its own server configuration (the `env`
//! block of `claude_desktop_config.json`, `claude mcp add -e`). It is never
//! a command-line argument, so it does not show in the process list or in
//! shell history, and it is never logged or echoed.
//!
//! An rmcp server over stdin and stdout that forwards `tools/list` and
//! `tools/call` to the running app's `POST /mcp` over loopback (see the
//! module docs of [`crate::mcp`] for why it never opens the data
//! directory). It handles the MCP handshake itself, so a client can start
//! it before the app is open; a call made while the app is closed answers
//! with a message saying so.
//!
//! Nothing but MCP messages is written to stdout; diagnostics go to stderr.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, ErrorData, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Value};
use std::time::Duration;

/// Longer than a tool's own limit (120 s for an `/api/v1` call), so the app
/// answers first.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);

pub const NOT_RUNNING: &str =
    "OpenAlgo Desktop is not running, so this request could not be sent. \
Open OpenAlgo Desktop, sign in, and try again.";
pub const BAD_TOKEN: &str =
    "OpenAlgo did not accept this AI client's token: it was revoked or copied incorrectly. \
Create a new token on the API Key page in OpenAlgo Desktop and update this client's configuration.";
pub const BUSY: &str =
    "OpenAlgo is receiving too many requests from this AI client. Wait a minute and try again.";

/// The environment variable that carries the MCP token.
pub const TOKEN_ENV: &str = "OPENALGO_MCP_TOKEN";

pub const USAGE: &str = "Usage: openalgo-desktop mcp [--url http://127.0.0.1:5000]\n\
The token is read from the OPENALGO_MCP_TOKEN environment variable, set in the AI client's \
configuration. Create it on the API Key page in OpenAlgo Desktop, which shows the full configuration.";

/// Shown when the client configuration does not supply the token.
pub const MISSING_TOKEN: &str =
    "OpenAlgo Desktop could not connect this AI client because no token was given. \
Create a token on the API Key page in OpenAlgo Desktop, paste the configuration it shows into this \
AI client (it sets OPENALGO_MCP_TOKEN), then restart the client.";

/// The forwarding server.
#[derive(Clone)]
pub struct Bridge {
    http: reqwest::Client,
    endpoint: String,
    token: String,
}

/// Why a forwarded request got no JSON-RPC answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// Trader-facing text (app closed, token refused, too many requests).
    Message(String),
}

impl Bridge {
    pub fn new(base_url: &str, token: &str) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()?;
        Ok(Self {
            http,
            endpoint: format!("{}/mcp", base_url.trim_end_matches('/')),
            token: token.to_string(),
        })
    }

    /// One JSON-RPC request to the app; `Ok` carries the JSON-RPC reply.
    pub async fn forward(&self, method: &str, params: Value) -> Result<Value, Failure> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let resp = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("OpenAlgo Desktop not reachable at {}: {}", self.endpoint, e);
                return Err(Failure::Message(NOT_RUNNING.into()));
            }
        };
        let status = resp.status().as_u16();
        let value = read_capped(resp).await;
        match status {
            200 => Ok(value),
            401 => Err(Failure::Message(BAD_TOKEN.into())),
            403 => Err(Failure::Message(
                value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(BAD_TOKEN)
                    .to_string(),
            )),
            429 => Err(Failure::Message(BUSY.into())),
            other => {
                tracing::warn!("OpenAlgo Desktop answered {} to an MCP request", other);
                Err(Failure::Message(NOT_RUNNING.into()))
            }
        }
    }
}

/// Largest reply the bridge reads from the app.
pub const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

/// The reply as JSON, read up to [`MAX_REPLY_BYTES`] (`Null` past it).
async fn read_capped(mut resp: reqwest::Response) -> Value {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if buf.len() + c.len() > MAX_REPLY_BYTES {
                    tracing::warn!("OpenAlgo Desktop sent an MCP reply over the size limit");
                    return Value::Null;
                }
                buf.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(_) => return Value::Null,
        }
    }
    serde_json::from_slice(&buf).unwrap_or(Value::Null)
}

fn rpc_error(reply: &Value) -> Option<ErrorData> {
    let e = reply.get("error")?;
    Some(ErrorData::new(
        ErrorCode(e.get("code").and_then(Value::as_i64).unwrap_or(-32603) as i32),
        e.get("message")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string(),
        e.get("data").cloned(),
    ))
}

fn internal(msg: &str) -> ErrorData {
    ErrorData::new(ErrorCode(-32603), msg.to_string(), None)
}

impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("openalgo", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let reply = match self.forward("tools/list", json!({})).await {
            Ok(r) => r,
            Err(Failure::Message(m)) => return Err(internal(&m)),
        };
        if let Some(e) = rpc_error(&reply) {
            return Err(e);
        }
        let result = reply.get("result").cloned().unwrap_or(json!({"tools": []}));
        serde_json::from_value(result).map_err(|e| {
            tracing::error!("Unreadable tools/list reply: {}", e);
            internal("OpenAlgo Desktop sent a tool list this client could not read.")
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let params =
            json!({"name": request.name, "arguments": request.arguments.unwrap_or_default()});
        let reply = match self.forward("tools/call", params).await {
            Ok(r) => r,
            Err(Failure::Message(m)) => {
                let r: CallToolResult = serde_json::from_value(json!({
                    "content": [{"type": "text", "text": m}],
                    "isError": true,
                }))
                .map_err(|_| internal(NOT_RUNNING))?;
                return Ok(r.into());
            }
        };
        if let Some(e) = rpc_error(&reply) {
            return Err(e);
        }
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        let r: CallToolResult = serde_json::from_value(result).map_err(|e| {
            tracing::error!("Unreadable tools/call reply: {}", e);
            internal("OpenAlgo Desktop sent a reply this client could not read.")
        })?;
        Ok(r.into())
    }
}

/// Serve MCP on `reader` / `writer` until the client closes it.
pub async fn serve<R, W>(bridge: Bridge, reader: R, writer: W) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let running = bridge
        .serve((reader, writer))
        .await
        .map_err(|e| format!("MCP session did not start: {}", e))?;
    running
        .waiting()
        .await
        .map_err(|e| format!("MCP session ended with an error: {}", e))?;
    Ok(())
}

/// Parsed `mcp` subcommand arguments (the token is never one of them).
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub url: String,
}

/// The app's default address (development port in debug builds).
pub fn default_url() -> String {
    let dev = cfg!(debug_assertions)
        || matches!(
            std::env::var("OPENALGO_DESKTOP_DEV_PORTS").as_deref(),
            Ok("1")
        );
    format!("http://127.0.0.1:{}", if dev { 5500 } else { 5000 })
}

/// Parse `--url`. Anything else is refused without echoing its value.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut url = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match key {
            "--url" => url = inline.or_else(|| it.next().cloned()),
            "-h" | "--help" => return Err(USAGE.into()),
            other if other.starts_with('-') => {
                return Err(format!("Unknown option {}.\n{}", other, USAGE))
            }
            _ => return Err(format!("Unexpected argument.\n{}", USAGE)),
        }
    }
    Ok(Options {
        url: url.unwrap_or_else(default_url),
    })
}

/// The token from the environment variable's value; refused when missing.
pub fn token_from(value: Option<String>) -> Result<String, &'static str> {
    value
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or(MISSING_TOKEN)
}

/// Entry point of `openalgo-desktop mcp ...` (called from `main` before
/// Tauri starts). Returns the process exit code.
pub fn main(args: &[String]) -> i32 {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .try_init();
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(m) => {
            eprintln!("{}", m);
            return 2;
        }
    };
    let token = match token_from(std::env::var(TOKEN_ENV).ok()) {
        Ok(t) => t,
        Err(m) => {
            eprintln!("{}", m);
            return 2;
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Could not start: {}", e);
            return 1;
        }
    };
    rt.block_on(async {
        let bridge = match Bridge::new(&opts.url, &token) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Could not start: {}", e);
                return 1;
            }
        };
        let (stdin, stdout) = rmcp::transport::io::stdio();
        match serve(bridge, stdin, stdout).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_have_no_token_flag() {
        let o = parse_args(&s(&["--url", "http://127.0.0.1:5000"])).unwrap();
        assert_eq!(o.url, "http://127.0.0.1:5000");
        assert_eq!(parse_args(&s(&["--url=http://x"])).unwrap().url, "http://x");
        assert_eq!(parse_args(&s(&[])).unwrap().url, default_url());
        // A token on the command line is refused and never echoed.
        for bad in [
            &["--token", "oamcp_secret"][..],
            &["--token=oamcp_secret"],
            &["oamcp_secret"],
        ] {
            let e = parse_args(&s(bad)).unwrap_err();
            assert!(!e.contains("oamcp_secret"), "{}", e);
        }
    }

    #[test]
    fn token_comes_from_the_environment_value() {
        assert_eq!(token_from(Some(" oamcp_x ".into())).unwrap(), "oamcp_x");
        assert_eq!(token_from(None).unwrap_err(), MISSING_TOKEN);
        assert_eq!(token_from(Some("  ".into())).unwrap_err(), MISSING_TOKEN);
    }
}
