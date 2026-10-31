//! Signing out with active triggers (parity D-18, D-06, lead decision 16):
//! `AuthService::lock_everything` stops the runs but leaves every
//! schedule, price alert and order watch registered and every workflow
//! active; while signed out a fire is skipped with the reason, so the
//! triggers carry on after the next sign-in.

use super::*;

#[tokio::test]
async fn signing_out_keeps_every_trigger_registered_and_its_workflow_active() {
    let t = app();
    // The app's own side restores the active workflows on its own task;
    // stopped first, the test's side is the app's only one.
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    t.ctx.flow.set_triggers(s.clone());
    let daily = json!({"scheduleType": "daily", "time": "09:15"});
    let alert_at = json!({"symbol": "SBIN", "price": 800});
    let watched = json!({"symbol": "SBIN"});
    let schedule = save(&t.ctx, node("start", daily.clone()), true);
    let alert = save(&t.ctx, node("priceAlert", alert_at.clone()), true);
    let watch = save(&t.ctx, node("orderUpdateTrigger", watched.clone()), true);
    let hooks = t.ctx.flow.triggers();
    hooks
        .register(schedule, "start", &data(daily))
        .await
        .unwrap();
    hooks
        .register(alert, "priceAlert", &data(alert_at))
        .await
        .unwrap();
    hooks
        .register(watch, "orderUpdateTrigger", &data(watched))
        .await
        .unwrap();
    let next = store::next_run(&t.ctx.sqlite.conn().unwrap(), schedule).unwrap();
    assert!(next.is_some());

    crate::services::auth_service::AuthService::lock_everything(&t.ctx).await;
    assert!(!t.ctx.security.is_unlocked());

    assert!(s.scheduler.has(schedule), "the schedule stays");
    assert!(s.alerts.get(alert).is_some(), "the alert stays");
    assert!(s.watches.get(watch).is_some(), "the watch stays");
    for id in [schedule, alert, watch] {
        assert!(active(&t.ctx, id), "workflow {} stays active", id);
    }
    assert_eq!(
        store::next_run(&t.ctx.sqlite.conn().unwrap(), schedule).unwrap(),
        next,
        "the schedule keeps its next run"
    );
    let status = t.ctx.flow.triggers().status();
    assert_eq!(
        (
            status["running"].clone(),
            status["alerts_count"].clone(),
            status["order_updates"]["watches_count"].clone()
        ),
        (json!(true), json!(1), json!(1)),
        "{}",
        status
    );
    // Signed out, a fire is skipped with the reason and stands nothing down.
    assert_eq!(
        s.fire(schedule, RunTrigger::Schedule, None).await,
        Fired::Skipped
    );
    let runs =
        crate::flow::store::executions::list(&t.ctx.sqlite.conn().unwrap(), schedule, 5).unwrap();
    assert_eq!(
        runs[0].error.as_deref(),
        Some("Skipped: OpenAlgo was signed out when this schedule fired, so the workflow did not run. Its triggers run again after you sign in.")
    );
    assert!(active(&t.ctx, schedule) && s.scheduler.has(schedule));
}
