//! A turn for the writer that is waiting.
//!
//! SQLite keeps no queue of writers. A waiting statement polls for the lock
//! (`retry_busy`), and a thread that has just committed can begin its next
//! write between two polls, again and again. On a Windows runner a write
//! polled about 3,300 times over the whole `BUSY_TIMEOUT` without once
//! finding the lock free, and failed "database is locked". An exit whose
//! order row cannot be written is not sent, so a thread taking a connection
//! while a statement is waiting steps back for a moment first.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A statement that polled this recently is still waiting: it polls about
/// every 1.5 ms.
const WAITING_WITHIN_MS: u64 = 5;

/// How long a thread steps back for a waiting statement: long enough for
/// that statement's next poll.
const STEP_BACK: Duration = Duration::from_millis(2);

/// When a statement last polled for the lock, from [`now_ms`]; 0 until one
/// has. One mark serves every database, since a checkout pays at most
/// [`STEP_BACK`] for it.
static LAST_POLL_MS: AtomicU64 = AtomicU64::new(0);

/// Milliseconds since the first call, counted from 1 so that 0 stays "never".
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(elapsed).unwrap_or(u64::MAX).saturating_add(1)
}

/// Mark that a statement is polling for the write lock.
pub(super) fn note_poll() {
    LAST_POLL_MS.store(now_ms(), Ordering::Relaxed);
}

fn someone_is_waiting(last_poll: u64, now: u64) -> bool {
    last_poll != 0 && now.saturating_sub(last_poll) < WAITING_WITHIN_MS
}

/// Step back if a statement is waiting for the write lock. With none
/// waiting this is one atomic read.
pub(super) fn give_way() {
    if someone_is_waiting(LAST_POLL_MS.load(Ordering::Relaxed), now_ms()) {
        std::thread::sleep(STEP_BACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_poll_just_now_means_someone_is_waiting() {
        assert!(!someone_is_waiting(0, 3), "no statement has polled yet");
        assert!(someone_is_waiting(100, 100));
        assert!(someone_is_waiting(100, 104));
        assert!(!someone_is_waiting(100, 105), "the waiter has gone");
    }

    #[test]
    fn a_statement_waiting_for_the_lock_leaves_its_mark() {
        assert!(super::super::retry_busy(0));
        assert_ne!(LAST_POLL_MS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_checkout_steps_back_while_a_statement_waits() {
        let dir = tempfile::tempdir().unwrap();
        let db = super::super::SqliteDb::new(&dir.path().join("turn.db")).unwrap();
        // A mark from the future reads as "just now" however long this
        // thread is held off the processor before it checks out.
        LAST_POLL_MS.store(u64::MAX, Ordering::Relaxed);
        let started = Instant::now();
        drop(db.conn().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(1));
    }
}
