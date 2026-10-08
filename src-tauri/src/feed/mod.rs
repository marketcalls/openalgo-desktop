//! Client-facing WebSocket market-data feed (`ws://127.0.0.1:8765`), the
//! desktop form of the web's `websocket_proxy/server.py`.
//!
//! The Python SDK, the web frontend's MarketDataManager, the Playground and
//! the `/websocket/test` pages connect here unchanged: `authenticate`,
//! `subscribe` / `unsubscribe` / `unsubscribe_all` with modes 1/2/3 and depth
//! 5/20/30/50, `market_data` frames, `subscribe_orders` and `order_update`
//! frames, `ping`, the error frame and the 4401 auth-timeout close.
//!
//! * [`source`]: the boundary consumed ([`source::MarketDataSource`]) and the
//!   normalized update types; [`source::FakeSource`] for tests.
//! * [`bridge`]: the production source over `websocket::WebSocketManager`.
//! * [`server`]: listener, connection lifecycle and protocol handling.
//! * [`registry`]: per-client subscriptions and source reference counts.
//! * [`outbox`]: per-client bounded queue (latest-per-symbol for slow
//!   clients).
//! * [`orders`]: `order.update` bus topic to `order_update` frames.
//! * [`FeedService`]: what the app starts, restarts on a port change and
//!   stops on exit.

pub mod auth;
pub mod bridge;
pub mod orders;
pub mod outbox;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod source;

pub use server::{start, FeedConfig, FeedDeps, FeedHandle, StartError};
pub use source::{FakeSource, InstrumentKey, MarketDataSource, MarketUpdate, Mode};

use crate::state::{AppState, ServerStatus};
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// How often the service checks the stored settings for a new host or port.
const CONFIG_POLL: Duration = Duration::from_secs(2);

/// The feed server as the app runs it: one instance per app, created once
/// (it registers the order-update relay on the event bus, which has no
/// unsubscribe), started after the HTTP server, restarted when the
/// WebSocket host or port changes, stopped on exit.
pub struct FeedService {
    ctx: Arc<AppState>,
    bridge: Arc<bridge::BrokerBridge>,
    relay: Arc<orders::OrderRelay>,
    handle: tokio::sync::Mutex<Option<(FeedHandle, (String, u16))>>,
    status: RwLock<ServerStatus>,
    watcher: Mutex<Option<JoinHandle<()>>>,
}

/// Feed listener settings from the stored server configuration.
pub fn config_from(ctx: &AppState) -> FeedConfig {
    let c = ctx.server_config();
    FeedConfig {
        host: if c.is_loopback() {
            "127.0.0.1".into()
        } else {
            c.bind_host.clone()
        },
        port: c.ws_port,
        ..FeedConfig::default()
    }
}

impl FeedService {
    /// Must be called inside the Tokio runtime, once per `AppState`.
    pub fn new(ctx: Arc<AppState>) -> Arc<Self> {
        let relay = orders::OrderRelay::register(&ctx.bus);
        Arc::new(Self {
            bridge: ctx.bridge.clone(),
            ctx,
            relay,
            handle: tokio::sync::Mutex::new(None),
            status: RwLock::new(ServerStatus::Starting),
            watcher: Mutex::new(None),
        })
    }

    /// The production source; the broker streaming layer feeds it.
    pub fn bridge(&self) -> &Arc<bridge::BrokerBridge> {
        &self.bridge
    }

    /// Running, or why not (a taken port carries a trader-facing message).
    pub fn status(&self) -> ServerStatus {
        self.status.read().clone()
    }

    pub async fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.handle
            .lock()
            .await
            .as_ref()
            .map(|(h, _)| h.local_addr())
    }

    async fn start_locked(
        &self,
        slot: &mut Option<(FeedHandle, (String, u16))>,
        cfg: FeedConfig,
    ) -> ServerStatus {
        let target = (cfg.host.clone(), cfg.port);
        let deps = FeedDeps {
            source: self.bridge.clone(),
            auth: Arc::new(auth::AppAuth::new(self.ctx.clone())),
            orders: Some(self.relay.receiver()),
            supported_brokers: crate::brokers::catalog::ALL_BROKERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let st = match server::start(cfg, deps).await {
            Ok(h) => {
                let st = ServerStatus::Running {
                    host: target.0.clone(),
                    port: h.local_addr().port(),
                };
                *slot = Some((h, target));
                st
            }
            Err(StartError::PortInUse { port, message }) => {
                ServerStatus::PortInUse { port, message }
            }
            Err(StartError::Failed { message }) => ServerStatus::Failed { message },
        };
        *self.ctx.feed_status.write() = st.clone();
        let previous = std::mem::replace(&mut *self.status.write(), st.clone());
        // Logged once per change; the watcher retries quietly.
        if previous != st {
            match &st {
                ServerStatus::PortInUse { message, .. } | ServerStatus::Failed { message } => {
                    tracing::error!("Market data feed not running: {}", message)
                }
                _ => {}
            }
        }
        st
    }

    /// Start the bridge, the listener (no-op when running) and the
    /// settings watcher.
    pub async fn start(self: &Arc<Self>) -> ServerStatus {
        self.bridge.start();
        let st = {
            let mut slot = self.handle.lock().await;
            if slot.is_some() {
                return self.status();
            }
            self.start_locked(&mut slot, config_from(&self.ctx)).await
        };
        let mut w = self.watcher.lock();
        if w.is_none() {
            let me = Arc::downgrade(self);
            *w = Some(tokio::spawn(async move {
                let mut tick = tokio::time::interval(CONFIG_POLL);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tick.tick().await;
                    let Some(svc) = me.upgrade() else { return };
                    svc.apply_config().await;
                }
            }));
        }
        st
    }

    /// Restart when the configured host or port differs from what is bound,
    /// or retry a start that failed. The watcher calls this every couple of
    /// seconds, so a settings change or a freed port takes effect on its own.
    pub async fn apply_config(&self) -> ServerStatus {
        let cfg = config_from(&self.ctx);
        let mut slot = self.handle.lock().await;
        let current = slot.as_ref().map(|(_, t)| t.clone());
        let wanted = (cfg.host.clone(), cfg.port);
        match current {
            Some(t) if t == wanted => return self.status(),
            Some(_) => {
                tracing::info!(
                    "WebSocket settings changed; moving the market data feed to {}:{}",
                    wanted.0,
                    wanted.1
                );
                if let Some((h, _)) = slot.take() {
                    h.stop().await;
                }
            }
            // Not running (port taken or address refused): try again, so
            // the feed comes up once the other program lets go of the port.
            None => {}
        }
        self.start_locked(&mut slot, cfg).await
    }

    /// Stop and start again with the current settings.
    pub async fn restart(&self) -> ServerStatus {
        let mut slot = self.handle.lock().await;
        if let Some((h, _)) = slot.take() {
            h.stop().await;
        }
        self.start_locked(&mut slot, config_from(&self.ctx)).await
    }

    /// Stop the watcher and the listener; release the port and every source
    /// subscription.
    pub async fn stop(&self) {
        if let Some(w) = self.watcher.lock().take() {
            w.abort();
        }
        if let Some((h, _)) = self.handle.lock().await.take() {
            h.stop().await;
        }
        self.bridge.stop().await;
        *self.status.write() = ServerStatus::Starting;
        *self.ctx.feed_status.write() = ServerStatus::Starting;
    }
}
