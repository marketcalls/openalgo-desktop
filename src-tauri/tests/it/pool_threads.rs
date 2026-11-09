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

/// fd-audit of the stores' connection manager (`db::sqlite::manager`,
/// 1.0.7): a store's pool opened, used and dropped a hundred times leaves
/// no descriptor open. Each pooled connection holds the database and its
/// `-wal`, so a connection a dropped pool kept would add at least two
/// descriptors a round.
#[test]
fn a_dropped_store_pool_leaves_no_descriptors_behind() {
    crate::isolated!(a_dropped_store_pool_leaves_no_descriptors_behind);
    use crate::feed_support::fd_count;
    use openalgo_desktop_lib::db::sqlite::{open_pool, Durability};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let round = || {
        let pool = open_pool(&path, 4, Durability::MAIN).unwrap();
        let held: Vec<_> = (0..4).map(|_| pool.get().unwrap()).collect();
        held[0]
            .execute_batch("CREATE TABLE IF NOT EXISTS t (v INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
    };
    // A pool's last open can still be finishing on its worker thread just
    // after the pool is dropped: the baseline is a count two reads 20 ms
    // apart agree on, and the count after is waited for, bounded.
    round();
    let mut baseline = fd_count();
    for _ in 0..500 {
        std::thread::sleep(Duration::from_millis(20));
        let now = fd_count();
        if now == baseline {
            break;
        }
        baseline = now;
    }
    for _ in 0..100 {
        round();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while fd_count() > baseline && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let after = fd_count();
    assert!(
        after <= baseline,
        "{} descriptors open after 100 pools were dropped, {} after the first",
        after,
        baseline
    );
}
