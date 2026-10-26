//! How a connection through the built-in tunnel is served, and how long
//! each part of it may take. Anyone who learns the tunnel's address can
//! connect; without these bounds a caller sending its body slowly, or
//! holding a connection open and idle, would keep a place among the
//! tunnel's connections (`services::tunnel::supervisor::MAX_CONNECTIONS`)
//! indefinitely, and enough of them would leave the trader's alerts
//! waiting. With them, a place is held for at most [`Limits::lifetime`].

use crate::server::envelope::error;
use crate::services::tunnel::provider::TunnelIo;
use crate::state::AppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tower::util::BoxCloneSyncService;
use tower::ServiceExt;

/// What answers a request through the tunnel: the app's router (tests: a
/// stand-in).
pub(crate) type App = BoxCloneSyncService<Request<Body>, Response, Infallible>;

/// The permits for requests' work through the tunnel
/// (`services::tunnel::MAX_WORK`; tests: their own).
pub(crate) type Work = Arc<tokio::sync::Semaphore>;

/// How long each part of a request through the tunnel may take.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// To send the request's headers (an idle connection is closed then).
    pub header: Duration,
    /// To send the request's body, once the headers are in.
    pub body: Duration,
    /// For the app's answer. Past it the caller is told the request is
    /// still being worked on, and the work runs on to its end: an order is
    /// never cut off half-way.
    pub answer: Duration,
    /// The connection's whole life, writing the answer included.
    pub lifetime: Duration,
}

impl Limits {
    /// The slowest legitimate call through the tunnel is an order through
    /// a broker: at most 10 s waiting for its turn on the broker's pacer
    /// (`common::ratelimit::Pacer`), then the broker request's own 30 s
    /// timeout (`brokers::common::http`), plus the API key check, about
    /// 45 s in all; the answer gets 60 s. Alerts are small and sent at
    /// once, so 10 s each for the headers and the body is generous. The
    /// lifetime covers all three and the writing of the answer.
    pub(crate) const STANDARD: Limits = Limits {
        header: Duration::from_secs(10),
        body: Duration::from_secs(10),
        answer: Duration::from_secs(60),
        lifetime: Duration::from_secs(90),
    };
}

/// The request with its whole body read, within `limit` and the app's
/// size cap, or the answer that refuses it.
pub(super) async fn whole_body(
    req: Request<Body>,
    limit: Duration,
) -> Result<Request<Body>, Response> {
    let (parts, body) = req.into_parts();
    let read = axum::body::to_bytes(body, crate::config::BODY_LIMIT_BYTES);
    match tokio::time::timeout(limit, read).await {
        Ok(Ok(bytes)) => Ok(Request::from_parts(parts, Body::from(bytes))),
        Ok(Err(_)) => Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The request is too large.",
        )),
        Err(_) => Err(error(
            StatusCode::REQUEST_TIMEOUT,
            "The request took too long to arrive. Send it again.",
        )),
    }
}

/// The app's answer, waited for at most `limit`.
///
/// The work is detached on purpose: it runs in a task the context owns
/// (`AppState::spawn`) and is never dropped when the wait ends or the
/// caller hangs up, because dropping a handler mid-flight could cut an
/// order off half-way (sent to the broker, never recorded). Detached work
/// is still bounded: the task holds one of `work`'s permits until the work
/// has actually ended, and a request that finds none free is answered 503
/// at once, before anything runs.
pub(super) async fn answer_within(
    ctx: &Arc<AppState>,
    work: Work,
    app: App,
    req: Request<Body>,
    limit: Duration,
) -> Response {
    let Ok(permit) = work.try_acquire_owned() else {
        tracing::warn!("Every place for work through the tunnel is taken; a request was refused");
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenAlgo is still working on earlier requests through the tunnel. Send this one \
             again in a moment.",
        );
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctx.spawn(async move {
        let answer = match app.oneshot(req).await {
            Ok(resp) => resp,
            Err(never) => match never {},
        };
        drop(permit);
        let _ = tx.send(answer);
    });
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => stopping(),
        Err(_) => {
            tracing::warn!(
                "A request through the tunnel is still running; the caller was answered"
            );
            error(
                StatusCode::GATEWAY_TIMEOUT,
                "OpenAlgo is still working on this request. Check the order book before \
                 sending it again.",
            )
        }
    }
}

/// The answer while OpenAlgo is stopping.
pub(super) fn stopping() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "OpenAlgo is stopping. Send the request again once it is running.",
    )
}

/// Serve one caller's connection (HTTP/1.1, as ngrok forwards it): one
/// request, no keep-alive, the headers within `limits.header`, and the
/// whole connection within `limits.lifetime`. When `drain` is cancelled,
/// the request in flight is finished and the connection closed.
pub(super) async fn connection<S>(
    io: Box<dyn TunnelIo>,
    service: S,
    limits: Limits,
    drain: CancellationToken,
) where
    S: tower::Service<Request<hyper::body::Incoming>, Response = Response, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header)
        .keep_alive(false);
    let conn = builder.serve_connection(TokioIo::new(io), TowerToHyperService::new(service));
    tokio::pin!(conn);
    let served = tokio::time::timeout(limits.lifetime, async {
        tokio::select! {
            r = conn.as_mut() => r,
            _ = drain.cancelled() => {
                conn.as_mut().graceful_shutdown();
                conn.as_mut().await
            }
        }
    })
    .await;
    match served {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!("A connection through the tunnel ended: {}", e),
        Err(_) => tracing::debug!("A connection through the tunnel was closed at its time limit"),
    }
}
