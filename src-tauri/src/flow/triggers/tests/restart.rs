//! A restart from the stored next run (parity T-003, T-002, lead decision
//! 7): the start-up restore reads each active schedule's `next_run_at` as
//! the last session wrote it, so a fire that fell due while OpenAlgo was
//! closed runs once within the 60 s grace and is skipped after it; and the
//! driver writes the job's next fire back to the row after every fire
//! (`persist_next`), so the next restart starts from it.

use super::*;

fn daily_at_0915() -> Value {
    node("start", json!({"scheduleType": "daily", "time": "09:15"}))
}

/// The app with its own trigger side stopped before any workflow exists:
/// the app restores the active workflows on a task of its own at start-up,
/// and that restore would rewrite the stored next runs below.
async fn app_without_its_trigger_side() -> crate::state::testing::TestCtx {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    t
}

/// The row's next run as the last session stored it (UTC, the column's
/// own text form).
fn store_next_run(app: &Arc<AppState>, id: i64, stored: &str) {
    app.sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE flow_workflows SET next_run_at = ?1 WHERE id = ?2",
            rusqlite::params![stored, id],
        )
        .unwrap();
}

fn next_run(app: &Arc<AppState>, id: i64) -> Option<String> {
    store::next_run(&app.sqlite.conn().unwrap(), id).unwrap()
}

fn runs(app: &Arc<AppState>, id: i64) -> Vec<crate::flow::contract::model::Execution> {
    crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 10).unwrap()
}

/// Run the scheduler's driver until the row records `next` and, when
/// `fired`, the workflow has a run; then stop the side.
async fn drive_until(s: &Arc<TriggerSide>, id: i64, next: &str, fired: bool) {
    let app = s.app().unwrap();
    let driver = s.clone();
    s.spawn(async move { driver.drive_schedules().await });
    for _ in 0..500 {
        let recorded = next_run(&app, id).as_deref() == Some(next);
        if recorded && (!fired || !runs(&app, id).is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    s.stop().await;
}

/// The last session stored today's 09:15 IST fire as the next run and
/// OpenAlgo opens at 09:15:40 IST: the restore makes that fire due, the
/// driver runs it (skipped here for want of an API key, which shows it
/// fired rather than being missed) and records tomorrow's 09:15 on the
/// row. A schedule with no stored next run catches nothing up.
#[tokio::test]
async fn a_restart_runs_the_stored_fire_within_the_grace_and_records_the_next() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app_without_its_trigger_side().await;
    let stored = save(&t.ctx, daily_at_0915(), true);
    let unstored = save(&t.ctx, daily_at_0915(), true);
    store_next_run(&t.ctx, stored, "2026-11-03 03:45:00");
    t.clock.set("2026-11-03T03:45:40Z".parse().unwrap());
    let s = side(&t);
    s.restore().await;
    assert_eq!(
        next_run(&t.ctx, stored).as_deref(),
        Some("2026-11-03T03:45:00"),
        "the stored fire is still due"
    );
    assert_eq!(
        next_run(&t.ctx, unstored).as_deref(),
        Some("2026-11-04T03:45:00")
    );
    drive_until(&s, stored, "2026-11-04T03:45:00", true).await;
    assert_eq!(
        next_run(&t.ctx, stored).as_deref(),
        Some("2026-11-04T03:45:00"),
        "the driver records the next fire"
    );
    let fired = runs(&t.ctx, stored);
    assert_eq!(fired.len(), 1);
    assert_eq!(
        fired[0].error.as_deref(),
        Some("Skipped: there is no API key, so the workflow did not run when this schedule fired. Generate an API key in OpenAlgo.")
    );
    assert!(runs(&t.ctx, unstored).is_empty());
    let text = logs.text();
    assert!(
        text.contains(&format!("Scheduled execution of workflow {}", stored)),
        "{}",
        text
    );
    assert!(!text.contains("was missed by more than"), "{}", text);
}

/// The stored 09:15 IST fire and OpenAlgo opening at 09:20 IST, five
/// minutes after it: the fire is skipped with the warning naming it, the
/// row records tomorrow's 09:15 and nothing is due now.
#[tokio::test]
async fn a_restart_past_the_grace_skips_the_stored_fire_and_says_so() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app_without_its_trigger_side().await;
    let id = save(&t.ctx, daily_at_0915(), true);
    store_next_run(&t.ctx, id, "2026-11-03 03:45:00");
    t.clock.set("2026-11-03T03:50:00Z".parse().unwrap());
    let s = side(&t);
    s.restore().await;
    assert_eq!(next_run(&t.ctx, id).as_deref(), Some("2026-11-04T03:45:00"));
    assert!(s.scheduler.tick(t.ctx.now()).is_empty());
    let warning = format!(
        "Flow schedule flow_workflow_{} missed its run at 2026-11-03 09:15:00 IST while OpenAlgo was closed; it is skipped",
        id
    );
    assert!(logs.text().contains(&warning), "{}", logs.text());
}

/// `persist_next`: registered at 09:14 IST, the schedule records today's
/// 09:15; once the driver fires it, the row holds tomorrow's 09:15, the
/// value the next start-up restores from.
#[tokio::test]
async fn the_driver_records_the_next_fire_on_the_row_after_each_fire() {
    let t = app_without_its_trigger_side().await;
    t.clock.set("2026-11-03T03:44:00Z".parse().unwrap());
    let id = save(&t.ctx, daily_at_0915(), true);
    let s = side(&t);
    s.register(
        id,
        "start",
        &data(json!({"scheduleType": "daily", "time": "09:15"})),
    )
    .await
    .unwrap();
    assert_eq!(next_run(&t.ctx, id).as_deref(), Some("2026-11-03T03:45:00"));
    t.clock.set("2026-11-03T03:45:10Z".parse().unwrap());
    drive_until(&s, id, "2026-11-04T03:45:00", false).await;
    assert_eq!(next_run(&t.ctx, id).as_deref(), Some("2026-11-04T03:45:00"));
}
