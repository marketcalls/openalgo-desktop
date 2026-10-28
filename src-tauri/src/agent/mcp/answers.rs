//! What the external data routes answer (the web's `get_mcp_servers` and
//! `test_mcp_servers` in `blueprints/agent.py`). The switch itself is the
//! `mcp_enabled` setting, written through `PUT /agent/api/settings`.

use crate::agent::reply::Refusal;
use crate::agent::settings::{schema::MCP_ENABLED, Snapshot};
use crate::state::AppState;
use serde_json::{json, Value};

/// `GET /agent/api/mcp`: the switch (read fresh) and the registered
/// servers. No server is contacted.
pub fn servers_answer(app: &AppState) -> Value {
    let servers: Vec<Value> = app
        .agent
        .mcp()
        .servers()
        .iter()
        .map(
            |s| json!({"key": s.key, "title": s.title, "description": s.description, "url": s.url}),
        )
        .collect();
    json!({
        "status": "success",
        "enabled": Snapshot::read(app).flag(MCP_ENABLED),
        "servers": servers,
    })
}

/// `POST /agent/api/mcp/test`: every server, or the one the optional
/// `{"server": key}` names, tested with a fresh session. A server that
/// fails is `ok: false` with a sentence, in a 200.
pub async fn test_answer(app: &AppState, body: &[u8]) -> Result<Value, Refusal> {
    // The web reads the body silently: none, or one that is not JSON, tests
    // every server.
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let server = match &parsed {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => m.get("server").cloned(),
        Some(_) => {
            return Err(Refusal::bad_request(
                "The request body must be a JSON object",
            ))
        }
    };
    let named = match server {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if app.agent.mcp().server_by_key(&s).is_some() => Some(s),
        Some(_) => return Err(Refusal::bad_request("Unknown external data server")),
    };
    let results = app.agent.mcp().check_servers(named.map(|s| vec![s])).await;
    Ok(json!({"status": "success", "results": results}))
}
