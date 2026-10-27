//! The web's `blueprints/agent.py`, split by area: the chat routes
//! ([`chat`]) and the conversation routes ([`conversations`]) here, and
//! the audit trail's read route ([`audit`], D-006) and what a paused smart
//! order will send ([`orders`], for the approval card), both desktop
//! additions; the config routes belong to the config part. Every route needs the signed-in
//! user and carries the session's CSRF token on writes, like every page
//! route; none is public.
//!
//! The handlers read the request, call one service in `crate::agent` and
//! shape the reply. Limits as the web: 240 requests a minute across the
//! ordinary routes, 30 a minute on the streaming ones (per caller), and a
//! 12 MB body.

pub mod audit;
pub mod chat;
pub mod conversations;
pub mod orders;

use super::{Access, RouteSpec};
use crate::agent::reply::Refusal;
use crate::server::envelope::{json_response, EarlyReply};
use crate::server::middleware::{source_of, AuthedUser};
use crate::server::ratelimit::{Admission, Bucket};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Request},
    http::{header, request::Parts, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{delete, get, post, MethodRouter},
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// The largest body a route here reads (the web's `MAX_REQUEST_BYTES`):
/// attachments travel base64 inside the JSON.
pub const MAX_REQUEST_BYTES: usize = 12_000_000;

/// The Agent's chat and conversation routes.
pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    vec![
        spec("/agent/api/conversations", Method::GET, || {
            get(conversations::list)
        }),
        spec("/agent/api/conversations", Method::POST, || {
            post(conversations::create).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/conversations/{id}", Method::GET, || {
            get(conversations::get_one)
        }),
        spec("/agent/api/conversations/{id}", Method::DELETE, || {
            delete(conversations::delete_one)
        }),
        spec(
            "/agent/api/conversations/{id}/messages/{message_id}",
            Method::DELETE,
            || delete(conversations::truncate),
        ),
        spec("/agent/api/chat/stream", Method::POST, || {
            post(chat::stream).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/chat/confirm", Method::POST, || {
            post(chat::confirm).layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        }),
        spec("/agent/api/chat/{run_id}/cancel", Method::POST, || {
            post(chat::cancel)
        }),
        spec("/agent/api/audit", Method::GET, || get(audit::list)),
        spec("/agent/api/orders/preview", Method::GET, || {
            get(orders::preview)
        }),
    ]
}

/// A refusal as the web's JSON error.
pub(crate) fn refused(r: Refusal) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

/// The web's 429 for these routes: JSON, never the app-wide redirect.
fn rate_limited(retry_after: u64) -> Response {
    let mut r = json_response(
        StatusCode::TOO_MANY_REQUESTS,
        json!({
            "status": "error",
            "message": "Rate limit exceeded. Please slow down your requests.",
            "retry_after": retry_after,
        }),
    );
    if let Ok(v) = HeaderValue::from_str(&retry_after.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// Which of the Agent's rate limits a route counts against.
pub const API: u8 = 0;
/// The streaming routes' limit.
pub const STREAM: u8 = 1;
/// The routes that reach a provider to test it.
pub const TEST: u8 = 2;

/// The signed-in caller of a route, counted against one of the Agent's
/// rate limits first (`LIMIT`: [`API`], [`STREAM`] or [`TEST`]). Match it as
/// `Caller(c): ApiCaller`.
pub struct Caller<const LIMIT: u8>(pub crate::agent::chat::Caller);

/// An ordinary route's caller (240 a minute, shared by all of them).
pub type ApiCaller = Caller<API>;
/// A streaming route's caller (30 a minute).
pub type StreamCaller = Caller<STREAM>;
/// A provider-test route's caller (12 a minute).
pub type TestCaller = Caller<TEST>;

impl<const LIMIT: u8> FromRequestParts<Arc<AppState>> for Caller<LIMIT> {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        ctx: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let Some(user) = parts.extensions.get::<AuthedUser>().cloned() else {
            return Err(refused(Refusal::new(401, "Not authenticated", None)));
        };
        let bucket = match LIMIT {
            STREAM => Bucket::AgentStream,
            TEST => Bucket::AgentTest,
            _ => Bucket::AgentApi,
        };
        let ip = source_of(&parts.extensions).ip();
        match ctx.limiter.admit(bucket, ip, ctx.limiter.now()) {
            Admission::Allowed => {}
            Admission::Over(wait) | Admission::OverflowOver(wait) => {
                return Err(rate_limited(wait.as_secs().max(1)))
            }
        }
        Ok(Caller(crate::agent::chat::Caller {
            user: user.username,
            session: user.session_id,
        }))
    }
}

/// A JSON object body of at most [`MAX_REQUEST_BYTES`], refused as the web
/// refuses it (the declared length is checked before anything is read).
pub struct AgentBody(pub Map<String, Value>);

fn too_large() -> Response {
    refused(Refusal::new(
        413,
        format!(
            "The request body may be at most {} MB",
            MAX_REQUEST_BYTES / 1_000_000
        ),
        None,
    ))
}

impl FromRequest<Arc<AppState>> for AgentBody {
    type Rejection = Response;

    async fn from_request(req: Request, ctx: &Arc<AppState>) -> Result<Self, Self::Rejection> {
        let declared = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        if declared.is_some_and(|n| n > MAX_REQUEST_BYTES) {
            return Err(too_large());
        }
        let bytes = Bytes::from_request(req, ctx)
            .await
            .map_err(|_| too_large())?;
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(m)) => Ok(AgentBody(m)),
            Ok(_) => Err(refused(Refusal::bad_request(
                "The request body must be a JSON object",
            ))),
            Err(_) => Err(refused(Refusal::bad_request("A JSON body is required"))),
        }
    }
}

/// An id in a path, or the app's JSON 404 (the web's `<int:...>` matches
/// nothing else).
pub(crate) fn path_id(raw: &str, path: &str) -> Result<i64, EarlyReply> {
    raw.parse::<i64>()
        .map_err(|_| Box::new(crate::server::envelope::not_found(path)))
}

/// A service answer as the route's reply.
pub(crate) fn answer(r: Result<Value, Refusal>, ok: StatusCode) -> Response {
    match r {
        Ok(v) => json_response(ok, v),
        Err(e) => refused(e),
    }
}
