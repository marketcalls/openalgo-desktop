//! The order stream of a broker whose market feed is its session's only
//! socket (`Broker::single_session_stream`: Flattrade, whose PiConnect
//! allows one socket per login, web issue #1806). Its order updates ride
//! that feed, so an update the broker pushed while the socket was down is
//! lost, and a socket that reconnects after the daily boundary does so with
//! a token the broker no longer honours. Web 73599f37c
//! (marketcalls/openalgo#2507, issue #2386) answers both, and so does this:
//!
//! * **catch-up** (`OrderCatchUp`, `catch_up_orders`): after each
//!   (re)connect of the feed, once the login is accepted and the order
//!   subscription has been written (`WebSocketManager::watch_ready`; the
//!   web reads after its `t=o` went out too), the order book is read once
//!   and every order whose state differs from the last known one is
//!   published as an order update; the first read only records (web
//!   `_catch_up`). An order's state is its status as the book and the push
//!   feed both report it (the book folds `trigger pending` into `open`, the
//!   push keeps it apart) and its filled quantity, kept from the reads and
//!   from every update the socket pushed, so an update already pushed is
//!   not published again (web `_remember`). A push that comes while a read
//!   is on its way is newer than that read's rows, so it is kept over its
//!   order's row, which publishes nothing: the web lets the row overwrite
//!   it (`known.update`), and so publishes the order's older state after
//!   the newer one and the newer one again at the next read. The first
//!   read keeps every push before it too (the web drops them, `_remember`),
//!   so the next read publishes what the book has moved past since.
//! * **rollover** (`SessionDay`): once the session's daily boundary has
//!   passed nothing is read, and the feed connects no more until the next
//!   sign-in (web `_login_this_trading_session`), the same rule
//!   `AppState::get_broker_session` applies to every other read.
//!
//! The web's other half, a relay between two processes and an order socket
//! of its own while no chart holds the market one, has no counterpart: the
//! desktop is one process, and its market socket is open for the whole
//! session. Hygiene: the state lives as long as the session (the runtime's
//! owned task holds it; teardown aborts it) and keeps one entry per order
//! of the session's day.

use super::order_event;
use crate::brokers::common::order_poll::to_update;
use crate::brokers::common::streaming::{ConnectGate, OrderUpdate};
use crate::brokers::types::{AuthToken, Order};
use crate::brokers::Broker;
use crate::clock::Clock;
use crate::error::Result;
use crate::events::EventBus;
use crate::session::boundary::is_fresh;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::watch;

#[cfg(test)]
mod tests;

/// An order's status as the book and the push feed agree on it, and its
/// filled quantity.
type State = (String, i64);

/// The order states a catch-up compares the book with.
#[derive(Debug, Default)]
pub struct OrderCatchUp {
    known: Mutex<Known>,
}

#[derive(Debug, Default)]
struct Known {
    /// Whether a read has recorded the book yet.
    read: bool,
    /// How many updates the socket has pushed: they are numbered from 1.
    pushes: u64,
    /// Order id to its last known state.
    states: HashMap<String, Told>,
}

/// An order's last known state, and the push that told it.
#[derive(Debug)]
struct Told {
    state: State,
    /// The push's number, 0 when a read of the order book told it.
    push: u64,
}

/// The pushes that came before a read of the order book was sent: one
/// numbered above it is newer than every row of that read.
#[derive(Debug, Clone, Copy)]
struct Issued(u64);

/// `status` as the order book and the push feed agree on it: the book
/// folds `trigger pending` into `open`, the push keeps it apart (web
/// `_comparable_status`).
fn comparable(status: &str) -> String {
    let s = status.trim().to_ascii_lowercase().replace('_', " ");
    if s == "trigger pending" {
        "open".to_string()
    } else {
        s
    }
}

fn state(o: &Order) -> State {
    (comparable(&o.status), i64::from(o.filled_quantity))
}

impl OrderCatchUp {
    /// An update the socket pushed: a later read showing the same state
    /// publishes nothing.
    pub fn remember(&self, u: &OrderUpdate) {
        if u.orderid.is_empty() {
            return;
        }
        let mut known = self.known.lock();
        known.pushes += 1;
        let told = Told {
            state: (comparable(&u.order_status), u.filled_quantity),
            push: known.pushes,
        };
        known.states.insert(u.orderid.clone(), told);
    }

    /// Taken as a read of the order book is sent.
    fn issued(&self) -> Issued {
        Issued(self.known.lock().pushes)
    }

    /// One read of the order book, sent at `issued`: the first only records
    /// it; a later one returns an update, with the prices the order book
    /// shows, for each order whose state differs from the last known one,
    /// in the book's order. An order listed twice counts once, as its last
    /// row. A row never overrides a push newer than the read (one after
    /// `issued`; for the first read, any): its order publishes nothing and
    /// keeps the pushed state, which the next read settles.
    fn changed(&self, book: &[Order], issued: Issued) -> Vec<OrderUpdate> {
        let mut known = self.known.lock();
        let first = !std::mem::replace(&mut known.read, true);
        let mut out = Vec::new();
        for o in last_rows(book) {
            let told = known.states.get(&o.order_id);
            if told.is_some_and(|t| first || t.push > issued.0) {
                continue;
            }
            let now = state(o);
            if !first && told.map(|t| &t.state) != Some(&now) {
                out.push(to_update(o));
            }
            let told = Told {
                state: now,
                push: 0,
            };
            known.states.insert(o.order_id.clone(), told);
        }
        out
    }
}

/// The book's rows with an order id, an order listed twice as its last
/// row, in the book's order.
fn last_rows(book: &[Order]) -> Vec<&Order> {
    let mut rows: Vec<&Order> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for o in book.iter().filter(|o| !o.order_id.is_empty()) {
        match at.get(o.order_id.as_str()) {
            Some(&i) => rows[i] = o,
            None => {
                at.insert(&o.order_id, rows.len());
                rows.push(o);
            }
        }
    }
    rows
}

/// The trading day of a session authenticated at `at`: current until the
/// next daily boundary (`hour`:`minute` IST) passes.
#[derive(Clone)]
pub struct SessionDay {
    clock: Arc<dyn Clock>,
    at: DateTime<Utc>,
    hour: u32,
    minute: u32,
}

impl SessionDay {
    pub fn new(clock: Arc<dyn Clock>, at: DateTime<Utc>, hour: u32, minute: u32) -> Self {
        Self {
            clock,
            at,
            hour,
            minute,
        }
    }

    /// Whether the session's daily boundary has not passed yet.
    pub fn current(&self) -> bool {
        is_fresh(self.at, self.clock.now(), self.hour, self.minute)
    }

    /// The feed's connect gate: no connect once the day has ended, with
    /// what the trader does about it.
    pub fn gate(self, broker: &'static str) -> ConnectGate {
        Arc::new(move || {
            (!self.current()).then(|| {
                format!(
                    "Your {} session ended at the daily session boundary. Log in to {} again.",
                    broker, broker
                )
            })
        })
    }
}

/// The session a catch-up reads the order book of and publishes for.
pub struct CatchUpSession {
    pub broker: Arc<dyn Broker>,
    pub auth: AuthToken,
    pub bus: Arc<EventBus>,
    /// The session's generation, stamped on every update (EV-02).
    pub generation: u64,
    /// False once the session's daily boundary has passed: nothing is read.
    pub current: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Read the session's order book once after each (re)connect of its market
/// feed and publish what changed (see the module docs). `ready` is the
/// feed's (`WebSocketManager::watch_ready`), subscribed before the feed
/// starts so its first session is seen. Runs until the feed's manager
/// goes; the runtime owns the task and aborts it at teardown. Public so
/// the feed corpus runs it as a session does.
pub async fn catch_up_orders(
    state: Arc<OrderCatchUp>,
    ready: watch::Receiver<u64>,
    session: CatchUpSession,
) {
    let CatchUpSession {
        broker,
        auth,
        bus,
        generation,
        current,
    } = session;
    let id = broker.id();
    let read = || {
        let (broker, auth) = (broker.clone(), auth.clone());
        async move { broker.get_order_book(&auth).await }
    };
    let publish = |u: OrderUpdate| {
        bus.publish(order_event(&u, id, generation));
    };
    run(&state, ready, id, (read, || current(), publish)).await;
}

/// The loop of `catch_up_orders`: `read` the book after each session that
/// `ready` announces while `current` says so, and `publish` each change.
/// Sessions announced while a read is on its way are read once after it.
async fn run<R, F, C, P>(
    state: &OrderCatchUp,
    mut ready: watch::Receiver<u64>,
    broker: &'static str,
    (read, current, publish): (R, C, P),
) where
    R: Fn() -> F,
    F: Future<Output = Result<Vec<Order>>>,
    C: Fn() -> bool,
    P: Fn(OrderUpdate),
{
    loop {
        // Marks every session announced so far as seen.
        if ready.changed().await.is_err() {
            return;
        }
        if !current() {
            tracing::info!(
                broker,
                "Order book not read after the reconnect: the session ended at the daily boundary"
            );
            continue;
        }
        let issued = state.issued();
        match read().await {
            Ok(book) => {
                for u in state.changed(&book, issued) {
                    publish(u);
                }
            }
            Err(e) => tracing::warn!(
                broker,
                "Order updates missed while the market data connection was down could not be read from the order book: {}",
                e.code()
            ),
        }
    }
}
