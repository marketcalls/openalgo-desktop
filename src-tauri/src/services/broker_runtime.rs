//! What runs while a broker session is live, started on login (or resume)
//! and torn down on logout, at the daily boundary and on app shutdown.
//!
//! `activate` (web `handle_auth_success` plus `order_update_service`):
//! 1. tear down whatever the previous session left running;
//! 2. master contract: download when the smart rule says so, else load the
//!    stored one (`master_contract_service::ensure`), with the web's
//!    Socket.IO events;
//! 3. the feed bridge learns the broker's depth levels;
//! 4. the market feed (`create_feed`) on the shared manager, a depth socket
//!    (`create_depth_feed`) when the broker streams deeper books, and the
//!    order feed (`create_order_feed`, a socket or the adapter's poller);
//! 5. order updates from any of them are published as `order.update` with
//!    the web payload, which the Socket.IO subscriber and the 8765 order
//!    stream consume.
//!
//! `teardown`: every task above is owned by one `JoinSet` and aborted; the
//! three managers are disconnected (sockets closed, subscriptions cleared);
//! the bridge forgets what it applied; the broker's `on_logout` stops what
//! the adapter runs itself (Groww's poller).

use crate::brokers::common::streaming::{FeedEvent, MarketEvent, OrderFeed, OrderUpdate};
use crate::brokers::types::AuthToken;
use crate::brokers::Broker;
use crate::error::AppError;
use crate::events::{self, Event, EventBus};
use crate::services::master_contract_service::{self, DownloadClaims};
use crate::state::{AppState, BrokerSession};
use crate::websocket::WebSocketManager;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

pub struct BrokerRuntime {
    /// The broker's order-update socket.
    pub order_ws: Arc<WebSocketManager>,
    /// The broker's deeper-book socket (Fyers 50, Dhan 20).
    pub depth_ws: Arc<WebSocketManager>,
    /// Master contract download claims (one per broker).
    pub claims: DownloadClaims,
    tasks: Arc<Mutex<JoinSet<()>>>,
    active: Mutex<Option<Arc<dyn Broker>>>,
    /// Serialises activate / teardown.
    lifecycle: tokio::sync::Mutex<()>,
}

impl Default for BrokerRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl BrokerRuntime {
    pub fn new() -> Self {
        Self {
            order_ws: Arc::new(WebSocketManager::new()),
            depth_ws: Arc::new(WebSocketManager::new()),
            claims: DownloadClaims::default(),
            tasks: Arc::new(Mutex::new(JoinSet::new())),
            active: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    /// Owned tasks still alive (finished ones are reaped first).
    pub fn task_count(&self) -> usize {
        let mut t = self.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    /// The broker whose session is running, if any.
    pub fn active_broker(&self) -> Option<String> {
        self.active.lock().as_ref().map(|b| b.id().to_string())
    }

    /// Run `fut` as a task of the current session (aborted on teardown).
    pub fn spawn_task<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        Self::spawn(&self.tasks, fut);
    }

    fn spawn<F>(tasks: &Arc<Mutex<JoinSet<()>>>, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut t = tasks.lock();
        while t.try_join_next().is_some() {}
        t.spawn(fut);
    }

    /// Start everything for a new broker session (returns once it is
    /// started; the master contract and the sockets come up in a task).
    pub async fn activate(&self, ctx: &Arc<AppState>, session: &BrokerSession) {
        let _guard = self.lifecycle.lock().await;
        self.teardown_locked(ctx).await;
        let Some(broker) = ctx.brokers.get(&session.broker_id) else {
            tracing::warn!("No adapter for {}; nothing to start", session.broker_id);
            return;
        };
        let auth = AuthToken::new(session.auth_token.expose())
            .with_feed(session.feed_token.as_ref().map(|s| s.expose().to_string()))
            .with_user_id(session.user_id.clone());
        *self.active.lock() = Some(broker.clone());
        // Every order update this session relays carries its generation; one
        // from an earlier session is never delivered (EV-02).
        let generation = ctx.bus.begin_session();

        // Order updates: subscribe before any socket connects so none is
        // missed. Kite carries them on the market feed.
        let bus = ctx.bus.clone();
        let id = broker.id();
        for rx in [
            ctx.websocket.subscribe_ticks(),
            self.order_ws.subscribe_ticks(),
        ] {
            let bus = bus.clone();
            Self::spawn(&self.tasks, relay_socket_orders(rx, bus, id, generation));
        }

        let weak = Arc::downgrade(ctx);
        let tasks = self.tasks.clone();
        Self::spawn(&self.tasks, async move {
            let Some(ctx) = weak.upgrade() else { return };
            start_session(ctx, broker, auth, tasks, generation).await;
        });
    }

    /// Stop everything the session runs. Idempotent.
    pub async fn teardown(&self, ctx: &AppState) {
        let _guard = self.lifecycle.lock().await;
        self.teardown_locked(ctx).await;
    }

    async fn teardown_locked(&self, ctx: &AppState) {
        // Updates still queued from the ending session are stale from here.
        ctx.bus.begin_session();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = ctx.websocket.disconnect().await;
        let _ = self.order_ws.disconnect().await;
        let _ = self.depth_ws.disconnect().await;
        ctx.bridge.reset();
        let broker = self.active.lock().take();
        if let Some(b) = broker {
            b.on_logout().await;
            tracing::info!("Broker streaming for {} stopped", b.id());
        }
    }
}

/// The session's start-up sequence (runs as an owned task).
async fn start_session(
    ctx: Arc<AppState>,
    broker: Arc<dyn Broker>,
    auth: AuthToken,
    tasks: Arc<Mutex<JoinSet<()>>>,
    generation: u64,
) {
    let id = broker.id();
    if let Err(e) = master_contract_service::ensure(&ctx, &broker, &auth).await {
        tracing::error!("Master contract for {} is not available: {}", id, e);
    }

    let caps = {
        let b = broker.clone();
        Arc::new(move |exchange: &str| b.feed_depth_levels(exchange))
    };
    ctx.bridge.set_depth_capability(caps);

    match broker.create_feed(&auth) {
        Ok(feed) => {
            if let Err(e) = ctx.websocket.connect(feed).await {
                tracing::error!("Market data feed for {} did not start: {}", id, e);
            }
        }
        Err(AppError::Unsupported(_)) => {
            tracing::info!("{} has no live market data feed in OpenAlgo Desktop", id)
        }
        Err(e) => tracing::warn!("Market data feed for {} not started: {}", id, e),
    }

    let deep = broker
        .capabilities()
        .depth_levels
        .iter()
        .copied()
        .filter(|l| *l > 5)
        .find_map(|l| broker.create_depth_feed(&auth, l).ok().map(|f| (l, f)));
    if let Some((levels, feed)) = deep {
        match ctx.runtime.depth_ws.connect(feed).await {
            Ok(()) => ctx.bridge.set_deep_levels(Some(levels)),
            Err(e) => tracing::warn!("Market depth feed for {} not started: {}", id, e),
        }
    }
    // Feed clients that were already subscribed are applied now that the
    // master and the sockets are in place.
    ctx.bridge.resync();

    match broker.create_order_feed(&auth) {
        Ok(OrderFeed::Socket(feed)) => {
            if let Err(e) = ctx.runtime.order_ws.connect(feed).await {
                tracing::warn!("Order updates for {} not started: {}", id, e);
            }
        }
        Ok(OrderFeed::Stream(rx)) => {
            let bus = ctx.bus.clone();
            BrokerRuntime::spawn(&tasks, relay_stream_orders(rx, bus, id, generation));
        }
        Err(AppError::Unsupported(_)) => {}
        Err(e) => tracing::warn!("Order updates for {} not started: {}", id, e),
    }
    tracing::info!("Broker session for {} is streaming", id);
}

/// The web's `OrderUpdateEvent` payload, stamped with its session.
pub fn order_event(u: &OrderUpdate, broker: &str, generation: u64) -> Event {
    Event::OrderUpdate(events::OrderUpdate {
        session_generation: generation,
        mode: "live".into(),
        broker: broker.to_string(),
        orderid: u.orderid.clone(),
        symbol: u.symbol.clone(),
        exchange: u.exchange.clone(),
        action: u.action.clone(),
        quantity: u.quantity,
        price: u.price,
        trigger_price: u.trigger_price,
        pricetype: u.pricetype.clone(),
        product: u.product.clone(),
        order_status: u.order_status.clone(),
        filled_quantity: u.filled_quantity,
        pending_quantity: u.pending_quantity,
        average_price: u.average_price,
        rejection_reason: u.rejection_reason.clone(),
    })
}

async fn relay_socket_orders(
    mut rx: broadcast::Receiver<MarketEvent>,
    bus: Arc<EventBus>,
    broker: &'static str,
    generation: u64,
) {
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if let FeedEvent::OrderUpdate(u) = &*ev {
                    bus.publish(order_event(u, broker, generation));
                }
            }
            Err(RecvError::Lagged(n)) => {
                // Skipped events may include order updates: the order
                // reconciler repairs from the broker's book (ARCH-01).
                tracing::warn!("Order update relay fell behind; skipped {} events", n);
                bus.report_relay_lag(n);
            }
            Err(RecvError::Closed) => return,
        }
    }
}

async fn relay_stream_orders(
    mut rx: mpsc::Receiver<OrderUpdate>,
    bus: Arc<EventBus>,
    broker: &'static str,
    generation: u64,
) {
    while let Some(u) = rx.recv().await {
        bus.publish(order_event(&u, broker, generation));
    }
}
