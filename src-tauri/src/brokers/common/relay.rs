//! Loopback relay for broker feeds the shared `WebSocketManager` cannot
//! speak to directly.
//!
//! Most feeds use the `BrokerFeed` hooks (async `prepare` before every
//! connect, `FeedEvent::Reply` for protocol replies, `on_authenticated`).
//! The relay remains for transports that are not a broker WebSocket at all
//! (IIFL Capital's MQTT, through `iiflcapital::mqtt_relay`) and for feeds
//! whose upstream session is still relayed (Nubra's order socket): the
//! feed's `ws_request` points the manager at `ws://127.0.0.1:<port>/<secret>`,
//! and for each manager connection the relay opens the broker side
//! (`Upstream::open`), runs the protocol (`Session`) and forwards data
//! frames both ways. Readiness and refusals reach the feed as control text
//! frames that `parse` turns into `AuthOk` / `AuthFailed`, so the manager's
//! reconnect, backoff, watchdog and re-subscribe logic is reused unchanged.
//!
//! Resources: one loopback listener and at most one live session per
//! relay. The accept loop is a single task owned by `RelayHandle` and
//! aborted when the handle (and so the feed) is dropped; the session task
//! lives in a `JoinSet` inside that loop, so it is aborted with it. A new
//! manager connection replaces the previous session, but only once it has
//! completed the handshake on the random path: a stray local connection
//! (a port scan, a wrong path, a socket that never finishes) cannot end the
//! live broker session. At most `MAX_PENDING` connections may be
//! mid-handshake; more are dropped. Every socket is closed (bounded) on
//! every exit path, so another local process cannot ride on the session.

use super::streaming::AUTH_ACK_DEADLINE;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A broker socket opened by `Upstream::open`.
pub type UpstreamWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Control frame text the relay sends when the broker session is usable.
pub const READY: &str = r#"{"openalgo_relay":"ready"}"#;
const AUTH_FAILED_KEY: &str = "auth_failed";

/// Budget for the manager's handshake with the relay.
const DOWNSTREAM_HANDSHAKE: Duration = Duration::from_secs(5);
/// Connections allowed to be mid-handshake at once; extra ones are dropped.
const MAX_PENDING: usize = 8;
/// Budget for opening the broker socket (authorize + TLS + handshake).
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(25);
/// Budget for closing a socket on the way out.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest one batch of frames may take to reach either socket (TIME-02):
/// a side that stops reading ends the session instead of holding it.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest a feed behind a relay waits for its ready frame
/// (`BrokerFeed::auth_ack_deadline`): the relay may take `OPEN_TIMEOUT` to
/// open the broker side (the MQTT relay twice: its authorize call, then the
/// CONNACK) before the broker confirms the login.
pub const READY_DEADLINE: Duration =
    Duration::from_secs(2 * OPEN_TIMEOUT.as_secs() + AUTH_ACK_DEADLINE.as_secs());

/// Outcome of opening the broker socket.
pub enum Open {
    Ready(Box<UpstreamWs>),
    /// The broker refused the stored login; the manager stops until the
    /// trader logs in again. The message is trader-facing.
    AuthFailed(String),
    /// Anything else (network, broker outage): the manager backs off and
    /// retries.
    Unavailable,
}

/// What one upstream frame produced.
#[derive(Debug, Default)]
pub struct Step {
    /// Frames for the feed (`BrokerFeed::parse`).
    pub down: Vec<Message>,
    /// Replies for the broker.
    pub up: Vec<Message>,
    /// The broker session became usable.
    pub ready: bool,
    /// The broker refused the session (trader-facing message).
    pub auth_failed: Option<String>,
}

/// Per-connection protocol state.
pub trait Session: Send {
    /// Frames to send to the broker right after it opens.
    fn on_open(&mut self) -> Vec<Message> {
        Vec::new()
    }
    /// Whether the session is usable as soon as the socket opens.
    fn ready_on_open(&self) -> bool {
        true
    }
    /// Treat the session as usable after this long without an explicit
    /// acknowledgement (Groww: 2 s, like the web).
    fn assume_ready_after(&self) -> Option<Duration> {
        None
    }
    /// Translate one broker frame.
    fn on_upstream(&mut self, msg: Message) -> Step;
    /// Frames from the feed (subscribe / unsubscribe) for the broker.
    fn on_downstream(&mut self, msg: Message) -> Vec<Message> {
        match msg {
            m @ (Message::Text(_) | Message::Binary(_)) => vec![m],
            _ => Vec::new(),
        }
    }
    /// Client keepalive sent to the broker on a fixed period.
    fn keepalive(&self) -> Option<(Duration, Message)> {
        None
    }
}

/// How to reach one broker feed.
#[async_trait]
pub trait Upstream: Send + Sync + 'static {
    fn broker(&self) -> &'static str;
    /// Open the broker socket (fresh authorize / token on every call).
    async fn open(&self) -> Open;
    fn session(&self) -> Box<dyn Session>;
}

/// Parse a relay control frame: `Some(Ok(()))` ready, `Some(Err(msg))`
/// refused, `None` when the text is not a relay control frame.
pub fn control(text: &str) -> Option<std::result::Result<(), String>> {
    if !text.starts_with("{\"openalgo_relay\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    match v.get("openalgo_relay").and_then(|s| s.as_str()) {
        Some("ready") => Some(Ok(())),
        Some(AUTH_FAILED_KEY) => Some(Err(v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("The broker refused the live market data session. Log in again.")
            .to_string())),
        _ => None,
    }
}

fn auth_failed_frame(message: &str) -> Message {
    // Key order matters: `control` recognises the frame by its prefix.
    Message::Text(format!(
        "{{\"openalgo_relay\":\"{}\",\"message\":{}}}",
        AUTH_FAILED_KEY,
        serde_json::Value::String(message.to_string())
    ))
}

/// A running relay. Dropping it stops the listener and any live session.
pub struct RelayHandle {
    url: String,
    task: JoinHandle<()>,
}

impl RelayHandle {
    /// Bind `127.0.0.1:0` and start accepting. Must run inside a tokio
    /// runtime (the manager's supervisor calls `ws_request` from one).
    pub fn start(upstream: Arc<dyn Upstream>) -> Result<Self> {
        let setup = |e: std::io::Error| {
            AppError::Internal(format!("Feed relay could not open a local port: {}", e))
        };
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(setup)?;
        std_listener.set_nonblocking(true).map_err(setup)?;
        let port = std_listener.local_addr().map_err(setup)?.port();
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Feed relay needs the async runtime".into()))?;
        let listener = {
            let _guard = handle.enter();
            tokio::net::TcpListener::from_std(std_listener).map_err(setup)?
        };
        let secret = hex::encode(rand::random::<[u8; 16]>());
        let path = format!("/{}", secret);
        let task = handle.spawn(accept_loop(listener, path.clone(), upstream));
        Ok(Self {
            url: format!("ws://127.0.0.1:{}{}", port, path),
            task,
        })
    }

    /// The address the manager connects to (contains the secret path).
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start the relay on first use and hand back its URL.
pub fn ensure_started(
    slot: &parking_lot::Mutex<Option<RelayHandle>>,
    upstream: impl FnOnce() -> Arc<dyn Upstream>,
) -> Result<String> {
    let mut guard = slot.lock();
    if let Some(h) = guard.as_ref() {
        if h.is_running() {
            return Ok(h.url().to_string());
        }
    }
    let h = RelayHandle::start(upstream())?;
    let url = h.url().to_string();
    *guard = Some(h);
    Ok(url)
}

async fn accept_loop(listener: tokio::net::TcpListener, path: String, upstream: Arc<dyn Upstream>) {
    // At most one live session; dropping the set (when this task is
    // aborted) aborts it.
    let mut sessions: JoinSet<()> = JoinSet::new();
    // Handshakes run apart from the live session, which is only replaced by
    // a connection that proved it knows the secret path.
    let mut pending: JoinSet<Option<WebSocketStream<TcpStream>>> = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    if pending.len() < MAX_PENDING {
                        pending.spawn(handshake(tcp, path.clone()));
                    }
                }
                Err(e) => {
                    tracing::warn!(broker = upstream.broker(), "Feed relay accept failed: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(done) = pending.join_next(), if !pending.is_empty() => {
                if let Ok(Some(down)) = done {
                    sessions.abort_all();
                    while sessions.try_join_next().is_some() {}
                    sessions.spawn(serve(down, upstream.clone()));
                }
            }
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
}

/// Accept the manager's WebSocket on the secret path, within the budget.
async fn handshake(tcp: TcpStream, path: String) -> Option<WebSocketStream<TcpStream>> {
    let check = crate::websocket::handshake::check(move |req: &Request, resp: Response| {
        if req.uri().path() == path {
            Ok(resp)
        } else {
            let mut r = ErrorResponse::new(None);
            *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            Err(Box::new(r))
        }
    });
    match tokio::time::timeout(
        DOWNSTREAM_HANDSHAKE,
        tokio_tungstenite::accept_hdr_async(tcp, check),
    )
    .await
    {
        Ok(Ok(ws)) => Some(ws),
        _ => None,
    }
}

/// The broker feed, opened within the timeout. A refused login sends the
/// downstream the auth-failed frame first; any failure closes it.
async fn open_or_close(
    upstream: &dyn Upstream,
    dw: &mut SplitSink<WebSocketStream<TcpStream>, Message>,
) -> Option<Box<UpstreamWs>> {
    let broker = upstream.broker();
    let opened = match tokio::time::timeout(OPEN_TIMEOUT, upstream.open()).await {
        Ok(o) => o,
        Err(_) => {
            tracing::debug!(broker, "Broker feed open timed out");
            Open::Unavailable
        }
    };
    match opened {
        Open::Ready(ws) => Some(ws),
        Open::AuthFailed(message) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.send(auth_failed_frame(&message))).await;
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
            None
        }
        Open::Unavailable => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
            None
        }
    }
}

/// Send `frames` in order, all within [`WRITE_TIMEOUT`]; false when the
/// socket failed or did not take them in time.
pub(crate) async fn send_within<S>(sink: &mut S, frames: Vec<Message>) -> bool
where
    S: futures_util::Sink<Message> + Unpin,
{
    let send = async {
        for f in frames {
            if sink.send(f).await.is_err() {
                return false;
            }
        }
        true
    };
    tokio::time::timeout(WRITE_TIMEOUT, send)
        .await
        .unwrap_or(false)
}

fn ready_frame() -> Vec<Message> {
    vec![Message::Text(READY.into())]
}

/// The relay's sign that the broker is there: a pong the feed's manager
/// hears as a frame (`BrokerFeed::parse` ignores it or reads a heartbeat).
pub(crate) fn alive_frame() -> Message {
    Message::Pong(Vec::new())
}

/// What the feed hears of one broker frame: the frames for it (the ready
/// frame among them), or, when the frame gave the feed nothing (a pong, a
/// heartbeat answer), the relay's sign that the broker is there. Without
/// it a quiet broker behind a relay sent the manager nothing, its stall
/// watchdog took the broker for dead, and the socket was dropped and made
/// again.
fn down_or_alive(down: Vec<Message>) -> Vec<Message> {
    if down.is_empty() {
        vec![alive_frame()]
    } else {
        down
    }
}

async fn serve(down: WebSocketStream<TcpStream>, upstream: Arc<dyn Upstream>) {
    let (mut dw, mut dr) = down.split();
    let Some(up) = open_or_close(&*upstream, &mut dw).await else {
        return;
    };
    let (mut uw, mut ur) = up.split();
    let mut session = upstream.session();
    let mut ok = send_within(&mut uw, session.on_open()).await;
    let mut ready = false;
    if ok && session.ready_on_open() {
        ready = true;
        ok = send_within(&mut dw, ready_frame()).await;
    }
    let keepalive = session.keepalive();
    let period = keepalive
        .as_ref()
        .map(|(d, _)| *d)
        .unwrap_or(Duration::from_secs(3600));
    let mut ka = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let assume = session
        .assume_ready_after()
        .unwrap_or(Duration::from_secs(3600));
    let ready_deadline = tokio::time::sleep(assume);
    tokio::pin!(ready_deadline);
    while ok {
        tokio::select! {
            m = dr.next() => match m {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(m)) => ok = send_within(&mut uw, session.on_downstream(m)).await,
            },
            m = ur.next() => match m {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(m)) => {
                    let step = session.on_upstream(m);
                    ok = send_within(&mut uw, step.up).await;
                    if let Some(message) = step.auth_failed {
                        let _ = send_within(&mut dw, vec![auth_failed_frame(&message)]).await;
                        break;
                    }
                    let mut down = step.down;
                    if step.ready && !ready {
                        ready = true;
                        down = ready_frame().into_iter().chain(down).collect();
                    }
                    ok = ok && send_within(&mut dw, down_or_alive(down)).await;
                }
            },
            _ = ka.tick(), if keepalive.is_some() => {
                if let Some((_, m)) = &keepalive {
                    ok = send_within(&mut uw, vec![m.clone()]).await;
                }
            }
            _ = &mut ready_deadline, if !ready => {
                ready = true;
                ok = send_within(&mut dw, ready_frame()).await;
            }
        }
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, uw.close()).await;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    /// Takes one frame, then never another (a peer that stopped reading).
    struct Stuck(usize);

    impl futures_util::Sink<Message> for Stuck {
        type Error = ();
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), ()>> {
            if self.0 == 0 {
                std::task::Poll::Ready(Ok(()))
            } else {
                std::task::Poll::Pending
            }
        }
        fn start_send(
            mut self: std::pin::Pin<&mut Self>,
            _: Message,
        ) -> std::result::Result<(), ()> {
            self.0 += 1;
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), ()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), ()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// TIME-02: a socket that stops taking frames fails the send at the
    /// write timeout instead of holding the session forever.
    #[tokio::test(start_paused = true)]
    async fn a_send_the_peer_never_takes_fails_at_the_write_timeout() {
        let start = tokio::time::Instant::now();
        let frames = vec![Message::Text("a".into()), Message::Text("b".into())];
        let sent =
            tokio::time::timeout(Duration::from_secs(120), send_within(&mut Stuck(0), frames))
                .await;
        assert_eq!(sent, Ok(false));
        assert_eq!(start.elapsed(), WRITE_TIMEOUT);
        // Relay feeds wait long enough for the relay to open the broker side.
        assert!(READY_DEADLINE > 2 * OPEN_TIMEOUT);
    }

    struct Echo {
        url: String,
        refuse: bool,
        opens: Arc<AtomicUsize>,
    }

    struct EchoSession;

    impl Session for EchoSession {
        fn on_upstream(&mut self, msg: Message) -> Step {
            Step {
                down: vec![msg],
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl Upstream for Echo {
        fn broker(&self) -> &'static str {
            "test"
        }
        async fn open(&self) -> Open {
            self.opens.fetch_add(1, Ordering::SeqCst);
            if self.refuse {
                return Open::AuthFailed("Log in again.".into());
            }
            match tokio_tungstenite::connect_async(self.url.as_str()).await {
                Ok((ws, _)) => Open::Ready(Box::new(ws)),
                Err(_) => Open::Unavailable,
            }
        }
        fn session(&self) -> Box<dyn Session> {
            Box::new(EchoSession)
        }
    }

    /// A broker that echoes every data frame back.
    async fn echo_server() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = l.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await {
                        while let Some(Ok(m)) = ws.next().await {
                            if m.is_binary() || m.is_text() {
                                let _ = ws.send(m).await;
                            }
                        }
                    }
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn relays_both_ways_after_ready() {
        let opens = Arc::new(AtomicUsize::new(0));
        let up = Arc::new(Echo {
            url: echo_server().await,
            refuse: false,
            opens: opens.clone(),
        });
        let relay = RelayHandle::start(up).unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(relay.url()).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert_eq!(control(first.to_text().unwrap()), Some(Ok(())));
        ws.send(Message::Binary(vec![1, 2, 3])).await.unwrap();
        let back = ws.next().await.unwrap().unwrap();
        assert_eq!(back, Message::Binary(vec![1, 2, 3]));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    /// A broker kept alive with pings that passes on only data, as Nubra's
    /// order session does.
    struct Quiet {
        url: String,
    }

    struct QuietSession;

    impl Session for QuietSession {
        fn on_upstream(&mut self, msg: Message) -> Step {
            match msg {
                m @ Message::Binary(_) => Step {
                    down: vec![m],
                    ..Default::default()
                },
                _ => Step::default(),
            }
        }
        fn keepalive(&self) -> Option<(Duration, Message)> {
            Some((Duration::from_millis(50), Message::Ping(Vec::new())))
        }
    }

    #[async_trait]
    impl Upstream for Quiet {
        fn broker(&self) -> &'static str {
            "test"
        }
        async fn open(&self) -> Open {
            match tokio_tungstenite::connect_async(self.url.as_str()).await {
                Ok((ws, _)) => Open::Ready(Box::new(ws)),
                Err(_) => Open::Unavailable,
            }
        }
        fn session(&self) -> Box<dyn Session> {
            Box::new(QuietSession)
        }
    }

    /// A quiet broker that answers its keepalive is heard by the feed: the
    /// relay passed on data only, so the manager's stall watchdog took a
    /// quiet broker for a dead one and remade the connection.
    #[tokio::test]
    async fn a_quiet_broker_answering_its_keepalive_is_heard_downstream() {
        let relay = RelayHandle::start(Arc::new(Quiet {
            url: echo_server().await,
        }))
        .unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(relay.url()).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert_eq!(control(first.to_text().unwrap()), Some(Ok(())));
        let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a sign that the broker is there")
            .unwrap()
            .unwrap();
        assert_eq!(next, alive_frame());
        assert_eq!(
            down_or_alive(vec![Message::Binary(vec![1])]),
            [Message::Binary(vec![1])]
        );
    }

    #[tokio::test]
    async fn wrong_path_is_refused_and_auth_failure_is_reported() {
        let up = Arc::new(Echo {
            url: String::new(),
            refuse: true,
            opens: Arc::default(),
        });
        let relay = RelayHandle::start(up).unwrap();
        let base = relay.url().rsplit_once('/').unwrap().0.to_string();
        let err = tokio_tungstenite::connect_async(format!("{}/guess", base)).await;
        assert!(err.is_err());
        let (mut ws, _) = tokio_tungstenite::connect_async(relay.url()).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert_eq!(
            control(first.to_text().unwrap()),
            Some(Err("Log in again.".to_string()))
        );
    }

    /// A stray local connection (port scan, wrong path, a socket that never
    /// completes the handshake) must not end the live broker session.
    #[tokio::test]
    async fn stray_connections_do_not_end_the_live_session() {
        let up = Arc::new(Echo {
            url: echo_server().await,
            refuse: false,
            opens: Arc::default(),
        });
        let relay = RelayHandle::start(up).unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(relay.url()).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        assert_eq!(control(first.to_text().unwrap()), Some(Ok(())));
        let addr = relay
            .url()
            .trim_start_matches("ws://")
            .split('/')
            .next()
            .unwrap()
            .to_string();
        let mut idle = Vec::new();
        for _ in 0..20 {
            idle.push(TcpStream::connect(&addr).await.unwrap());
        }
        let base = relay.url().rsplit_once('/').unwrap().0.to_string();
        assert!(tokio_tungstenite::connect_async(format!("{}/guess", base))
            .await
            .is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        ws.send(Message::Binary(vec![9, 9])).await.unwrap();
        let back = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("live session still answers")
            .unwrap()
            .unwrap();
        assert_eq!(back, Message::Binary(vec![9, 9]));
        drop(idle);
    }

    #[tokio::test]
    async fn dropping_the_handle_stops_the_listener() {
        let up = Arc::new(Echo {
            url: String::new(),
            refuse: false,
            opens: Arc::default(),
        });
        let relay = RelayHandle::start(up).unwrap();
        let url = relay.url().to_string();
        drop(relay);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(tokio_tungstenite::connect_async(url).await.is_err());
    }

    #[test]
    fn control_frames() {
        assert_eq!(control(READY), Some(Ok(())));
        assert_eq!(control(r#"{"status":"failed"}"#), None);
        let Message::Text(t) = auth_failed_frame("x") else {
            panic!("not text")
        };
        assert_eq!(control(&t), Some(Err("x".into())));
    }
}
