//! The web's `blueprints/flow.py`, split by area. This file has the
//! workflow routes (list, create, get, update, delete, run, history,
//! export, import, replace); the trigger side's routes (activate,
//! deactivate, webhook settings, the public webhooks, monitor status) and
//! the data lookups (index symbols, lot sizes, option strikes) join the
//! table from their own files. Every route here needs the signed-in user
//! and carries the session's CSRF token on writes; none is public.
//!
//! The handlers read the request and call one function in
//! `crate::flow::service`, which answers the web's codes and shapes. A path
//! id that is not a whole number answers the JSON 404, as the web's
//! `<int:workflow_id>` converter does.

use super::{Access, RouteSpec};
use crate::flow::service::{self as svc, transfer, Body};
use crate::server::envelope::{json_response, not_found};
use crate::server::routes::webui::int_id;
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, OriginalUri, Path, Query, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{delete, get, post, put, MethodRouter},
};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The largest workflow body read: well above a 500-node, 1,000-edge graph
/// (the validator's caps; parity IE-08).
pub const MAX_WORKFLOW_BYTES: usize = 8 * 1024 * 1024;

fn spec(
    path: &'static str,
    method: Method,
    make: fn() -> MethodRouter<Arc<AppState>>,
) -> RouteSpec {
    RouteSpec {
        path,
        method,
        access: Access::User,
        make,
    }
}

/// The workflow routes.
pub fn table() -> Vec<RouteSpec> {
    vec![
        spec("/flow/api/workflows", Method::GET, || get(list)),
        spec("/flow/api/workflows", Method::POST, || {
            post(create).layer(DefaultBodyLimit::max(MAX_WORKFLOW_BYTES))
        }),
        spec("/flow/api/workflows/import", Method::POST, || {
            post(import).layer(DefaultBodyLimit::max(MAX_WORKFLOW_BYTES))
        }),
        spec("/flow/api/workflows/{id}", Method::GET, || get(one)),
        spec("/flow/api/workflows/{id}", Method::PUT, || {
            put(update).layer(DefaultBodyLimit::max(MAX_WORKFLOW_BYTES))
        }),
        spec("/flow/api/workflows/{id}", Method::DELETE, || {
            delete(remove)
        }),
        spec("/flow/api/workflows/{id}/execute", Method::POST, || {
            post(execute)
        }),
        spec("/flow/api/workflows/{id}/executions", Method::GET, || {
            get(executions)
        }),
        spec("/flow/api/workflows/{id}/export", Method::GET, || {
            get(export)
        }),
        spec("/flow/api/workflows/{id}/replace", Method::POST, || {
            post(replace).layer(DefaultBodyLimit::max(MAX_WORKFLOW_BYTES))
        }),
    ]
}

fn send(r: Reply) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

pub async fn list(State(ctx): Ctx) -> Response {
    send(svc::list(&ctx).await)
}

pub async fn create(State(ctx): Ctx, body: Bytes) -> Response {
    send(svc::create(&ctx, Body::parse(&body)).await)
}

pub async fn one(State(ctx): Ctx, Path(id): Path<String>, uri: OriginalUri) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::get(&ctx, id).await),
        None => not_found(uri.path()),
    }
}

pub async fn update(
    State(ctx): Ctx,
    Path(id): Path<String>,
    uri: OriginalUri,
    body: Bytes,
) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::update(&ctx, id, Body::parse(&body)).await),
        None => not_found(uri.path()),
    }
}

pub async fn remove(State(ctx): Ctx, Path(id): Path<String>, uri: OriginalUri) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::delete(&ctx, id).await),
        None => not_found(uri.path()),
    }
}

pub async fn execute(State(ctx): Ctx, Path(id): Path<String>, uri: OriginalUri) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::run_now(&ctx, id).await),
        None => not_found(uri.path()),
    }
}

pub async fn executions(
    State(ctx): Ctx,
    Path(id): Path<String>,
    uri: OriginalUri,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::history(&ctx, id, q.get("limit").map(String::as_str)).await),
        None => not_found(uri.path()),
    }
}

pub async fn export(State(ctx): Ctx, Path(id): Path<String>, uri: OriginalUri) -> Response {
    match int_id(&id) {
        Some(id) => send(transfer::export(&ctx, id).await),
        None => not_found(uri.path()),
    }
}

pub async fn import(State(ctx): Ctx, body: Bytes) -> Response {
    send(transfer::import(&ctx, Body::parse(&body)).await)
}

pub async fn replace(
    State(ctx): Ctx,
    Path(id): Path<String>,
    uri: OriginalUri,
    body: Bytes,
) -> Response {
    match int_id(&id) {
        Some(id) => send(transfer::replace(&ctx, id, Body::parse(&body)).await),
        None => not_found(uri.path()),
    }
}
