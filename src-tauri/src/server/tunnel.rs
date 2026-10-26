//! The built-in tunnel's side of the HTTP server (`services::tunnel`).
//!
//! Every connection the tunnel hands over is served by the app's own
//! router with every layer, so a request through it meets the same checks
//! as one through any tunnel: a tunnel caller (`Source::Tunnel`), one
//! shared identity, never local, matching no IP allowlist, its own sign-in
//! and failure budgets. Two things more, because this tunnel is the app's
//! own:
//!
//! - Each request is marked as coming through it before any layer runs
//!   ([`ThroughBuiltInTunnel`]), so it is a tunnel caller whatever its
//!   headers say: the class never depends on what ngrok adds.
//! - Only the public routes are reachable through it ([`reach`]): `/api/v1`
//!   (its API key), the strategy and Chartink webhooks (the address is the
//!   credential) and `/mcp` (its bearer token, behind the Remote MCP
//!   switch). The app's pages, its sign-in and every session route stay on
//!   this computer. A broker's sign-in redirect is sent on to this
//!   computer's address, where the browser session that started it is.

use crate::brokers::catalog;
use crate::server::envelope;
use crate::server::source::ThroughBuiltInTunnel;
use crate::services::tunnel::provider::TunnelIo;
use crate::services::tunnel::ServeConnection;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, HeaderValue, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::future::BoxFuture;
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::{Layer, ServiceExt};
use tower_http::normalize_path::{NormalizePath, NormalizePathLayer};

/// How long a caller may take to send a request's headers.
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// The peer every tunnel request carries: loopback, as for any tunnel on
/// this computer. No caller's address is ever taken from the tunnel.
const TUNNEL_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// The answer to anything else through the tunnel.
pub(crate) const NOT_THROUGH_THE_TUNNEL: &str = "This address takes alerts and API calls for \
OpenAlgo Desktop only. To use OpenAlgo, open it on the computer where it is installed.";

/// What a caller through the tunnel may reach at a path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// A public route: served by the app.
    App,
    /// A broker's sign-in redirect: sent on to this computer's address.
    ThisComputer,
    /// Not served through the tunnel.
    Refused,
}

/// The routes a tunnel caller may reach, by exact shape. A path with an
/// empty, `.` or `..` segment reaches nothing.
pub(crate) fn reach(path: &str) -> Reach {
    let trimmed = match path.strip_suffix('/') {
        Some(p) if !p.is_empty() => p,
        _ => path,
    };
    let Some(rest) = trimmed.strip_prefix('/') else {
        return Reach::Refused;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Reach::Refused;
    }
    match segments.as_slice() {
        ["api", "v1", ..] => Reach::App,
        ["strategy", "webhook", _] | ["chartink", "webhook", _] => Reach::App,
        ["mcp"] | ["mcp", "healthz"] => Reach::App,
        [broker, "callback"] if catalog::ALL_BROKERS.contains(broker) => Reach::ThisComputer,
        _ => Reach::Refused,
    }
}

/// One request through the tunnel.
async fn handle(
    ctx: &Weak<AppState>,
    app: NormalizePath<Router>,
    mut req: Request<Body>,
) -> Response {
    match reach(req.uri().path()) {
        Reach::Refused => envelope::error(StatusCode::NOT_FOUND, NOT_THROUGH_THE_TUNNEL),
        Reach::ThisComputer => back_to_this_computer(ctx, req.uri()),
        Reach::App => {
            req.extensions_mut().insert(ConnectInfo(TUNNEL_PEER));
            req.extensions_mut().insert(ThroughBuiltInTunnel);
            match app.oneshot(req).await {
                Ok(resp) => resp,
                Err(never) => match never {},
            }
        }
    }
}

/// Send a broker's sign-in redirect on to the app on this computer, the
/// same path and query, method and body kept (307), so the sign-in
/// completes with the browser session that started it, as when the broker
/// redirects there directly. No `Referer` carries the query on.
fn back_to_this_computer(ctx: &Weak<AppState>, uri: &Uri) -> Response {
    let port = ctx.upgrade().map(|c| c.live_port()).unwrap_or(0);
    if port == 0 {
        return envelope::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenAlgo is not ready yet. Start the broker sign-in again in a moment.",
        );
    }
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let Ok(location) = HeaderValue::from_str(&format!("http://127.0.0.1:{}{}", port, path)) else {
        return envelope::error(StatusCode::BAD_REQUEST, "Request blocked.");
    };
    let mut resp = StatusCode::TEMPORARY_REDIRECT.into_response();
    let h = resp.headers_mut();
    h.insert(header::LOCATION, location);
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

/// Serve one caller's connection (HTTP/1.1, as ngrok forwards it) until it
/// closes; when `drain` is cancelled, finish the request in flight and
/// close.
async fn serve_connection(
    ctx: Weak<AppState>,
    app: NormalizePath<Router>,
    io: Box<dyn TunnelIo>,
    drain: CancellationToken,
) {
    let service = tower::service_fn(move |req: Request<Incoming>| {
        let (ctx, app) = (ctx.clone(), app.clone());
        async move { Ok::<_, Infallible>(handle(&ctx, app, req.map(Body::new)).await) }
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT);
    let conn = builder.serve_connection(TokioIo::new(io), TowerToHyperService::new(service));
    tokio::pin!(conn);
    let ended = tokio::select! {
        r = conn.as_mut() => r,
        _ = drain.cancelled() => {
            conn.as_mut().graceful_shutdown();
            conn.await
        }
    };
    if let Err(e) = ended {
        tracing::debug!("A connection through the tunnel ended: {}", e);
    }
}

/// How the tunnel's connections are served: the app's router, built for
/// this tunnel (its Socket.IO handle is dropped: Socket.IO is a session
/// route, never reachable through the tunnel).
pub(crate) fn connections(ctx: &Arc<AppState>) -> ServeConnection {
    let app = NormalizePathLayer::trim_trailing_slash().layer(crate::server::router(ctx.clone()).0);
    let weak = Arc::downgrade(ctx);
    Arc::new(
        move |io: Box<dyn TunnelIo>, drain: CancellationToken| -> BoxFuture<'static, ()> {
            Box::pin(serve_connection(weak.clone(), app.clone(), io, drain))
        },
    )
}

#[cfg(test)]
mod tests;
