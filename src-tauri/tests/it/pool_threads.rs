//! CI flake e: an app context leaves no threads behind once it is dropped,
//! the HTTP app built on it included. Two causes kept them: the Socket.IO
//! layer held the context strongly while the context held the layer's
//! handle (a cycle: every context a test built the app on stayed alive with
//! its databases and their 13 threads), and each database pool's worker
//! threads outlived their pool for up to 30 s (r2d2's thread pool runs
//! every job still scheduled first, and its idle reaper always is). A test
//! process reached macOS's 2,048 threads per process, and the next pool
//! could not start its threads (`WouldBlock`).

use openalgo_desktop_lib::brokers::BrokerRegistry;
use openalgo_desktop_lib::clock::ManualClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::services::health_service::process_stats;
use openalgo_desktop_lib::state::{AppState, OpenOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Open an app context on a runtime of its own, build the HTTP app on it as
/// the server and the tests do, then drop all of them.
fn open_and_drop() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: Arc::new(MemoryKeyStore::new()),
                clock: ManualClock::new(chrono::Utc::now()),
                brokers: Arc::new(BrokerRegistry::with(vec![])),
            },
        )
        .unwrap();
        drop(openalgo_desktop_lib::server::app(ctx.clone()));
        let gone = Arc::downgrade(&ctx);
        drop(ctx);
        assert_eq!(gone.strong_count(), 0, "the context itself is dropped");
    });
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_dropped_app_context_leaves_no_threads_behind() {
    crate::isolated!(a_dropped_app_context_leaves_no_threads_behind);
    let threads = || process_stats().threads.expect("this system counts threads");
    // A first context starts what lives for the whole process.
    open_and_drop();
    let baseline = threads();
    for _ in 0..5 {
        open_and_drop();
    }
    // A pool's threads end just after it is dropped: wait for that, bounded.
    let deadline = Instant::now() + Duration::from_secs(10);
    while threads() > baseline + 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        threads() <= baseline + 2,
        "{} threads after five contexts were dropped, {} after the first",
        threads(),
        baseline
    );
}
