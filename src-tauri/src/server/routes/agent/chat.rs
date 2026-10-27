//! `/agent/api/chat`: stream a turn, resume a paused one, stop one (the
//! web's chat routes). A turn's frames stream as SSE over the POST.

use super::{refused, AgentBody, ApiCaller, Caller, StreamCaller};
use crate::agent::chat as service;
use crate::agent::contract::frames::SSE_HEADERS;
use crate::agent::stream::sse_body;
use crate::server::envelope::json_response;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn streamed(r: Result<service::Streaming, crate::agent::reply::Refusal>) -> Response {
    let s = match r {
        Ok(s) => s,
        Err(e) => return refused(e),
    };
    let mut resp = Response::new(sse_body(s.frames, s.slot));
    for (k, v) in SSE_HEADERS {
        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            resp.headers_mut().insert(name, HeaderValue::from_static(v));
        }
    }
    resp
}

/// `POST /agent/api/chat/stream`: one turn, as SSE.
pub async fn stream(
    State(ctx): Ctx,
    Caller(c): StreamCaller,
    AgentBody(body): AgentBody,
) -> Response {
    streamed(service::start(&ctx, &c, body).await)
}

/// `POST /agent/api/chat/confirm`: resume a paused run, as SSE.
pub async fn confirm(
    State(ctx): Ctx,
    Caller(c): StreamCaller,
    AgentBody(body): AgentBody,
) -> Response {
    streamed(service::resume(&ctx, &c, body).await)
}

/// `POST /agent/api/chat/{run_id}/cancel`.
pub async fn cancel(State(ctx): Ctx, Caller(c): ApiCaller, Path(run_id): Path<String>) -> Response {
    match service::cancel(&ctx, &c, &run_id) {
        Ok(v) => json_response(StatusCode::OK, v),
        Err(e) => refused(e),
    }
}
