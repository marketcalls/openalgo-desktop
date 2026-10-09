//! Web `blueprints/chartink.py`: the Chartink pages' JSON routes (signed-in
//! user, CSRF on writes) and the public webhook, whose URL id is the
//! credential. The handlers parse and hand off to `crate::chartink`.

use super::{Access, RouteSpec};
use crate::chartink::{service as svc, webhook};
use crate::server::envelope::json_response;
use crate::server::middleware::{ClientIp, User};
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{FromRequest, Path, Query, Request, State},
    http::{header, Method, StatusCode},
    response::Response,
    routing::{get, post},
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

macro_rules! c {
    ($m:ident, $acc:ident, $path:expr, $f:path) => {
        RouteSpec {
            path: $path,
            method: Method::$m,
            access: Access::$acc,
            make: || c!(@route $m, $f),
        }
    };
    (@route GET, $f:path) => { get($f) };
    (@route POST, $f:path) => { post($f) };
}

/// The Chartink routes, each with its access rule.
pub fn table() -> Vec<RouteSpec> {
    vec![
        c!(GET, User, "/chartink/api/strategies", strategies),
        c!(GET, User, "/chartink/api/strategy/{id}", strategy),
        c!(POST, User, "/chartink/api/strategy", create),
        c!(POST, User, "/chartink/api/strategy/{id}/toggle", toggle),
        c!(POST, User, "/chartink/{id}/delete", delete),
        c!(POST, User, "/chartink/{id}/configure", configure),
        c!(
            POST,
            User,
            "/chartink/{id}/symbol/{mapping_id}/delete",
            delete_symbol
        ),
        c!(GET, User, "/chartink/search", search),
        // Public: the webhook id in the URL is the credential.
        c!(
            POST,
            Public,
            "/chartink/webhook/{webhook_id}",
            webhook_route
        ),
    ]
}

fn send(r: Reply) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

/// `<int:id>`: anything else is the web's 404.
fn int_id(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

fn not_found() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({"status": "error", "message": "Not found"}),
    )
}

pub async fn strategies(State(ctx): Ctx, User(u): User) -> Response {
    send(svc::list(&ctx, &u.username))
}

pub async fn strategy(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::get(&ctx, &u.username, id)),
        None => not_found(),
    }
}

pub async fn create(State(ctx): Ctx, User(u): User, body: Bytes) -> Response {
    let parsed = serde_json::from_slice::<Value>(&body).ok();
    let map = parsed.as_ref().and_then(Value::as_object);
    send(svc::create(&ctx, &u.username, map))
}

pub async fn toggle(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::toggle(&ctx, &u.username, id)),
        None => not_found(),
    }
}

pub async fn delete(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    match int_id(&id) {
        Some(id) => send(svc::delete(&ctx, &u.username, id)),
        None => not_found(),
    }
}

/// JSON, or a form post (`symbol=...&exchange=...` or `symbols=...`).
fn body_fields(content_type: &str, body: &[u8]) -> Map<String, Value> {
    if !content_type.contains("application/x-www-form-urlencoded") {
        if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(body) {
            return m;
        }
    }
    serde_urlencoded::from_bytes::<Vec<(String, String)>>(body)
        .map(|pairs| {
            pairs
                .into_iter()
                .filter(|(k, _)| k != "csrf_token")
                .map(|(k, v)| (k, Value::String(v)))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn configure(
    State(ctx): Ctx,
    User(u): User,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found();
    };
    let ct = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = match Bytes::from_request(req, &()).await {
        Ok(b) => b,
        Err(_) => {
            return json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"status": "error", "error": "The request is too large."}),
            )
        }
    };
    send(svc::configure(
        &ctx,
        &u.username,
        id,
        &body_fields(&ct, &body),
    ))
}

pub async fn delete_symbol(
    State(ctx): Ctx,
    User(u): User,
    Path((id, mapping_id)): Path<(String, String)>,
) -> Response {
    match (int_id(&id), int_id(&mapping_id)) {
        (Some(id), Some(mid)) => send(svc::delete_symbol(&ctx, &u.username, id, mid)),
        _ => not_found(),
    }
}

pub async fn search(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    send(svc::search(
        &ctx,
        q.get("q").map(String::as_str).unwrap_or(""),
        q.get("exchange").map(String::as_str),
    ))
}

/// `POST /chartink/webhook/{webhook_id}`. Public. Rate limits and the size
/// cap come before any lookup or body read; nothing secret is logged.
pub async fn webhook_route(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    Path(webhook_id): Path<String>,
    req: Request,
) -> Response {
    use crate::server::ratelimit::Bucket;
    // Tunnel callers are counted per webhook id, never all together (S-03).
    let ip = crate::server::middleware::limiter_key(ip, "chartink-webhook", &webhook_id);
    if !webhook::admit(&ctx.chartink.guard, &ctx.limiter, ip, &webhook_id) {
        return json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({
                "status": "error",
                "error": "Rate limit exceeded. Please slow down your requests.",
            }),
        );
    }
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let too_large = || {
        json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({
                "status": "error",
                "error": format!("Payload larger than {} bytes", webhook::MAX_PAYLOAD_BYTES),
            }),
        )
    };
    if declared.is_some_and(|d| d > webhook::MAX_PAYLOAD_BYTES) {
        return too_large();
    }
    let body: Body = req.into_body();
    let Ok(bytes) = to_bytes(body, webhook::MAX_PAYLOAD_BYTES).await else {
        return too_large();
    };
    let (status, body, failed_auth) = svc::webhook_alert(&ctx, &webhook_id, &bytes).await;
    if failed_auth {
        // Counted per caller address; over the limit the address is refused
        // before any lookup.
        let _ = ctx
            .limiter
            .check(Bucket::WebhookFail, ip, ctx.limiter.now());
    }
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        body,
    )
}
