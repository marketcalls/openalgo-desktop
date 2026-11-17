//! A busy database must not stop scheduled workflows (web ceb116807,
//! issues #2355 and #2326). The web had two defects: recording a run left
//! the write lock held for the whole run (a no-op DELETE that was never
//! committed), and one refused write of a job's next run time ended the
//! scheduler's thread. Here a real second connection holds the write lock
//! (`BEGIN IMMEDIATE`), as the web's tests do. The next runs the database
//! refuses are held and written later (`services::next_run_saves`).

use super::*;
use crate::flow::store::executions;
use crate::services::tunnel::fake::{capture_logs, LogCapture};
use crate::state::testing::TestCtx;
use std::time::{Duration, Instant};

/// The next run recorded at registration (09:15 IST on 3 November) and
/// the ones after the next two fires.
const REGISTERED: &str = "2026-11-03T03:45:00";
const AFTER_ONE_FIRE: &str = "2026-11-04T03:45:00";
const AFTER_TWO_FIRES: &str = "2026-11-05T03:45:00";

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

/// Web test/test_flow_scheduler_resilience.py::test_recording_a_run_leaves_the_database_free_for_other_writers
/// and test/test_flow_scheduler_resilience.py::test_trimming_with_nothing_to_trim_leaves_the_database_free: recording
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

/// Web test/test_flow_scheduler_resilience.py::test_trimming_that_removes_rows_still_removes_them_and_lets_go.
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

/// Web test/test_flow_scheduler_resilience.py::test_a_database_locked_at_the_save_does_not_stop_the_scheduler,
/// test/test_flow_scheduler_resilience.py::test_a_run_is_dispatched_once_when_its_save_is_refused
/// and test/test_sqlite_lock_cooperative.py::test_a_scheduler_keeps_firing_while_the_lock_outlasts_the_budget:
/// two workflows are due while another connection holds the write lock
/// past the pool's whole busy budget (`db::sqlite::retry_busy`, 5 s).
/// Both still fire at once and once only (the driver never waits on the
/// save of a next run before dispatching, so one locked save cannot hold
/// every other schedule back), the driver goes on, and once the lock goes
/// each next run is saved, so a restart does not run a fire that has been
/// made.
#[tokio::test]
async fn schedules_fire_while_the_database_is_locked_and_are_saved_after() {
    let logs = capture_logs();
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

    let holder = drive_locked(&t, &s);
    let held = Instant::now();

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
    for id in ids {
        assert_eq!(dispatched(&logs, id), 1, "{}", logs.text());
    }
    s.stop().await;
}

/// Workflows with a daily 09:15 IST schedule, registered on a trigger side
/// of their own (the app's is stopped) at 09:14 on 3 November, the clock
/// then moved just past the fire. Inactive ones skip their runs, which then
/// write nothing.
async fn due_at_0915(t: &TestCtx, n: usize, active: bool) -> (Arc<TriggerSide>, Vec<i64>) {
    t.ctx.flow.triggers().stop().await;
    t.clock.set("2026-11-03T03:44:00Z".parse().unwrap());
    let s = side(t);
    let mut ids = Vec::new();
    for _ in 0..n {
        let id = save(&t.ctx, node("start", json!({})), active);
        let daily = data(json!({"scheduleType": "daily", "time": "09:15"}));
        s.register(id, "start", &daily).await.unwrap();
        ids.push(id);
    }
    t.clock.set("2026-11-03T03:45:01Z".parse().unwrap());
    (s, ids)
}

/// Start the driver while the connection returned holds the write lock
/// (taken as soon as the app's own background writes allow).
fn drive_locked(t: &TestCtx, s: &Arc<TriggerSide>) -> rusqlite::Connection {
    let holder = other_connection(t);
    holder.busy_timeout(Duration::from_secs(5)).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let driver = s.clone();
    s.spawn(async move { driver.drive_schedules().await });
    holder
}

/// How many times the workflow's scheduled run was dispatched.
fn dispatched(logs: &LogCapture, id: i64) -> usize {
    logs.text()
        .matches(&format!("Scheduled execution of workflow {id}\n"))
        .count()
}

/// Wait (bounded) until `done`.
async fn until(what: &str, logs: &LogCapture, done: impl Fn() -> bool) {
    let since = Instant::now();
    while !done() {
        assert!(
            since.elapsed() < Duration::from_secs(15),
            "{what}: {}",
            logs.text()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn next_run(t: &TestCtx, id: i64) -> Option<String> {
    store::next_run(&t.ctx.sqlite.conn().unwrap(), id).unwrap()
}

/// A next run whose save waited for the lock is not written once its job
/// no longer has it: here the schedule went while the save waited (as a
/// stand-down's does, before its own write). The row keeps what it had
/// instead of a fire the scheduler no longer has, which a restart would
/// read as due.
#[tokio::test]
async fn a_save_that_waited_writes_nothing_its_job_no_longer_has() {
    let logs = capture_logs();
    let t = app();
    let (s, ids) = due_at_0915(&t, 1, false).await;
    let id = ids[0];
    let holder = drive_locked(&t, &s);
    until("never dispatched", &logs, || dispatched(&logs, id) == 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.scheduler.remove(id));
    holder.execute_batch("COMMIT").unwrap();
    until("the save never settled", &logs, || s.next_runs.is_settled()).await;
    assert_eq!(next_run(&t, id).as_deref(), Some(REGISTERED));
    s.stop().await;
}

/// Two fires of one workflow while the database is locked: only the
/// newest next run is kept (the web's held state), and once the lock goes
/// the row ends on it, never on the fire made in between, whichever save
/// had started first.
#[tokio::test]
async fn two_fires_while_locked_leave_the_newest_next_run() {
    let logs = capture_logs();
    let t = app();
    let (s, ids) = due_at_0915(&t, 1, false).await;
    let id = ids[0];
    let holder = drive_locked(&t, &s);
    until("never dispatched", &logs, || dispatched(&logs, id) == 1).await;
    t.clock.set("2026-11-04T03:45:01Z".parse().unwrap());
    s.wake_scheduler.notify_one();
    until("the second fire", &logs, || dispatched(&logs, id) == 2).await;
    holder.execute_batch("COMMIT").unwrap();
    until("the saves never settled", &logs, || {
        s.next_runs.is_settled()
    })
    .await;
    assert_eq!(next_run(&t, id).as_deref(), Some(AFTER_TWO_FIRES));
    s.stop().await;
}

/// More schedules fall due at once than the pool has connections (8),
/// while the database is locked. Their next runs are saved one at a time,
/// so a read, which the lock does not block, still gets a connection at
/// once; a save per due job took every connection for the busy budget.
#[tokio::test]
async fn next_runs_refused_by_a_locked_database_leave_the_pool_free() {
    let logs = capture_logs();
    let t = app();
    let (s, ids) = due_at_0915(&t, 10, false).await;
    let holder = drive_locked(&t, &s);
    until("never dispatched", &logs, || {
        ids.iter().all(|id| dispatched(&logs, *id) == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let asked = Instant::now();
    let read = t.ctx.sqlite.conn().unwrap();
    let waited = asked.elapsed();
    drop(read);
    assert!(
        waited < Duration::from_secs(1),
        "a read waited {waited:?} for a connection"
    );
    holder.execute_batch("COMMIT").unwrap();
    until("the saves never settled", &logs, || {
        s.next_runs.is_settled()
    })
    .await;
    for id in ids {
        assert_eq!(next_run(&t, id).as_deref(), Some(AFTER_ONE_FIRE));
    }
    s.stop().await;
}

/// Web test/test_flow_scheduler_resilience.py::test_stopping_with_a_schedule_still_refused_says_so:
/// the triggers stop while a next run is still refused. The warning names
/// the workflow, once, and the stop waits for the database no longer than
/// the last round's bound.
#[tokio::test]
async fn stopping_with_a_next_run_still_refused_says_so() {
    let logs = capture_logs();
    let t = app();
    let (s, ids) = due_at_0915(&t, 1, false).await;
    let id = ids[0];
    let holder = drive_locked(&t, &s);
    until("never dispatched", &logs, || dispatched(&logs, id) == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stopping = Instant::now();
    s.stop().await;
    assert!(stopping.elapsed() < Duration::from_secs(8));
    let said = logs
        .text()
        .matches(&format!(
            "Scheduler: stopped before the database took the next run of workflow {id}. A run of theirs made in the last minute may be made again on the next start."
        ))
        .count();
    assert_eq!(said, 1, "{}", logs.text());
    holder.execute_batch("COMMIT").unwrap();
    assert_eq!(next_run(&t, id).as_deref(), Some(REGISTERED));
}

/// Web test/test_flow_scheduler_resilience.py::test_what_is_held_is_saved_when_the_scheduler_stops:
/// a next run refused while the database was locked, and not tried again
/// yet, is written by the last round at stop.
#[tokio::test]
async fn what_is_held_is_saved_when_the_triggers_stop() {
    let logs = capture_logs();
    let t = app();
    let (s, ids) = due_at_0915(&t, 1, false).await;
    let id = ids[0];
    let holder = drive_locked(&t, &s);
    let refused = format!("could not save the next run of workflow {id} (");
    until("never refused", &logs, || logs.text().contains(&refused)).await;
    // The saver tries again a second after the refusal; the stop comes first.
    holder.execute_batch("COMMIT").unwrap();
    s.stop().await;
    assert_eq!(next_run(&t, id).as_deref(), Some(AFTER_ONE_FIRE));
    assert!(!logs.text().contains("stopped before"), "{}", logs.text());
}
