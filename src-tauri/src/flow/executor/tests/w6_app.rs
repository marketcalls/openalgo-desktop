//! Wave 6: Flow at the app's start and quit (E-42, S-30): a run the last
//! session left `running` is closed when the app opens, and the quit waits
//! for runs only as long as their grace period.

use super::w6_run::{app, history, key, save, NOW};
use super::w6_walk::level_of;
use super::*;
use crate::security::Unlocked;
use crate::services::tunnel::fake::capture_logs;
use std::time::Duration;

/// E-42 (quit): quitting gives runs the grace period to write their
/// records, then ends a run that does not stop, so the quit never waits
/// longer than that on Flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_waits_for_runs_to_record_and_ends_the_rest() {
    let t = app();
    let nodes = json!([
        node("start", "start", json!({})),
        node("d", "delay", json!({"delayValue": 60})),
    ]);
    let id = save(&t.ctx, "A", nodes, json!([edge("start", "d", None)]));
    assert_eq!(execute(&t.ctx, id, key()).await["accepted"], true);
    // A run that never looks at its stop token.
    t.ctx.flow.spawn(std::future::pending());
    assert_eq!(t.ctx.flow.running(), 2);
    let logs = capture_logs();
    let started = std::time::Instant::now();
    let quit = tokio::time::timeout(Duration::from_secs(60), t.ctx.shutdown()).await;
    assert!(quit.is_ok(), "the quit waited on a run that never stops");
    assert!(started.elapsed() >= crate::flow::STOP_GRACE);
    assert_eq!(t.ctx.flow.running(), 0);
    assert_eq!(history(&t.ctx, id)[0].status, "failed");
    assert_eq!(
        level_of(&logs.text(), "Flow runs did not stop in time; ending them").as_deref(),
        Some("WARN"),
        "{}",
        logs.text()
    );
}

/// S-30, E-42 (start): opening the app over a run the last session left
/// `running` closes it as failed with the interrupted sentence and the
/// opening time, and says so in the log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_the_app_closes_a_run_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn crate::security::keystore::KeyStore> =
        Arc::new(crate::security::keystore::MemoryKeyStore::new());
    let clock = crate::clock::ManualClock::new(NOW.parse().unwrap());
    let open = || {
        AppState::open(
            dir.path(),
            crate::state::OpenOptions {
                keystore: keys.clone(),
                clock: clock.clone(),
                brokers: Arc::new(crate::brokers::BrokerRegistry::new()),
            },
        )
        .unwrap()
    };
    let first = open();
    first.security.install(&Unlocked::random().unwrap());
    first.flow.triggers().stop().await;
    let id = save(
        &first,
        "A",
        json!([node("start", "start", json!({}))]),
        json!([]),
    );
    let run = executions::create(&first.sqlite.conn().unwrap(), id, first.now()).unwrap();
    first.shutdown().await;
    for _ in 0..1000 {
        if Arc::strong_count(&first) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(Arc::strong_count(&first), 1, "app state outlives shutdown");
    drop(first);
    clock.advance(chrono::Duration::minutes(5));
    let logs = capture_logs();
    let again = open();
    let row = &history(&again, id)[0];
    assert_eq!(row.id, run);
    assert_eq!(
        (row.status.as_str(), row.error.as_deref()),
        ("failed", Some(executions::INTERRUPTED))
    );
    assert_eq!(row.completed_at.as_deref(), Some("2026-10-30T03:50:00"));
    assert_eq!(
        level_of(
            &logs.text(),
            "Closed 1 Flow run(s) the last session left unfinished"
        )
        .as_deref(),
        Some("INFO"),
        "{}",
        logs.text()
    );
    again.shutdown().await;
}
