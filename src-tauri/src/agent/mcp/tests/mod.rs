//! External data (the web's `test/test_agent_mcp.py`): the client against a
//! scripted server behind the transport seam ([`client`]), the toolkit, the
//! switch, the prompt section, the registry and the taint boundary
//! ([`toolkit`]), and the real HTTP transport against a local fake MCP
//! server built from rmcp's own model types ([`http`]). Nothing here
//! touches the network.

mod client;
mod http;
mod toolkit;

use super::client::McpClient;
use super::registry::McpServerSpec;
use super::testing::{FakeClock, FakeServer, Seam};
use std::sync::Arc;

/// A client for `spec` on a fresh scripted server and clock.
pub(super) fn client_for(spec: McpServerSpec) -> (Arc<McpClient>, Arc<FakeServer>, Arc<FakeClock>) {
    let server = Arc::new(FakeServer::default());
    let clock = Arc::new(FakeClock::default());
    let client = Arc::new(McpClient::new(
        Arc::new(spec),
        Arc::new(Seam(server.clone())),
        clock.clone(),
    ));
    (client, server, clock)
}
