//! A tunnel provider for tests: nothing reaches the network. Each open
//! answers from a script (by default it opens), records what it was opened
//! with and when, and the test hands the open tunnel its callers or loses
//! it.

use super::provider::{OpenError, OpenRequest, OpenTunnel, TunnelIo, TunnelProvider};
use super::{Tunnel, TunnelStatus};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::sync::mpsc;
use tokio::time::Instant;

#[derive(Default)]
pub(crate) struct FakeProvider {
    script: Mutex<VecDeque<OpenError>>,
    /// Opens on another address than the domain asked for.
    elsewhere: std::sync::atomic::AtomicBool,
    opens: Mutex<Vec<(Instant, OpenRequest)>>,
    callers: Mutex<Vec<mpsc::Sender<Box<dyn TunnelIo>>>>,
    closed: Arc<AtomicUsize>,
}

impl FakeProvider {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The next `times` opens fail with `problem`.
    pub(crate) fn fail_next(&self, problem: OpenError, times: usize) {
        let mut s = self.script.lock();
        s.extend(std::iter::repeat_n(problem, times));
    }

    /// The next tunnels open on another address than their domain.
    pub(crate) fn open_elsewhere(&self) {
        self.elsewhere.store(true, Ordering::SeqCst);
    }

    /// What every open was asked for, in order.
    pub(crate) fn opens(&self) -> Vec<OpenRequest> {
        self.opens.lock().iter().map(|(_, r)| r.clone()).collect()
    }

    /// When every open was asked for.
    pub(crate) fn open_times(&self) -> Vec<Instant> {
        self.opens.lock().iter().map(|(t, _)| *t).collect()
    }

    /// How many tunnels were closed.
    pub(crate) fn closed(&self) -> usize {
        self.closed.load(Ordering::SeqCst)
    }

    /// A caller connects through the tunnel opened last; the caller's end.
    pub(crate) async fn call(&self) -> DuplexStream {
        let (caller, ours) = tokio::io::duplex(64 * 1024);
        let tx = self.callers.lock().last().cloned().expect("an open tunnel");
        tx.send(Box::new(ours)).await.expect("the tunnel accepts");
        caller
    }

    /// The tunnel opened last is lost (ngrok's session ended).
    pub(crate) fn lose(&self) {
        self.callers.lock().pop();
    }
}

#[async_trait]
impl TunnelProvider for FakeProvider {
    async fn open(&self, request: &OpenRequest) -> Result<Box<dyn OpenTunnel>, OpenError> {
        self.opens.lock().push((Instant::now(), request.clone()));
        if let Some(problem) = self.script.lock().pop_front() {
            return Err(problem);
        }
        let (tx, incoming) = mpsc::channel(8);
        self.callers.lock().push(tx);
        let url = if self.elsewhere.load(Ordering::SeqCst) {
            "https://elsewhere.ngrok-free.app".to_string()
        } else {
            format!("https://{}", request.domain)
        };
        Ok(Box::new(FakeTunnel {
            url,
            incoming,
            closed: self.closed.clone(),
        }))
    }
}

struct FakeTunnel {
    url: String,
    incoming: mpsc::Receiver<Box<dyn TunnelIo>>,
    closed: Arc<AtomicUsize>,
}

#[async_trait]
impl OpenTunnel for FakeTunnel {
    fn url(&self) -> String {
        self.url.clone()
    }

    async fn accept(&mut self) -> Option<Box<dyn TunnelIo>> {
        self.incoming.recv().await
    }

    async fn close(self: Box<Self>) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

/// The tunnel's status once `pred` holds (at most 10 s).
pub(crate) async fn wait_for(
    tunnel: &Tunnel,
    pred: impl Fn(&TunnelStatus) -> bool,
) -> TunnelStatus {
    wait_status(tunnel.watch(), Duration::from_secs(10), pred).await
}

/// The status on `rx` once `pred` holds, waiting at most `within` of the
/// test's clock.
pub(crate) async fn wait_status(
    mut rx: tokio::sync::watch::Receiver<TunnelStatus>,
    within: Duration,
    pred: impl Fn(&TunnelStatus) -> bool,
) -> TunnelStatus {
    tokio::time::timeout(within, async {
        loop {
            {
                let s = rx.borrow_and_update();
                if pred(&s) {
                    return s.clone();
                }
            }
            rx.changed().await.expect("the tunnel's status");
        }
    })
    .await
    .expect("the tunnel reached the status")
}

/// Every line logged on this thread, at every level and from every crate
/// (the SDK's included), while it lives. A `#[tokio::test]` runs on one
/// thread, so its spawned tasks are caught too.
pub(crate) struct LogCapture {
    buf: Arc<Mutex<Vec<u8>>>,
    _guard: tracing::subscriber::DefaultGuard,
}

impl LogCapture {
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock()).into_owned()
    }
}

struct Writer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Writer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn capture_logs() -> LogCapture {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let w = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || Writer(w.clone()))
        .finish();
    LogCapture {
        buf,
        _guard: tracing::subscriber::set_default(sub),
    }
}
