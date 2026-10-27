//! The ChatGPT plan's routes (the ChatGPT part of the web's
//! `blueprints/agent.py`): its status, the device-flow sign-in, cancelling
//! it and signing the plan out. The sign-in reaches OpenAI, so it counts
//! against the 12-a-minute test limit; the rest share the 240.

use super::{answer, refused, ApiCaller, Caller, TestCaller, MAX_REQUEST_BYTES};
use crate::agent::chatgpt::service;
use crate::server::routes::{Access, RouteSpec};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{delete, get, post, MethodRouter},
};
use serde_json::Value;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// An optional JSON body, read as the web's `get_json(silent=True)`: none,
/// or one that is not JSON, is absent.
pub(super) fn optional_json(bytes: &Bytes) -> Option<Value> {
    if bytes.is_empty() {
        return None;
    }
    serde_json::from_slice(bytes).ok()
}

/// `GET /agent/api/chatgpt/status`.
pub async fn status(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    answer(service::status(&ctx).await, StatusCode::OK)
}

/// `POST /agent/api/chatgpt/login` with an optional `{"force": bool}`.
pub async fn login(State(ctx): Ctx, Caller(_): TestCaller, body: Bytes) -> Response {
    match service::force_of(optional_json(&body).as_ref()) {
        Ok(force) => answer(service::login(&ctx, force).await, StatusCode::OK),
        Err(r) => refused(r),
    }
}

/// `POST /agent/api/chatgpt/cancel`.
pub async fn cancel(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    answer(service::cancel(&ctx).await, StatusCode::OK)
}

/// `DELETE /agent/api/chatgpt/session`.
pub async fn sign_out(State(ctx): Ctx, Caller(_): ApiCaller) -> Response {
    answer(service::sign_out(&ctx).await, StatusCode::OK)
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
        spec("/agent/api/chatgpt/status", Method::GET, || get(status)),
        spec("/agent/api/chatgpt/login", Method::POST, || {
            post(login).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/chatgpt/cancel", Method::POST, || post(cancel)),
        spec("/agent/api/chatgpt/session", Method::DELETE, || {
            delete(sign_out)
        }),
    ]
}
