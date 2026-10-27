//! Where a run's frames go: to the client through a bounded queue, and into
//! the turn's recorder.
//!
//! The queue holds [`QUEUE_CAPACITY`] frames and a full queue waits rather
//! than drops (the web's `QUEUE_MAXSIZE` and `_offer`): a client that stops
//! reading slows the run instead of growing memory. A client that has gone
//! (the stream was dropped) stops the run, as the web cancels on a
//! disconnect, so a turn nobody reads stops billing.

use super::recorder::TurnRecorder;
use crate::agent::contract::frames::Frame;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Frames buffered before the run waits for the client.
pub const QUEUE_CAPACITY: usize = 4096;

pub struct FrameOut {
    tx: mpsc::Sender<Frame>,
    cancel: CancellationToken,
    pub recorder: TurnRecorder,
    client_gone: bool,
}

impl FrameOut {
    pub fn new(tx: mpsc::Sender<Frame>, cancel: CancellationToken) -> FrameOut {
        FrameOut {
            tx,
            cancel,
            recorder: TurnRecorder::default(),
            client_gone: false,
        }
    }

    /// Record the frame and hand it to the client, waiting while the queue
    /// is full.
    pub async fn send(&mut self, frame: Frame) {
        self.recorder.observe(&frame);
        if self.client_gone {
            return;
        }
        if self.tx.send(frame).await.is_err() {
            self.client_gone = true;
            tracing::info!(
                "Agent stream client disconnected; cancelling run {}",
                if self.recorder.run_id.is_empty() {
                    "unknown"
                } else {
                    &self.recorder.run_id
                }
            );
            self.cancel.cancel();
        }
    }

    pub fn client_gone(&self) -> bool {
        self.client_gone
    }

    /// Resolves when the client has gone (its stream was dropped), even
    /// while the run is sending nothing: the web noticed at its next
    /// heartbeat; this notices at once.
    pub fn gone(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let tx = self.tx.clone();
        async move { tx.closed().await }
    }
}
