//! `/agent/api/conversations`: list, open, fetch, delete, and truncate from
//! a message (the web's conversation routes).

use super::{answer, path_id, AgentBody, ApiCaller, Caller};
use crate::agent::conversations as service;
use crate::state::AppState;
use axum::{
    extract::{OriginalUri, Path, Query, State},
    http::StatusCode,
    response::Response,
};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// `GET`: the trader's conversations, most recently updated first.
pub async fn list(
    State(ctx): Ctx,
    Caller(c): ApiCaller,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let r = service::list(
        &ctx,
        &c.user,
        q.get("surface").map(String::as_str),
        q.get("limit").map(String::as_str),
    )
    .await;
    answer(r, StatusCode::OK)
}

/// `POST`: open a conversation on one surface (201).
pub async fn create(State(ctx): Ctx, Caller(c): ApiCaller, AgentBody(body): AgentBody) -> Response {
    answer(
        service::create(&ctx, &c.user, &body).await,
        StatusCode::CREATED,
    )
}

/// `GET {id}`: one conversation and its messages.
pub async fn get_one(
    State(ctx): Ctx,
    Caller(c): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => answer(service::get(&ctx, &c.user, id).await, StatusCode::OK),
        Err(r) => *r,
    }
}

/// `DELETE {id}`.
pub async fn delete_one(
    State(ctx): Ctx,
    Caller(c): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path(id): Path<String>,
) -> Response {
    match path_id(&id, uri.path()) {
        Ok(id) => answer(service::delete(&ctx, &c.user, id).await, StatusCode::OK),
        Err(r) => *r,
    }
}

/// `DELETE {id}/messages/{message_id}`: the message and everything after.
pub async fn truncate(
    State(ctx): Ctx,
    Caller(c): ApiCaller,
    OriginalUri(uri): OriginalUri,
    Path((id, message_id)): Path<(String, String)>,
) -> Response {
    match (path_id(&id, uri.path()), path_id(&message_id, uri.path())) {
        (Ok(id), Ok(message_id)) => answer(
            service::truncate(&ctx, &c.user, id, message_id).await,
            StatusCode::OK,
        ),
        (Err(r), _) | (_, Err(r)) => *r,
    }
}
