//! web: test/test_strategy_module_scheduler.py

use super::*;
use openalgo_desktop_lib::strategy::scheduler::{
    planned_jobs, start_job_id, stop_job_id, JobFunc, MISFIRE_GRACE, TIMEZONE,
};

fn scheduler(enabled: bool, start: Option<&str>, stop: Option<&str>) -> Value {
    json!({"enabled": enabled, "days": ["MON", "TUE", "WED", "THU", "FRI"],
           "start_time": start, "auto_stop_time": stop, "default_mode": "sandbox"})
}

fn make(t: &T, overrides: Value) -> i64 {
    t.make(config("Sched", json!([short_call_leg()]), overrides))
}

#[test]
fn the_timezone_reaches_every_trigger() {
    // PORTED DEFECT. Flow's and Historify's cron jobs carry no timezone,
    // so a 09:20 IST entry fires at server-local 09:20.
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_strategy_jobs(sid);
    for id in [start_job_id(sid), stop_job_id(sid)] {
        assert_eq!(t.m.scheduler.get(&id).unwrap().timezone, TIMEZONE);
    }
    assert_eq!(TIMEZONE, "Asia/Kolkata");
}

#[test]
fn every_job_carries_the_project_job_defaults() {
    // PORTED DEFECT. python_strategy inherits a 1 s misfire grace, so a
    // 09:15 entry that slips two seconds is dropped silently.
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_strategy_jobs(sid);
    for j in t.m.scheduler.jobs() {
        assert!(j.coalesce);
        assert_eq!(j.max_instances, 1);
        assert_eq!(j.misfire_grace, MISFIRE_GRACE);
        assert_eq!(MISFIRE_GRACE.as_secs(), 60);
    }
}

#[test]
fn jobs_are_plain_values_with_plain_arguments() {
    // PORTED DEFECT. python_strategy scheduled closures no store can hold.
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_strategy_jobs(sid);
    let start = t.m.scheduler.get(&start_job_id(sid)).unwrap();
    let stop = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
    assert_eq!(
        (start.func, start.strategy_id),
        (JobFunc::RunScheduledStart, sid)
    );
    assert_eq!(
        (stop.func, stop.strategy_id),
        (JobFunc::RunScheduledStop, sid)
    );
    assert_eq!(
        (start.hour, start.minute, stop.hour, stop.minute),
        (9, 20, 15, 10)
    );
}

#[test]
fn an_invalid_time_is_skipped_rather_than_installed() {
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("9:70"), Some("not a time"))}),
    );
    assert!(t.m.sync_strategy_jobs(sid).is_empty());
}

#[test]
fn a_broken_stop_time_does_not_take_the_start_job_down_with_it() {
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("25:00"))}),
    );
    assert_eq!(t.m.sync_strategy_jobs(sid), vec![start_job_id(sid)]);
}

#[test]
fn an_intraday_exit_time_installs_the_square_off_the_original_never_scheduled() {
    // PORTED DEFECT, and the one real gap closed: with exit_time set and
    // auto_stop_time blank, the original installed no stop job at all.
    let t = t();
    let sid = make(
        &t,
        json!({"strategy_type": "intraday", "entry_time": "09:20",
        "exit_time": "15:20", "scheduler": scheduler(true, Some("09:20"), None)}),
    );
    let installed = t.m.sync_strategy_jobs(sid);
    assert!(installed.contains(&stop_job_id(sid)));
    let j = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
    assert_eq!((j.hour, j.minute), (15, 20));
}

#[test]
fn an_intraday_exit_time_is_squared_off_with_the_scheduler_switched_off() {
    let t = t();
    let sid = make(
        &t,
        json!({"strategy_type": "intraday", "entry_time": "09:20",
        "exit_time": "15:20", "scheduler": scheduler(false, None, None)}),
    );
    assert_eq!(t.m.sync_strategy_jobs(sid), vec![stop_job_id(sid)]);
    let j = t.m.scheduler.get(&stop_job_id(sid)).unwrap();
    assert_eq!(j.days.len(), 5, "weekdays");
}

#[test]
fn an_unknown_day_rejects_the_whole_list() {
    let t = t();
    let row =
        t.m.store
            .get_strategy_unscoped(make(
                &t,
                json!({
        "scheduler": {"enabled": true, "days": ["MON", "FUNDAY"], "start_time": "09:20",
                      "auto_stop_time": "15:10"}}),
            ))
            .unwrap()
            .unwrap();
    assert!(planned_jobs(&row).is_empty());
}

#[test]
fn deleting_a_strategy_drops_its_jobs_and_orphans_are_swept() {
    let t = t();
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_all_jobs();
    assert_eq!(t.m.scheduler.len(), 2);
    t.m.store.delete_strategy(sid, USER).unwrap();
    let r = t.m.sync_all_jobs();
    assert_eq!(r["orphans_removed"], 2);
    assert!(t.m.scheduler.is_empty());
}

#[tokio::test]
async fn a_job_fires_once_in_its_ist_slot_within_the_grace() {
    let t = t_at(ist(2026, 10, 7, 9, 19));
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_strategy_jobs(sid);
    assert!(t.m.run_due_jobs().await.is_empty());
    t.clock
        .set(ist(2026, 10, 7, 9, 20) + chrono::Duration::seconds(30));
    assert_eq!(t.m.run_due_jobs().await, vec![start_job_id(sid)]);
    assert!(
        t.m.run_due_jobs().await.is_empty(),
        "coalesced: once per slot"
    );
    assert_eq!(
        t.m.store.get_strategy(sid, USER).unwrap().unwrap().status,
        "running"
    );
    assert_eq!(
        t.run(
            t.m.store.list_runs(sid, 1).unwrap()[0]["id"]
                .as_i64()
                .unwrap()
        )
        .trigger_source,
        "scheduler"
    );
}

#[tokio::test]
async fn a_slot_missed_by_more_than_the_grace_is_dropped() {
    let t = t_at(ist(2026, 10, 7, 9, 22));
    let sid = make(
        &t,
        json!({"scheduler": scheduler(true, Some("09:20"), Some("15:10"))}),
    );
    t.m.sync_strategy_jobs(sid);
    assert!(t.m.run_due_jobs().await.is_empty());
}

#[tokio::test]
async fn a_scheduled_live_start_without_the_opt_in_is_refused_and_recorded() {
    let t = t();
    let mut sch = scheduler(true, Some("09:20"), Some("15:10"));
    sch["default_mode"] = json!("live");
    let sid = make(&t, json!({"scheduler": sch}));
    t.m.run_scheduled_start(sid).await;
    assert!(t.gw.placed().is_empty());
    assert!(t.event_kinds(sid).contains(&"live_disabled".to_string()));
}

#[tokio::test]
async fn the_scheduled_square_off_stops_a_running_strategy() {
    let t = t();
    let sid = make(&t, json!({}));
    let run = t.start_filled(sid, 100.0).await;
    t.m.run_scheduled_stop(sid).await;
    assert_eq!(
        t.run(run).stop_requested_reason.as_deref(),
        Some("scheduler")
    );
    // `exit_scheduler` is not an order kind, so the square-off records
    // `exit_close_all`, as on the web.
    assert_eq!(t.orders(run).last().unwrap().kind, "exit_close_all");
}

#[tokio::test]
async fn pending_stops_are_retried_by_the_reconcile_pass() {
    let t = t();
    let sid = make(&t, json!({}));
    let run = t.start_filled(sid, 100.0).await;
    t.gw.reject_next("Rate limited");
    assert!(t.m.stop_run(run, USER, "manual").await.stop_pending);
    let r = t.m.reconcile_pending_stops().await;
    assert_eq!(r["examined"], 1);
    assert_eq!(t.gw.actions(), vec!["SELL", "BUY", "BUY"]);
}
