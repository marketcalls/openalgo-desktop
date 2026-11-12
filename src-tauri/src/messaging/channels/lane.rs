//! Each webhook channel's own bounded queue of posts (the web's `_Lane`
//! in `services/alerts/channels/webhook.py`).
//!
//! A lane holds at most [`MAX_PENDING`] posts, waiting or running, and runs
//! [`WORKERS_PER_CHANNEL`] at a time. No other channel shares it, so a
//! provider that has stopped answering fills its own queue and holds its
//! own workers while the other provider's alerts keep going. Past the cap a
//! post is dropped and logged, because a stalled provider must not grow
//! memory in a process that runs all day. A failed post is logged and not
//! retried: an alert retried a minute later describes a market that has
//! moved.
//!
//! A lane is made on first use, inside the runtime. Its workers are tasks
//! the app context owns (`AppState::spawn`), ended at shutdown, and each
//! queued post holds one of the lane's permits until it has run, on every
//! path (a post that panics is caught and logged, and its permit returns).

use super::webhook::WebhookKind;
use crate::state::AppState;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use parking_lot::Mutex;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};

/// Posts waiting or running at once, per webhook channel.
pub const MAX_PENDING: usize = 100;

/// Posts running at once, per webhook channel.
pub const WORKERS_PER_CHANNEL: usize = 2;

type Run = Box<dyn FnOnce(Arc<AppState>) -> BoxFuture<'static, ()> + Send>;

struct Job {
    run: Run,
    /// Returned when the job is dropped: after it ran, or unrun at shutdown.
    _permit: OwnedSemaphorePermit,
}

/// One channel's lane.
pub(crate) struct Lane {
    name: &'static str,
    capacity: usize,
    permits: Arc<Semaphore>,
    tx: mpsc::Sender<Job>,
}

impl Lane {
    /// A lane of `capacity` posts with its workers started on `ctx`.
    fn start(ctx: &Arc<AppState>, name: &'static str, capacity: usize) -> Arc<Lane> {
        let (tx, rx) = mpsc::channel::<Job>(capacity.max(1));
        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        for _ in 0..WORKERS_PER_CHANNEL {
            let rx = rx.clone();
            let weak = Arc::downgrade(ctx);
            ctx.spawn(work(name, rx, weak));
        }
        Arc::new(Lane {
            name,
            capacity,
            permits: Arc::new(Semaphore::new(capacity)),
            tx,
        })
    }

    /// Posts waiting or running.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.capacity - self.permits.available_permits()
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Queue `run`; false when the lane is full or closed.
    pub(crate) fn submit(&self, run: Run) -> bool {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            tracing::warn!(
                "The {} alert queue is full ({} waiting); an alert was dropped",
                self.name,
                self.capacity
            );
            return false;
        };
        // Never full while a permit was free: the channel holds as many
        // jobs as there are permits.
        self.tx
            .try_send(Job {
                run,
                _permit: permit,
            })
            .is_ok()
    }
}

/// One worker: take the next post and run it, until the lane is gone.
async fn work(
    name: &'static str,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Job>>>,
    ctx: Weak<AppState>,
) {
    loop {
        let job = rx.lock().await.recv().await;
        let Some(job) = job else { return };
        let Some(ctx) = ctx.upgrade() else { return };
        if AssertUnwindSafe((job.run)(ctx))
            .catch_unwind()
            .await
            .is_err()
        {
            tracing::error!("A {} alert send failed unexpectedly", name);
        }
    }
}

/// The two webhook channels' lanes, made on first use.
#[derive(Default)]
pub(crate) struct Lanes {
    slack: Mutex<Option<Arc<Lane>>>,
    discord: Mutex<Option<Arc<Lane>>>,
}

impl Lanes {
    /// The lane for `kind`, started on `ctx` the first time.
    pub(crate) fn get(&self, ctx: &Arc<AppState>, kind: WebhookKind) -> Arc<Lane> {
        let (slot, name) = match kind {
            WebhookKind::Slack => (&self.slack, "slack"),
            WebhookKind::Discord => (&self.discord, "discord"),
        };
        slot.lock()
            .get_or_insert_with(|| Lane::start(ctx, name, MAX_PENDING))
            .clone()
    }
}

/// Run `job` on `kind`'s lane; false when its queue is full.
pub(crate) fn submit<F, Fut>(ctx: &Arc<AppState>, kind: WebhookKind, job: F) -> bool
where
    F: FnOnce(Arc<AppState>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let lane = ctx.messaging.channels.lanes.get(ctx, kind);
    lane.submit(Box::new(move |ctx| job(ctx).boxed()))
}

#[cfg(test)]
pub(crate) fn probe(ctx: &Arc<AppState>, capacity: usize) -> Arc<Lane> {
    Lane::start(ctx, "probe", capacity)
}
