//! The loopback socket that wakes the simulated feed once a second, as the
//! web's `_mock_streaming_loop` sleeps one second between rounds. It sends
//! the tick frame and nothing else: the prices are made in the feed, so a
//! local program that finds the port learns nothing from it.
//!
//! Resources, as `brokers::common::relay`: one listener on `127.0.0.1:0`
//! with a random path, its accept loop one task owned by [`Ticker`] and
//! aborted when it (and so the feed) is dropped; at most one live
//! connection, held in a `JoinSet` inside that task and replaced only by a
//! connection that completed the handshake on the path; at most
//! `MAX_PENDING` handshakes at once; the socket closed within a bound on
//! every way out.

use crate::brokers::common::relay::{handshake, send_within};
use crate::error::{AppError, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// The frame that starts one simulated second.
pub(crate) const TICK: &str = r#"{"openalgo_sim":"tick"}"#;
/// web `time.sleep(1.0)` between rounds.
const PERIOD: Duration = Duration::from_secs(1);
/// Connections allowed to be mid-handshake at once; extra ones are dropped.
const MAX_PENDING: usize = 8;
/// Budget for closing the socket on the way out.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// A running ticker. Dropping it stops the listener and the connection.
pub(crate) struct Ticker {
    url: String,
    task: JoinHandle<()>,
}

impl Ticker {
    /// Bind `127.0.0.1:0` and start accepting. Must run inside a tokio
    /// runtime (the manager calls `ws_request` from one).
    fn start() -> Result<Self> {
        let setup = |e: std::io::Error| {
            AppError::Internal(format!(
                "Dhan Sandbox feed could not open a local port: {}",
                e
            ))
        };
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(setup)?;
        std_listener.set_nonblocking(true).map_err(setup)?;
        let port = std_listener.local_addr().map_err(setup)?.port();
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Dhan Sandbox feed needs the async runtime".into()))?;
        let listener = {
            let _guard = runtime.enter();
            tokio::net::TcpListener::from_std(std_listener).map_err(setup)?
        };
        let path = format!("/{}", hex::encode(rand::random::<[u8; 16]>()));
        let task = runtime.spawn(accept_loop(listener, path.clone()));
        Ok(Self {
            url: format!("ws://127.0.0.1:{}{}", port, path),
            task,
        })
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Start the ticker on first use (again if it stopped) and hand back its
/// address, which carries the secret path.
pub(crate) fn ensure_started(slot: &parking_lot::Mutex<Option<Ticker>>) -> Result<String> {
    let mut held = slot.lock();
    if let Some(t) = held.as_ref() {
        if !t.task.is_finished() {
            return Ok(t.url.clone());
        }
    }
    let t = Ticker::start()?;
    let url = t.url.clone();
    *held = Some(t);
    Ok(url)
}

async fn accept_loop(listener: tokio::net::TcpListener, path: String) {
    let mut live: JoinSet<()> = JoinSet::new();
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
                    tracing::warn!(broker = "dhan_sandbox", "Simulated feed accept failed: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            },
            Some(done) = pending.join_next(), if !pending.is_empty() => {
                if let Ok(Some(ws)) = done {
                    live.abort_all();
                    while live.try_join_next().is_some() {}
                    live.spawn(serve(ws));
                }
            }
            Some(_) = live.join_next(), if !live.is_empty() => {}
        }
    }
}

/// One tick frame a second until the feed's side goes away.
async fn serve(ws: WebSocketStream<TcpStream>) {
    let (mut tx, mut rx) = ws.split();
    let mut every = tokio::time::interval_at(Instant::now() + PERIOD, PERIOD);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = every.tick() => {
                if !send_within(&mut tx, vec![Message::Text(TICK.into())]).await {
                    break;
                }
            }
            m = rx.next() => match m {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, tx.close()).await;
}
