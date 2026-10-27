//! One client connection: the handshake, its place in a connection pool, the
//! session loop and the close (web `websocket_proxy/server.py`).

use super::budget::Meter;
use super::session::Session;
use super::{
    credentials_moved, handshake_source, release_connection, ws_config, Shared, AUTH_TIMEOUT_CODE,
    CREDENTIALS_CHANGED_MSG,
};
use crate::feed::outbox::{Outbox, Outgoing};
use crate::feed::protocol::code;
use crate::feed::registry::ClientId;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio::time::{interval_at, sleep_until, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;
use tokio_util::sync::CancellationToken;

/// Which connection pool a connection holds a place in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pool {
    /// A loopback connection in its handshake.
    Pending,
    Local,
    Network,
    /// None (refused after the handshake).
    Released,
}

/// Removes the client from the registry (releasing its source keys) and
/// decrements the connection count, whatever ends the connection.
struct ClientGuard {
    shared: Arc<Shared>,
    id: ClientId,
    pool: Pool,
    remote: Option<std::net::IpAddr>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.shared.registry.remove_client(self.id);
        self.shared.active.fetch_sub(1, Ordering::Relaxed);
        let pool = match self.pool {
            Pool::Pending => Some(&self.shared.pending_loopback),
            Pool::Local => Some(&self.shared.local),
            Pool::Network => Some(&self.shared.network),
            Pool::Released => None,
        };
        if let Some(counter) = pool {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(ip) = self.remote {
            release_connection(&mut self.shared.per_address.lock(), ip);
        }
    }
}

/// Aborts the writer if the connection task ends first.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type WsSink =
    futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<TcpStream>, Message>;
type WsStream = futures_util::stream::SplitStream<WebSocketStream<TcpStream>>;

async fn write_loop(mut sink: WsSink, outbox: Arc<Outbox>) {
    while let Some(item) = outbox.next().await {
        let msg = match item {
            Outgoing::Text(s) => Message::Text(s),
            Outgoing::Ping => Message::Ping(Vec::new()),
            Outgoing::Close(code, reason) => {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    })))
                    .await;
                return;
            }
        };
        if sink.feed(msg).await.is_err() {
            return;
        }
        if outbox.is_empty() && sink.flush().await.is_err() {
            return;
        }
    }
}

/// The WebSocket handshake, within its deadline (shorter for a loopback
/// connection not yet placed in a pool) and under the origin and host
/// policy as the settings are now. Answers the socket and who is calling,
/// classified from the handshake exactly as an HTTP request is
/// (`handshake_source`).
async fn handshake(
    stream: TcpStream,
    peer: std::net::IpAddr,
    shared: &Shared,
    pending: bool,
) -> Option<(WebSocketStream<TcpStream>, crate::server::source::Source)> {
    // The policy as the settings are now, and the port this connection
    // came in on.
    let policy = (shared.cfg.handshake.0)();
    let ws_port = stream.local_addr().map(|a| a.port()).unwrap_or(0);
    let source = Arc::new(parking_lot::Mutex::new(
        crate::server::source::Source::Tunnel,
    ));
    let seen = source.clone();
    let check_origin = crate::websocket::handshake::check(
        move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let get = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok());
            *seen.lock() = handshake_source(peer, req.headers(), ws_port);
            if policy.allows(get("origin"), get("host"), ws_port) {
                Ok(resp)
            } else {
                tracing::warn!(
                    "Market data feed refused a connection from another site or host name"
                );
                let mut refused =
                    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(
                        "Request blocked.".into(),
                    ));
                *refused.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                Err(Box::new(refused))
            }
        },
    );
    let deadline = if pending {
        shared
            .cfg
            .loopback_handshake_timeout
            .min(shared.cfg.handshake_timeout)
    } else {
        shared.cfg.handshake_timeout
    };
    match timeout(
        deadline,
        tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            check_origin,
            Some(ws_config(&shared.cfg)),
        ),
    )
    .await
    {
        Ok(Ok(ws)) => {
            let source = *source.lock();
            Some((ws, source))
        }
        _ => None,
    }
}

/// A loopback connection takes its place in this computer's pool or,
/// through the tunnel, in the one every other caller shares, now that the
/// handshake told them apart. A full pool closes it at once (false).
async fn take_pool_place(
    shared: &Shared,
    guard: &mut ClientGuard,
    ws: &mut WebSocketStream<TcpStream>,
    source: crate::server::source::Source,
) -> bool {
    shared.pending_loopback.fetch_sub(1, Ordering::Relaxed);
    guard.pool = Pool::Released;
    let (counter, cap, pool) = if source == crate::server::source::Source::Local {
        (&shared.local, shared.cfg.local_connections, Pool::Local)
    } else {
        (&shared.network, shared.cfg.max_connections, Pool::Network)
    };
    let placed = counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < cap).then_some(n + 1)
        })
        .is_ok();
    if !placed {
        let _ = timeout(
            Duration::from_secs(2),
            ws.close(Some(CloseFrame {
                code: CloseCode::Again,
                reason: "Too many connections".into(),
            })),
        )
        .await;
        return false;
    }
    guard.pool = pool;
    true
}

/// One frame from the client: an oversized one closes with 1009, one over
/// the connection's budget with 1008 (RATE-01), both before it is parsed;
/// the rest are handled by the session. A client that authenticates while
/// the credentials change is closed. True when the connection is now
/// closing.
async fn on_frame(
    session: &mut Session,
    msg: Message,
    credentials: &mut Option<tokio::sync::watch::Receiver<u64>>,
    outbox: &Outbox,
    max_message_bytes: usize,
) -> bool {
    if msg.len() > max_message_bytes {
        // Over the message size limit: close 1009 as the web does.
        outbox.close(1009, "message too big");
        return true;
    }
    if !session.meter.admit(msg.len(), Instant::now()) {
        tracing::warn!(
            "Feed client {} sent more messages than it may; closing",
            session.id
        );
        outbox.close(1008, "too many messages");
        return true;
    }
    // What the credentials were before this message: a client
    // that authenticates while they change is closed, the rest
    // start from the value they authenticated under.
    let was_authenticated = session.user_id.is_some();
    let before = credentials.as_ref().map(|c| *c.borrow());
    match msg {
        Message::Text(t) => session.handle(&t).await,
        Message::Binary(b) => match String::from_utf8(b) {
            Ok(t) => session.handle(&t).await,
            Err(_) => session.error(code::INVALID_JSON, "Invalid JSON message", None),
        },
        // Pings are answered by tungstenite; a close is answered
        // on the next read, which then ends the stream.
        _ => {}
    }
    if !was_authenticated && session.user_id.is_some() {
        if let Some(c) = credentials.as_mut() {
            if Some(*c.borrow_and_update()) != before {
                session.error(code::AUTHENTICATION_ERROR, CREDENTIALS_CHANGED_MSG, None);
                session.close = Some((AUTH_TIMEOUT_CODE, "authentication expired"));
            }
        }
    }
    if let Some((code, reason)) = session.close.take() {
        outbox.close_after_control(code, reason);
        return true;
    }
    false
}

/// Gives the writer two seconds to finish, then, after a close frame was
/// sent, finishes the closing handshake.
async fn wind_down(
    mut writer: AbortOnDrop,
    writer_done: bool,
    closing: bool,
    stream: &mut WsStream,
) {
    if !writer_done
        && timeout(Duration::from_secs(2), &mut writer.0)
            .await
            .is_err()
    {
        writer.0.abort();
    }
    if closing {
        // Finish the closing handshake: keep reading, through errors, until
        // the peer answers our close frame or a second passes. After an
        // oversized message the rest of its payload is still unread, and
        // dropping a socket with unread data makes Windows send a reset that
        // discards our close frame (the client then sees 1006, not 1009).
        let _ = timeout(Duration::from_secs(1), async {
            while let Some(item) = stream.next().await {
                if let Err(WsError::ConnectionClosed | WsError::AlreadyClosed | WsError::Io(_)) =
                    item
                {
                    break;
                }
            }
        })
        .await;
    }
}

/// A new client: its id and outbox, registered, and the guard that
/// releases both and its place in a connection pool however the
/// connection ends. A loopback connection's pool is decided after the
/// handshake.
fn register(
    shared: &Arc<Shared>,
    peer: std::net::IpAddr,
    remote: Option<std::net::IpAddr>,
) -> (ClientId, Arc<Outbox>, ClientGuard) {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let outbox = Arc::new(
        Outbox::new(
            shared.cfg.control_queue_cap,
            shared.cfg.max_subscriptions_per_client * 3,
        )
        .with_totals(shared.losses.clone()),
    );
    shared.registry.add_client(id, outbox.clone());
    let guard = ClientGuard {
        shared: shared.clone(),
        id,
        pool: if crate::server::addr::canonical(peer).is_loopback() {
            Pool::Pending
        } else {
            Pool::Network
        },
        remote,
    };
    (id, outbox, guard)
}

/// The protocol session of a connection that has not authenticated yet.
fn new_session(
    id: ClientId,
    source: crate::server::source::Source,
    shared: &Arc<Shared>,
    outbox: &Arc<Outbox>,
) -> Session {
    Session {
        id,
        caller: source.ip(),
        close: None,
        closing_on_failure: false,
        shared: shared.clone(),
        outbox: outbox.clone(),
        user_id: None,
        broker: None,
        meter: Meter::new(shared.cfg.frame_budget, Instant::now()),
    }
}

pub(super) async fn serve_conn(
    stream: TcpStream,
    peer: std::net::IpAddr,
    remote: Option<std::net::IpAddr>,
    shared: Arc<Shared>,
    token: CancellationToken,
) {
    let (id, outbox, mut guard) = register(&shared, peer, remote);
    let Some((mut ws, source)) =
        handshake(stream, peer, &shared, guard.pool == Pool::Pending).await
    else {
        return;
    };
    if guard.pool == Pool::Pending && !take_pool_place(&shared, &mut guard, &mut ws, source).await {
        return;
    }
    let (sink, mut stream) = ws.split();
    let mut writer = AbortOnDrop(tokio::spawn(write_loop(sink, outbox.clone())));
    let mut session = new_session(id, source, &shared, &outbox);

    let auth_deadline = sleep_until(Instant::now() + shared.cfg.auth_timeout);
    tokio::pin!(auth_deadline);
    let ping_every = shared.cfg.ping_interval;
    let mut ping = interval_at(Instant::now() + ping_every, ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    let mut writer_done = false;
    // Whether we sent a close frame and should wait for the peer's reply.
    let mut closing = false;
    // Moves when what the API key reaches changes (sign-out, the broker
    // session ending or changing, a regenerated key): an authenticated
    // client is then closed and must connect and authenticate again.
    let mut credentials = shared.auth.credentials_changed();

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                outbox.close(1001, "server shutting down");
                closing = true;
                break;
            }
            moved = credentials_moved(&mut credentials), if session.user_id.is_some() => {
                if moved {
                    tracing::info!("Feed client {}: its authentication no longer holds; closing", id);
                    session.error(code::AUTHENTICATION_ERROR, CREDENTIALS_CHANGED_MSG, None);
                    outbox.close_after_control(AUTH_TIMEOUT_CODE, "authentication expired");
                    closing = true;
                    break;
                }
            }
            _ = outbox.overflowed() => {
                tracing::warn!("Feed client {} stopped reading; disconnecting it", id);
                outbox.close(1008, "client too slow");
                closing = true;
                break;
            }
            _ = &mut auth_deadline, if session.user_id.is_none() => {
                tracing::info!("Feed client {} did not authenticate in time; closing", id);
                outbox.close(AUTH_TIMEOUT_CODE, "auth timeout");
                closing = true;
                break;
            }
            _ = ping.tick() => {
                if last_seen.elapsed() > ping_every + shared.cfg.ping_timeout {
                    tracing::info!("Feed client {} stopped answering pings", id);
                    outbox.finish();
                    break;
                }
                outbox.push_ping();
            }
            _ = &mut writer.0, if !writer_done => {
                writer_done = true;
                break;
            }
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(WsError::Capacity(_))) => {
                        // Over the message size limit: close 1009 as the web does.
                        outbox.close(1009, "message too big");
                        closing = true;
                        break;
                    }
                    _ => {
                        outbox.finish();
                        break;
                    }
                };
                last_seen = Instant::now();
                let max = shared.cfg.max_message_bytes;
                if on_frame(&mut session, msg, &mut credentials, &outbox, max).await {
                    closing = true;
                    break;
                }
            }
        }
    }

    wind_down(writer, writer_done, closing, &mut stream).await;
}
