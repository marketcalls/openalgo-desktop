//! Web `blueprints/chartink.py`: the Chartink pages' JSON routes (signed-in
//! user, CSRF on writes) and the public webhook, whose URL id is the
//! credential. The handlers parse and hand off to `crate::chartink`.

use super::{Access, RouteSpec};
use crate::chartink::{service as svc, webhook};
use crate::server::envelope::{json_response, EarlyReply};
use crate::server::middleware::{
    count_failure, failures_exhausted, limiter_key, ClientIp, Source, User,
};
use crate::server::ratelimit::Bucket;
use crate::server::routes::webui::int_id;
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
    crate::server::middleware::Src(source): crate::server::middleware::Src,
    crate::server::middleware::Deferred(deferred): crate::server::middleware::Deferred,
    Path(webhook_id): Path<String>,
    req: Request,
) -> Response {
    let known = match admit_webhook_call(&ctx, ip, source, deferred, &webhook_id) {
        Ok(known) => known,
        Err(r) => return *r,
    };
    if known && !ctx.security.is_unlocked() {
        return crate::server::envelope::webhook_while_locked("error");
    }
    let bytes = match read_payload(req).await {
        Ok(b) => b,
        Err(r) => return *r,
    };
    let (status, body, failed_auth) = svc::webhook_alert(&ctx, &webhook_id, &bytes).await;
    if failed_auth && known {
        // The id went away between the check and the alert: a failure.
        count_failure(&ctx, ip, Bucket::WebhookFail);
    }
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        body,
    )
}

/// The webhook's admission, before any body read: whether the id is
/// known, or the rate-limited answer.
fn admit_webhook_call(
    ctx: &AppState,
    ip: std::net::IpAddr,
    source: Source,
    deferred: bool,
    webhook_id: &str,
) -> Result<bool, EarlyReply> {
    let limited = || {
        json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({
                "status": "error",
                "error": "Rate limit exceeded. Please slow down your requests.",
            }),
        )
    };
    // This computer and devices on the network: the web's per-address
    // limit, before any lookup. Tunnel callers share one identity, so they
    // are limited per webhook once the id checked out (S-03).
    let tunnel = source == Source::Tunnel;
    let mut deferred = deferred;
    if !tunnel {
        match webhook::admit_address(&ctx.chartink.guard, &ctx.limiter, ip) {
            None => return Err(Box::new(limited())),
            Some(d) => deferred |= d,
        }
    }
    // The id first: a locator lookup and a constant-time match. A valid id
    // is limited only by its own windows. An unknown one counts against the
    // caller's failure budget; once that is spent, the caller's unknown ids
    // are refused at once (no body read, no probe recorded), while valid
    // ids still pass.
    let known = svc::webhook_known(ctx, webhook_id);
    if known {
        let tunnel_key = tunnel.then(|| limiter_key(ip, "chartink-webhook", webhook_id));
        if !webhook::admit_webhook(&ctx.chartink.guard, &ctx.limiter, tunnel_key, webhook_id) {
            return Err(Box::new(limited()));
        }
    } else {
        if deferred || failures_exhausted(ctx, ip, Bucket::WebhookFail) {
            ctx.chartink
                .guard
                .note_throttled("too many failed webhook attempts from one caller");
            return Err(Box::new(limited()));
        }
        count_failure(ctx, ip, Bucket::WebhookFail);
    }
    Ok(known)
}

/// The alert body, at most `MAX_PAYLOAD_BYTES` (a larger declared or sent
/// body is refused).
async fn read_payload(req: Request) -> Result<Bytes, EarlyReply> {
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
        return Err(Box::new(too_large()));
    }
    let body: Body = req.into_body();
    to_bytes(body, webhook::MAX_PAYLOAD_BYTES)
        .await
        .map_err(|_| Box::new(too_large()))
}
