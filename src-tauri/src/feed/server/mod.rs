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
//!
//! A connection's protocol state and its message handlers are `session`.

use super::auth::FeedAuth;
use super::orders::order_update_frame;
use super::registry::{Registry, RegistryStats};
use super::source::{InstrumentKey, MarketDataSource, MarketUpdate, Mode};
use crate::events::OrderUpdate;
use conn::serve_conn;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{timeout, MissedTickBehavior};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_util::sync::CancellationToken;

mod budget;
mod conn;
mod session;

pub use budget::FrameBudget;

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
    /// The handshake deadline for loopback connections (this computer and
    /// the tunnel, not yet told apart): short, so a stalled handshake
    /// through the tunnel holds a place in their shared pending pool only
    /// briefly.
    pub loopback_handshake_timeout: Duration,
    pub max_connections: usize,
    /// Connections this computer's programs (the SDK, Amibroker, Excel) may
    /// hold, in a pool of their own: `max_connections` caps every other
    /// caller (devices on the network, the tunnel), so they can never fill
    /// it (availability guarantee G1).
    pub local_connections: usize,
    pub max_message_bytes: usize,
    /// Control frames (acks, errors, order updates) a client may leave
    /// unread before it is disconnected.
    pub control_queue_cap: usize,
    pub max_subscriptions_per_client: usize,
    /// Minimum spacing of market frames per `(instrument, mode)`. Zero
    /// forwards every tick (the web's current behaviour); otherwise the
    /// latest tick in each window is sent at the window's end.
    pub throttle: Duration,
    /// What one connection may send before it is closed (RATE-01).
    pub frame_budget: FrameBudget,
    /// Who may open a connection, read at each handshake (security
    /// review S-10).
    pub handshake: HandshakeGate,
}

/// Who may open the feed (security review S-10), worked out from the live
/// settings at every handshake ([`HandshakeGate`]).
///
/// * The `Host` of every upgrade must name the feed on this computer
///   (`127.0.0.1`, `localhost` or `[::1]` on the feed's port), one of this
///   machine's own interface addresses on the feed's port when LAN access is
///   on, or a configured public tunnel host. Anything else (a DNS-rebinding
///   page names its own host) is refused, with or without an `Origin`.
/// * No `Origin` (the SDK, Amibroker, other programs): accepted, as on the
///   web; the API key is still required to `authenticate`.
/// * An `Origin`, which browsers send: http(s) only, and either the app's
///   own page (exactly `127.0.0.1` or `localhost` on the live HTTP port, an
///   interface address on that port when LAN access is on, `localhost:5173`
///   in development builds), the configured public host, or the feed's own
///   address on this computer (Python `websocket-client`, which the SDK
///   uses, names it by default; no web page can be served from it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakePolicy {
    /// The port the app's pages are served on now (0: no listener).
    pub app_port: u16,
    /// This machine's own interface addresses while LAN access is on;
    /// empty otherwise.
    pub interfaces: Vec<std::net::IpAddr>,
    /// Public tunnel addresses (lower-case host, port).
    pub public: Vec<(String, u16)>,
    /// Development build: the Vite page on `localhost:5173` too.
    pub development: bool,
}

impl Default for HandshakePolicy {
    fn default() -> Self {
        Self {
            app_port: crate::config::DEFAULT_HTTP_PORT,
            interfaces: Vec::new(),
            public: Vec::new(),
            development: false,
        }
    }
}

use crate::server::source::{is_loopback_name as loopback_name, split_authority};

/// The identity a connection counts against
/// [`MAX_CONNECTIONS_PER_REMOTE_ADDRESS`]: a device on the network as it is
/// counted everywhere ([`crate::server::addr::budget_key`]). A loopback
/// peer (every program on this computer and the tunnel share it) is not
/// capped per address; the global `max_connections` caps it.
pub fn connection_key(peer: std::net::IpAddr) -> Option<std::net::IpAddr> {
    let peer = crate::server::addr::canonical(peer);
    (!peer.is_loopback()).then(|| crate::server::addr::budget_key(peer))
}

/// Count a connection from `key` ([`connection_key`]) if it is under the
/// cap: its own, and for a device in one of this machine's own IPv6
/// networks its network's aggregate too (five times the cap), so a device
/// rotating through the prefix is capped in total. `false` when over.
pub fn admit_connection(
    counts: &mut HashMap<std::net::IpAddr, usize>,
    key: std::net::IpAddr,
) -> bool {
    let net = crate::server::addr::aggregate_key(key);
    let count = |k: &std::net::IpAddr| counts.get(k).copied().unwrap_or(0);
    let cap = crate::server::ratelimit::AGGREGATE_FACTOR * MAX_CONNECTIONS_PER_REMOTE_ADDRESS;
    if count(&key) >= MAX_CONNECTIONS_PER_REMOTE_ADDRESS || net.is_some_and(|n| count(&n) >= cap) {
        return false;
    }
    for k in std::iter::once(key).chain(net) {
        *counts.entry(k).or_insert(0) += 1;
    }
    true
}

/// Release a connection counted by [`admit_connection`].
pub fn release_connection(counts: &mut HashMap<std::net::IpAddr, usize>, key: std::net::IpAddr) {
    let net = crate::server::addr::aggregate_key(key);
    for k in std::iter::once(key).chain(net) {
        if let Some(n) = counts.get_mut(&k) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.remove(&k);
            }
        }
    }
}

/// Who opened a feed connection: the classifier the HTTP server uses
/// ([`crate::server::source::classify`]), with the feed's port as the
/// listener's own. So a connection through a tunnel (a forwarding header,
/// or a host other than this computer) is the shared tunnel identity, a
/// device on the network its address, and only a plain local connection
/// this computer.
pub fn handshake_source(
    peer: std::net::IpAddr,
    headers: &axum::http::HeaderMap,
    ws_port: u16,
) -> crate::server::source::Source {
    crate::server::source::classify(Some(peer), headers, None, &|p| p.unwrap_or(80) == ws_port)
}

impl HandshakePolicy {
    /// No page and no host but the feed on loopback (the app is gone).
    pub fn closed() -> Self {
        Self {
            app_port: 0,
            ..Self::default()
        }
    }

    fn interface(&self, name: &str) -> bool {
        name.parse::<std::net::IpAddr>().is_ok_and(|ip| {
            let ip = match ip {
                std::net::IpAddr::V6(v6) => v6
                    .to_ipv4_mapped()
                    .map(std::net::IpAddr::V4)
                    .unwrap_or(std::net::IpAddr::V6(v6)),
                v4 => v4,
            };
            !ip.is_unspecified() && self.interfaces.contains(&ip)
        })
    }

    fn public_host(&self, name: &str, port: Option<u16>) -> bool {
        self.public
            .iter()
            .any(|(h, p)| h == name && port.is_none_or(|port| port == *p))
    }

    /// Whether an upgrade naming `host`, from a page at `origin`, may open
    /// the feed listening on `ws_port`.
    pub fn allows(&self, origin: Option<&str>, host: Option<&str>, ws_port: u16) -> bool {
        self.host_allowed(host, ws_port) && self.origin_allowed(origin, ws_port)
    }

    fn host_allowed(&self, host: Option<&str>, ws_port: u16) -> bool {
        let Some((name, port)) = host.and_then(split_authority) else {
            return false;
        };
        if self.public_host(&name, port) {
            return true;
        }
        port == Some(ws_port) && (loopback_name(&name) || self.interface(&name))
    }

    fn origin_allowed(&self, origin: Option<&str>, ws_port: u16) -> bool {
        let Some(origin) = origin else {
            return true;
        };
        let Ok(url) = url::Url::parse(origin.trim()) else {
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
        if port.is_some_and(|p| self.public_host(&name, Some(p))) {
            return true;
        }
        // A program naming the feed it connects to.
        if port == Some(ws_port) && (loopback_name(&name) || self.interface(&name)) {
            return true;
        }
        // The app's own page on the live port.
        let page = matches!(name.as_str(), "127.0.0.1" | "localhost") || self.interface(&name);
        if self.app_port != 0 && port == Some(self.app_port) && page {
            return true;
        }
        self.development && name == "localhost" && port == Some(5173)
    }
}

/// Supplies the [`HandshakePolicy`] at each handshake, so a moved HTTP
/// port, LAN access or tunnel address takes effect at once.
#[derive(Clone)]
pub struct HandshakeGate(pub Arc<dyn Fn() -> HandshakePolicy + Send + Sync>);

impl HandshakeGate {
    /// Always the same policy (tests, and the default).
    pub fn fixed(policy: HandshakePolicy) -> Self {
        Self(Arc::new(move || policy.clone()))
    }
}

impl std::fmt::Debug for HandshakeGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HandshakeGate")
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
            loopback_handshake_timeout: Duration::from_secs(3),
            max_connections: 256,
            local_connections: 64,
            max_message_bytes: 1024 * 1024,
            control_queue_cap: 1024,
            max_subscriptions_per_client: 3000,
            throttle: Duration::ZERO,
            frame_budget: FrameBudget::default(),
            handshake: HandshakeGate::fixed(HandshakePolicy::default()),
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
        "Live market data for your trading platforms and the Python SDK could not start because \
port {} is already used by another program. Close the other program (for example OpenAlgo web \
or another copy of OpenAlgo), or choose a different market data port in Server Settings. Live \
market data starts by itself once the port is free.",
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
    /// Connections of callers other than this computer: devices on the
    /// network (from accept) and tunnel callers (once the handshake shows
    /// them); capped by `max_connections`.
    network: AtomicUsize,
    /// This computer's connections; capped by `local_connections`.
    local: AtomicUsize,
    /// Loopback connections still in the handshake (this computer or the
    /// tunnel, not yet told apart); capped by both pools together.
    pending_loopback: AtomicUsize,
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
                    "Live market data could not open port {}. Choose a different market data \
port in Server Settings.",
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
        network: AtomicUsize::new(0),
        local: AtomicUsize::new(0),
        pending_loopback: AtomicUsize::new(0),
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
                let loopback = crate::server::addr::canonical(peer.ip()).is_loopback();
                let full = if loopback {
                    shared.pending_loopback.load(Ordering::Relaxed)
                        >= shared.cfg.max_connections + shared.cfg.local_connections
                } else {
                    shared.network.load(Ordering::Relaxed) >= shared.cfg.max_connections
                };
                if full {
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
                // A banned address is closed before the handshake, as every
                // HTTP surface refuses it.
                if shared.auth.refused(peer.ip()) {
                    tracing::warn!("Market data feed refused a blocked address");
                    drop(stream);
                    continue;
                }
                let remote = connection_key(peer.ip());
                if let Some(ip) = remote {
                    if !admit_connection(&mut shared.per_address.lock(), ip) {
                        tracing::warn!("Market data feed: one network address opened too many connections");
                        drop(stream);
                        continue;
                    }
                }
                if loopback {
                    shared.pending_loopback.fetch_add(1, Ordering::Relaxed);
                } else {
                    shared.network.fetch_add(1, Ordering::Relaxed);
                }
                shared.active.fetch_add(1, Ordering::Relaxed);
                conns.spawn(serve_conn(
                    stream,
                    peer.ip(),
                    remote,
                    shared.clone(),
                    token.clone(),
                ));
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

/// The error frame's message when an authenticated client is closed because
/// its authentication no longer holds (then closed with 4401).
pub const CREDENTIALS_CHANGED_MSG: &str =
    "Your OpenAlgo session ended or the API key changed. Connect again and authenticate with the current API key.";

/// Resolves to true when `rx` sees a new value; never resolves when there is
/// no receiver or its sender is gone.
async fn credentials_moved(rx: &mut Option<tokio::sync::watch::Receiver<u64>>) -> bool {
    match rx {
        Some(r) => match r.changed().await {
            Ok(()) => true,
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
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
    /// A device rotating through our own IPv6 network is capped in total by
    /// the network's aggregate; each address by its own cap; released
    /// connections free both.
    #[test]
    fn rotating_connections_hit_the_networks_aggregate() {
        use super::{admit_connection, release_connection, MAX_CONNECTIONS_PER_REMOTE_ADDRESS};
        let mut counts = std::collections::HashMap::new();
        let key = |n: u32| -> std::net::IpAddr { format!("fe80::{:x}", n + 1).parse().unwrap() };
        let mut admitted = 0;
        for n in 0..1000 {
            if admit_connection(&mut counts, key(n)) {
                admitted += 1;
            }
        }
        let cap = crate::server::ratelimit::AGGREGATE_FACTOR * MAX_CONNECTIONS_PER_REMOTE_ADDRESS;
        assert_eq!(admitted, cap);
        for _ in 0..MAX_CONNECTIONS_PER_REMOTE_ADDRESS {
            assert!(admit_connection(
                &mut counts,
                "192.168.1.9".parse().unwrap()
            ));
        }
        assert!(!admit_connection(
            &mut counts,
            "192.168.1.9".parse().unwrap()
        ));
        for n in 0..cap as u32 {
            release_connection(&mut counts, key(n));
        }
        for _ in 0..MAX_CONNECTIONS_PER_REMOTE_ADDRESS {
            release_connection(&mut counts, "192.168.1.9".parse().unwrap());
        }
        assert!(counts.is_empty(), "{:?}", counts);
    }

    /// The per-address connection cap counts a device on the network as it
    /// is counted everywhere; loopback is capped by the global limit only.
    #[test]
    fn connections_are_capped_per_network_identity() {
        use super::connection_key;
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert_eq!(connection_key(ip("127.0.0.1")), None);
        assert_eq!(connection_key(ip("::1")), None);
        assert_eq!(connection_key(ip("::ffff:127.0.0.1")), None);
        assert_eq!(connection_key(ip("192.168.1.9")), Some(ip("192.168.1.9")));
        assert_eq!(
            connection_key(ip("::ffff:192.168.1.9")),
            Some(ip("192.168.1.9"))
        );
        // A foreign IPv6 device by its /64; link-local devices one by one.
        assert_eq!(
            connection_key(ip("2001:db8:cc:dd::1")),
            connection_key(ip("2001:db8:cc:dd:9::2"))
        );
        assert_ne!(connection_key(ip("fe80::1")), connection_key(ip("fe80::2")));
    }

    use super::HandshakePolicy;

    const WS: u16 = 8765;

    fn policy() -> HandshakePolicy {
        HandshakePolicy {
            app_port: 5000,
            interfaces: vec![],
            public: vec![("abc.ngrok.app".into(), 443)],
            development: false,
        }
    }

    /// S-10: programs without an `Origin` and the app's own pages connect;
    /// other sites, rebinding host names and foreign hosts do not.
    #[test]
    fn only_the_apps_own_pages_and_programs_are_let_in() {
        let p = policy();
        let local = Some("127.0.0.1:8765");
        // A program (the SDK, Amibroker): no Origin.
        assert!(p.allows(None, local, WS));
        assert!(p.allows(None, Some("localhost:8765"), WS));
        assert!(p.allows(None, Some("[::1]:8765"), WS));
        // The app's page on the live HTTP port; websocket-client's default
        // Origin, the feed's own address.
        assert!(p.allows(Some("http://127.0.0.1:5000"), local, WS));
        assert!(p.allows(Some("http://localhost:5000"), local, WS));
        assert!(p.allows(Some("http://127.0.0.1:8765"), local, WS));
        // The public host, as Origin and as Host.
        assert!(p.allows(Some("https://abc.ngrok.app"), Some("abc.ngrok.app"), WS));
        assert!(p.allows(None, Some("abc.ngrok.app"), WS));
        // Refused: another site, another local port, a foreign IP, null,
        // non-http schemes.
        for origin in [
            "https://evil.example",
            "http://127.0.0.1:3000",
            "http://192.168.1.5:5000",
            "null",
            "file:///tmp/x.html",
            "http://abc.ngrok.app:8080",
        ] {
            assert!(!p.allows(Some(origin), local, WS), "{}", origin);
        }
        // DNS rebinding: Origin and Host both name the attacker's host.
        assert!(!p.allows(
            Some("http://evil.example:8765"),
            Some("evil.example:8765"),
            WS
        ));
        // Even without an Origin, a host name that is not ours is refused,
        // as is a missing Host or another port.
        assert!(!p.allows(None, Some("evil.example:8765"), WS));
        assert!(!p.allows(None, None, WS));
        assert!(!p.allows(None, Some("127.0.0.1:9999"), WS));
        // Development adds the Vite page on localhost:5173 only.
        let dev = HandshakePolicy {
            development: true,
            ..policy()
        };
        assert!(dev.allows(Some("http://localhost:5173"), local, WS));
        assert!(!dev.allows(Some("http://127.0.0.1:5173"), local, WS));
        assert!(!p.allows(Some("http://localhost:5173"), local, WS));
        // No listener for the app's pages: none of them is let in.
        let closed = HandshakePolicy::closed();
        assert!(!closed.allows(Some("http://127.0.0.1:5000"), local, WS));
        assert!(closed.allows(None, local, WS));
    }

    /// S-10: with LAN access on, only this machine's own interface
    /// addresses count, as Origin and as Host; any other IP is refused.
    #[test]
    fn lan_access_accepts_this_machines_own_addresses_only() {
        let lan = HandshakePolicy {
            interfaces: vec!["192.168.1.5".parse().unwrap()],
            ..policy()
        };
        let ours = Some("192.168.1.5:8765");
        assert!(lan.allows(None, ours, WS));
        assert!(lan.allows(Some("http://192.168.1.5:5000"), ours, WS));
        assert!(lan.allows(Some("http://[::ffff:192.168.1.5]:5000"), ours, WS));
        assert!(!lan.allows(Some("http://192.168.1.9:5000"), ours, WS));
        assert!(!lan.allows(Some("http://10.0.0.1:5000"), ours, WS));
        assert!(!lan.allows(None, Some("192.168.1.9:8765"), WS));
        assert!(!lan.allows(Some("http://evil.example:5000"), ours, WS));
        // Without LAN access an interface address is not ours.
        assert!(!policy().allows(Some("http://192.168.1.5:5000"), ours, WS));
    }
}
