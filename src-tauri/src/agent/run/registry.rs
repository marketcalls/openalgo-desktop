//! The turns running now, so one can be stopped (the cancel route, a
//! client that went away) and all of them can be (sign-out, lock, account
//! reset, quit).
//!
//! An entry lives exactly as long as its run's task: the task holds a
//! [`Registered`] guard that removes it on every exit path. The table is
//! bounded by the stream cap ([`crate::agent::stream::MAX_CONCURRENT_STREAMS`]),
//! since every run holds a stream slot.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Entry {
    cancel: CancellationToken,
    /// The browser session that started it (D-015: only it may stop it).
    session: String,
}

#[derive(Default)]
pub struct RunRegistry {
    runs: Mutex<HashMap<String, Entry>>,
}

/// Removes its run from the registry when dropped.
pub struct Registered {
    registry: Arc<RunRegistry>,
    run_id: String,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.registry.runs.lock().remove(&self.run_id);
    }
}

impl RunRegistry {
    /// Record a run; the guard removes it.
    pub fn register(
        self: &Arc<Self>,
        run_id: &str,
        session: &str,
        cancel: CancellationToken,
    ) -> Registered {
        self.runs.lock().insert(
            run_id.to_string(),
            Entry {
                cancel,
                session: session.to_string(),
            },
        );
        Registered {
            registry: self.clone(),
            run_id: run_id.to_string(),
        }
    }

    /// Stop a run started by `session`; anything else is ignored (the web
    /// answers success either way, so live run ids cannot be probed).
    pub fn cancel(&self, run_id: &str, session: &str) -> bool {
        match self.runs.lock().get(run_id) {
            Some(e) if e.session == session => {
                e.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Stop every run (sign-out, lock, reset, quit).
    pub fn cancel_all(&self) -> usize {
        let runs = self.runs.lock();
        for e in runs.values() {
            e.cancel.cancel();
        }
        runs.len()
    }

    pub fn len(&self) -> usize {
        self.runs.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-015: the web cancelled any run id from any session.
    #[test]
    fn only_the_starting_session_can_stop_a_run_and_the_guard_removes_it() {
        let reg = Arc::new(RunRegistry::default());
        let token = CancellationToken::new();
        let guard = reg.register("r1", "session-a", token.clone());
        assert!(!reg.cancel("r1", "session-b"));
        assert!(!token.is_cancelled());
        assert!(reg.cancel("r1", "session-a"));
        assert!(token.is_cancelled());
        drop(guard);
        assert!(reg.is_empty());
    }

    #[test]
    fn every_run_stops_at_sign_out() {
        let reg = Arc::new(RunRegistry::default());
        let (a, b) = (CancellationToken::new(), CancellationToken::new());
        let _ga = reg.register("a", "s", a.clone());
        let _gb = reg.register("b", "t", b.clone());
        assert_eq!(reg.cancel_all(), 2);
        assert!(a.is_cancelled() && b.is_cancelled());
    }
}
