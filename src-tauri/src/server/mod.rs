//! The HTTP server: one axum router on one port serving the React app, the
//! browser-session routes, broker callbacks, `/api/v1` and Socket.IO.
//!
//! Layers, outermost first: trailing-slash normalisation, tracing (method
//! and path only, never the query), CORS limited to the app origin, security
//! headers, Host check (DNS rebinding), body limit, browser session + CSRF.

pub mod addr;
pub mod api_v1;
mod csp;
pub mod envelope;
pub mod form;
pub mod middleware;
pub mod origin;
pub mod ratelimit;
pub mod routes;
pub mod socketio;
pub mod source;
pub mod spa;
pub(crate) mod tunnel;

#[cfg(test)]
mod tests;

use crate::state::{AppState, ServerStatus};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request},
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware as mw,
    response::Response,
    Router, ServiceExt,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::Layer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::normalize_path::{NormalizePath, NormalizePathLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// Whether a path is in an API namespace: `/api/...`, or a page's own
/// `/<area>/api[/...]` (`/scalping/api/...`, `/agent/api/...`).
///
/// An unknown path there is a mistyped or removed endpoint, never a page, so
/// it answers the JSON 404 on every method instead of the app's 404 page: an
/// API caller gets a body it can parse. The web's 404 handler does this for
/// `/api/` and Flow's `/flow/webhook/`; the desktop extends it to the
/// per-area API prefixes, where the web still answers with its page.
fn is_api_path(path: &str) -> bool {
    let mut segments = path.strip_prefix('/').unwrap_or(path).split('/');
    match (segments.next(), segments.next()) {
        (Some("api"), Some(_)) => true,
        (Some("flow"), Some("webhook")) => true,
        (Some(area), Some("api")) => !area.is_empty(),
        _ => false,
    }
}

async fn fallback(method: Method, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if is_api_path(&path) {
        return envelope::not_found(&path);
    }
    if method == Method::GET || method == Method::HEAD {
        return spa::serve(req.uri());
    }
    envelope::not_found(&path)
}

async fn method_not_allowed(method: Method, req: Request) -> Response {
    let path = req.uri().path().to_string();
    // A page address that is also a POST-only backend route (`/setup`): a
    // browser visit or refresh gets the app, as the web serves both on one
    // path. API routes keep the web's 404.
    if (method == Method::GET || method == Method::HEAD)
        && !path.starts_with("/api/")
        && spa::is_spa_route(&path)
    {
        return spa::serve(req.uri());
    }
    if path.starts_with("/api/v1/") {
        // Web contract: wrong method on an API route is 404.
        return envelope::not_found(&path);
    }
    envelope::error(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed")
}

fn cors(ctx: Arc<AppState>) -> CorsLayer {
    CorsLayer::new()
        // NEW-SEC-01: the exact origin, not only its host name (server/origin.rs).
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, parts| {
                let host = origin::request_host(&parts.headers)
                    .or(parts.uri.authority().map(|a| a.as_str()));
                origin
                    .to_str()
                    .is_ok_and(|o| origin::allowed(&ctx, o, host))
            },
        ))
        .allow_credentials(true)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("x-csrftoken"),
            HeaderName::from_static("x-requested-with"),
        ])
}

/// The router with every layer, without trailing-slash normalisation.
pub fn router(ctx: Arc<AppState>) -> (Router, socketioxide::SocketIo) {
    let (sio_layer, io) = socketio::layer(&ctx);
    let api = api_v1::router().route_layer(mw::from_fn_with_state(
        ctx.clone(),
        middleware::api_rate_limit,
    ));
    let app = Router::new()
        .merge(routes::router())
        .merge(api)
        .fallback(fallback)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(sio_layer)
        .layer(mw::from_fn_with_state(ctx.clone(), middleware::session_layer))
        .layer(DefaultBodyLimit::max(crate::config::BODY_LIMIT_BYTES))
        .layer(mw::from_fn_with_state(ctx.clone(), middleware::host_check))
        // Traffic, latency and blocked-address checks (services::monitor).
        .layer(mw::from_fn_with_state(
            ctx.clone(),
            crate::services::monitor::layer,
        ))
        // Who is calling (this computer, a device on the network, or a
        // tunnel caller), classified once before every per-caller control
        // (security review S-03).
        .layer(mw::from_fn_with_state(ctx.clone(), middleware::peer_layer))
        // Content Security Policy, naming the feed's origin (`csp`).
        .layer(mw::from_fn_with_state(ctx.clone(), csp::layer))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(cors(ctx.clone()))
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &Request<Body>| {
                tracing::debug_span!("http", method = %req.method(), path = %req.uri().path())
            }),
        )
        .with_state(ctx);
    (app, io)
}

/// The full service as served (also what the in-process tests drive).
pub fn app(ctx: Arc<AppState>) -> NormalizePath<Router> {
    let (router, io) = router(ctx.clone());
    ctx.ui.set(Some(io));
    ctx.ui.set_owner(Arc::downgrade(&ctx));
    NormalizePathLayer::trim_trailing_slash().layer(router)
}

/// Trader-facing explanation for a port that is already taken.
pub fn port_in_use_message(port: u16) -> String {
    let mac = if cfg!(target_os = "macos") && port == 5000 {
        " On a Mac this is usually AirPlay Receiver: open System Settings, General, AirDrop and Handoff, turn off AirPlay Receiver, then choose Try again."
    } else {
        " Close the other program (for example another copy of OpenAlgo or OpenAlgo web), then choose Try again."
    };
    format!(
        "OpenAlgo could not start because port {} is already used by another program.{} You can also choose a different port.",
        port, mac
    )
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    token: CancellationToken,
    join: JoinHandle<()>,
}

impl ServerHandle {
    /// Stop accepting, let in-flight requests finish (bounded), release the port.
    pub async fn stop(self) {
        self.token.cancel();
        if tokio::time::timeout(Duration::from_secs(5), self.join)
            .await
            .is_err()
        {
            tracing::warn!("HTTP server did not stop in time");
        }
    }
}

/// Bind and serve. A taken port is reported in `ctx.server_status` with a
/// trader-facing message, never only logged.
pub async fn start(ctx: Arc<AppState>) -> Result<ServerHandle, ServerStatus> {
    let cfg = ctx.server_config();
    let host = if cfg.is_loopback() {
        "127.0.0.1".to_string()
    } else {
        cfg.bind_host.clone()
    };
    let addr: SocketAddr = match format!("{}:{}", host, cfg.http_port).parse() {
        Ok(a) => a,
        Err(_) => {
            let st = ServerStatus::Failed {
                message: "The server address in Settings is not valid. Use 127.0.0.1.".into(),
            };
            *ctx.server_status.write() = st.clone();
            return Err(st);
        }
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::error!("Port {} is already in use", cfg.http_port);
            let st = ServerStatus::PortInUse {
                port: cfg.http_port,
                message: port_in_use_message(cfg.http_port),
            };
            *ctx.server_status.write() = st.clone();
            return Err(st);
        }
        Err(e) => {
            tracing::error!("Could not bind {}: {}", addr, e);
            let st = ServerStatus::Failed {
                message: format!(
                    "OpenAlgo could not open port {}. Choose a different port and try again.",
                    cfg.http_port
                ),
            };
            *ctx.server_status.write() = st.clone();
            return Err(st);
        }
    };
    // The port actually bound, the one source of truth for what is live.
    let addr = listener.local_addr().unwrap_or(addr);
    let bound = addr.port();
    let service = app(ctx.clone());
    let token = ctx.shutdown.child_token();
    let stop = token.clone();
    // Live from here on; set before the task starts so a listener that dies
    // at once still clears it.
    *ctx.server_status.write() = ServerStatus::Running {
        host: host.clone(),
        port: bound,
    };
    // Weak: the context does not own this task (the server handle does).
    let owner = Arc::downgrade(&ctx);
    let join = tokio::spawn(async move {
        let r = axum::serve(
            listener,
            ServiceExt::<Request>::into_make_service_with_connect_info::<SocketAddr>(service),
        )
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .await;
        if let Err(e) = &r {
            tracing::error!("HTTP server stopped: {}", e);
        }
        // This listener no longer holds the port: nothing may trust it.
        if let Some(ctx) = owner.upgrade() {
            let mut st = ctx.server_status.write();
            if matches!(&*st, ServerStatus::Running { port, .. } if *port == bound) {
                *st = match r {
                    Ok(()) => ServerStatus::Starting,
                    Err(_) => ServerStatus::Failed {
                        message: "The OpenAlgo server stopped. Restart it from Settings.".into(),
                    },
                };
            }
        }
    });
    tracing::info!("OpenAlgo server listening on http://{}", addr);
    Ok(ServerHandle { addr, token, join })
}
