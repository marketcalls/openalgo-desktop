//! A busy database must not stop scheduled workflows (web ceb116807,
//! issues #2355 and #2326). The web had two defects: recording a run left
//! the write lock held for the whole run (a no-op DELETE that was never
//! committed), and one refused write of a job's next run time ended the
//! scheduler's thread. Here a real second connection holds the write lock
//! (`BEGIN IMMEDIATE`), as the web's tests do.

use super::*;
use crate::flow::store::executions;
use std::time::{Duration, Instant};

/// A second connection to the app's `openalgo.db` that gives up at once.
fn other_connection(t: &crate::state::testing::TestCtx) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(t.dir.path().join("openalgo.db")).unwrap();
    c.busy_timeout(std::time::Duration::ZERO).unwrap();
    c
}

/// Whether another connection can take the write lock now.
fn can_write(t: &crate::state::testing::TestCtx) -> bool {
    let other = other_connection(t);
    match other.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => {
            other.execute_batch("ROLLBACK").unwrap();
            true
        }
        Err(e) => {
            assert!(e.to_string().contains("locked"), "{e}");
            false
        }
    }
}

/// Web test_recording_a_run_leaves_the_database_free_for_other_writers and
/// test_trimming_with_nothing_to_trim_leaves_the_database_free: recording
/// a run trims the workflow's history, and with nothing to trim the
/// connection that recorded it (still checked out, as a run's would be)
/// holds nothing.
#[tokio::test]
async fn recording_a_run_leaves_the_database_free_for_other_writers() {
    let t = app();
    let id = save(&t.ctx, node("start", json!({})), true);
    let conn = t.ctx.sqlite.conn().unwrap();
    executions::create(&conn, id, t.ctx.now()).unwrap();
    assert!(conn.is_autocommit(), "a transaction was left open");
    assert!(can_write(&t), "the run still holds the database write lock");
    assert_eq!(executions::prune(&conn, id, t.ctx.now()).unwrap(), 0);
    assert!(can_write(&t));
}

/// Web test_trimming_that_removes_rows_still_removes_them_and_lets_go.
#[tokio::test]
async fn trimming_that_removes_rows_still_lets_go() {
    let t = app();
    let id = save(&t.ctx, node("start", json!({})), true);
    let conn = t.ctx.sqlite.conn().unwrap();
    let old = t.ctx.now() - chrono::Duration::days(executions::RETENTION_DAYS + 1);
    executions::create(&conn, id, old).unwrap();
    executions::create(&conn, id, old).unwrap();
    executions::create(&conn, id, t.ctx.now()).unwrap();
    assert_eq!(executions::list(&conn, id, 10).unwrap().len(), 1);
    assert!(conn.is_autocommit());
    assert!(can_write(&t));
}

/// Web test_a_database_locked_at_the_save_does_not_stop_the_scheduler and
/// test_a_scheduler_keeps_firing_while_the_lock_outlasts_the_budget: two
/// workflows are due while another connection holds the write lock past
/// the pool's whole busy budget (`db::sqlite::retry_busy`, 5 s). Both
/// still fire at once (the driver never waits on the save of a next run
/// before dispatching, so one locked save cannot hold every other
/// schedule back), the driver goes on, and once the lock goes each next
/// run is saved, so a restart does not run a fire that has been made.
#[tokio::test]
async fn schedules_fire_while_the_database_is_locked_and_are_saved_after() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app();
    t.ctx.flow.triggers().stop().await;
    t.clock.set("2026-11-03T03:44:00Z".parse().unwrap());
    let daily = || data(json!({"scheduleType": "daily", "time": "09:15"}));
    let s = side(&t);
    let ids = [
        save(&t.ctx, node("start", json!({})), true),
        save(&t.ctx, node("start", json!({})), true),
    ];
    for id in ids {
        s.register(id, "start", &daily()).await.unwrap();
    }
    t.clock.set("2026-11-03T03:45:01Z".parse().unwrap());

    let holder = other_connection(&t);
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let held = Instant::now();
    let driver = s.clone();
    s.spawn(async move { driver.drive_schedules().await });

    let fired = |id: i64| {
        logs.text()
            .contains(&format!("Scheduled execution of workflow {id}"))
    };
    while !ids.iter().all(|id| fired(*id)) && held.elapsed() < Duration::from_secs(4) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        ids.iter().all(|id| fired(*id)),
        "a locked save held the schedules back: {}",
        logs.text()
    );
    assert!(held.elapsed() < Duration::from_secs(4));

    // Past the pool's busy budget, so the first save of each next run fails.
    tokio::time::sleep(Duration::from_millis(6_500).saturating_sub(held.elapsed())).await;
    holder.execute_batch("COMMIT").unwrap();
    let saved = |id: i64| {
        store::next_run(&t.ctx.sqlite.conn().unwrap(), id)
            .unwrap()
            .as_deref()
            == Some("2026-11-04T03:45:00")
    };
    let released = Instant::now();
    while !ids.iter().all(|id| saved(*id)) && released.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        ids.iter().all(|id| saved(*id)),
        "a next run refused while the database was locked was never saved: {}",
        logs.text()
    );
    s.stop().await;
}
