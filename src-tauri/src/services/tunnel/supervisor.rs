//! The tunnel's one task: open the tunnel, serve its connections, and
//! after a failure or a loss open it again with a capped backoff, until
//! stopped. A refused authtoken or domain is not retried: only a settings
//! change (which starts a new supervisor) can fix it.

use super::provider::{OpenError, OpenRequest, OpenTunnel, TunnelIo, TunnelProvider};
use super::status::TunnelStatus;
use futures_util::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// How one caller's connection is served. The second argument is cancelled
/// when the tunnel stops: the connection finishes the request it is on
/// and closes.
pub type ServeConnection =
    Arc<dyn Fn(Box<dyn TunnelIo>, CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

/// How long opening a tunnel may take before it counts as unreachable.
pub(super) const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest wait between attempts.
pub(super) const MAX_RETRY: Duration = Duration::from_secs(30);
/// Callers served at once. The next is turned away at once (its
/// connection closed; ngrok answers it with an error), never queued: each
/// place is held at most `server::tunnel::Limits::lifetime`.
pub(super) const MAX_CONNECTIONS: usize = 128;
/// How long requests in flight (an order being placed) get to finish
/// when the tunnel stops, before their connections are dropped.
pub(super) const DRAIN: Duration = Duration::from_secs(10);
/// How long closing the session with ngrok may take.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// The wait before the next attempt after `failures` in a row: 1 s,
/// doubling, at most [`MAX_RETRY`].
pub(super) fn backoff(failures: u32) -> Duration {
    let doubled = 1u64
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u64::MAX);
    Duration::from_secs(doubled).min(MAX_RETRY)
}

pub(super) struct Supervisor {
    pub provider: Arc<dyn TunnelProvider>,
    pub request: OpenRequest,
    pub status: Arc<watch::Sender<TunnelStatus>>,
    pub serve: ServeConnection,
    pub stop: CancellationToken,
}

/// How serving an open tunnel ended.
#[derive(PartialEq, Eq)]
enum Ended {
    Stopped,
    Lost,
}

impl Supervisor {
    pub(super) async fn run(self) {
        let mut failures = 0u32;
        self.status.send_replace(TunnelStatus::connecting(false));
        loop {
            let opened = tokio::select! {
                _ = self.stop.cancelled() => return,
                r = tokio::time::timeout(OPEN_TIMEOUT, self.provider.open(&self.request)) => {
                    r.unwrap_or(Err(OpenError::Unreachable))
                }
            };
            match opened {
                Ok(tunnel) => {
                    if self.serve_until_lost(tunnel).await == Ended::Stopped {
                        return;
                    }
                    tracing::warn!("The ngrok tunnel was lost; reconnecting");
                    self.status.send_replace(TunnelStatus::connecting(true));
                    failures = 1;
                }
                Err(problem) => {
                    self.status.send_replace(TunnelStatus::failed(
                        problem,
                        self.request.domain.as_deref(),
                    ));
                    if !problem.retries() {
                        self.stop.cancelled().await;
                        return;
                    }
                    failures = failures.saturating_add(1);
                }
            }
            tokio::select! {
                _ = self.stop.cancelled() => return,
                _ = tokio::time::sleep(backoff(failures)) => {}
            }
        }
    }

    /// Online: serve every connection until the tunnel is lost or the
    /// supervisor stopped, then let requests in flight finish (bounded)
    /// and close the tunnel.
    async fn serve_until_lost(&self, mut tunnel: Box<dyn OpenTunnel>) -> Ended {
        let url = tunnel.url();
        tracing::info!("The ngrok tunnel is online at {}", url);
        self.status.send_replace(TunnelStatus::online(&url));
        let drain = CancellationToken::new();
        let mut conns = JoinSet::new();
        let mut turned_away = 0u64;
        let ended = loop {
            tokio::select! {
                _ = self.stop.cancelled() => break Ended::Stopped,
                next = tunnel.accept() => match next {
                    Some(io) => {
                        while conns.try_join_next().is_some() {}
                        if conns.len() >= MAX_CONNECTIONS {
                            drop(io);
                            turned_away += 1;
                            if turned_away.is_power_of_two() {
                                tracing::warn!(
                                    "The tunnel is serving {} callers; {} more turned away",
                                    MAX_CONNECTIONS,
                                    turned_away
                                );
                            }
                        } else {
                            conns.spawn((self.serve)(io, drain.child_token()));
                        }
                    }
                    None => break Ended::Lost,
                },
            }
        };
        drain.cancel();
        let drained =
            tokio::time::timeout(DRAIN, async { while conns.join_next().await.is_some() {} }).await;
        if drained.is_err() {
            tracing::warn!("Requests through the tunnel did not finish in time; closing them");
        }
        conns.shutdown().await;
        if tokio::time::timeout(CLOSE_TIMEOUT, tunnel.close())
            .await
            .is_err()
        {
            tracing::debug!("Closing the ngrok session took too long; dropped");
        }
        ended
    }
}
