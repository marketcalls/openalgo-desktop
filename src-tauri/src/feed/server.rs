//! The listener and per-connection protocol handling.
//!
//! Task ownership:
//! * the accept task owns a `JoinSet` of connection tasks; on stop it stops
//!   accepting (releasing the port), lets connections send a close frame,
//!   then aborts what is left;
//! * each connection task owns its writer task and aborts it on exit;
//! * the market and order dispatch loops are owned by the [`FeedHandle`].
//!
//! A connection's registry entry is removed by a drop guard, so source
//! subscriptions are released on every exit path, including an abrupt
//! disconnect or an aborted task.

use super::auth::{AuthOutcome, FeedAuth};
use super::orders::order_update_frame;
use super::outbox::{Outbox, Outgoing};
use super::protocol::{self as p, code, py_str, py_truthy, py_type_name, request_id};
use super::registry::{ClientId, Registry, RegistryStats};
use super::source::{InstrumentKey, MarketDataSource, MarketUpdate, Mode, DEFAULT_DEPTH};
use crate::events::OrderUpdate;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval_at, sleep_until, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_util::sync::CancellationToken;

/// Close code the web uses when a socket does not authenticate in time.
pub const AUTH_TIMEOUT_CODE: u16 = 4401;

/// Listener settings. Defaults match the web (`WS_AUTH_GRACE_SECONDS` 15,
/// `WS_PING_INTERVAL` / `WS_PING_TIMEOUT` 20, websockets `max_size` 1 MiB).
#[derive(Debug, Clone)]
pub struct FeedConfig {
    pub host: String,
    pub port: u16,
    pub auth_timeout: Duration,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub handshake_timeout: Duration,
    pub max_connections: usize,
    pub max_message_bytes: usize,
    /// Control frames (acks, errors, order updates) a client may leave
    /// unread before it is disconnected.
    pub control_queue_cap: usize,
    pub max_subscriptions_per_client: usize,
    /// Minimum spacing of market frames per `(instrument, mode)`. Zero
    /// forwards every tick (the web's current behaviour); otherwise the
    /// latest tick in each window is sent at the window's end.
    pub throttle: Duration,
    /// Which browser pages may open a connection (security review S-10).
    pub origins: OriginPolicy,
}

/// The pages allowed to open the feed from a browser: the app's own. A
/// client that is not a browser sends no `Origin` and is accepted exactly
/// as on the web; so is one whose `Origin` names the feed address itself
/// (Python `websocket-client`, which the SDK uses, sends that by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginPolicy {
    /// Ports the app's pages are served on (the HTTP port, and the Vite dev
    /// server in development builds).
    pub app_ports: Vec<u16>,
    /// LAN access is on: the app's page may be opened by IP address.
    pub lan: bool,
    /// The configured tunnel host, whose pages are the app's too.
    pub public_host: Option<String>,
}

impl Default for OriginPolicy {
    fn default() -> Self {
        Self {
            app_ports: vec![crate::config::DEFAULT_HTTP_PORT],
            lan: false,
            public_host: None,
        }
    }
}

impl OriginPolicy {
    /// Whether a handshake with this `Origin` (and `Host`) is accepted.
    pub fn allows(&self, origin: Option<&str>, host: Option<&str>) -> bool {
        let Some(origin) = origin.map(str::trim) else {
            return true;
        };
        let Ok(url) = url::Url::parse(origin) else {
            return false;
        };
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(name) = url
            .host_str()
            .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
        else {
            return false;
        };
        let port = url.port_or_known_default();
        // A non-browser client naming the address it connects to.
        if let Some(host) = host {
            let authority = match url.port() {
                Some(p) => format!("{}:{}", url.host_str().unwrap_or_default(), p),
                None => url.host_str().unwrap_or_default().to_string(),
            };
            if authority.eq_ignore_ascii_case(host.trim()) {
                return true;
            }
        }
        if self
            .public_host
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(&name))
        {
            return true;
        }
        let app_port = port.is_some_and(|p| self.app_ports.contains(&p));
        let loopback = matches!(name.as_str(), "127.0.0.1" | "localhost" | "::1");
        app_port && (loopback || (self.lan && name.parse::<std::net::IpAddr>().is_ok()))
    }
}

/// Open connections one address on the network may hold. Loopback is not
/// capped this way: every program on this computer shares 127.0.0.1.
pub const MAX_CONNECTIONS_PER_REMOTE_ADDRESS: usize = 32;

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: crate::config::DEFAULT_WS_PORT,
            auth_timeout: Duration::from_secs(15),
            ping_interval: Duration::from_secs(20),
            ping_timeout: Duration::from_secs(20),
            handshake_timeout: Duration::from_secs(10),
            max_connections: 256,
            max_message_bytes: 1024 * 1024,
            control_queue_cap: 1024,
            max_subscriptions_per_client: 3000,
            throttle: Duration::ZERO,
            origins: OriginPolicy::default(),
        }
    }
}

/// What the server is built from.
pub struct FeedDeps {
    pub source: Arc<dyn MarketDataSource>,
    pub auth: Arc<dyn FeedAuth>,
    /// Order updates to relay (`None`: no order stream, acks still work).
    pub orders: Option<broadcast::Receiver<Arc<OrderUpdate>>>,
    /// Names answered to `get_supported_brokers`.
    pub supported_brokers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    PortInUse { port: u16, message: String },
    Failed { message: String },
}

/// Trader-facing explanation for a taken feed port.
pub fn port_in_use_message(port: u16) -> String {
    format!(
        "Live market data for your programs could not start because port {} is already used by \
another program. Close the other program (for example OpenAlgo web or another copy of \
OpenAlgo), or choose a different WebSocket port in Settings, then restart the server.",
        port
    )
}

struct Shared {
    cfg: FeedConfig,
    registry: Registry,
    auth: Arc<dyn FeedAuth>,
    brokers: Vec<String>,
    next_id: AtomicU64,
    active: AtomicUsize,
    /// Open connections per non-loopback address.
    per_address: parking_lot::Mutex<HashMap<std::net::IpAddr, usize>>,
}

/// A running feed server.
pub struct FeedHandle {
    addr: SocketAddr,
    token: CancellationToken,
    accept: JoinHandle<()>,
    loops: JoinSet<()>,
    shared: Arc<Shared>,
}

impl FeedHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Open WebSocket connections.
    pub fn connections(&self) -> usize {
        self.shared.active.load(Ordering::Relaxed)
    }

    pub fn stats(&self) -> RegistryStats {
        self.shared.registry.stats()
    }

    pub fn queue_lengths(&self) -> Vec<usize> {
        self.shared.registry.queue_lengths()
    }

    /// Stop accepting, close every client (code 1001), release the port and
    /// every source subscription, and join all owned tasks.
    pub async fn stop(mut self) {
        self.token.cancel();
        if timeout(Duration::from_secs(5), &mut self.accept)
            .await
            .is_err()
        {
            tracing::warn!("Market data feed did not stop in time");
            self.accept.abort();
        }
        self.loops.abort_all();
        while self.loops.join_next().await.is_some() {}
    }
}

/// Bind `cfg.host:cfg.port` and serve.
pub async fn start(cfg: FeedConfig, deps: FeedDeps) -> Result<FeedHandle, StartError> {
    let host = match cfg.host.as_str() {
        "localhost" | "::1" | "" => "127.0.0.1".to_string(),
        h => h.to_string(),
    };
    let addr: SocketAddr =
        format!("{}:{}", host, cfg.port)
            .parse()
            .map_err(|_| StartError::Failed {
                message: "The WebSocket address in Settings is not valid. Use 127.0.0.1.".into(),
            })?;
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::debug!("WebSocket port {} is already in use", cfg.port);
            return Err(StartError::PortInUse {
                port: cfg.port,
                message: port_in_use_message(cfg.port),
            });
        }
        Err(e) => {
            tracing::debug!("Could not bind the market data feed on {}: {}", addr, e);
            return Err(StartError::Failed {
                message: format!(
                    "Live market data could not open port {}. Choose a different WebSocket port \
in Settings and restart the server.",
                    cfg.port
                ),
            });
        }
    };
    let addr = listener.local_addr().unwrap_or(addr);
    let updates = deps.source.updates();
    let shared = Arc::new(Shared {
        registry: Registry::new(deps.source, cfg.max_subscriptions_per_client),
        auth: deps.auth,
        brokers: deps.supported_brokers,
        next_id: AtomicU64::new(1),
        active: AtomicUsize::new(0),
        per_address: parking_lot::Mutex::new(HashMap::new()),
        cfg,
    });
    let token = CancellationToken::new();
    let mut loops = JoinSet::new();
    loops.spawn(market_loop(shared.clone(), updates));
    if let Some(rx) = deps.orders {
        loops.spawn(order_loop(shared.clone(), rx));
    }
    let accept = tokio::spawn(accept_loop(listener, shared.clone(), token.clone()));
    tracing::info!("Market data feed listening on ws://{}", addr);
    Ok(FeedHandle {
        addr,
        token,
        accept,
        loops,
        shared,
    })
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>, token: CancellationToken) {
    let mut conns: JoinSet<()> = JoinSet::new();
    let rejecting = Arc::new(AtomicUsize::new(0));
    loop {
        while conns.try_join_next().is_some() {}
        tokio::select! {
            _ = token.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(a) => a,
                    Err(e) => {
                        // Out of descriptors and similar: back off instead of spinning.
                        tracing::warn!("Market data feed accept failed: {}", e);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                if shared.active.load(Ordering::Relaxed) >= shared.cfg.max_connections {
                    if rejecting.load(Ordering::Relaxed) < 16 {
                        rejecting.fetch_add(1, Ordering::Relaxed);
                        let r = rejecting.clone();
                        let cfg = shared.cfg.clone();
                        conns.spawn(async move {
                            reject(stream, &cfg).await;
                            r.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    continue;
                }
                let remote = (!peer.ip().is_loopback()).then_some(peer.ip());
                if let Some(ip) = remote {
                    let mut m = shared.per_address.lock();
                    let n = m.entry(ip).or_insert(0);
                    if *n >= MAX_CONNECTIONS_PER_REMOTE_ADDRESS {
                        drop(m);
                        tracing::warn!("Market data feed: one network address opened too many connections");
                        drop(stream);
                        continue;
                    }
                    *n += 1;
                }
                shared.active.fetch_add(1, Ordering::Relaxed);
                conns.spawn(serve_conn(stream, remote, shared.clone(), token.clone()));
            }
        }
    }
    drop(listener);
    let drained = timeout(Duration::from_secs(3), async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        conns.abort_all();
        while conns.join_next().await.is_some() {}
    }
}

/// tungstenite stops reading at its own size limit, which leaves the rest of
/// the oversized payload unread in the socket; closing such a socket makes
/// Windows send a reset that discards our close frame. So tungstenite accepts
/// up to twice our limit and the read loop enforces the real one on a message
/// it has consumed in full, which closes cleanly with 1009 everywhere.
fn transport_limit(cfg: &FeedConfig) -> usize {
    cfg.max_message_bytes
        .saturating_mul(2)
        .max(cfg.max_message_bytes.saturating_add(64 * 1024))
}

fn ws_config(cfg: &FeedConfig) -> WebSocketConfig {
    WebSocketConfig {
        max_message_size: Some(transport_limit(cfg)),
        max_frame_size: Some(transport_limit(cfg)),
        max_write_buffer_size: cfg.max_message_bytes.max(64 * 1024) * 4,
        ..Default::default()
    }
}

async fn reject(stream: TcpStream, cfg: &FeedConfig) {
    if let Ok(Ok(mut ws)) = timeout(
        cfg.handshake_timeout,
        tokio_tungstenite::accept_async_with_config(stream, Some(ws_config(cfg))),
    )
    .await
    {
        let _ = timeout(
            Duration::from_secs(2),
            ws.close(Some(CloseFrame {
                code: CloseCode::Again,
                reason: "Too many connections".into(),
            })),
        )
        .await;
    }
}

/// Removes the client from the registry (releasing its source keys) and
/// decrements the connection count, whatever ends the connection.
struct ClientGuard {
    shared: Arc<Shared>,
    id: ClientId,
    remote: Option<std::net::IpAddr>,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.shared.registry.remove_client(self.id);
        self.shared.active.fetch_sub(1, Ordering::Relaxed);
        if let Some(ip) = self.remote {
            let mut m = self.shared.per_address.lock();
            if let Some(n) = m.get_mut(&ip) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.remove(&ip);
                }
            }
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

async fn serve_conn(
    stream: TcpStream,
    remote: Option<std::net::IpAddr>,
    shared: Arc<Shared>,
    token: CancellationToken,
) {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let outbox = Arc::new(Outbox::new(
        shared.cfg.control_queue_cap,
        shared.cfg.max_subscriptions_per_client * 3,
    ));
    shared.registry.add_client(id, outbox.clone());
    let _guard = ClientGuard {
        shared: shared.clone(),
        id,
        remote,
    };
    let origins = shared.cfg.origins.clone();
    let check_origin =
        move |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
              resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let get = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok());
            if origins.allows(get("origin"), get("host")) {
                Ok(resp)
            } else {
                tracing::warn!("Market data feed refused a web page from another site");
                let mut refused =
                    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(
                        "Request blocked.".into(),
                    ));
                *refused.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                Err(refused)
            }
        };
    let ws = match timeout(
        shared.cfg.handshake_timeout,
        tokio_tungstenite::accept_hdr_async_with_config(
            stream,
            check_origin,
            Some(ws_config(&shared.cfg)),
        ),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        _ => return,
    };
    let (sink, mut stream) = ws.split();
    let mut writer = AbortOnDrop(tokio::spawn(write_loop(sink, outbox.clone())));
    let mut session = Session {
        id,
        shared: shared.clone(),
        outbox: outbox.clone(),
        user_id: None,
        broker: None,
    };

    let auth_deadline = sleep_until(Instant::now() + shared.cfg.auth_timeout);
    tokio::pin!(auth_deadline);
    let ping_every = shared.cfg.ping_interval;
    let mut ping = interval_at(Instant::now() + ping_every, ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    let mut writer_done = false;
    // Whether we sent a close frame and should wait for the peer's reply.
    let mut closing = false;

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                outbox.close(1001, "server shutting down");
                closing = true;
                break;
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
                if msg.len() > shared.cfg.max_message_bytes {
                    // Over the message size limit: close 1009 as the web does.
                    outbox.close(1009, "message too big");
                    closing = true;
                    break;
                }
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
            }
        }
    }

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

/// One client's protocol state.
struct Session {
    id: ClientId,
    shared: Arc<Shared>,
    outbox: Arc<Outbox>,
    user_id: Option<String>,
    broker: Option<String>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Python `a or b` on two optional JSON fields.
fn or_field<'a>(data: &'a Value, a: &str, b: &str) -> &'a Value {
    match data.get(a) {
        Some(v) if py_truthy(v) => v,
        _ => data.get(b).unwrap_or(&Value::Null),
    }
}

/// What Python raises when a handler calls `.get` on, or iterates, a
/// non-dict element.
fn not_a_dict(v: &Value) -> String {
    format!("'{}' object has no attribute 'get'", py_type_name(v))
}

/// Python `for x in value` over a truthy non-list: the items (or the error).
fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        other => Err(format!("'{}' object is not iterable", py_type_name(other))),
    }
}

impl Session {
    fn send(&self, frame: String) {
        self.outbox.push_control(frame);
    }

    fn error(&self, code: &str, message: &str, rid: Option<&Value>) {
        self.send(p::error_frame(code, message, rid));
    }

    async fn handle(&mut self, text: &str) {
        let data: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return self.error(code::INVALID_JSON, "Invalid JSON message", None),
        };
        if !data.is_object() {
            return self.error(code::SERVER_ERROR, &not_a_dict(&data), None);
        }
        let action = or_field(&data, "action", "type");
        let result = match action.as_str() {
            Some("authenticate") | Some("auth") => {
                self.authenticate(&data).await;
                Ok(())
            }
            Some("subscribe") => self.subscribe(&data),
            Some("unsubscribe") | Some("unsubscribe_all") => self.unsubscribe(&data),
            Some("subscribe_orders") => {
                self.orders(&data, true);
                Ok(())
            }
            Some("unsubscribe_orders") => {
                self.orders(&data, false);
                Ok(())
            }
            Some("get_broker_info") => {
                self.broker_info();
                Ok(())
            }
            Some("get_supported_brokers") => {
                self.send(p::to_json(&p::SupportedBrokers {
                    kind: "supported_brokers",
                    status: "success",
                    brokers: &self.shared.brokers,
                    count: self.shared.brokers.len(),
                }));
                Ok(())
            }
            Some("ping") => {
                self.send(p::pong(&data, now_ms()));
                Ok(())
            }
            _ => {
                let msg = format!("Invalid action: {}", py_str(action));
                self.error(code::INVALID_ACTION, &msg, None);
                Ok(())
            }
        };
        if let Err(msg) = result {
            self.error(code::SERVER_ERROR, &msg, None);
        }
    }

    async fn authenticate(&mut self, data: &Value) {
        let key = or_field(data, "api_key", "apikey");
        if !py_truthy(key) {
            return self.error(code::AUTHENTICATION_ERROR, "API key is required", None);
        }
        let outcome = match key.as_str() {
            Some(k) => self.shared.auth.authenticate(k).await,
            None => AuthOutcome::Invalid,
        };
        match outcome {
            AuthOutcome::Invalid => self.error(code::AUTHENTICATION_ERROR, "Invalid API key", None),
            AuthOutcome::NoBroker { user_id } => {
                // The web records the user before the broker check, so the
                // socket counts as authenticated from here on.
                self.user_id = Some(user_id);
                self.broker = None;
                self.error(
                    code::BROKER_ERROR,
                    "No broker configuration found for user",
                    None,
                );
            }
            AuthOutcome::Ok { user_id, broker } => {
                self.shared
                    .registry
                    .set_identity(self.id, &user_id, &broker);
                self.send(p::auth_ack(&broker, &user_id));
                self.user_id = Some(user_id);
                self.broker = Some(broker);
            }
        }
    }

    fn subscribe(&mut self, data: &Value) -> Result<(), String> {
        let rid = request_id(data);
        if self.user_id.is_none() {
            self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
            return Ok(());
        }
        let symbols_v = data.get("symbols").unwrap_or(&Value::Null);
        let default_mode = Value::String("Quote".into());
        let raw_mode = data.get("mode").unwrap_or(&default_mode);
        let depth_v = match data.get("depth") {
            Some(v) if !v.is_null() => Some(v),
            _ => data.get("depth_level").filter(|v| !v.is_null()),
        };
        let mode = match p::normalize_mode(raw_mode) {
            Ok(m) => m,
            Err(e) => {
                self.error(code::INVALID_MODE, &e, rid);
                return Ok(());
            }
        };
        let requested_depth: i64 = match depth_v {
            None => DEFAULT_DEPTH as i64,
            Some(v) => match v.as_i64() {
                Some(d) if !v.is_f64() && d > 0 => d,
                _ => {
                    self.error(
                        code::INVALID_PARAMETERS,
                        "Depth must be one of 5, 20, 30 or 50",
                        rid,
                    );
                    return Ok(());
                }
            },
        };
        let mut items: Vec<Value> = if py_truthy(symbols_v) {
            py_iter(symbols_v)?
        } else {
            Vec::new()
        };
        if items.is_empty() {
            let s = data.get("symbol").unwrap_or(&Value::Null);
            let e = data.get("exchange").unwrap_or(&Value::Null);
            if py_truthy(s) && py_truthy(e) {
                items.push(serde_json::json!({"symbol": s, "exchange": e}));
            }
        }
        if items.is_empty() {
            self.error(
                code::INVALID_PARAMETERS,
                "At least one symbol must be specified",
                rid,
            );
            return Ok(());
        }
        let Some(broker) = self.broker.clone() else {
            self.error(code::BROKER_ERROR, "Broker adapter not found", rid);
            return Ok(());
        };

        let source = self.shared.registry.source().clone();
        let mut results = Vec::with_capacity(items.len());
        let mut all_ok = true;
        for item in &items {
            if !item.is_object() {
                return Err(not_a_dict(item));
            }
            let sym = item.get("symbol").unwrap_or(&Value::Null);
            let exch = item.get("exchange").unwrap_or(&Value::Null);
            if !py_truthy(sym) || !py_truthy(exch) {
                continue;
            }
            let key = InstrumentKey::new(py_str(sym), py_str(exch));
            let outcome: Result<i64, String> = if !source.resolve(&key) {
                Err(format!(
                    "Token not found for {} on {}",
                    key.symbol, key.exchange
                ))
            } else if mode == Mode::Depth {
                let supported = source.supported_depths(&key.exchange);
                let actual = if supported.iter().any(|d| *d as i64 == requested_depth) {
                    Some(requested_depth as u8)
                } else {
                    supported
                        .iter()
                        .copied()
                        .filter(|d| (*d as i64) <= requested_depth)
                        .max()
                };
                match actual {
                    Some(d) => self
                        .shared
                        .registry
                        .subscribe(self.id, &key, mode, d)
                        .map(|_| d as i64),
                    None => Err(format!(
                        "Depth level {} is not supported by this broker",
                        requested_depth
                    )),
                }
            } else {
                self.shared
                    .registry
                    .subscribe(self.id, &key, mode, DEFAULT_DEPTH)
                    .map(|_| requested_depth)
            };
            results.push(match outcome {
                Ok(depth) => p::SubscribeItem::Ok {
                    symbol: sym.clone(),
                    exchange: exch.clone(),
                    status: "success",
                    mode: mode.label(),
                    depth,
                    broker: broker.clone(),
                },
                Err(message) => {
                    all_ok = false;
                    p::SubscribeItem::Err {
                        symbol: sym.clone(),
                        exchange: exch.clone(),
                        status: "error",
                        message,
                        broker: broker.clone(),
                    }
                }
            });
        }
        self.send(p::to_json(&p::SubscribeAck {
            kind: "subscribe",
            status: if all_ok { "success" } else { "partial" },
            subscriptions: results,
            message: "Subscription processing complete",
            broker: &broker,
            request_id: rid,
        }));
        Ok(())
    }

    fn unsubscribe(&mut self, data: &Value) -> Result<(), String> {
        let rid = request_id(data);
        if self.user_id.is_none() {
            self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
            return Ok(());
        }
        let is_all = data.get("type").and_then(Value::as_str) == Some("unsubscribe_all")
            || data.get("action").and_then(Value::as_str) == Some("unsubscribe_all");
        let symbols_v = data.get("symbols").unwrap_or(&Value::Null);
        let mut items: Vec<Value> = if py_truthy(symbols_v) {
            py_iter(symbols_v)?
        } else {
            Vec::new()
        };
        let default_mode = Value::from(2);
        let top_mode = data.get("mode").unwrap_or(&default_mode);
        if items.is_empty() && !is_all {
            let s = data.get("symbol").unwrap_or(&Value::Null);
            let e = data.get("exchange").unwrap_or(&Value::Null);
            if py_truthy(s) && py_truthy(e) {
                let m = match p::normalize_mode(top_mode) {
                    Ok(m) => m,
                    Err(err) => {
                        self.error(code::INVALID_MODE, &err, rid);
                        return Ok(());
                    }
                };
                items.push(serde_json::json!({"symbol": s, "exchange": e, "mode": m.as_u8()}));
            }
        }
        if items.is_empty() && !is_all {
            self.error(
                code::INVALID_PARAMETERS,
                "Either symbols or unsubscribe_all is required",
                rid,
            );
            return Ok(());
        }
        let Some(broker) = self.broker.clone() else {
            self.error(code::BROKER_ERROR, "Broker adapter not found", rid);
            return Ok(());
        };

        let mut ok = Vec::new();
        let mut failed = Vec::new();
        let item = |sym: Value, exch: Value, mode: Mode| p::UnsubscribeItem {
            symbol: sym,
            exchange: exch,
            mode: Some(mode.label()),
            status: "success",
            message: None,
            broker: broker.clone(),
        };
        if is_all {
            for (key, mode) in self.shared.registry.unsubscribe_all(self.id) {
                let sym = Value::String(key.symbol.clone());
                let exch = Value::String(key.exchange.clone());
                ok.push(item(sym, exch, mode));
            }
        } else {
            for it in &items {
                let raw_mode = match it {
                    Value::Object(o) => o.get("mode").unwrap_or(top_mode),
                    Value::Array(_) | Value::String(_) => top_mode,
                    other => {
                        return Err(format!(
                            "argument of type '{}' is not iterable",
                            py_type_name(other)
                        ))
                    }
                };
                if !it.is_object() {
                    // `"mode" in item` worked on a str/list; `.get` does not.
                    return Err(not_a_dict(it));
                }
                let sym = it.get("symbol").cloned().unwrap_or(Value::Null);
                let exch = it.get("exchange").cloned().unwrap_or(Value::Null);
                let mode = match p::normalize_mode(raw_mode) {
                    Ok(m) => m,
                    Err(e) => {
                        failed.push(p::UnsubscribeItem {
                            symbol: sym,
                            exchange: exch,
                            mode: None,
                            status: "error",
                            message: Some(e),
                            broker: broker.clone(),
                        });
                        continue;
                    }
                };
                if !py_truthy(&sym) || !py_truthy(&exch) {
                    continue;
                }
                let key = InstrumentKey::new(py_str(&sym), py_str(&exch));
                self.shared.registry.unsubscribe(self.id, &key, mode);
                ok.push(item(sym, exch, mode));
            }
        }
        let status = match (ok.is_empty(), failed.is_empty()) {
            (_, true) => "success",
            (false, false) => "partial",
            (true, false) => "error",
        };
        self.send(p::to_json(&p::UnsubscribeAck {
            kind: "unsubscribe",
            status,
            message: "Unsubscription processing complete",
            successful: ok,
            failed,
            broker: &broker,
            request_id: rid,
        }));
        Ok(())
    }

    fn orders(&mut self, data: &Value, on: bool) {
        let rid = request_id(data);
        if self.user_id.is_none() {
            return self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, rid);
        }
        self.shared.registry.set_orders(self.id, on);
        self.send(p::to_json(&p::SimpleAck {
            kind: if on {
                "subscribe_orders"
            } else {
                "unsubscribe_orders"
            },
            status: "success",
            message: if on {
                "Subscribed to order updates"
            } else {
                "Unsubscribed from order updates"
            },
            request_id: rid,
        }));
    }

    fn broker_info(&self) {
        if self.user_id.is_none() {
            return self.error(code::NOT_AUTHENTICATED, p::NOT_AUTHENTICATED_MSG, None);
        }
        let Some(broker) = self.broker.clone() else {
            return self.error(code::BROKER_ERROR, "Broker information not available", None);
        };
        let user = self.user_id.clone().unwrap_or_default();
        self.send(p::to_json(&p::BrokerInfo {
            kind: "broker_info",
            status: "success",
            broker: &broker,
            adapter_status: self.shared.auth.adapter_status(),
            user_id: &user,
        }));
    }
}

/// Source updates to subscribers. With a throttle, the latest update per
/// `(instrument, mode)` is held and flushed once per window; the pending map
/// is bounded by the number of keys the source streams.
async fn market_loop(shared: Arc<Shared>, mut rx: broadcast::Receiver<Arc<MarketUpdate>>) {
    let throttle = shared.cfg.throttle;
    let mut lagged: u64 = 0;
    let mut note_lag = |n: u64| {
        lagged += n;
        tracing::warn!(
            "Market data feed fell behind the broker stream; skipped {} updates ({} total)",
            n,
            lagged
        );
    };
    if throttle.is_zero() {
        loop {
            match rx.recv().await {
                Ok(u) => shared.registry.dispatch(&u),
                Err(RecvError::Lagged(n)) => note_lag(n),
                Err(RecvError::Closed) => return,
            }
        }
    }
    let mut pending: HashMap<(InstrumentKey, Mode), Arc<MarketUpdate>> = HashMap::new();
    let mut tick = tokio::time::interval(throttle);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Ok(u) => {
                    pending.insert((u.key.clone(), u.mode), u);
                }
                Err(RecvError::Lagged(n)) => note_lag(n),
                Err(RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                for (_, u) in pending.drain() {
                    shared.registry.dispatch(&u);
                }
            }
        }
    }
}

async fn order_loop(shared: Arc<Shared>, mut rx: broadcast::Receiver<Arc<OrderUpdate>>) {
    loop {
        match rx.recv().await {
            Ok(u) => {
                shared
                    .registry
                    .broadcast_orders(|user_id| order_update_frame(&u, user_id));
            }
            Err(RecvError::Lagged(n)) => {
                tracing::warn!("Order update stream fell behind; skipped {} updates", n)
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod origin_tests {
    use super::OriginPolicy;

    #[test]
    fn only_the_apps_own_pages_and_non_browsers_are_let_in() {
        let p = OriginPolicy {
            app_ports: vec![5000],
            lan: false,
            public_host: Some("abc.ngrok.app".into()),
        };
        assert!(p.allows(None, Some("127.0.0.1:8765")));
        assert!(p.allows(Some("http://127.0.0.1:5000"), Some("127.0.0.1:8765")));
        assert!(p.allows(Some("http://localhost:5000"), None));
        assert!(p.allows(Some("https://abc.ngrok.app"), None));
        assert!(p.allows(Some("http://127.0.0.1:8765"), Some("127.0.0.1:8765")));
        assert!(!p.allows(Some("http://127.0.0.1:3000"), Some("127.0.0.1:8765")));
        assert!(!p.allows(Some("https://evil.example"), Some("127.0.0.1:8765")));
        assert!(!p.allows(Some("null"), Some("127.0.0.1:8765")));
        assert!(!p.allows(Some("http://192.168.1.5:5000"), Some("127.0.0.1:8765")));
        let lan = OriginPolicy { lan: true, ..p };
        assert!(lan.allows(Some("http://192.168.1.5:5000"), Some("192.168.1.5:8765")));
        assert!(!lan.allows(Some("http://evil.example:5000"), Some("192.168.1.5:8765")));
    }
}
