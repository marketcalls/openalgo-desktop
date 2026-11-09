//! The fake broker socket a feed connects to: a WebSocket server on an
//! ephemeral loopback port that records every data frame the feed sends
//! (per connection, in order) and sends the frames a case pushes on the
//! connection that is open now. Closing that connection makes the
//! manager reconnect, as a broker that drops the socket does.
//!
//! The accept loop owns every connection task (a `JoinSet`), and dropping
//! the `Upstream` aborts it, so nothing outlives the case.

use super::case::Wire;
use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::websocket::handshake;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// How long a step waits for the feed to connect or send.
pub(crate) const DEADLINE: Duration = Duration::from_secs(5);

/// A frame the feed sent, and on which connection (from 1).
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub(crate) connection: usize,
    pub(crate) wire: Wire,
}

/// The opening handshake of one connection.
#[derive(Debug, Clone, Default)]
pub(crate) struct Handshake {
    /// Path and query, as the request line carries them.
    pub(crate) path: String,
    /// Header names in lower case.
    pub(crate) headers: Vec<(String, String)>,
}

enum Out {
    Frame(Message),
    /// Close, with this code and reason if given.
    Close(Option<(u16, String)>),
}

#[derive(Default)]
struct State {
    connections: Vec<Handshake>,
    received: Vec<Received>,
    consumed: usize,
    /// The connection open now and the way to write to it.
    live: Option<(usize, mpsc::Sender<Out>)>,
    /// Refuse the next upgrades with this HTTP status, this many times.
    refuse: Option<(u16, usize)>,
    /// Upgrades refused so far.
    refused: usize,
    /// Record the feed's close (a `disconnect` step asked for it).
    record_close: bool,
}

pub(crate) struct Upstream {
    url: String,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Upstream {
    /// Listen on `127.0.0.1:0`; text frames equal to one in `ignore`
    /// (a heartbeat) are not recorded.
    pub(crate) async fn bind(ignore: Vec<String>) -> Upstream {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake broker binds an ephemeral port");
        let url = format!(
            "ws://{}",
            listener.local_addr().expect("the fake broker's address")
        );
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let ignore = Arc::new(ignore);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((tcp, _)) = listener.accept().await {
                let refusal = next_refusal(&shared);
                let Some((ws, hs)) = open(tcp, refusal).await else {
                    continue;
                };
                let (tx, rx) = mpsc::channel(64);
                let n = {
                    let mut st = shared.lock();
                    st.connections.push(hs);
                    let n = st.connections.len();
                    st.live = Some((n, tx));
                    n
                };
                connections.spawn(serve(ws, n, rx, shared.clone(), ignore.clone()));
            }
        });
        Upstream { url, state, task }
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// The handshake of connection `n`, once it has been made.
    pub(crate) async fn connection(&self, n: usize) -> Option<Handshake> {
        until(|| self.state.lock().connections.get(n - 1).cloned()).await
    }

    /// The next `count` frames not yet taken, once they have come; `Err`
    /// with what did come if they do not.
    pub(crate) async fn take(&self, count: usize) -> Result<Vec<Received>, Vec<Received>> {
        let ready = until(|| {
            let st = self.state.lock();
            (st.received.len() - st.consumed >= count).then_some(())
        })
        .await;
        let mut st = self.state.lock();
        let from = st.consumed;
        match ready {
            Some(()) => {
                st.consumed += count;
                Ok(st.received[from..from + count].to_vec())
            }
            None => Err(st.received[from..].to_vec()),
        }
    }

    /// Frames the feed sent that no step has taken.
    pub(crate) fn leftover(&self) -> Vec<Received> {
        let st = self.state.lock();
        st.received[st.consumed..].to_vec()
    }

    /// Send `wire` on the open connection (connection `on`, if named),
    /// waiting for the feed to connect.
    pub(crate) async fn push(&self, wire: &Wire, on: Option<usize>) -> Result<(), String> {
        let tx = until(|| match &self.state.lock().live {
            Some((n, tx)) if on.is_none_or(|want| want == *n) => Some(tx.clone()),
            _ => None,
        })
        .await
        .ok_or_else(|| match on {
            Some(n) => format!("connection {} never opened", n),
            None => "the feed is not connected".to_string(),
        })?;
        let msg = match wire {
            Wire::Text(t) => Message::Text(t.clone()),
            Wire::Binary(b) => Message::Binary(b.clone()),
            Wire::Closed(_) | Wire::Ended => return Err("a close is not pushed".to_string()),
        };
        tx.send(Out::Frame(msg))
            .await
            .map_err(|_| "the connection closed before the frame was sent".to_string())
    }

    /// Close the open connection, as a broker dropping the socket does,
    /// with a close frame's code and reason if given (Sahi's 4401, 4403,
    /// 4429).
    pub(crate) async fn drop_live(&self, close: Option<(u16, String)>) -> Result<(), String> {
        let live = self.state.lock().live.take();
        let (_, tx) = live.ok_or_else(|| "no connection is open".to_string())?;
        tx.send(Out::Close(close))
            .await
            .map_err(|_| "the connection had already closed".to_string())
    }

    /// Refuse the next `times` upgrades with HTTP `status` (a broker's 429).
    pub(crate) fn refuse(&self, status: u16, times: usize) {
        self.state.lock().refuse = Some((status, times));
    }

    /// From now on, record the feed closing a connection (its close frame,
    /// or an end without one) as a frame it sent: the case expects a close
    /// on this socket (`Case::close_sockets`).
    pub(crate) fn expect_close(&self) {
        self.state.lock().record_close = true;
    }

    /// Connections opened and upgrades refused so far.
    pub(crate) fn counts(&self) -> (usize, usize) {
        let st = self.state.lock();
        (st.connections.len(), st.refused)
    }
}

/// The status to refuse the next upgrade with, if a step asked for one.
fn next_refusal(state: &Mutex<State>) -> Option<u16> {
    let mut st = state.lock();
    let (status, times) = st.refuse?;
    st.refuse = (times > 1).then_some((status, times - 1));
    st.refused += 1;
    Some(status)
}

/// Dropping the fake broker stops it listening and closes every
/// connection.
impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Accept the WebSocket handshake, keeping its path and headers, or
/// refuse it with `refusal`'s HTTP status.
async fn open(
    tcp: TcpStream,
    refusal: Option<u16>,
) -> Option<(WebSocketStream<TcpStream>, Handshake)> {
    let mut hs = Handshake::default();
    let cb = handshake::check(|req: &Request, resp: Response| {
        if let Some(status) = refusal {
            let mut no = ErrorResponse::new(Some(format!("refused with {}", status)));
            *no.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN);
            return Err(Box::new(no));
        }
        hs.path = req.uri().to_string();
        hs.headers = req
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        Ok(resp)
    });
    let ws = tokio_tungstenite::accept_hdr_async(tcp, cb).await.ok()?;
    Some((ws, hs))
}

/// One connection: record what the feed sends, send what the case pushes.
async fn serve(
    ws: WebSocketStream<TcpStream>,
    n: usize,
    mut rx: mpsc::Receiver<Out>,
    state: Arc<Mutex<State>>,
    ignore: Arc<Vec<String>>,
) {
    let (mut write, mut read) = ws.split();
    let mut closed = false;
    loop {
        tokio::select! {
            msg = read.next() => {
                let wire = match msg {
                    Some(Ok(Message::Text(t))) if !ignore.contains(&t) => Wire::Text(t),
                    Some(Ok(Message::Binary(b))) => Wire::Binary(b),
                    Some(Ok(Message::Close(frame))) if state.lock().record_close && !closed => {
                        closed = true;
                        Wire::Closed(frame.map(|f| (u16::from(f.code), f.reason.to_string())))
                    }
                    Some(Ok(_)) => continue,
                    _ => {
                        let mut st = state.lock();
                        if st.record_close && !closed {
                            st.received.push(Received { connection: n, wire: Wire::Ended });
                        }
                        break;
                    }
                };
                state.lock().received.push(Received { connection: n, wire });
            }
            out = rx.recv() => match out {
                Some(Out::Frame(m)) => {
                    if write.send(m).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close(Some((code, reason)))) => {
                    let frame = CloseFrame {
                        code: CloseCode::from(code),
                        reason: reason.into(),
                    };
                    let _ = write.send(Message::Close(Some(frame))).await;
                    let _ = write.close().await;
                    break;
                }
                Some(Out::Close(None)) | None => {
                    let _ = write.close().await;
                    break;
                }
            },
        }
    }
    let mut st = state.lock();
    if st.live.as_ref().is_some_and(|(live, _)| *live == n) {
        st.live = None;
    }
}

/// Poll `f` every few milliseconds until it gives a value or
/// [`DEADLINE`] passes.
pub(crate) async fn until<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
