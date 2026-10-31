//! The scheduler, the start-up restore and stop against the web's lines
//! (`flow_scheduler_service.py`, the monitors' `restore_*` and `shutdown`,
//! `app.py` 850-880), and the schedule behaviours those lines report: a
//! missed fire, a one-time fire on time, the market-hours gate on the NSE
//! calendar and only on a schedule (parity T-001, T-002, T-008, T-010,
//! T-011, T-013, T-016, T-017, T-019, T-021, T-022, T-045, T-046, T-060,
//! T-061, T-090, T-091, T-101, T-112, X-115, R-08, R-27, S-21,
//! N-marketHours-01; decisions 35, 36).
//!
//! Every test that saves an active workflow stops the app's own trigger
//! side first, so its start-up restore cannot register the same workflow.

use super::support::*;
use super::*;
use crate::flow::nodes::timing::{is_within_market_hours, MarketHours};

fn run_count(app: &Arc<AppState>, id: i64) -> usize {
    crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 10)
        .unwrap()
        .len()
}

/// Where `line` was first logged.
fn at(logs: &Logged, line: &str) -> usize {
    let all = logs.0.lock();
    all.iter()
        .position(|m| m == line)
        .unwrap_or_else(|| panic!("never logged {:?}: {:?}", line, all))
}

/// An IST wall time in November 2026 as an instant.
fn ist_at(d: u32, h: u32, m: u32, s: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono_tz::Asia::Kolkata
        .with_ymd_and_hms(2026, 11, d, h, m, s)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The recorded scheduler job id (written on the pool's thread, so read
/// back rather than taken from the log).
fn job_column(app: &Arc<AppState>, id: i64) -> Option<String> {
    app.sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT schedule_job_id FROM flow_workflows WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}

/// A graph the reads cannot decode: a blob where the nodes text belongs.
fn unreadable(app: &Arc<AppState>, id: i64) {
    app.sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE flow_workflows SET nodes = X'5B5D' WHERE id = ?1",
            [id],
        )
        .unwrap();
}

/// T-008, T-011, T-013 (decision 36): each schedule type logs the web's
/// lines, an invalid one with the web's ` - {e}` (Python's own error
/// text), weekly days as the web's `wed` list in their given order.
#[test]
fn the_schedule_types_log_the_webs_lines() {
    let now = ist_at(3, 9, 30, 0);
    let (_guard, logs) = logged();
    let build = |v: Value| scheduler::Schedule::from_node(&data(v), now);
    build(json!({"scheduleType": "once", "executeAt": "2026-11-04T10:00"})).unwrap();
    assert!(logs.has("Creating one-time trigger: 2026-11-04 10:00:00"));
    for (given, e) in [
        ("soon", "Invalid isoformat string: 'soon'"),
        ("laterZ", "Invalid isoformat string: 'later+00:00'"),
    ] {
        assert_eq!(
            build(json!({"scheduleType": "once", "executeAt": given})),
            Err(format!("Invalid datetime format: {}", given))
        );
        let line = format!("Invalid execute_at datetime: {} - {}", given, e);
        assert!(logs.has(&line), "{} in {:?}", line, logs.0.lock());
    }
    // A date alone takes the Time field (T-009): the error is the time's.
    build(json!({"scheduleType": "once", "executeAt": "2026-11-04", "time": "9am"})).unwrap_err();
    assert!(logs.has(
        "Invalid execute_at datetime: 2026-11-04 - invalid literal for int() with base 10: '9am'"
    ));
    build(json!({"scheduleType": "daily", "time": "9:15"})).unwrap();
    assert!(logs.has("Creating daily trigger: 9:15"));
    for (time, e) in [
        ("9am", "invalid literal for int() with base 10: '9am'"),
        ("", "invalid literal for int() with base 10: ''"),
        ("9:15:x", "invalid literal for int() with base 10: 'x'"),
        ("0915", "not enough values to unpack (expected 2, got 1)"),
        ("09:15:30", "too many values to unpack (expected 2)"),
        (
            "25:00",
            "Error validating expression '25': the last value (25) is higher than the maximum value (23)",
        ),
        (
            "09:60",
            "Error validating expression '60': the last value (60) is higher than the maximum value (59)",
        ),
        ("-1:00", "Unrecognized expression \"-1\" for field \"hour\""),
    ] {
        assert_eq!(
            build(json!({"scheduleType": "daily", "time": time})),
            Err(format!("Invalid time format: {}", time))
        );
        let line = format!("Invalid time format: {} - {}", time, e);
        assert!(logs.has(&line), "{} in {:?}", line, logs.0.lock());
    }
    build(json!({"scheduleType": "weekly", "time": "15:00", "days": [2]})).unwrap();
    assert!(logs.has("Creating weekly trigger: wed at 15:00"));
    // No time: the default 09:15 (R-27); the names in the given order,
    // repeats kept, unknown entries dropped.
    build(json!({"scheduleType": "weekly", "days": [4, 0, 9, "2", 4]})).unwrap();
    assert!(logs.has("Creating weekly trigger: fri,mon,fri at 09:15"));
    let no_days = "Unrecognized expression \"\" for field \"day_of_week\"";
    for (time, days, e) in [
        ("09:15", json!([7, 8]), no_days),
        ("25:00", json!([9]), no_days),
        ("9am", json!([2]), "invalid literal for int() with base 10: '9am'"),
        (
            "25:00",
            json!([2]),
            "Error validating expression '25': the last value (25) is higher than the maximum value (23)",
        ),
    ] {
        assert_eq!(
            build(json!({"scheduleType": "weekly", "time": time, "days": days})),
            Err("Invalid weekly schedule configuration".into())
        );
        let line = format!("Invalid weekly schedule config: {}", e);
        assert!(logs.has(&line), "{} in {:?}", line, logs.0.lock());
    }
}

/// T-001, T-016, R-27: adding a job clears the workflow's previous one
/// first (debug when there is none), then logs `Added job`; a start node
/// without a time runs at 09:15; removal logs the web's line, and a
/// second removal only the debug line.
#[tokio::test]
async fn a_job_is_added_replaced_and_removed_with_the_webs_lines() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    let id = save(&t.ctx, node("start", json!({})), false);
    let job = scheduler::job_id(id);
    let (_guard, logs) = logged();
    s.register(id, "start", &data(json!({"scheduleType": "daily"})))
        .await
        .unwrap();
    let none = format!("No scheduler job {} to remove", job);
    let added = format!("Added job {}", job);
    assert!(at(&logs, &none) < at(&logs, "Creating daily trigger: 09:15"));
    assert!(at(&logs, "Creating daily trigger: 09:15") < at(&logs, &added));
    let next = || store::next_run(&t.ctx.sqlite.conn().unwrap(), id).unwrap();
    assert_eq!(
        next().as_deref(),
        Some("2026-11-04T03:45:00"),
        "09:15 IST tomorrow"
    );
    s.register(
        id,
        "start",
        &data(json!({"scheduleType": "daily", "time": "09:00"})),
    )
    .await
    .unwrap();
    let removed = format!("Removed job {}", job);
    assert!(logs.has(&removed), "the previous job is removed first");
    assert_eq!(s.scheduler.workflow_ids(), vec![id], "one job per workflow");
    assert_eq!(logs.starting_with(&added).len(), 2);
    assert_eq!(next().as_deref(), Some("2026-11-04T03:30:00"));
    // A manual schedule registers nothing and clears nothing.
    s.register(id, "start", &data(json!({"scheduleType": "manual"})))
        .await
        .unwrap();
    assert!(s.scheduler.has(id));
    s.unregister(id).await.unwrap();
    assert_eq!(logs.starting_with(&removed).len(), 2);
    assert!(!s.scheduler.has(id));
    s.unregister(id).await.unwrap();
    assert_eq!(
        logs.starting_with(&none).len(),
        2,
        "a missing job is not an error"
    );
}

/// T-002, T-019: a daily fire more than 60 s late runs nothing and keeps
/// the schedule; the next one, within the grace, runs (here into the
/// no-key skip, recorded) with the web's lines.
#[tokio::test]
async fn a_missed_fire_runs_nothing_and_one_within_the_grace_runs() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    let daily = json!({"scheduleType": "daily"});
    let id = save(&t.ctx, node("start", daily.clone()), true);
    s.register(id, "start", &data(daily)).await.unwrap();
    let (_guard, logs) = logged();
    // Wednesday, 90 s after its 09:15 IST fire.
    t.clock.set(ist_at(4, 9, 16, 30));
    let due = s.scheduler.tick(s.now());
    assert_eq!(due.len(), 1);
    assert!(due[0].missed && !due[0].once, "{:?}", due[0]);
    s.run_scheduled(due[0].clone()).await;
    assert_eq!(run_count(&t.ctx, id), 0, "a missed fire runs nothing");
    assert!(logs.has(&format!(
        "Scheduled run of workflow {} was missed by more than 60 s; skipping it",
        id
    )));
    let ran = format!("Scheduled execution of workflow {}", id);
    assert!(!logs.has(&ran));
    assert!(active(&t.ctx, id) && s.scheduler.has(id), "still scheduled");
    // Thursday, 30 s after its fire.
    t.clock.set(ist_at(5, 9, 15, 30));
    let due = s.scheduler.tick(s.now());
    assert_eq!(due.len(), 1);
    assert!(!due[0].missed);
    s.run_scheduled(due[0].clone()).await;
    assert!(logs.has(&ran));
    assert!(logs.has(&format!("No API key available for workflow {}", id)));
    assert_eq!(run_count(&t.ctx, id), 1, "the skip is recorded");
}

/// T-010, T-112 (WEB-DEFECT): a one-time schedule that fired on time runs
/// once and stands its workflow down, so a restart within the 60 s grace
/// restores nothing and cannot run it a second time.
#[tokio::test]
async fn a_one_time_schedule_that_fired_on_time_stands_its_workflow_down() {
    let t = keyed();
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    let once = json!({"scheduleType": "once", "executeAt": "2026-11-03", "time": "10:00"});
    let id = save(&t.ctx, node("start", once.clone()), true);
    s.register(id, "start", &data(once)).await.unwrap();
    assert_eq!(job_column(&t.ctx, id), Some(scheduler::job_id(id)));
    let (_guard, logs) = logged();
    t.clock.set(ist_at(3, 10, 0, 20));
    let due = s.scheduler.tick(s.now());
    assert_eq!(due.len(), 1);
    assert!(due[0].once && !due[0].missed, "on time: {:?}", due[0]);
    s.run_scheduled(due[0].clone()).await;
    assert!(logs.has(&format!("Scheduled execution of workflow {}", id)));
    assert_eq!(run_count(&t.ctx, id), 1, "it ran");
    assert!(!logs.has(&format!("No API key available for workflow {}", id)));
    assert!(logs.has(&format!(
        "The one-time schedule of workflow {} has fired; the workflow was deactivated so it cannot run again.",
        id
    )));
    assert!(!active(&t.ctx, id) && !s.scheduler.has(id));
    assert_eq!(job_column(&t.ctx, id), None, "the job id is cleared");
    // The app restarted 30 s later.
    t.clock.set(ist_at(3, 10, 0, 50));
    let again = side(&t);
    again.restore().await;
    assert!(again.scheduler.workflow_ids().is_empty());
    assert!(again.scheduler.tick(again.now()).is_empty());
    assert_eq!(run_count(&t.ctx, id), 1, "never a second run");
}

/// T-021, T-101, X-115, N-marketHours-01: with no exchange named the gate
/// reads NSE's calendar: it follows NSE's hours when BSE's differ, either
/// way round; outside them the run is skipped with the web's debug line.
#[tokio::test]
async fn the_gate_reads_the_nse_calendar_when_no_exchange_is_named() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    // Tuesday 2026-11-03, 13:00 IST.
    t.clock.set(ist_at(3, 13, 0, 0));
    let s = side(&t);
    let gated = json!({"scheduleType": "daily", "marketHoursOnly": true});
    let id = save(&t.ctx, node("start", gated), true);
    let open = |exchange: Option<&str>| {
        let hours = MarketHours {
            enabled: true,
            exchange: exchange.map(|e| json!(e)),
            ..Default::default()
        };
        is_within_market_hours(&t.ctx.sqlite.conn().unwrap(), t.ctx.now(), &hours)
    };
    let close_at = |exchange: &str, end: &str| {
        crate::db::sqlite::market_calendar::update_timing(
            &t.ctx.sqlite.conn().unwrap(),
            exchange,
            "09:15",
            end,
        )
        .unwrap()
    };
    let (_guard, logs) = logged();
    close_at("BSE", "12:00");
    assert_eq!(
        (open(None), open(Some("NSE")), open(Some("BSE"))),
        (true, true, false)
    );
    s.scheduled_run(id, true).await;
    assert_eq!(
        run_count(&t.ctx, id),
        1,
        "inside NSE's hours it runs (into the no-key skip)"
    );
    close_at("BSE", "15:30");
    close_at("NSE", "12:00");
    assert_eq!(
        (open(None), open(Some("NSE")), open(Some("BSE"))),
        (false, false, true)
    );
    s.scheduled_run(id, true).await;
    assert_eq!(
        run_count(&t.ctx, id),
        1,
        "outside NSE's hours: nothing at all"
    );
    assert!(logs.has(&format!(
        "Skipping scheduled workflow {}: outside market hours",
        id
    )));
}

/// T-022: the market-hours switch gates only a scheduled run. On a
/// Saturday a workflow whose start node has it on still runs from Run Now,
/// its webhook, and a price-alert or order-update fire.
#[tokio::test]
async fn a_gated_workflow_runs_from_every_other_trigger() {
    let t = keyed();
    t.ctx.flow.triggers().stop().await;
    // Saturday 2026-10-31, 12:00 IST.
    t.clock.set("2026-10-31T06:30:00Z".parse().unwrap());
    let nodes = json!([
        {"id": "s", "type": "start", "position": {"x": 0, "y": 0},
            "data": {"scheduleType": "daily", "marketHoursOnly": true}},
        {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "hello"}}
    ]);
    let edges = json!([{"id": "e", "source": "s", "target": "l"}]);
    let (token, secret) = live_hook(&t.ctx, nodes, edges);
    let id = workflows::list(&t.ctx.sqlite.conn().unwrap()).unwrap()[0].id;
    let s = side(&t);
    s.scheduled_run(id, true).await;
    assert_eq!(run_count(&t.ctx, id), 0, "the schedule is gated");
    let r = crate::flow::service::run_now(&t.ctx, id).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(run_count(&t.ctx, id), 1, "Run Now is not gated");
    let a = crate::flow::service::webhooks::trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(a.reply.status, 200, "{}", a.reply.body);
    assert_eq!(run_count(&t.ctx, id), 2, "the webhook is not gated");
    let price = s.fire(id, RunTrigger::PriceAlert, Some(json!({}))).await;
    assert!(price.executed(), "{:?}", price);
    let order = s.fire(id, RunTrigger::OrderUpdate, Some(json!({}))).await;
    assert!(order.executed(), "{:?}", order);
    assert_eq!(run_count(&t.ctx, id), 4, "neither monitor is gated");
}

/// T-019: when the graph cannot be read at fire time the web's error is
/// logged and the job's stored flag decides the gate.
#[tokio::test]
async fn a_failed_scheduling_read_keeps_the_stored_gate() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    // Saturday 2026-10-31, 12:00 IST.
    t.clock.set("2026-10-31T06:30:00Z".parse().unwrap());
    let s = side(&t);
    let ungated = json!({"scheduleType": "daily", "marketHoursOnly": false});
    let id = save(&t.ctx, node("start", ungated), true);
    unreadable(&t.ctx, id);
    let (_guard, logs) = logged();
    s.scheduled_run(id, true).await;
    assert!(logs.has(&format!(
        "Could not read scheduling config for workflow {}; using the schedule's stored setting",
        id
    )));
    assert!(logs.has(&format!(
        "Skipping scheduled workflow {}: outside market hours",
        id
    )));
    assert_eq!(run_count(&t.ctx, id), 0, "the stored gate holds");
    s.scheduled_run(id, false).await;
    assert_eq!(
        run_count(&t.ctx, id),
        1,
        "an ungated job goes on to the run"
    );
    assert!(logs.has(&format!("No API key available for workflow {}", id)));
}

/// T-017, T-045, T-060 (decision 36): stop, with no reset first, clears
/// the jobs, the alerts and the watches and stops taking order updates,
/// writing the web's three shutdown lines once, after the poll's own
/// `Price monitoring stopped`; a later attach takes updates again.
#[tokio::test]
async fn stop_clears_every_registration_and_says_so_once() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    s.start_drivers();
    s.watches.attach();
    let id = save(&t.ctx, node("start", json!({})), false);
    let (_guard, logs) = logged();
    s.register(id, "start", &data(json!({"scheduleType": "daily"})))
        .await
        .unwrap();
    s.register(
        id,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "price": 800})),
    )
    .await
    .unwrap();
    s.register(id, "orderUpdateTrigger", &data(json!({"symbol": "SBIN"})))
        .await
        .unwrap();
    wait_for(&logs, "Price monitoring started with 1 alerts").await;
    s.stop().await;
    assert!(
        s.scheduler.workflow_ids().is_empty(),
        "the jobs are cleared"
    );
    assert!(s.alerts.is_empty() && s.watches.get(id).is_none());
    let lines = [
        "Flow Scheduler shutdown",
        "FlowPriceMonitor shutdown",
        "FlowOrderUpdateMonitor shutdown",
    ];
    assert!(at(&logs, "Price monitoring stopped") < at(&logs, lines[1]));
    s.stop().await;
    for line in lines {
        assert_eq!(logs.starting_with(line).len(), 1, "{} once", line);
    }
    assert!(!logs.has("Flow triggers stopped"));
    let update = crate::events::OrderUpdate {
        orderid: "2511030001".into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        order_status: "complete".into(),
        ..Default::default()
    };
    let watch = order_monitor::Watch::from_node(id, &data(json!({"symbol": "SBIN"}))).unwrap();
    s.watches.add(watch);
    assert!(s.watches.claim(&update).is_empty(), "detached");
    s.watches.attach();
    assert_eq!(s.watches.claim(&update).len(), 1, "attached again");
}

/// R-08, T-046, T-061, T-091 (decision 36): the restore writes the web's
/// lines for each kind (a skipped watch or alert, the no-symbol warning, a
/// restored and a failed schedule, each count), in the web's order:
/// watches, then alerts, then schedules.
#[tokio::test]
async fn restore_logs_the_webs_lines_in_the_webs_order() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    let ctx = &t.ctx;
    let watch = save(
        ctx,
        node("orderUpdateTrigger", json!({"orderId": "42"})),
        true,
    );
    let bad_watch = save(
        ctx,
        node("orderUpdateTrigger", json!({"status": "complete"})),
        true,
    );
    let alert = save(
        ctx,
        node("priceAlert", json!({"symbol": "SBIN", "price": 800})),
        true,
    );
    let no_symbol = save(ctx, node("priceAlert", json!({"price": 800})), true);
    let bad_alert = save(
        ctx,
        node("priceAlert", json!({"symbol": "SBIN", "price": "abc"})),
        true,
    );
    let daily = save(ctx, node("start", json!({"scheduleType": "daily"})), true);
    let bad_daily = save(
        ctx,
        node("start", json!({"scheduleType": "daily", "time": "9am"})),
        true,
    );
    let s = side(&t);
    let (_guard, logs) = logged();
    s.restore().await;
    let one = |prefix: String| assert_eq!(logs.starting_with(&prefix).len(), 1, "{}", prefix);
    one(format!(
        "Skipping invalid order-update watch for workflow {}: ",
        bad_watch
    ));
    one(format!(
        "Skipping invalid price alert for workflow {}: ",
        bad_alert
    ));
    assert!(logs.has(&format!(
        "Cannot restore price alert for workflow {}: no symbol configured",
        no_symbol
    )));
    assert!(logs.has("Invalid time format: 9am - invalid literal for int() with base 10: '9am'"));
    assert!(logs.has(&format!(
        "Could not restore the scheduler job for active workflow {}; it will not fire until it is reactivated",
        bad_daily
    )));
    let job = scheduler::job_id(daily);
    let restored = [
        format!("No scheduler job {} to remove", job),
        "Creating daily trigger: 09:15".to_string(),
        format!("Added job {}", job),
        format!(
            "Restored missing scheduler job for active workflow {}.",
            daily
        ),
    ];
    for pair in restored.windows(2) {
        assert!(at(&logs, &pair[0]) < at(&logs, &pair[1]), "{:?}", pair);
    }
    assert_eq!(job_column(ctx, daily), Some(job));
    let watches = at(
        &logs,
        "Restored 1 order-update watch(es) from active workflows",
    );
    let alerts = at(&logs, "Restored 1 price alert(s) from active workflows");
    let schedules = at(
        &logs,
        "Scheduler reconciliation: removed 0 orphaned job(s), restored 1 missing job(s)",
    );
    assert!(watches < alerts && alerts < schedules);
    for gone in [bad_watch, no_symbol, bad_alert, bad_daily] {
        assert!(!active(ctx, gone), "workflow {} stood down", gone);
    }
    assert!(active(ctx, watch) && active(ctx, alert) && active(ctx, daily));
}

/// T-090, R-08: when the active workflows cannot be read each of the
/// three start-up steps logs the web's failure line, in its order, and
/// nothing is registered or stood down.
#[tokio::test]
async fn a_failed_restore_read_logs_each_steps_line() {
    let t = app();
    t.ctx.flow.triggers().stop().await;
    let id = save(
        &t.ctx,
        node("priceAlert", json!({"symbol": "SBIN", "price": 800})),
        true,
    );
    unreadable(&t.ctx, id);
    let s = side(&t);
    let (_guard, logs) = logged();
    s.restore().await;
    let watches = at(&logs, "Failed to restore Flow order-update watches");
    let alerts = at(&logs, "Failed to restore Flow price alerts");
    let schedules = at(&logs, "Failed to reconcile Flow scheduler jobs");
    assert!(watches < alerts && alerts < schedules);
    assert!(s.alerts.is_empty());
    assert!(active(&t.ctx, id), "a failed read stands nothing down");
}

/// S-21: recording the job id and clearing it at deactivation write the
/// web's `Set schedule job ID {job_id}` / `None` lines; nothing to clear,
/// or no such workflow, writes none. The deactivate route clears the id.
#[tokio::test]
async fn the_job_id_is_set_and_cleared_with_the_webs_line() {
    let t = keyed();
    t.ctx.flow.triggers().stop().await;
    let id = save(&t.ctx, node("start", json!({})), true);
    let conn = t.ctx.sqlite.conn().unwrap();
    let now = t.ctx.now();
    let (_guard, logs) = logged();
    let job = scheduler::job_id(id);
    assert!(store::set_schedule_job_id(&conn, id, Some(job.as_str()), now).unwrap());
    assert!(logs.has(&format!("Set schedule job ID {} for workflow {}", job, id)));
    assert!(store::deactivate(&conn, id, now).unwrap());
    let cleared = format!("Set schedule job ID None for workflow {}", id);
    assert!(logs.has(&cleared));
    assert!(store::deactivate(&conn, id, now).unwrap());
    assert_eq!(logs.starting_with(&cleared).len(), 1, "nothing to clear");
    assert!(!store::set_schedule_job_id(&conn, 999, Some("flow_workflow_999"), now).unwrap());
    assert!(!store::deactivate(&conn, 999, now).unwrap());
    assert!(logs
        .starting_with("Set schedule job ID flow_workflow_999")
        .is_empty());
    assert!(!logs.has("Set schedule job ID None for workflow 999"));
    drop(conn);
    let daily = save(
        &t.ctx,
        node("start", json!({"scheduleType": "daily"})),
        false,
    );
    let r = crate::flow::service::activation::activate(&t.ctx, daily).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(job_column(&t.ctx, daily), Some(scheduler::job_id(daily)));
    let r = crate::flow::service::activation::deactivate(&t.ctx, daily).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(job_column(&t.ctx, daily), None);
}
