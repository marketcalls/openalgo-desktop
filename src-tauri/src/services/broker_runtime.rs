//! What runs while a broker session is live, started on login (or resume)
//! and torn down on logout, at the daily boundary and on app shutdown.
//!
//! `activate` (web `handle_auth_success` plus `order_update_service`):
//! 1. tear down whatever the previous session left running;
//! 2. master contract: download when the smart rule says so, else load the
//!    stored one (`master_contract_service::ensure`), with the web's
//!    Socket.IO events;
//! 3. the feed bridge learns the broker's depth levels;
//! 4. the market feed (`create_feed`) on the shared manager, pooled over as
//!    many connections as the broker allows (`limits`, audit C07), a depth
//!    socket (`create_depth_feed`) when the broker streams deeper books,
//!    and the order feed (`create_order_feed`, a socket or the adapter's
//!    poller);
//! 5. order updates from any of them are published as `order.update` with
//!    the web payload, which the Socket.IO subscriber and the 8765 order
//!    stream consume.
//!
//! `teardown`: every task above is owned by one `JoinSet` and aborted; the
//! three managers are disconnected (sockets closed, subscriptions cleared);
//! the bridge forgets what it applied; the broker's `on_logout` stops what
//! the adapter runs itself (Groww's poller).

use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedMode, FeedRefusal, FeedSubscription, MarketEvent, OrderFeed,
    OrderUpdate,
};
use crate::brokers::types::AuthToken;
use crate::brokers::Broker;
use crate::error::AppError;
use crate::events::{self, Event, EventBus};
use crate::feed::bridge;
use crate::feed::{InstrumentKey, Mode};
use crate::services::master_contract_service::{self, DownloadClaims};
use crate::state::{AppState, BrokerSession};
use crate::websocket::{FeedFactory, WebSocketManager};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

mod limits;
#[cfg(test)]
mod relay_tests;

/// Where this session's order updates come from (health: an app asking
/// for order updates is not the broker's order feed running).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderSource {
    /// No broker session, or the broker streams no order updates here.
    #[default]
    None,
    /// The broker's own order socket (`order_ws`).
    Socket,
    /// The market data feed carries them (Kite, the Noren family).
    MarketFeed,
    /// The adapter reads them itself, from its order book or a stream of
    /// its own (Groww, 5paisa, Samco).
    Adapter,
    /// The broker has an order feed, and it did not start.
    NotStarted,
}

pub struct BrokerRuntime {
    /// The broker's order-update socket.
    pub order_ws: Arc<WebSocketManager>,
    /// The broker's deeper-book socket (Fyers 50, Dhan 20).
    pub depth_ws: Arc<WebSocketManager>,
    /// Master contract download claims (one per broker).
    pub claims: DownloadClaims,
    tasks: Arc<Mutex<JoinSet<()>>>,
    active: Mutex<Option<Arc<dyn Broker>>>,
    /// When the session last activated was authenticated (the daily
    /// boundary ends only a session from before it, SES-02); read only
    /// while `active` holds a broker.
    started: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
    /// Serialises activate / teardown.
    lifecycle: tokio::sync::Mutex<()>,
    /// Where the running session's order updates come from.
    order_source: Mutex<OrderSource>,
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
            started: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
            order_source: Mutex::new(OrderSource::None),
        }
    }

    /// Owned tasks still alive (finished ones are reaped first).
    pub fn task_count(&self) -> usize {
        let mut t = self.tasks.lock();
        while t.try_join_next().is_some() {}
        t.len()
    }

    /// Where the running session's order updates come from.
    pub fn order_source(&self) -> OrderSource {
        *self.order_source.lock()
    }

    /// The broker whose session is running, if any.
    pub fn active_broker(&self) -> Option<String> {
        self.active.lock().as_ref().map(|b| b.id().to_string())
    }

    /// Stop what runs only if it is a daily-boundary session authenticated
    /// before `boundary` (SES-02): checked under the lifecycle lock, so a
    /// session activated after the boundary keeps running, and a continuous
    /// (crypto) session is never stopped here (SES-01). Returns whether a
    /// session was stopped.
    pub async fn teardown_started_before(
        &self,
        ctx: &AppState,
        boundary: chrono::DateTime<chrono::Utc>,
    ) -> bool {
        let _guard = self.lifecycle.lock().await;
        let daily = self
            .active
            .lock()
            .as_ref()
            .is_some_and(|b| crate::brokers::catalog::has_daily_boundary(b.id()));
        let old = daily && self.started.lock().is_some_and(|t| t < boundary);
        if old {
            self.teardown_locked(ctx).await;
        }
        old
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
        let previous = self.teardown_locked(ctx).await;
        // Switching brokers without a logout: the master in memory is the
        // previous broker's, and its tokens must not reach this session's
        // feed or orders even if this broker's master fails to load.
        if previous.is_some_and(|p| p != session.broker_id) {
            ctx.clear_symbol_cache();
        }
        let Some(broker) = ctx.brokers.get(&session.broker_id) else {
            tracing::warn!("No adapter for {}; nothing to start", session.broker_id);
            return;
        };
        let auth = AuthToken::new(session.auth_token.expose())
            .with_feed(session.feed_token.as_ref().map(|s| s.expose().to_string()))
            .with_user_id(session.user_id.clone());
        *self.active.lock() = Some(broker.clone());
        *self.started.lock() = Some(session.authenticated_at);
        // Every order update this session relays carries its generation; one
        // from an earlier session is never delivered (EV-02).
        let generation = ctx.bus.begin_session();

        // Order updates: subscribe before any socket connects so none is
        // missed. Kite carries them on the market feed.
        let bus = ctx.bus.clone();
        for feed in [ctx.websocket.clone(), self.order_ws.clone()] {
            let rx = feed.subscribe_ticks();
            let bus = bus.clone();
            Self::spawn(
                &self.tasks,
                relay_socket_orders(rx, feed, bus, (broker.clone(), auth.clone(), generation)),
            );
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

    /// Returns the broker whose session was running, if any.
    async fn teardown_locked(&self, ctx: &AppState) -> Option<&'static str> {
        // Updates still queued from the ending session are stale from here.
        ctx.bus.begin_session();
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = ctx.websocket.disconnect().await;
        let _ = self.order_ws.disconnect().await;
        let _ = self.depth_ws.disconnect().await;
        ctx.bridge.reset();
        *self.order_source.lock() = OrderSource::None;
        let broker = self.active.lock().take();
        let b = broker?;
        b.on_logout().await;
        tracing::info!("Broker streaming for {} stopped", b.id());
        Some(b.id())
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
    let master_ready = match master_contract_service::ensure(&ctx, &broker, &auth).await {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("Master contract for {} is not available: {}", id, e);
            master_contract_service::report_unavailable(&ctx, id);
            false
        }
    };

    let caps = {
        let b = broker.clone();
        Arc::new(move |exchange: &str| b.feed_depth_levels(exchange))
    };
    ctx.bridge.set_depth_capability(caps);
    let refusal = {
        let b = broker.clone();
        Arc::new(move |exchange: &str| b.feed_refusal(exchange))
    };
    ctx.bridge.set_exchange_refusal(refusal);

    // A feed that cannot stream makes feed subscribes say why (WS-01).
    match broker.create_feed(&auth) {
        Ok(feed) => {
            let more: FeedFactory = {
                let (b, auth) = (broker.clone(), auth.clone());
                Arc::new(move || b.create_feed(&auth).ok())
            };
            let limits = limits::pool_limits(id);
            ctx.websocket.set_refusal(Some(feed_refusal(&broker)));
            ctx.websocket.set_presubscribe(broker.feed_presubscribe());
            if let Err(e) = ctx.websocket.connect_pool(feed, more, limits).await {
                tracing::error!("Market data feed for {} did not start: {}", id, e);
                ctx.bridge.set_unavailable(bridge::FEED_NOT_STARTED);
            }
        }
        Err(AppError::Unsupported(_)) => {
            tracing::info!("{} has no live market data feed in OpenAlgo Desktop", id);
            ctx.bridge.set_unavailable(bridge::NO_LIVE_FEED);
        }
        Err(e) => {
            tracing::warn!("Market data feed for {} not started: {}", id, e);
            ctx.bridge.set_unavailable(bridge::FEED_NOT_STARTED);
        }
    }

    start_deep_feed(&ctx, &broker, &auth).await;
    // Feed clients that were already subscribed are applied now that the
    // master and the sockets are in place. Without a master they wait: a
    // later download or cache reload resyncs them.
    if master_ready {
        ctx.bridge.resync();
    }

    let source = match broker.create_order_feed(&auth) {
        Ok(OrderFeed::Socket(feed)) => match ctx.runtime.order_ws.connect(feed).await {
            Ok(()) => OrderSource::Socket,
            Err(e) => {
                tracing::warn!("Order updates for {} not started: {}", id, e);
                OrderSource::NotStarted
            }
        },
        Ok(OrderFeed::Stream(rx)) => {
            let bus = ctx.bus.clone();
            BrokerRuntime::spawn(&tasks, relay_stream_orders(rx, bus, id, generation));
            OrderSource::Adapter
        }
        Err(AppError::Unsupported(_)) if broker.capabilities().order_feed => {
            OrderSource::MarketFeed
        }
        Err(AppError::Unsupported(_)) => OrderSource::None,
        Err(e) => {
            tracing::warn!("Order updates for {} not started: {}", id, e);
            OrderSource::NotStarted
        }
    };
    *ctx.runtime.order_source.lock() = source;
    tracing::info!("Broker session for {} is streaming", id);
}

/// The broker's deeper-book socket (Fyers 50, Dhan 20), when it streams
/// deeper books than its market feed. One that does not start serves those
/// depths at the market feed's levels, acknowledged as the broker's web
/// adapter does (`feed::bridge::deep`).
async fn start_deep_feed(ctx: &AppState, broker: &Arc<dyn Broker>, auth: &AuthToken) {
    let id = broker.id();
    let Some(levels) = broker
        .capabilities()
        .depth_levels
        .iter()
        .copied()
        .find(|l| *l > 5)
    else {
        return;
    };
    ctx.runtime
        .depth_ws
        .set_room_refusal(limits::depth_room_refusal(id));
    let started = match broker.create_depth_feed(auth, levels) {
        Ok(feed) => ctx
            .runtime
            .depth_ws
            .connect_pool(
                feed,
                Arc::new(|| None::<Box<dyn BrokerFeed>>),
                limits::depth_limits(id),
            )
            .await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match started {
        Ok(()) => {
            ctx.bridge.set_deep_levels(Some(levels));
            ctx.bridge
                .set_deep_book_timeout(limits::deep_book_timeout(id));
        }
        Err(e) => {
            tracing::warn!("Market depth feed for {} not started: {}", id, e);
            ctx.bridge.set_deep_unavailable(levels);
        }
    }
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

/// What the session's broker refuses on its feed, for every consumer of it
/// (Flow and the sandbox as well as feed clients): the mode or depth its
/// web adapter refuses (`feed::refusals`), else the exchange
/// (`Broker::feed_refusal`).
fn feed_refusal(broker: &Arc<dyn Broker>) -> FeedRefusal {
    let b = broker.clone();
    Arc::new(move |s: &FeedSubscription| {
        let key = InstrumentKey::new(s.symbol.clone(), s.exchange.clone());
        let mode = match s.mode {
            FeedMode::Ltp => Mode::Ltp,
            FeedMode::Quote => Mode::Quote,
            FeedMode::Depth => Mode::Depth,
        };
        crate::feed::refusals::refusal(b.id(), &key, mode, i64::from(s.depth))
            .or_else(|| b.feed_refusal(&s.exchange))
    })
}

/// Longest an order update waits for the broker to complete it
/// (`Broker::complete_order_update`) before it is published as it came.
const ORDER_COMPLETION_DEADLINE: Duration = Duration::from_secs(5);

/// `update` as the broker completed it with the session's `auth`, or as it
/// came when the broker did not answer within the deadline (bk-b RB-11).
async fn completed(broker: &dyn Broker, auth: &AuthToken, update: OrderUpdate) -> OrderUpdate {
    let asked = broker.complete_order_update(auth, update.clone());
    match tokio::time::timeout(ORDER_COMPLETION_DEADLINE, asked).await {
        Ok(done) => done,
        Err(_) => {
            tracing::warn!(
                broker = broker.id(),
                "The broker did not complete an order update in time; publishing it as it came"
            );
            update
        }
    }
}

async fn relay_socket_orders(
    mut rx: broadcast::Receiver<MarketEvent>,
    feed: Arc<WebSocketManager>,
    bus: Arc<EventBus>,
    (broker, auth, generation): (Arc<dyn Broker>, AuthToken, u64),
) {
    loop {
        match rx.recv().await {
            Ok(ev) => {
                if let FeedEvent::OrderUpdate(u) = &*ev {
                    let u = completed(&*broker, &auth, u.clone()).await;
                    bus.publish(order_event(&u, broker.id(), generation));
                }
            }
            Err(RecvError::Lagged(n)) => {
                // Skipped events may include order updates: the order
                // reconciler repairs from the broker's book (ARCH-01).
                tracing::warn!("Order update relay fell behind; skipped {} events", n);
                bus.report_relay_lag(n);
                feed.stats()
                    .note_skipped(crate::websocket::Reader::OrderRelay, n);
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
