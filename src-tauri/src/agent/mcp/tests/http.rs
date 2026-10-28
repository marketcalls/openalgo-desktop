//! The real HTTP transport against a local fake MCP server on an ephemeral
//! port, its answers written from rmcp's own model types (the MCP SDK the
//! desktop's server uses): the handshake, the session header, a JSON and an
//! event-stream reply, UTF-8, an expired session, no redirect followed and
//! a read timeout.

use crate::agent::mcp::client::McpClient;
use crate::agent::mcp::registry::McpServerSpec;
use crate::agent::mcp::transport::{HttpTransport, SystemClock};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use parking_lot::Mutex;
use rmcp::model::{
    CallToolResult, Content, Implementation, InitializeResult, ListToolsResult, ServerCapabilities,
    Tool,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct Seen {
    user_agents: Vec<String>,
    sessions: Vec<Option<String>>,
    forget_next: bool,
    issued: usize,
}

type Shared = Arc<Mutex<Seen>>;

fn rpc(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

async fn mcp(State(seen): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let message: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let session = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut s = seen.lock();
    s.user_agents.push(
        headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .into(),
    );
    s.sessions.push(session.clone());
    let id = message["id"].clone();
    match message["method"].as_str() {
        Some("initialize") => {
            s.issued += 1;
            let result = InitializeResult::new(ServerCapabilities::default())
                .with_server_info(Implementation::new("rmcp-fake", "1"));
            let mut r = rpc(&id, serde_json::to_value(result).unwrap()).into_response();
            r.headers_mut()
                .insert("mcp-session-id", format!("h{}", s.issued).parse().unwrap());
            r.headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            r
        }
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some(_) if s.forget_next => {
            s.forget_next = false;
            (StatusCode::NOT_FOUND, "Session not found").into_response()
        }
        Some("tools/list") => {
            let schema = json!({"type": "object", "properties": {"symbol": {"type": "string"}}});
            let tool = Tool::new(
                "get_close",
                "Close in \u{20b9} for one symbol.",
                Arc::new(schema.as_object().cloned().unwrap()),
            );
            let listed = serde_json::to_value(ListToolsResult::with_all_items(vec![tool])).unwrap();
            let body = format!(
                ": keepalive\n\nevent: message\ndata: {}\n\n",
                rpc(&id, listed)
            );
            ([("content-type", "text/event-stream")], body).into_response()
        }
        Some("tools/call") => {
            let result = CallToolResult::success(vec![Content::text("{\"close\":812.5}")]);
            (
                [("content-type", "application/json")],
                rpc(&id, serde_json::to_value(result).unwrap()),
            )
                .into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn moved() -> Response {
    (StatusCode::TEMPORARY_REDIRECT, [("location", "/mcp")], "").into_response()
}

async fn silent() -> Response {
    std::future::pending::<()>().await;
    StatusCode::OK.into_response()
}

struct Fake {
    port: u16,
    seen: Shared,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fake() -> Fake {
    let seen: Shared = Arc::default();
    let app = axum::Router::new()
        .route("/mcp", post(mcp))
        .route("/moved", post(moved))
        .route("/silent", post(silent))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Fake { port, seen, task }
}

fn client(port: u16, path: &str, read: Duration) -> McpClient {
    let mut spec = McpServerSpec::new(
        "local",
        "Local data",
        &format!("http://127.0.0.1:{}{}", port, path),
        "d",
        "u",
    );
    spec.read_timeout = read;
    spec.max_retries = 0;
    let transport = HttpTransport::new(spec.connect_timeout, spec.read_timeout).unwrap();
    McpClient::new(
        Arc::new(spec),
        Arc::new(transport),
        Arc::new(SystemClock::default()),
    )
}

#[tokio::test]
async fn the_real_transport_speaks_to_an_mcp_server() {
    let f = fake().await;
    let c = client(f.port, "/mcp", Duration::from_secs(10));
    let check = c.check().await;
    assert!(check.ok, "{:?}", check);
    assert_eq!(
        (check.tool_count, check.server_name.as_str()),
        (1, "rmcp-fake")
    );
    let tools = c.list_tools(false).await.unwrap();
    assert!(tools[0]["description"]
        .as_str()
        .unwrap()
        .contains('\u{20b9}'));
    let r = c.call_tool("get_close", &Map::new()).await.unwrap();
    assert_eq!(r.text, "{\"close\":812.5}");
    {
        let s = f.seen.lock();
        assert!(s
            .user_agents
            .iter()
            .all(|u| u.starts_with("OpenAlgo-Agent/")));
        assert_eq!(s.sessions[0], None);
        assert_eq!(s.sessions.last().cloned().flatten().as_deref(), Some("h1"));
    }
    // The server forgets the session: one fresh handshake, then the call.
    f.seen.lock().forget_next = true;
    c.call_tool("get_close", &Map::new()).await.unwrap();
    assert_eq!(f.seen.lock().issued, 2);
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let f = fake().await;
    let e = client(f.port, "/moved", Duration::from_secs(10))
        .list_tools(true)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "protocol_error");
    assert!(
        f.seen.lock().user_agents.is_empty(),
        "the redirect target was never asked"
    );
}

#[tokio::test]
async fn a_server_that_holds_the_request_times_out() {
    let f = fake().await;
    let e = client(f.port, "/silent", Duration::from_millis(300))
        .list_tools(true)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "timed_out");
    assert!(e.message.starts_with("Local data took too long to answer."));
}
