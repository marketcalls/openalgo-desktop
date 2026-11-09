//! The subscription worker: one task per feed connection that runs the
//! feed's subscription commands in order (`run_command`) and sends a batch
//! that failed for a passing reason again (BF-03).

use super::release::Waited;
use super::{run_command, Command, CommandReport, SubscriptionTarget, SUBSCRIBE_GAP};
use crate::security::Secret;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// First wait before a failed batch is sent again; doubled per attempt.
const RETRY_BASE: Duration = Duration::from_secs(1);
/// Longest wait between retries of a failed batch.
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Attempts after the first before a batch is given up (the next
/// reconnect subscribes it again).
pub(crate) const RETRY_ATTEMPTS: u32 = 5;

/// A failed batch waiting to be sent again.
struct Pending {
    cmd: Command,
    attempt: u32,
    due: tokio::time::Instant,
}

/// Run commands in order until the queue closes, sending failed batches
/// again with doubling waits from `retry_base` up to `RETRY_MAX`, at most
/// `RETRY_ATTEMPTS` times (BF-03). A newer command for the same
/// instruments and message code replaces their pending retry, so a failed
/// unsubscribe sent again never undoes a later subscribe, nor a failed
/// subscribe a later unsubscribe. A refused token sets `reconnect`,
/// which the feed turns into a reconnect. Ends (and so cancels every
/// pending retry) when the feed drops the queue or aborts the task.
pub(crate) async fn run_commands(
    t: &SubscriptionTarget<'_>,
    mut rx: mpsc::Receiver<Command>,
    snapshots: &mpsc::Sender<String>,
    reconnect: &AtomicBool,
    retry_base: Duration,
) {
    let mut first = true;
    let mut pending: std::collections::VecDeque<Pending> = std::collections::VecDeque::new();
    loop {
        let (cmd, attempt) = match pending.front().map(|p| p.due) {
            Some(due) => tokio::select! {
                biased;
                c = rx.recv() => match c {
                    Some(c) => (c, 0),
                    None => return,
                },
                _ = tokio::time::sleep_until(due) => match pending.pop_front() {
                    Some(p) => (p.cmd, p.attempt),
                    None => continue,
                },
            },
            None => match rx.recv().await {
                Some(c) => (c, 0),
                None => return,
            },
        };
        for p in pending.iter_mut().filter(|p| p.cmd.code == cmd.code) {
            p.cmd.instruments.retain(|i| !cmd.instruments.contains(i));
        }
        drop_replaced(&mut pending);
        let report = run_command(t, &cmd, snapshots, &mut first).await;
        if report.reconnect {
            reconnect.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let release = old_code_to_release(&cmd, &report);
        let released_for_a_caller = release.as_ref().is_some_and(|r| r.waited.is_some());
        let sent_again =
            cmd.waited.as_ref().is_some_and(Waited::admits) && !report.retry.is_empty();
        if let Some(waited) = &cmd.waited {
            waited.ran(&report, released_for_a_caller || sent_again);
        }
        if let Some(release) = release {
            let due = tokio::time::Instant::now();
            schedule(
                &mut pending,
                Pending {
                    cmd: release,
                    attempt: 0,
                    due,
                },
            );
        }
        if cmd.waited.as_ref().is_some_and(|w| !w.admits()) {
            // Its caller keeps the subscription and asks again (C01).
            continue;
        }
        if report.retry.is_empty() {
            continue;
        }
        if attempt >= RETRY_ATTEMPTS {
            tracing::warn!(
                broker = t.broker,
                instruments = report.retry.len(),
                "Subscription still failing after {} retries; the next reconnect sends it again",
                RETRY_ATTEMPTS
            );
            if let Some(waited) = &cmd.waited {
                waited.gave_up(&report.retry);
            }
            continue;
        }
        let wait = retry_base
            .saturating_mul(1u32 << attempt.min(16))
            .min(RETRY_MAX);
        let p = Pending {
            cmd: Command {
                subscribe: cmd.subscribe,
                code: cmd.code,
                instruments: report.retry,
                // A retry report is not an answer: the waiter goes with it.
                waited: cmd.waited.clone(),
                then_release: cmd.then_release,
            },
            attempt: attempt + 1,
            due: tokio::time::Instant::now() + wait,
        };
        schedule(&mut pending, p);
    }
}

/// Forget the retries a newer command replaced whole; a waiter on one is
/// told nothing was refused (the newer command decides).
fn drop_replaced(pending: &mut std::collections::VecDeque<Pending>) {
    for p in pending.iter().filter(|p| p.cmd.instruments.is_empty()) {
        if let Some(waited) = &p.cmd.waited {
            waited.ran(&CommandReport::default(), false);
        }
    }
    pending.retain(|p| !p.cmd.instruments.is_empty());
}

/// `p` among what waits, in the order it falls due.
fn schedule(pending: &mut std::collections::VecDeque<Pending>, p: Pending) {
    let at = pending
        .iter()
        .position(|q| q.due > p.due)
        .unwrap_or(pending.len());
    pending.insert(at, p);
}

/// A mode change's release of its old message code, for the instruments
/// the broker took at the new one: what it refused keeps the old code, and
/// what is to be sent again goes when that send is taken (review X2). A
/// release's caller waits on this step too (a downgrade, review B3).
fn old_code_to_release(cmd: &Command, report: &CommandReport) -> Option<Command> {
    let code = cmd.then_release?;
    let instruments: Vec<serde_json::Value> = cmd
        .instruments
        .iter()
        .filter(|i| !report.retry.contains(i) && !report.refused.iter().any(|(r, _)| r == *i))
        .cloned()
        .collect();
    (!instruments.is_empty()).then_some(Command {
        subscribe: false,
        code,
        instruments,
        waited: cmd.waited.clone().filter(|w| !w.admits()),
        then_release: None,
    })
}

/// What the worker task owns: where its calls go, and where it hands the
/// subscribe answers' snapshots and a needed resync back to the feed.
pub(super) struct WorkerSetup {
    pub(super) broker: &'static str,
    /// `XtsHooks::split_duplicate_batch` of the member.
    pub(super) split_duplicates: bool,
    pub(super) http: reqwest::Client,
    pub(super) url: String,
    pub(super) token: Secret,
    pub(super) snapshots: mpsc::Sender<String>,
    pub(super) resync: Arc<AtomicBool>,
}

/// The task the feed spawns: runs its commands until the feed drops the
/// queue or aborts the task.
pub(super) async fn subscription_worker(setup: WorkerSetup, rx: mpsc::Receiver<Command>) {
    let target = SubscriptionTarget {
        broker: setup.broker,
        http: &setup.http,
        url: &setup.url,
        token: setup.token.expose(),
        gap: SUBSCRIBE_GAP,
        split_duplicates: setup.split_duplicates,
    };
    run_commands(&target, rx, &setup.snapshots, &setup.resync, RETRY_BASE).await;
}
