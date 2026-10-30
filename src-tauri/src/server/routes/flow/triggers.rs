//! The trigger side's routes of `blueprints/flow.py`: activate, deactivate,
//! the webhook settings, the monitors' status (each for the signed-in user,
//! writes with the session's CSRF token), and the two public webhooks
//! `POST /flow/webhook/{token}` and `POST /flow/webhook/{token}/{symbol}`,
//! public with their own credential: the token, plus the secret its auth
//! type asks for. Only the webhooks are rate limited
//! (R-09): by caller address before any lookup, and by token once it is
//! known, both at the web's 100 a minute and both spellings on one budget;
//! an over-limit caller gets the web's JSON 429 with `Retry-After`.

use super::{send, spec};
use crate::flow::service::{activation, webhooks, Body};
use crate::server::envelope::{json_response, not_found, EarlyReply};
use crate::server::middleware::{
    count_failure, failures_exhausted, limiter_key, ClientIp, Deferred, Src,
};
use crate::server::ratelimit::{Admission, Bucket};
use crate::server::routes::webui::int_id;
use crate::server::routes::{Access, RouteSpec};
use crate::server::source::{Source, PROXIED_CALLER};
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{OriginalUri, Path, Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{get, post},
};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The session routes, and the two public webhooks (listed in the reviewed
/// public-route list in `server/tests.rs`).
pub fn table() -> Vec<RouteSpec> {
    vec![
        spec("/flow/api/workflows/{id}/activate", Method::POST, || {
            post(activate)
        }),
        spec("/flow/api/workflows/{id}/deactivate", Method::POST, || {
            post(deactivate)
        }),
        spec("/flow/api/workflows/{id}/webhook", Method::GET, || {
            get(webhook_info)
        }),
        spec(
            "/flow/api/workflows/{id}/webhook/enable",
            Method::POST,
            || post(webhook_enable),
        ),
        spec(
            "/flow/api/workflows/{id}/webhook/disable",
            Method::POST,
            || post(webhook_disable),
        ),
        spec(
            "/flow/api/workflows/{id}/webhook/regenerate",
            Method::POST,
            || post(webhook_regenerate),
        ),
        spec(
            "/flow/api/workflows/{id}/webhook/regenerate-secret",
            Method::POST,
            || post(webhook_regenerate_secret),
        ),
        spec(
            "/flow/api/workflows/{id}/webhook/auth-type",
            Method::POST,
            || post(webhook_auth_type),
        ),
        spec("/flow/api/monitor/status", Method::GET, || {
            get(monitor_status)
        }),
        // Public: the URL token is the credential, plus the secret the
        // workflow's auth type asks for (the JSON `secret` or `?secret=`).
        RouteSpec {
            path: "/flow/webhook/{token}",
            method: Method::POST,
            access: Access::Public,
            make: || post(webhook),
        },
        RouteSpec {
            path: "/flow/webhook/{token}/{symbol}",
            method: Method::POST,
            access: Access::Public,
            make: || post(webhook_with_symbol),
        },
    ]
}

/// A session route on one workflow: the JSON 404 for an id that is not a
/// whole number, as the web's `<int:workflow_id>`.
macro_rules! by_id {
    ($name:ident, $svc:path) => {
        pub async fn $name(State(ctx): Ctx, Path(id): Path<String>, uri: OriginalUri) -> Response {
            match int_id(&id) {
                Some(id) => send($svc(&ctx, id).await),
                None => not_found(uri.path()),
            }
        }
    };
}

by_id!(activate, activation::activate);
by_id!(deactivate, activation::deactivate);
by_id!(webhook_info, webhooks::info);
by_id!(webhook_enable, webhooks::enable);
by_id!(webhook_disable, webhooks::disable);
by_id!(webhook_regenerate, webhooks::regenerate);
by_id!(webhook_regenerate_secret, webhooks::regenerate_secret);

pub async fn webhook_auth_type(
    State(ctx): Ctx,
    Path(id): Path<String>,
    uri: OriginalUri,
    body: Bytes,
) -> Response {
    match int_id(&id) {
        Some(id) => send(webhooks::set_auth_type(&ctx, id, Body::parse(&body)).await),
        None => not_found(uri.path()),
    }
}

pub async fn monitor_status(State(ctx): Ctx) -> Response {
    send(activation::monitor_status(&ctx))
}

// ------------------------------------------------------------------ public

/// The web's blueprint 429: JSON, never the app's redirect, with
/// `Retry-After` (R-11).
fn rate_limited() -> Response {
    let (_, window) = Bucket::FlowWebhook.limit();
    let secs = window.as_secs();
    let mut r = json_response(
        StatusCode::TOO_MANY_REQUESTS,
        json!({
            "status": "error",
            "message": "Rate limit exceeded. Please slow down your requests.",
            "limit": Bucket::FlowWebhook.describe(),
            "retry_after": secs,
        }),
    );
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// The webhook's admission before the body is read: whether the token is
/// known, or the 429. This computer and devices on the network are limited
/// by address first; every caller of a known token by the token (tunnel
/// callers share one address, so the token is their meaningful limit). An
/// unknown token counts against the caller's failure budget, and once that
/// is spent its unknown tokens are refused at once.
async fn admit(
    ctx: &Arc<AppState>,
    ip: std::net::IpAddr,
    source: Source,
    deferred: bool,
    token: &str,
) -> Result<bool, EarlyReply> {
    let mut deferred = deferred;
    if source != Source::Tunnel {
        match ctx
            .limiter
            .admit(Bucket::FlowWebhook, ip, ctx.limiter.now())
        {
            Admission::Allowed => {}
            Admission::OverflowOver(_) => deferred = true,
            Admission::Over(_) => return Err(Box::new(rate_limited())),
        }
    }
    let known = webhooks::token_known(ctx, token).await;
    if known {
        let key = limiter_key(PROXIED_CALLER, "flow-webhook", token);
        if ctx
            .limiter
            .check(Bucket::FlowWebhook, key, ctx.limiter.now())
            .is_err()
        {
            return Err(Box::new(rate_limited()));
        }
    } else {
        if deferred || failures_exhausted(ctx, ip, Bucket::WebhookFail) {
            return Err(Box::new(rate_limited()));
        }
        count_failure(ctx, ip, Bucket::WebhookFail);
    }
    Ok(known)
}

/// The largest webhook body read (the server's default request limit).
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// What both public routes share.
async fn run_webhook(
    ctx: Arc<AppState>,
    (ip, source, deferred): (std::net::IpAddr, Source, bool),
    token: String,
    symbol: Option<String>,
    req: Request,
) -> Response {
    let known = match admit(&ctx, ip, source, deferred, &token).await {
        Ok(k) => k,
        Err(limited) => return *limited,
    };
    if known && !ctx.security.is_unlocked() {
        return crate::server::envelope::webhook_while_locked("error");
    }
    let form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/x-www-form-urlencoded"));
    let url_secret = req.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "secret")
            .map(|(_, v)| v.into_owned())
    });
    let Ok(body) = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await else {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"error": "The alert is too large."}),
        );
    };
    let mut payload = webhooks::read_payload(&body, form);
    if let Some(s) = symbol {
        payload.insert("symbol".into(), json!(s));
    }
    let call = webhooks::WebhookCall {
        token,
        payload,
        url_secret,
    };
    let answer = webhooks::trigger(&ctx, call).await;
    if answer.wrong_secret {
        count_failure(&ctx, ip, Bucket::WebhookFail);
    }
    send(answer.reply)
}

/// `POST /flow/webhook/{token}`. Public: the token and the secret.
pub async fn webhook(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    Src(source): Src,
    Deferred(deferred): Deferred,
    Path(token): Path<String>,
    req: Request,
) -> Response {
    run_webhook(ctx, (ip, source, deferred), token, None, req).await
}

/// `POST /flow/webhook/{token}/{symbol}`: the path's symbol replaces any in
/// the body (R-49).
pub async fn webhook_with_symbol(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    Src(source): Src,
    Deferred(deferred): Deferred,
    Path((token, symbol)): Path<(String, String)>,
    req: Request,
) -> Response {
    run_webhook(ctx, (ip, source, deferred), token, Some(symbol), req).await
}
