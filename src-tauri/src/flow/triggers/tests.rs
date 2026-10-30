//! The trigger side on a real app context: registration of each kind, the
//! one stand-down path, the skip while no broker is logged in, the
//! start-up restore, reset and stop (parity T-043, T-047, T-082, T-090,
//! T-092, T-111, X-215, X-217, X-246..X-252, D-06, lead decision 16).

use super::*;
use crate::flow::contract::node::RunTrigger;
use crate::flow::store::workflows::{self, NewWorkflow};
use crate::security::Unlocked;
use serde_json::json;

fn app() -> crate::state::testing::TestCtx {
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-11-03T04:00:00Z".parse().unwrap(),
    );
    t.ctx.security.install(&Unlocked::random().unwrap());
    t
}

fn side(t: &crate::state::testing::TestCtx) -> Arc<TriggerSide> {
    TriggerSide::new(Arc::downgrade(&t.ctx), t.ctx.clock.clone())
}

fn save(app: &Arc<AppState>, nodes: Value, active: bool) -> i64 {
    let conn = app.sqlite.conn().unwrap();
    let w = workflows::create(
        &conn,
        &app.security,
        &NewWorkflow {
            name: "Opening range",
            description: None,
            nodes: &nodes,
            edges: &json!([]),
        },
        app.now(),
    )
    .unwrap();
    workflows::set_active(&conn, w.id, active, app.now()).unwrap();
    w.id
}

fn active(app: &Arc<AppState>, id: i64) -> bool {
    store::is_active(&app.sqlite.conn().unwrap(), id)
        .unwrap()
        .unwrap()
}

fn node(kind: &str, data: Value) -> Value {
    json!([{"id": "t", "type": kind, "position": {"x": 0, "y": 0}, "data": data}])
}

fn data(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// T-082, T-030, T-051, R-27..R-29: each trigger kind registers its own
/// thing; a webhook trigger and a manual start register nothing; a bad
/// node is refused as the web's ValueError; the job id and next run are
/// recorded on the row and the job holds no API key (X-174, X-233).
#[tokio::test]
async fn each_trigger_kind_registers_its_own_watcher() {
    let t = app();
    let s = side(&t);
    let id = save(&t.ctx, node("start", json!({})), true);
    let sched = data(json!({"scheduleType": "daily", "time": "09:15"}));
    assert_eq!(s.register(id, "start", &sched).await, Ok(()));
    assert!(s.scheduler.has(id));
    let conn = t.ctx.sqlite.conn().unwrap();
    let (job, key): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT schedule_job_id, api_key FROM flow_workflows WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(job.as_deref(), Some("flow_workflow_1"));
    assert_eq!(key, None, "no key is stored for a schedule");
    assert_eq!(
        store::next_run(&conn, id).unwrap().as_deref(),
        Some("2026-11-04T03:45:00")
    );
    drop(conn);
    assert_eq!(
        s.register(id, "start", &data(json!({"scheduleType": "manual"})))
            .await,
        Ok(())
    );
    assert_eq!(s.register(id, "webhookTrigger", &Map::new()).await, Ok(()));
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
    assert!(s.alerts.get(id).is_some() && s.watches.get(id).is_some());
    assert_eq!(
        s.register(id, "orderUpdateTrigger", &Map::new()).await,
        Err(TriggerError::Invalid(
            "orderUpdateTrigger needs an Order ID or a Symbol to watch".into()
        ))
    );
    assert_eq!(
        s.register(id, "start", &data(json!({"scheduleType": "hourly"})))
            .await,
        Err(TriggerError::Invalid(
            "Invalid schedule configuration: type=hourly".into()
        ))
    );
    // T-084, R-30: unregister removes every kind.
    s.unregister(id).await.unwrap();
    assert!(!s.scheduler.has(id) && s.alerts.get(id).is_none() && s.watches.get(id).is_none());
    // A job whose id cannot be recorded (no such workflow) is undone.
    assert_eq!(
        s.register(999, "start", &sched).await,
        Err(TriggerError::Failed(
            "Could not record the scheduler job id for this workflow".into()
        ))
    );
    assert!(!s.scheduler.has(999));
}

/// T-043 (WEB-DEFECT): standing a workflow down gives back its feed
/// subscriptions as the deactivate route does (the web's automatic
/// deactivations kept them), removes every registration and clears the
/// row.
#[tokio::test]
async fn standing_down_releases_the_feed_and_every_registration() {
    let t = app();
    let s = side(&t);
    let id = save(&t.ctx, node("priceAlert", json!({"symbol": "SBIN"})), true);
    t.ctx
        .flow
        .subscriptions
        .record(Some(id), "SBIN", "NSE", "LTP");
    s.register(id, "priceAlert", &data(json!({"symbol": "SBIN"})))
        .await
        .unwrap();
    s.stand_down(id, "test").await;
    assert!(!t.ctx.flow.subscriptions.has(id));
    assert!(s.alerts.get(id).is_none());
    assert!(!active(&t.ctx, id));
}

/// Lead decision 16, D-06: while OpenAlgo is signed out a trigger does
/// not run; the skip is written to the workflow's history with the
/// reason, and the workflow stays active. Signed in with no broker logged
/// in, nothing pauses: the run goes on (here it stops for want of an API
/// key, which this context has none of).
#[tokio::test]
async fn a_trigger_while_signed_out_is_skipped_and_recorded() {
    let t = app();
    let s = side(&t);
    let id = save(&t.ctx, node("start", json!({})), true);
    assert_eq!(
        runs::paused(&t.ctx, RunTrigger::Schedule),
        None,
        "no broker is no pause"
    );
    t.ctx.security.lock_with(|| {});
    assert_eq!(s.fire(id, RunTrigger::Schedule, None).await, Fired::Skipped);
    let runs = crate::flow::store::executions::list(&t.ctx.sqlite.conn().unwrap(), id, 5).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "failed");
    let reason = runs[0].error.clone().unwrap();
    assert_eq!(
        reason,
        "Skipped: OpenAlgo was signed out when this schedule fired, so the workflow did not run. Its triggers run again after you sign in."
    );
    assert_eq!(runs[0].logs[0]["message"], json!(reason));
    assert!(active(&t.ctx, id), "a skip never deactivates");
}

use super::runs::Fired;

/// T-046, T-061, T-091 and T-047 (WEB-DEFECT): at start-up the active
/// workflows' triggers come back; one that cannot be restored is stood
/// down with the reason instead of staying Active with nothing watching;
/// inactive and manual workflows get nothing (X-246..X-250).
#[tokio::test]
async fn start_up_restores_the_active_triggers() {
    let t = app();
    let ctx = &t.ctx;
    let alert = save(
        ctx,
        node("priceAlert", json!({"symbol": "SBIN", "price": 800})),
        true,
    );
    let no_symbol = save(ctx, node("priceAlert", json!({"price": 800})), true);
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
    let daily = save(
        ctx,
        node("start", json!({"scheduleType": "daily", "time": "09:15"})),
        true,
    );
    let manual = save(ctx, node("start", json!({"scheduleType": "manual"})), true);
    let inactive = save(ctx, node("start", json!({"scheduleType": "daily"})), false);
    let s = side(&t);
    s.restore().await;
    assert!(s.alerts.get(alert).is_some());
    assert!(s.watches.get(watch).is_some());
    assert!(s.scheduler.has(daily));
    assert!(!s.scheduler.has(manual) && !s.scheduler.has(inactive));
    assert_eq!(s.scheduler.workflow_ids(), vec![daily]);
    for gone in [no_symbol, bad_watch] {
        assert!(!active(ctx, gone), "workflow {} stood down", gone);
    }
    assert!(active(ctx, alert) && active(ctx, watch) && active(ctx, daily) && active(ctx, manual));
}

/// T-010 (WEB-DEFECT): a one-time schedule that fired (or was missed)
/// stands its workflow down, so a restart can never run it again.
#[tokio::test]
async fn a_one_time_schedule_retires_its_workflow() {
    let t = app();
    let id = save(
        &t.ctx,
        node(
            "start",
            json!({"scheduleType": "once", "executeAt": "2026-11-02", "time": "10:00"}),
        ),
        true,
    );
    let s = side(&t);
    s.restore().await;
    let due = s.scheduler.tick(s.now());
    assert_eq!(due.len(), 1);
    assert!(due[0].missed && due[0].once);
    s.run_scheduled(due[0].clone()).await;
    assert!(!active(&t.ctx, id));
    assert!(!s.scheduler.has(id));
}

/// T-093, R-57: the status is the price monitor's with the order
/// monitor's under `order_updates`.
#[tokio::test]
async fn the_status_has_both_monitors() {
    let t = app();
    let s = side(&t);
    assert_eq!(
        s.status(),
        json!({"running": false, "alerts_count": 0, "poll_interval": 5, "alerts": [],
            "order_updates": {"watches_count": 0, "watches": []}})
    );
    s.register(
        3,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "condition": "above", "price": 800})),
    )
    .await
    .unwrap();
    let v = s.status();
    assert_eq!(v["running"], true);
    assert_eq!(v["alerts"][0]["condition"], "greater_than");
}

/// T-045, T-060, T-090, X-215, X-217, X-344: stop ends every task, twice
/// is harmless, and nothing starts afterwards; reset forgets everything.
#[tokio::test]
async fn stop_ends_every_task_and_reset_forgets_everything() {
    let t = app();
    let s = side(&t);
    s.start_drivers();
    s.register(1, "orderUpdateTrigger", &data(json!({"symbol": "SBIN"})))
        .await
        .unwrap();
    s.reset().await;
    assert!(s.watches.get(1).is_none());
    assert_eq!(s.task_count(), 2);
    s.stop().await;
    assert_eq!(s.task_count(), 0);
    s.stop().await;
    s.start_drivers();
    assert_eq!(s.task_count(), 0, "a stopped side starts nothing");
    // The app's own side was installed at start-up and answers too.
    assert_eq!(t.ctx.flow.triggers().status()["alerts_count"], 0);
}

/// T-050, T-102, X-341, D-25: the order-update monitor rides the bus's
/// critical lane; an update reaches the watch, and with no broker logged
/// in its run is skipped (recorded) and the one-time watch re-armed.
#[tokio::test]
async fn an_order_update_from_the_bus_reaches_its_watch() {
    let t = app();
    assert!(t
        .ctx
        .bus
        .queue_depths()
        .iter()
        .any(|q| q.name == "FlowOrderUpdateMonitor" && q.lane == crate::events::Lane::Critical));
    let id = save(
        &t.ctx,
        node("orderUpdateTrigger", json!({"symbol": "SBIN"})),
        true,
    );
    let hooks = t.ctx.flow.triggers();
    hooks
        .register(
            id,
            "orderUpdateTrigger",
            &data(json!({"symbol": "SBIN", "status": "complete"})),
        )
        .await
        .unwrap();
    t.ctx.bus.publish(crate::events::Event::OrderUpdate(
        crate::events::OrderUpdate {
            orderid: "2511030001".into(),
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            order_status: "complete".into(),
            ..Default::default()
        },
    ));
    let mut runs = Vec::new();
    for _ in 0..200 {
        runs = crate::flow::store::executions::list(&t.ctx.sqlite.conn().unwrap(), id, 5).unwrap();
        if !runs.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(runs.len(), 1, "the update reached the watch");
    assert!(runs[0]
        .error
        .as_deref()
        .unwrap()
        .contains("this order update fired"));
    assert!(active(&t.ctx, id));
    assert_eq!(
        hooks.status()["order_updates"]["watches_count"],
        1,
        "re-armed, not spent"
    );
}

/// A feed that refuses while `refuse` is set and can record a new
/// subscription for the same key during an unsubscribe.
struct Feed {
    refuse: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
    resubscribe: Option<Arc<crate::flow::contract::subscriptions::Subscriptions>>,
}

#[async_trait]
impl FeedRelease for Feed {
    async fn unsubscribe(&self, symbol: &str, exchange: &str, mode: &str) -> bool {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(s) = &self.resubscribe {
            s.record(Some(1), symbol, exchange, mode);
        }
        !self.refuse.load(std::sync::atomic::Ordering::SeqCst) && !symbol.starts_with("FAIL")
    }
}

fn feed(refuse: bool) -> Feed {
    Feed {
        refuse: std::sync::atomic::AtomicBool::new(refuse),
        calls: Default::default(),
        resubscribe: None,
    }
}

/// X-315, X-316, N-stream-04: a release the feed did not confirm keeps
/// each entry for a retry, a partial release keeps only what failed, and
/// nothing is left behind once every release went through (100 workflows,
/// 200 unsubscribes).
#[tokio::test]
async fn a_failed_release_keeps_each_subscription_until_the_retry() {
    let subs = crate::flow::contract::subscriptions::Subscriptions::default();
    let f = feed(true);
    for id in 0..100 {
        subs.record(Some(id), "SBIN", &format!("X{}", id), "LTP");
    }
    for id in 0..100 {
        assert_eq!(subs.release(id, &f).await, 0);
        assert!(subs.has(id));
    }
    f.refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    for id in 0..100 {
        assert_eq!(subs.release(id, &f).await, 1);
        assert!(!subs.has(id));
    }
    assert_eq!(f.calls.load(std::sync::atomic::Ordering::SeqCst), 200);
    subs.record(Some(1), "PASS", "NSE", "LTP");
    subs.record(Some(1), "FAIL", "NSE", "LTP");
    assert_eq!(subs.release(1, &f).await, 1);
    assert_eq!(
        subs.entries(1),
        vec![("FAIL".into(), "NSE".into(), "LTP".into())]
    );
}

/// X-317, N-stream-05: a subscription recorded again while its release is
/// in flight survives that release; the next one clears it.
#[tokio::test]
async fn a_subscription_recorded_during_its_release_survives_it() {
    let subs = Arc::new(crate::flow::contract::subscriptions::Subscriptions::default());
    subs.record(Some(1), "SBIN", "NSE", "LTP");
    let mut f = feed(false);
    f.resubscribe = Some(subs.clone());
    assert_eq!(subs.release(1, &f).await, 1);
    assert!(subs.has(1), "the newer record stays");
    f.resubscribe = None;
    assert_eq!(subs.release(1, &f).await, 1);
    assert!(!subs.has(1));
}

/// A one-time trigger is spent by any run that reached the graph, a broker
/// rejection included (X-220); only `already_running` and a skip leave it
/// armed (X-218).
#[test]
fn only_a_run_that_reached_the_graph_spends_a_one_shot() {
    assert!(Fired::Ran(json!({"status": "error", "message": "insufficient margin"})).executed());
    assert!(!Fired::AlreadyRunning.executed());
    assert!(!Fired::Skipped.executed());
}

/// A webhook workflow, active and enabled; its token and secret.
fn live_hook(app: &Arc<AppState>, nodes: Value, edges: Value) -> (String, String) {
    let conn = app.sqlite.conn().unwrap();
    let w = workflows::create(
        &conn,
        &app.security,
        &NewWorkflow {
            name: "Hook",
            description: None,
            nodes: &nodes,
            edges: &edges,
        },
        app.now(),
    )
    .unwrap();
    workflows::set_active(&conn, w.id, true, app.now()).unwrap();
    workflows::set_webhook_enabled(&conn, w.id, true, app.now()).unwrap();
    (
        w.webhook_token.unwrap(),
        w.webhook_secret.unwrap().expose().to_string(),
    )
}

fn hook_call(token: &str, secret: &str) -> crate::flow::service::webhooks::WebhookCall {
    crate::flow::service::webhooks::WebhookCall {
        token: token.into(),
        payload: data(json!({"secret": secret})),
        url_secret: None,
    }
}

/// R-53, T-071: no API key answers the web's 500; R-54, V-136: a workflow
/// the strict validator refuses answers 400 with its first message, before
/// anything runs.
#[tokio::test]
async fn a_webhook_needs_a_key_and_a_runnable_workflow() {
    use crate::flow::service::webhooks::trigger;
    let t = app();
    let incomplete = node("webhookTrigger", json!({}));
    let (token, secret) = live_hook(&t.ctx, incomplete.clone(), json!([]));
    let a = trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(
        (a.reply.status, a.reply.body.clone()),
        (
            500,
            json!({"error": "No API key configured for workflow execution. Please re-activate the workflow."})
        )
    );
    // With an account and its key (made before the workflow: a new
    // account's keys drop every earlier webhook secret).
    let t = app();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    let (token, secret) = live_hook(&t.ctx, incomplete, json!([]));
    let a = trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(a.reply.status, 400, "{}", a.reply.body);
    assert_eq!(a.reply.body["error"], "Workflow cannot be executed");
    assert_eq!(
        a.reply.body["message"],
        a.reply.body["errors"][0]["message"]
    );
}

/// N-marketHours-01, T-019, T-022: a gated schedule outside the window is
/// skipped silently (no run, no record); an ungated one goes on to the run
/// (here recorded as skipped: no broker is logged in).
#[tokio::test]
async fn the_market_hours_gate_skips_a_scheduled_run_silently() {
    let t = app();
    // Saturday 2026-10-31, 12:00 IST.
    t.clock.set("2026-10-31T06:30:00Z".parse().unwrap());
    let gated = save(
        &t.ctx,
        node(
            "start",
            json!({"scheduleType": "daily", "marketHoursOnly": true}),
        ),
        true,
    );
    let open = save(
        &t.ctx,
        node(
            "start",
            json!({"scheduleType": "daily", "marketHoursOnly": false}),
        ),
        true,
    );
    let s = side(&t);
    for id in [gated, open] {
        s.scheduled_run(id, true).await;
    }
    let runs =
        |id| crate::flow::store::executions::list(&t.ctx.sqlite.conn().unwrap(), id, 5).unwrap();
    assert!(
        runs(gated).is_empty(),
        "outside market hours: nothing at all"
    );
    assert_eq!(
        runs(open).len(),
        1,
        "the graph's own setting wins over the stored one"
    );
}

/// X-344, T-090: the app's own shutdown stops the trigger side (its alerts
/// and watches go, the monitor reports stopped).
#[tokio::test]
async fn the_apps_shutdown_stops_the_trigger_side() {
    let t = app();
    let hooks = t.ctx.flow.triggers();
    hooks
        .register(
            1,
            "priceAlert",
            &data(json!({"symbol": "SBIN", "price": 1})),
        )
        .await
        .unwrap();
    assert_eq!(hooks.status()["running"], true);
    t.ctx.shutdown().await;
    let v = hooks.status();
    assert_eq!(
        (v["running"].clone(), v["alerts_count"].clone()),
        (json!(false), json!(0))
    );
}

/// T-038 (WEB-DEFECT), T-037: an alert past its window is dropped and its
/// workflow stood down (the web kept the row Active, so a restart re-armed
/// it with a fresh window).
#[tokio::test]
async fn an_expired_alert_stands_its_workflow_down() {
    let t = app();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    let id = save(&t.ctx, node("priceAlert", json!({"symbol": "SBIN"})), true);
    let s = side(&t);
    s.register(
        id,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "price": 1, "expiration": "1h"})),
    )
    .await
    .unwrap();
    t.clock.advance(chrono::Duration::minutes(61));
    s.check_prices(false).await;
    assert!(s.alerts.get(id).is_none());
    assert!(
        !active(&t.ctx, id),
        "stood down, so a restart does not re-arm it"
    );
}
