//! Loopback relay between the shared `WebSocketManager` and Nuvama's
//! streaming socket, which is plain TCP carrying one JSON object per line
//! both ways (web `streaming/nuvama_socket.py`, the SDK's `feed/feed.py`).
//!
//! The manager speaks WebSocket only, so the feed's `ws_request` points it
//! at `ws://127.0.0.1:<port>/<random secret>`; for each manager connection
//! the relay opens one TCP socket to Nuvama and translates:
//!
//! * a text frame from the feed becomes one line (`<json>\n`); the quote
//!   and depth requests (`streaming_type` `quote` / `quote2`) are batched
//!   as the web batches them: Nuvama replaces a streaming type's whole
//!   symbol set with each subscribe and drops requests sent back to back,
//!   so the latest request of each type goes once the changes have been
//!   quiet for 0.5 s (or 2 s after the first), one type at a time, at least
//!   0.5 s apart; an unsubscribe of a type this socket never subscribed is
//!   not sent (web `_flush_one`). Any other request (the order stream's
//!   subscription) goes at once;
//! * each line Nuvama sends becomes one text frame; the bare `{}` lines it
//!   answers the keepalive with become the relay's sign that the broker is
//!   there (`relay::alive_frame`);
//! * `{}\n` goes to Nuvama every 30 s (web `HEARTBEAT_SECONDS`). Nuvama
//!   need not answer it, and an order socket is silent until an order
//!   moves, so a keepalive the socket took is also passed on as that sign:
//!   the web never drops a quiet socket, and the manager's stall watchdog
//!   would. The market feed judges a stall itself (`data_stalled`, the
//!   web's 180 s without data while subscribed).
//!
//! The socket opens with the relay's `READY` control frame once the TCP
//! connection is up (Nuvama has no login exchange on the socket); a
//! connection that cannot be opened closes the feed's side, so the manager
//! backs off and retries. A partial line over 1 MiB is a broken stream
//! (web `MAX_PENDING_BYTES`) and ends the session.
//!
//! Resources: one loopback listener per relay, its accept loop a single
//! task owned by [`LineRelay`] and aborted on drop; at most one live
//! session, in a `JoinSet` inside that task, replaced only by a connection
//! that completed the handshake on the secret path. Both sockets are closed
//! (bounded) on every exit path.

use crate::brokers::common::relay::{self, alive_frame, send_within, READY};
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// web `CONNECT_TIMEOUT`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// web `HEARTBEAT_SECONDS`.
pub const HEARTBEAT: Duration = Duration::from_secs(30);
/// web `SEND_GAP`, `BATCH_QUIET`, `BATCH_MAX_WAIT`.
pub const SEND_GAP: Duration = Duration::from_millis(500);
pub const BATCH_QUIET: Duration = Duration::from_millis(500);
pub const BATCH_MAX_WAIT: Duration = Duration::from_secs(2);
/// web `MAX_PENDING_BYTES`.
pub const MAX_LINE: usize = 1 << 20;
const MAX_PENDING_HANDSHAKES: usize = 8;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// The streaming types whose requests replace a symbol set.
const BATCHED: &[&str] = &["quote", "quote2"];

/// Where Nuvama's streaming socket is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// What one feed request is: its streaming type and whether it subscribes.
fn classify(text: &str) -> Option<(String, bool)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let req = v.get("request")?;
    let kind = req.get("streaming_type")?.as_str()?;
    let subscribe = req.get("request_type").and_then(|r| r.as_str()) == Some("subscribe");
    Some((kind.to_string(), subscribe))
}

/// The quote and depth requests waiting to go (web `_dirty`, `_active`,
/// `_first_change`, `_last_change`, `SEND_GAP`).
#[derive(Debug, Default)]
pub struct Batcher {
    /// Streaming type -> its latest request and whether it subscribes.
    pending: BTreeMap<String, (String, bool)>,
    /// Types this socket has subscribed (so an unsubscribe goes out).
    active: HashSet<String>,
    first: Option<Instant>,
    last: Option<Instant>,
    next_send: Option<Instant>,
}

impl Batcher {
    /// Queue `line` as the latest request of `kind`.
    pub fn put(&mut self, kind: &str, line: String, subscribe: bool, now: Instant) {
        self.pending.insert(kind.to_string(), (line, subscribe));
        self.last = Some(now);
        self.first.get_or_insert(now);
    }

    /// When the next request may go, or `None` with nothing waiting.
    pub fn due(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            return None;
        }
        let last = self.last?;
        let first = self.first.unwrap_or(last);
        let settled = (last + BATCH_QUIET).min(first + BATCH_MAX_WAIT);
        Some(match self.next_send {
            Some(n) => settled.max(n),
            None => settled,
        })
    }

    /// The next request to send at `now` (one streaming type, the first in
    /// order), or `None` when none is due.
    pub fn take(&mut self, now: Instant) -> Option<String> {
        while self.due().is_some_and(|d| d <= now) {
            let kind = self.pending.keys().next()?.clone();
            let (line, subscribe) = self.pending.remove(&kind)?;
            if self.pending.is_empty() {
                self.first = None;
            }
            if !subscribe && !self.active.contains(&kind) {
                continue;
            }
            if subscribe {
                self.active.insert(kind);
            } else {
                self.active.remove(&kind);
            }
            self.next_send = Some(now + SEND_GAP);
            return Some(line);
        }
        None
    }
}

/// A running relay. Dropping it stops the listener and any live session.
pub struct LineRelay {
    url: String,
    task: JoinHandle<()>,
}

impl LineRelay {
    /// Bind `127.0.0.1:0` and start accepting (needs a tokio runtime).
    pub fn start(endpoint: Endpoint) -> Result<Self> {
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
        let path = format!("/{}", hex::encode(rand::random::<[u8; 16]>()));
        let task = handle.spawn(accept_loop(listener, path.clone(), endpoint));
        Ok(Self {
            url: format!("ws://127.0.0.1:{}{}", port, path),
            task,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for LineRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start the relay on first use and hand back its URL.
pub fn ensure_started(
    slot: &parking_lot::Mutex<Option<LineRelay>>,
    endpoint: &Endpoint,
) -> Result<String> {
    let mut guard = slot.lock();
    if let Some(r) = guard.as_ref() {
        if r.is_running() {
            return Ok(r.url().to_string());
        }
    }
    let r = LineRelay::start(endpoint.clone())?;
    let url = r.url().to_string();
    *guard = Some(r);
    Ok(url)
}

async fn accept_loop(listener: tokio::net::TcpListener, path: String, endpoint: Endpoint) {
    let mut sessions: JoinSet<()> = JoinSet::new();
    let mut pending: JoinSet<Option<WebSocketStream<TcpStream>>> = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    if pending.len() < MAX_PENDING_HANDSHAKES {
                        pending.spawn(relay::handshake(tcp, path.clone()));
                    }
                }
                Err(e) => {
                    tracing::warn!(broker = "nuvama", "Feed relay accept failed: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(done) = pending.join_next(), if !pending.is_empty() => {
                if let Ok(Some(down)) = done {
                    sessions.abort_all();
                    while sessions.try_join_next().is_some() {}
                    sessions.spawn(serve(down, endpoint.clone()));
                }
            }
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
}

/// Split complete lines off `buf`: each text line, and whether a `{}`
/// keepalive answer came. A partial line stays in `buf`.
pub fn split_lines(buf: &mut Vec<u8>) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut alive = false;
    while let Some(i) = buf.iter().position(|b| *b == b'\n') {
        let raw: Vec<u8> = buf.drain(..=i).collect();
        let line = String::from_utf8_lossy(&raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line == "{}" {
            alive = true;
        } else {
            lines.push(line);
        }
    }
    (lines, alive)
}

async fn write_line(up: &mut tokio::net::tcp::OwnedWriteHalf, line: &str) -> bool {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, up.write_all(&bytes)).await,
        Ok(Ok(()))
    )
}

async fn open(endpoint: &Endpoint) -> Option<TcpStream> {
    let addr = (endpoint.host.as_str(), endpoint.port);
    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => Some(s),
        Ok(Err(e)) => {
            tracing::warn!(
                broker = "nuvama",
                "Nuvama stream could not connect: {}",
                e.kind()
            );
            None
        }
        Err(_) => {
            tracing::warn!(broker = "nuvama", "Nuvama stream connect timed out");
            None
        }
    }
}

/// One feed request: batched, or written at once.
async fn on_feed_frame(
    text: &str,
    batch: &mut Batcher,
    up: &mut tokio::net::tcp::OwnedWriteHalf,
) -> bool {
    match classify(text) {
        Some((kind, subscribe)) if BATCHED.contains(&kind.as_str()) => {
            batch.put(&kind, text.to_string(), subscribe, Instant::now());
            true
        }
        _ => write_line(up, text).await,
    }
}

async fn serve(down: WebSocketStream<TcpStream>, endpoint: Endpoint) {
    let (mut dw, mut dr) = down.split();
    let Some(tcp) = open(&endpoint).await else {
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
        return;
    };
    let _ = tcp.set_nodelay(true);
    let (mut ur, mut uw) = tcp.into_split();
    let mut ok = send_within(&mut dw, vec![Message::Text(READY.into())]).await;
    let mut batch = Batcher::default();
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut ka = tokio::time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
    while ok {
        let due = batch.due();
        tokio::select! {
            m = dr.next() => match m {
                Some(Ok(Message::Text(t))) => ok = on_feed_frame(&t, &mut batch, &mut uw).await,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            n = ur.read(&mut chunk) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let (lines, alive) = split_lines(&mut buf);
                    if buf.len() > MAX_LINE {
                        tracing::warn!(broker = "nuvama", "Nuvama stream sent no line break in {} bytes", buf.len());
                        break;
                    }
                    let mut frames: Vec<Message> = lines.into_iter().map(Message::Text).collect();
                    if frames.is_empty() && alive {
                        frames.push(alive_frame());
                    }
                    if !frames.is_empty() {
                        ok = send_within(&mut dw, frames).await;
                    }
                }
            },
            _ = ka.tick() => {
                ok = write_line(&mut uw, "{}").await
                    && send_within(&mut dw, vec![alive_frame()]).await;
            }
            _ = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                if let Some(line) = batch.take(Instant::now()) {
                    ok = write_line(&mut uw, &line).await;
                }
            }
        }
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, uw.shutdown()).await;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, dw.close()).await;
}
