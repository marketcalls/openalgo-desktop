//! A turn's frames as Server-Sent Events over the POST that asked for it
//! (the web's `stream._pump` and the blueprint's stream admission).
//!
//! Each frame is `data: {json}\n\n` with no `event:` line. A heartbeat
//! comment goes out at once, so the browser and anything between commit to
//! the connection before the model says anything, and again after
//! [`HEARTBEAT`] of quiet, so an idle stream is not closed while a model
//! thinks. The stream ends when the run's last frame has gone.
//!
//! At most [`MAX_CONCURRENT_STREAMS`] answers stream at once (always on the
//! desktop, D-016); one more is refused with [`STREAMS_BUSY_MESSAGE`]. The
//! slot travels with the response body and is released when the body ends
//! or the client goes away.

use crate::agent::contract::frames::{sse, Frame, HEARTBEAT};
use axum::body::{Body, Bytes};
use futures_util::stream;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Answers streaming at once.
pub const MAX_CONCURRENT_STREAMS: usize = 6;

/// What a seventh answer is told (429, kind `busy`).
pub const STREAMS_BUSY_MESSAGE: &str =
    "Several answers are already being written. Wait for one to finish, then send this again.";

/// A comment after this long without a frame.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);

/// The stream slots, owned by the Agent module.
#[derive(Debug, Default)]
pub struct StreamSlots {
    open: Arc<AtomicUsize>,
}

/// One admitted stream; released when dropped.
#[derive(Debug)]
pub struct Slot {
    open: Arc<AtomicUsize>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

impl StreamSlots {
    /// Take a slot, or `None` when every slot is in use.
    pub fn admit(&self) -> Option<Slot> {
        self.open
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_CONCURRENT_STREAMS).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot {
                open: self.open.clone(),
            })
    }

    pub fn open(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

struct Pump {
    rx: mpsc::Receiver<Frame>,
    _slot: Slot,
    started: bool,
}

/// The response body: a heartbeat at once, each frame as it comes, a
/// heartbeat after a quiet [`HEARTBEAT_EVERY`], and the end when the run
/// is done. Dropping the body (the client went away) drops the receiver,
/// which stops the run.
pub fn sse_body(rx: mpsc::Receiver<Frame>, slot: Slot) -> Body {
    let pump = Pump {
        rx,
        _slot: slot,
        started: false,
    };
    let frames = stream::unfold(pump, |mut p| async move {
        if !p.started {
            p.started = true;
            return Some((
                Ok::<Bytes, Infallible>(Bytes::from_static(HEARTBEAT.as_bytes())),
                p,
            ));
        }
        match tokio::time::timeout(HEARTBEAT_EVERY, p.rx.recv()).await {
            Ok(Some(frame)) => Some((Ok(Bytes::from(sse(&frame))), p)),
            Ok(None) => None,
            Err(_) => Some((Ok(Bytes::from_static(HEARTBEAT.as_bytes())), p)),
        }
    });
    Body::from_stream(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::frames::DoneReason;
    use http_body_util::BodyExt;

    #[test]
    fn a_seventh_stream_is_refused_and_a_slot_comes_back() {
        let slots = StreamSlots::default();
        let held: Vec<Slot> = (0..MAX_CONCURRENT_STREAMS)
            .map(|_| slots.admit().unwrap())
            .collect();
        assert!(slots.admit().is_none());
        drop(held);
        assert_eq!(slots.open(), 0);
        assert!(slots.admit().is_some());
    }

    #[tokio::test]
    async fn the_body_opens_with_a_heartbeat_and_ends_with_the_run() {
        let slots = StreamSlots::default();
        let (tx, rx) = mpsc::channel(4);
        let body = sse_body(rx, slots.admit().unwrap());
        tx.send(Frame::Token { delta: "hi".into() }).await.unwrap();
        tx.send(Frame::Done {
            reason: DoneReason::Stop,
        })
        .await
        .unwrap();
        drop(tx);
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            ": heartbeat\n\ndata: {\"type\":\"token\",\"delta\":\"hi\"}\n\ndata: {\"type\":\"done\",\"reason\":\"stop\"}\n\n"
        );
        assert_eq!(slots.open(), 0, "the slot is released with the body");
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_stream_gets_a_heartbeat_every_fifteen_seconds() {
        let slots = StreamSlots::default();
        let (tx, rx) = mpsc::channel(4);
        let mut body = sse_body(rx, slots.admit().unwrap());
        let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(first, HEARTBEAT.as_bytes());
        let second = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(second, HEARTBEAT.as_bytes(), "after 15 s of quiet");
        drop(tx);
        assert!(body.frame().await.is_none());
    }
}
