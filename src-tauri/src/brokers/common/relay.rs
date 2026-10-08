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

use crate::error::{AppError, Result};
use async_trait::async_trait;
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
    let check =
        move |req: &Request, resp: Response| -> std::result::Result<Response, ErrorResponse> {
            if req.uri().path() == path {
                Ok(resp)
            } else {
                let mut r = ErrorResponse::new(None);
                *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
                Err(r)
            }
        };
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

async fn serve(down: WebSocketStream<TcpStream>, upstream: Arc<dyn Upstream>) {
    let broker = upstream.broker();
    let (mut dw, mut dr) = down.split();
    let opened = match tokio::time::timeout(OPEN_TIMEOUT, upstream.open()).await {
        Ok(o) => o,
        Err(_) => {
            tracing::debug!(broker, "Broker feed open timed out");
            Open::Unavailable
        }
    };
    let up = match opened {
        Open::Ready(ws) => ws,
        Open::AuthFailed(message) => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.send(auth_failed_frame(&message))).await;
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
            return;
        }
        Open::Unavailable => {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
            return;
        }
    };
    let (mut uw, mut ur) = up.split();
    let mut session = upstream.session();
    let mut ok = true;
    for f in session.on_open() {
        if uw.send(f).await.is_err() {
            ok = false;
            break;
        }
    }
    let mut ready = false;
    if ok && session.ready_on_open() {
        ready = true;
        ok = dw.send(Message::Text(READY.into())).await.is_ok();
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
                Some(Ok(m)) => {
                    for f in session.on_downstream(m) {
                        if uw.send(f).await.is_err() {
                            ok = false;
                            break;
                        }
                    }
                }
            },
            m = ur.next() => match m {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(m)) => {
                    let step = session.on_upstream(m);
                    for f in step.up {
                        if uw.send(f).await.is_err() {
                            ok = false;
                        }
                    }
                    if let Some(message) = step.auth_failed {
                        let _ = dw.send(auth_failed_frame(&message)).await;
                        break;
                    }
                    if step.ready && !ready {
                        ready = true;
                        if dw.send(Message::Text(READY.into())).await.is_err() {
                            ok = false;
                        }
                    }
                    for f in step.down {
                        if dw.send(f).await.is_err() {
                            ok = false;
                            break;
                        }
                    }
                }
            },
            _ = ka.tick(), if keepalive.is_some() => {
                if let Some((_, m)) = &keepalive {
                    if uw.send(m.clone()).await.is_err() {
                        ok = false;
                    }
                }
            }
            _ = &mut ready_deadline, if !ready => {
                ready = true;
                if dw.send(Message::Text(READY.into())).await.is_err() {
                    ok = false;
                }
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
