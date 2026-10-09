//! The Agent's API (web `blueprints/agent.py`, `/agent/api/*`).
//!
//! The Agent is deferred in the desktop (maintainer, 2026-10-07: it needs
//! a provider layer of its own, and LiteLLM has no Rust support). Its pages
//! stay in the frontend, so every call they make is answered here, signed
//! in, with the web's error envelope and a sentence a trader can read,
//! instead of falling through to the page's 404. The status is 503, the
//! web's answer when its server cannot run the agent module (`_error(..,
//! 503)` without the `agno` package).

use super::{Access, RouteSpec};
use crate::server::envelope::json_response;
use crate::state::AppState;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put, MethodRouter};
use serde_json::json;
use std::sync::Arc;

/// What every Agent call is told.
pub const UNAVAILABLE_MESSAGE: &str = "The AI agent is not available in OpenAlgo Desktop yet. Everything else works as usual; the agent will come in a later version.";

/// Every `/agent/api` call: the web's `{status, message}` error with 503.
pub async fn unavailable() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"status": "error", "message": UNAVAILABLE_MESSAGE}),
    )
}

/// Builds the method router of one route.
type MakeRoute = fn() -> MethodRouter<Arc<AppState>>;

pub fn table() -> Vec<RouteSpec> {
    let routes: [(Method, MakeRoute); 5] = [
        (Method::GET, || get(unavailable)),
        (Method::POST, || post(unavailable)),
        (Method::PUT, || put(unavailable)),
        (Method::PATCH, || patch(unavailable)),
        (Method::DELETE, || delete(unavailable)),
    ];
    ["/agent/api", "/agent/api/{*rest}"]
        .into_iter()
        .flat_map(|path| {
            routes.iter().map(move |(method, make)| RouteSpec {
                path,
                method: method.clone(),
                access: Access::User,
                make: *make,
            })
        })
        .collect()
}
