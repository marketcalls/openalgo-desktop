//! The built-in ngrok tunnel: TradingView, Chartink and GoCharting alerts
//! reach the desktop with nothing for the trader to install (the web runs
//! ngrok through pyngrok, `utils/ngrok_manager.py`, with `NGROK_ALLOW` and
//! the domain from `HOST_SERVER`).
//!
//! | Module | What lives there |
//! |---|---|
//! | [`provider`] | the trait a tunnel is opened through (tests use a fake) |
//! | [`ngrok_sdk`] | ngrok's SDK as that provider |
//! | [`supervisor`] | the one task: open, serve, reconnect with a capped backoff |
//! | [`settings`] | the switch, the sealed authtoken and the reserved domain |
//! | [`status`] | what the trader sees: Stopped, Connecting, Online, Error |
//!
//! The tunnel runs only while the trader is signed in: it opens at the
//! sign-in when switched on ([`autostart`]), follows every settings change
//! ([`save`]), and closes at the sign-out, a fresh start and quitting
//! ([`Tunnel::stop`]). It opens only on the saved domain (the free one
//! ngrok gives every account), never on another address, and with the
//! switch on the stored Host Server URL is `https://<domain>`
//! (`db::sqlite::tunnel::pin_host_server`): the webhook addresses, the
//! Host check, the trusted origins and the automatic broker redirect use
//! that one address whether the tunnel is online or not (maintainer,
//! 2026-10-27). What a caller through it may reach is decided in
//! `server::tunnel`.

#[cfg(test)]
pub(crate) mod fake;
pub mod ngrok_sdk;
pub mod provider;
pub mod settings;
pub mod status;
mod supervisor;

pub use settings::{TunnelChange, TunnelView};
pub use status::{Off, TunnelState, TunnelStatus};
pub use supervisor::ServeConnection;

use provider::{OpenRequest, TunnelProvider};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::state::AppState;

/// How long a stop waits for the supervisor: its drain of requests in
/// flight and the close with ngrok, with room to spare.
const STOP_WAIT: Duration = Duration::from_secs(20);

/// Requests through the tunnel whose work may be running at once, waited
/// for or not (`server::tunnel::serve::answer_within`): the work of a
/// request whose caller was answered "still working" runs on, so the
/// connection cap alone would not bound it.
pub(crate) const MAX_WORK: usize = 128;

/// The running supervisor and what it was started with.
struct Running {
    request: OpenRequest,
    stop: CancellationToken,
    join: JoinHandle<()>,
}

/// The tunnel's state in the app context.
pub struct Tunnel {
    provider: parking_lot::RwLock<Arc<dyn TunnelProvider>>,
    status: Arc<watch::Sender<TunnelStatus>>,
    /// One change at a time: a start, a restart or a stop holds it from
    /// reading the settings to the supervisor being started or stopped.
    running: tokio::sync::Mutex<Option<Running>>,
    /// One permit per request whose work is running ([`MAX_WORK`]), for
    /// the app's whole life, across reconnects and restarts.
    work: Arc<tokio::sync::Semaphore>,
}

impl Default for Tunnel {
    fn default() -> Self {
        Self {
            provider: parking_lot::RwLock::new(Arc::new(ngrok_sdk::NgrokProvider::default())),
            status: Arc::new(watch::channel(TunnelStatus::stopped(Off::SignedOut)).0),
            running: tokio::sync::Mutex::new(None),
            work: Arc::new(tokio::sync::Semaphore::new(MAX_WORK)),
        }
    }
}

impl Tunnel {
    pub fn status(&self) -> TunnelStatus {
        self.status.borrow().clone()
    }

    /// The permits for requests' work through the tunnel ([`MAX_WORK`]).
    pub(crate) fn work(&self) -> Arc<tokio::sync::Semaphore> {
        self.work.clone()
    }

    /// Stop the tunnel and say why. Waits for requests in flight through it
    /// (bounded) and for the session with ngrok to close.
    pub async fn stop(&self, why: Off) {
        let mut running = self.running.lock().await;
        stop_running(&mut running).await;
        self.status.send_replace(TunnelStatus::stopped(why));
    }

    /// Open tunnels through `provider` from now on (tests: a fake).
    #[cfg(test)]
    pub(crate) fn use_provider(&self, provider: Arc<dyn TunnelProvider>) {
        *self.provider.write() = provider;
    }

    #[cfg(test)]
    pub(crate) fn watch(&self) -> watch::Receiver<TunnelStatus> {
        self.status.subscribe()
    }
}

async fn stop_running(running: &mut Option<Running>) {
    let Some(mut r) = running.take() else { return };
    r.stop.cancel();
    if tokio::time::timeout(STOP_WAIT, &mut r.join).await.is_err() {
        tracing::warn!("The tunnel did not stop in time; ended");
        r.join.abort();
    }
}

/// Bring the tunnel to what the settings say. `restart` opens it again
/// even when it already runs with these settings (a save from the
/// settings page, so the trader can retry after fixing their account).
pub async fn apply(ctx: &Arc<AppState>, restart: bool) {
    let tunnel = &ctx.tunnel;
    let mut running = tunnel.running.lock().await;
    match settings::wanted(ctx).await {
        Ok(request) => {
            let same = running
                .as_ref()
                .is_some_and(|r| r.request == request && !r.join.is_finished());
            if same && !restart {
                return;
            }
            stop_running(&mut running).await;
            *running = Some(start(ctx, request));
        }
        Err(off) => {
            stop_running(&mut running).await;
            tunnel.status.send_replace(TunnelStatus::stopped(off));
        }
    }
}

fn start(ctx: &Arc<AppState>, request: OpenRequest) -> Running {
    let stop = ctx.shutdown.child_token();
    let supervisor = supervisor::Supervisor {
        provider: ctx.tunnel.provider.read().clone(),
        request: request.clone(),
        status: ctx.tunnel.status.clone(),
        serve: crate::server::tunnel::connections(ctx),
        stop: stop.clone(),
    };
    Running {
        request,
        stop,
        join: tokio::spawn(supervisor.run()),
    }
}

/// At each sign-in: open the tunnel when it is switched on (the keys that
/// open the authtoken are in memory now). Its own task, so the sign-in
/// answers at once.
pub fn autostart(ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    ctx.spawn(async move {
        if let Some(ctx) = weak.upgrade() {
            apply(&ctx, false).await;
        }
    });
}

/// Save a change from the settings page and apply it at once.
pub async fn save(ctx: &Arc<AppState>, change: TunnelChange) -> Result<TunnelView> {
    settings::store_change(ctx, change).await?;
    apply(ctx, true).await;
    settings::view(ctx).await
}

#[cfg(test)]
mod tests;
