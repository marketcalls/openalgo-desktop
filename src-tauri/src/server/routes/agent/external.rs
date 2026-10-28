//! The external data (MCP) part of the web's `blueprints/agent.py`: the
//! registered servers and the switch, and a real connection test. The
//! switch is written through `PUT /agent/api/settings`.

use super::{answer, ApiCaller, Caller, TestCaller};
use crate::agent::mcp::answers;
use crate::agent::reply::Refusal;
use crate::server::routes::{Access, RouteSpec};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::State,
    http::{Method, StatusCode},
    response::Response,
    routing::{get, post, MethodRouter},
};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// `GET /agent/api/mcp`: the servers and whether the switch is on.
pub async fn servers(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    let r = ctx
        .blocking(|c| Ok(answers::servers_answer(c)))
        .await
        .map_err(|e| {
            tracing::error!("Could not read the agent MCP setting: {}", e);
            Refusal::new(500, "Could not read the external data setting", None)
        });
    answer(r, StatusCode::OK)
}

/// `POST /agent/api/mcp/test` with an optional `{"server": key}` (12 a
/// minute, the web's `agent_test` limit).
pub async fn test(State(ctx): Ctx, Caller(_): TestCaller, body: Bytes) -> Response {
    answer(answers::test_answer(&ctx, &body).await, StatusCode::OK)
}

pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    vec![
        spec("/agent/api/mcp", Method::GET, || get(servers)),
        spec("/agent/api/mcp/test", Method::POST, || post(test)),
    ]
}
