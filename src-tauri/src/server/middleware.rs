//! Request middleware: Host check, browser session + CSRF, the signed-in
//! user guard, and the `/api/v1` rate limit.

use crate::server::envelope::{error, json_response, not_authenticated};
use crate::server::form::{csrf_rejected, tokens_match, FormData};
use crate::server::ratelimit::Bucket;
use crate::session::web::{WebSession, COOKIE_NAME};
use crate::state::AppState;
use axum::{
    body::Body,
    extract::{ConnectInfo, FromRequest, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

/// The browser session for this request, if the cookie named one.
#[derive(Clone, Debug, Default)]
pub struct SessionCtx(pub Option<WebSession>);

/// Inserted by the user guard for handlers behind it.
#[derive(Clone, Debug)]
pub struct AuthedUser {
    pub username: String,
    pub session_id: String,
}

pub fn client_ip(req: &Request) -> IpAddr {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

/// Client address (loopback when the server runs in-process in tests).
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip())
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ))
    }
}

/// Session for this request (always present behind `session_layer`).
pub struct Sess(pub Option<WebSession>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Sess {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(Sess(
            parts
                .extensions
                .get::<SessionCtx>()
                .and_then(|s| s.0.clone()),
        ))
    }
}

/// The signed-in user (only on routes behind `require_user`).
pub struct User(pub AuthedUser);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for User {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthedUser>()
            .cloned()
            .map(User)
            .ok_or_else(not_authenticated)
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| {
            let mut it = kv.trim().splitn(2, '=');
            match (it.next(), it.next()) {
                (Some(k), Some(v)) if k == name => Some(v.to_string()),
                _ => None,
            }
        })
        .next()
}

pub fn session_cookie(id: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={}; HttpOnly; SameSite=Lax; Path=/",
        COOKIE_NAME, id
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("session=; Path=/"))
}

pub fn clear_session_cookie() -> HeaderValue {
    HeaderValue::from_static("session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

// ------------------------------------------------------------------ Host check

/// Hosts the server answers to. Blocks DNS rebinding: a page on
/// `evil.example` resolving to 127.0.0.1 sends `Host: evil.example`.
pub fn host_allowed(ctx: &AppState, host: &str) -> bool {
    let cfg = ctx.server_config();
    let host = host.to_ascii_lowercase();
    let (name, port) = if let Some(rest) = host.strip_prefix('[') {
        // [v6]:port
        match rest.split_once(']') {
            Some((n, p)) => (
                n.to_string(),
                p.strip_prefix(':').and_then(|p| p.parse::<u16>().ok()),
            ),
            None => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((n, p)) => (n.to_string(), Some(p.parse::<u16>().unwrap_or(0))),
            None => (host.clone(), None),
        }
    };
    if let Some(public) = cfg
        .host_server
        .as_deref()
        .and_then(|h| url::Url::parse(h).ok())
    {
        if public
            .host_str()
            .map(|h| h.eq_ignore_ascii_case(&name))
            .unwrap_or(false)
        {
            return true;
        }
    }
    let port_ok = match port {
        None => cfg.http_port == 80,
        Some(p) => {
            p == cfg.http_port || p == ctx.listening_port() || (cfg!(debug_assertions) && p == 5173)
        }
    };
    if !port_ok {
        return false;
    }
    if matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return true;
    }
    // LAN access by IP address is allowed when the trader bound beyond
    // loopback; hostnames other than localhost never are.
    !cfg.is_loopback() && name.parse::<IpAddr>().is_ok()
}

pub async fn host_check(State(ctx): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    // HTTP/1.1 names the host in `Host`; HTTP/2 in `:authority`, which
    // arrives as the URI's authority.
    let named = match req.headers().get(header::HOST) {
        Some(h) => Some(h.to_str().ok()),
        None => req.uri().authority().map(|a| Some(a.as_str())),
    };
    match named {
        Some(Some(host)) if host_allowed(&ctx, host) => {}
        // A request over a real connection must name the host (security
        // review S-13); in-process callers (the tests) have no connection
        // and no `Host`.
        None if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none() => {}
        _ => {
            tracing::warn!("Rejected request with a missing or unexpected Host header");
            return error(StatusCode::BAD_REQUEST, "Request blocked.");
        }
    }
    next.run(req).await
}

// --------------------------------------------------------- session and CSRF

fn is_mutating(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Routes that never use the browser session's CSRF token.
fn csrf_exempt(path: &str) -> bool {
    path.starts_with("/api/v1/")
        || path == "/api/v1"
        || path.starts_with("/socket.io")
        || matches!(path, "/auth/login" | "/setup")
        || path.starts_with("/strategy/webhook/")
        // The OpenScript runner page: each call carries its run's secret.
        || path.starts_with("/openscript/runner/host/")
        || path.starts_with("/chartink/webhook/")
        // MCP: bearer token, never the browser session.
        || path == "/mcp"
        // Broker form-POST redirects (state-verified in the handler).
        || path
            .strip_suffix("/callback")
            .and_then(|p| p.strip_prefix('/'))
            .is_some_and(crate::brokers::catalog::posts_callback)
}

/// The CSRF and same-origin checks the session layer applies to a write,
/// for a handler on a path the layer exempts that still serves a
/// cookie-authenticated form for some callers (`/<broker>/callback`):
/// same-origin, and the session's token in the `X-CSRFToken` header or the
/// `csrf_token` form field.
pub fn write_allowed(
    ctx: &AppState,
    headers: &HeaderMap,
    expected: &str,
    form_token: Option<&str>,
) -> bool {
    if foreign_origin(ctx, headers) {
        return false;
    }
    let header = headers
        .get("x-csrftoken")
        .or_else(|| headers.get("x-csrf-token"))
        .and_then(|v| v.to_str().ok());
    header
        .or(form_token)
        .is_some_and(|t| tokens_match(t, expected))
}

/// A page navigation the app itself made, or one the trader typed: the
/// only GETs allowed to trigger a side effect such as a broker OTP send.
/// `Sec-Fetch-Site` decides when present (`same-origin` or `none`);
/// without it (older webviews) the `Origin`, else the `Referer`, must name
/// this app. Anything else (cross-site, or no evidence at all) is not.
pub fn same_origin_navigation(ctx: &AppState, headers: &HeaderMap) -> bool {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(site) = get("sec-fetch-site") {
        return site == "same-origin" || site == "none";
    }
    if let Some(origin) = get("origin") {
        if origin == "null" {
            return false;
        }
        let host = origin
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        return host_allowed(ctx, host);
    }
    let Some(url) = get("referer").and_then(|r| url::Url::parse(r).ok()) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = match url.port() {
        Some(p) => format!("{}:{}", host, p),
        None => host.to_string(),
    };
    host_allowed(ctx, &host)
}

/// Same-origin check for cookie-authenticated writes (web `logout` uses
/// `Sec-Fetch-Site`; this applies it to every write).
fn foreign_origin(ctx: &AppState, headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" && site != "none" {
            return true;
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin == "null" {
            return true;
        }
        let host = origin
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        return !host_allowed(ctx, host);
    }
    false
}

pub async fn session_layer(
    State(ctx): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let now = ctx.now();
    let session =
        cookie_value(req.headers(), COOKIE_NAME).and_then(|id| ctx.sessions.get(&id, now));
    let path = req.uri().path().to_string();

    if is_mutating(req.method()) && !csrf_exempt(&path) {
        if foreign_origin(&ctx, req.headers()) {
            return error(StatusCode::FORBIDDEN, "Request blocked.");
        }
        let Some(s) = session.as_ref() else {
            return csrf_rejected();
        };
        let header_token = req
            .headers()
            .get("x-csrftoken")
            .or_else(|| req.headers().get("x-csrf-token"))
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        match header_token {
            Some(t) => {
                if !tokens_match(&t, &s.csrf_token) {
                    return csrf_rejected();
                }
            }
            None => {
                // Look for the `csrf_token` form field, then hand the body on.
                let (parts, body) = req.into_parts();
                let bytes = match axum::body::to_bytes(body, crate::config::BODY_LIMIT_BYTES).await
                {
                    Ok(b) => b,
                    Err(_) => {
                        return error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large.")
                    }
                };
                let probe = Request::from_parts(parts.clone(), Body::from(bytes.clone()));
                let ok = match FormData::from_request(probe, &()).await {
                    Ok(f) => tokens_match(f.get("csrf_token").unwrap_or(""), &s.csrf_token),
                    Err(_) => false,
                };
                if !ok {
                    return csrf_rejected();
                }
                req = Request::from_parts(parts, Body::from(bytes));
            }
        }
    }

    req.extensions_mut().insert(SessionCtx(session));
    next.run(req).await
}

/// Guard for every non-public session route.
pub async fn require_user(req: Request, next: Next) -> Response {
    let user = req
        .extensions()
        .get::<SessionCtx>()
        .and_then(|s| s.0.clone())
        .and_then(|s| s.user.clone().map(|u| (u, s.id.clone())));
    match user {
        Some((username, session_id)) => {
            let mut req = req;
            req.extensions_mut().insert(AuthedUser {
                username,
                session_id,
            });
            next.run(req).await
        }
        None => not_authenticated(),
    }
}

/// Pages that are also JSON endpoints (`/apikey`): a browser navigation gets
/// the SPA, a JSON request needs the signed-in user.
pub async fn require_user_for_json(req: Request, next: Next) -> Response {
    let wants_json = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false);
    if req.method() == Method::GET && !wants_json {
        return crate::server::spa::index_response(StatusCode::OK);
    }
    require_user(req, next).await
}

// ----------------------------------------------------------- rate limiting

/// `/api/v1` per-IP moving window. 429 body is Flask-Limiter's
/// `{"message": "100 per 1 second"}`; no rate-limit headers.
pub async fn api_rate_limit(
    State(ctx): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let bucket = Bucket::for_api_path(req.uri().path());
    if ctx
        .limiter
        .check(bucket, client_ip(&req), ctx.limiter.now())
        .is_err()
    {
        return json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"message": bucket.describe()}),
        );
    }
    next.run(req).await
}

/// Login-type limits (5 per minute and 25 per hour per IP).
pub fn login_limited(ctx: &AppState, ip: IpAddr) -> Option<Response> {
    let now = ctx.limiter.now();
    let over = ctx.limiter.check(Bucket::LoginMinute, ip, now).is_err()
        || ctx.limiter.check(Bucket::LoginHour, ip, now).is_err();
    over.then(|| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many login attempts. Please wait a minute and try again.",
        )
    })
}

/// Adds the session cookie to a response.
pub fn with_cookie(mut resp: Response, id: &str) -> Response {
    resp.headers_mut()
        .append(header::SET_COOKIE, session_cookie(id));
    resp
}

pub fn redirect(to: &str) -> Response {
    (
        StatusCode::FOUND,
        [(
            header::LOCATION,
            HeaderValue::from_str(to).unwrap_or(HeaderValue::from_static("/")),
        )],
    )
        .into_response()
}
