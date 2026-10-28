//! The log reports (traffic, order, health, analyzer and latency logs) are
//! built a few at a time. Each one reads and formats thousands of rows on
//! a blocking thread, which keeps a core busy for its whole build. Sixteen
//! at once (two tabs opening every report page) took every core of a
//! three-core machine, and the async worker that carries the feed and the
//! risk owners waited for one: 244-317 ms on a macOS CI runner, 300-550 ms
//! on a Mac's four efficiency cores (`tests/it/webui_reports.rs`). A report
//! waiting its turn waits on the async side, which holds no thread.

use crate::error::{AppError, Result};
use crate::state::AppState;
use std::num::NonZeroUsize;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The reports that may be built at once ([`slots`]).
pub struct ReportGate(Arc<Semaphore>);

impl Default for ReportGate {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self(Arc::new(Semaphore::new(slots(cores))))
    }
}

/// Two builds at once, and one on a two-core machine, so a core is always
/// left for the async workers. A report needs no more: it is one page a
/// trader reads.
fn slots(cores: usize) -> usize {
    cores.saturating_sub(1).clamp(1, 2)
}

impl ReportGate {
    /// Wait for a slot. The slot is held until the permit is dropped.
    async fn enter(&self) -> Result<OwnedSemaphorePermit> {
        self.0
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Internal("The report gate is closed".into()))
    }
}

impl AppState {
    /// [`AppState::blocking`] for a log report: it waits for a slot of the
    /// [`ReportGate`] first, and the build holds the slot until it ends,
    /// even when the request that asked for it is gone.
    pub async fn report<T, F>(self: &Arc<Self>, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&AppState) -> Result<T> + Send + 'static,
    {
        let slot = self.reports.enter().await?;
        self.blocking(move |c| {
            let _slot = slot;
            f(c)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_core_is_always_left_for_the_async_workers() {
        assert_eq!(slots(1), 1);
        assert_eq!(slots(2), 1);
        assert_eq!(slots(3), 2);
        assert_eq!(slots(8), 2);
    }

    #[tokio::test]
    async fn a_third_report_waits_for_a_slot() {
        let gate = ReportGate(Arc::new(Semaphore::new(slots(3))));
        let first = gate.enter().await.unwrap();
        let _second = gate.enter().await.unwrap();
        let third = tokio::time::timeout(Duration::from_millis(50), gate.enter()).await;
        assert!(third.is_err(), "a third build started beside two");
        drop(first);
        let third = tokio::time::timeout(Duration::from_secs(5), gate.enter()).await;
        assert!(third.is_ok_and(|p| p.is_ok()), "a freed slot was not taken");
    }
}
