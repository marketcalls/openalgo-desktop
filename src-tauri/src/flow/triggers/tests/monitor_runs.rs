//! The monitors' runs end to end on a real app context: what each trigger
//! puts in `{{webhook.*}}`, a queued run dropped when its registration was
//! replaced or its workflow stood down, the four-run pools, a one-shot
//! watch spent by its run, an every-time watch's follow-up run (parity
//! N-orderUpdateTrigger-01/-02, T-039, T-055..T-057, T-111, X-157).

use super::*;
use crate::flow::triggers::order_monitor::Watch;
use crate::flow::triggers::price_monitor::{Alert, Observed};

/// An app with an account and its API key (made before any workflow: a
/// new account's keys drop every earlier webhook secret).
fn keyed_app() -> crate::state::testing::TestCtx {
    let t = app();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    t
}

/// A trigger node of `kind` and a Log node writing `message` after it (on
/// the true branch of a priceAlert).
fn logged_after(kind: &str, data: Value, message: &str) -> (Value, Value) {
    (
        json!([
            {"id": "t", "type": kind, "position": {"x": 0, "y": 0}, "data": data},
            {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": message}}
        ]),
        if kind == "priceAlert" {
            json!([{"id": "e", "source": "t", "target": "l", "sourceHandle": "true"}])
        } else {
            json!([{"id": "e", "source": "t", "target": "l"}])
        },
    )
}

fn save_graph(app: &Arc<AppState>, (nodes, edges): (Value, Value)) -> i64 {
    let conn = app.sqlite.conn().unwrap();
    let w = workflows::create(
        &conn,
        &app.security,
        &NewWorkflow {
            name: "Monitor",
            description: None,
            nodes: &nodes,
            edges: &edges,
        },
        app.now(),
    )
    .unwrap();
    workflows::set_active(&conn, w.id, true, app.now()).unwrap();
    w.id
}

fn runs(app: &Arc<AppState>, id: i64) -> Vec<crate::flow::contract::model::Execution> {
    crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 10).unwrap()
}

/// The finished runs of `id`, waiting (bounded) until there are `n`.
async fn finished(
    app: &Arc<AppState>,
    id: i64,
    n: usize,
) -> Vec<crate::flow::contract::model::Execution> {
    for _ in 0..500 {
        let r = runs(app, id);
        if r.len() >= n && r.iter().all(|e| e.status != "running") {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("workflow {} never had {} finished runs", id, n);
}

fn log_lines(run: &crate::flow::contract::model::Execution) -> Vec<String> {
    run.logs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn alert_data() -> Value {
    json!({"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800, "trigger": "once"})
}

/// An alert for `id`, latched by a tick at 812.
fn fired_alert(s: &TriggerSide, id: i64, now: chrono::DateTime<chrono::Utc>) -> (u64, Alert) {
    let gen = s
        .alerts
        .add(Alert::from_node(id, &data(alert_data()), now).unwrap());
    let Observed::Fire(a) = s.alerts.observe(id, gen, 812.0) else {
        panic!("the alert should fire at 812");
    };
    (gen, *a)
}

fn update(orderid: &str) -> crate::events::OrderUpdate {
    crate::events::OrderUpdate {
        orderid: orderid.into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        order_status: "complete".into(),
        filled_quantity: 10,
        average_price: 812.5,
        ..Default::default()
    }
}

/// N-orderUpdateTrigger-02, T-040: a price-alert run carries the web's
/// payload (`trigger_type`, `trigger_price`, `triggered_at`); a scheduled
/// run injects nothing, so a reference to it stays as written.
#[tokio::test]
async fn each_monitor_run_carries_its_own_payload() {
    let t = keyed_app();
    let s = side(&t);
    let id = save_graph(
        &t.ctx,
        logged_after(
            "priceAlert",
            alert_data(),
            "{{webhook.trigger_type}} at {{webhook.trigger_price}} on {{webhook.triggered_at}}",
        ),
    );
    let (_, alert) = fired_alert(&s, id, t.ctx.now());
    s.queue_price_run(alert, 812.0);
    let run = &finished(&t.ctx, id, 1).await[0];
    let line = log_lines(run)
        .into_iter()
        .find(|l| l.starts_with("[LOG] "))
        .expect("the Log node ran");
    assert!(
        line.starts_with("[LOG] price_alert at 812.0 on 2026-11-03T09:30:00"),
        "{}",
        line
    );
    assert!(!line.contains("{{"), "{}", line);

    let sched = save_graph(
        &t.ctx,
        logged_after(
            "start",
            json!({"scheduleType": "daily", "time": "09:15"}),
            "{{webhook.trigger_type}}",
        ),
    );
    s.scheduled_run(sched, false).await;
    let run = &finished(&t.ctx, sched, 1).await[0];
    assert!(
        log_lines(run).contains(&"[LOG] {{webhook.trigger_type}}".to_string()),
        "{:?}",
        log_lines(run)
    );
}

/// N-orderUpdateTrigger-01, T-055, T-056: an order update runs its
/// workflow with the web's payload keys, and a one-shot watch is spent by
/// that run: the watch is gone and the workflow stood down.
#[tokio::test]
async fn an_order_update_run_reads_the_order_and_spends_a_one_shot() {
    let t = keyed_app();
    let s = side(&t);
    s.watches.attach();
    let watched = json!({"symbol": "SBIN", "status": "complete"});
    let id = save_graph(
        &t.ctx,
        logged_after(
            "orderUpdateTrigger",
            watched.clone(),
            "{{webhook.trigger_type}} {{webhook.orderid}} {{webhook.symbol}} {{webhook.exchange}} {{webhook.order_status}} {{webhook.filled_quantity}} {{webhook.average_price}}",
        ),
    );
    s.register(id, "orderUpdateTrigger", &data(watched))
        .await
        .unwrap();
    s.on_order_update(&update("2511030001"));
    let run = &finished(&t.ctx, id, 1).await[0];
    assert!(
        log_lines(run)
            .contains(&"[LOG] order_update 2511030001 SBIN NSE complete 10 812.5".to_string()),
        "{:?}",
        log_lines(run)
    );
    for _ in 0..500 {
        if !active(&t.ctx, id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !active(&t.ctx, id),
        "a spent one-shot stands its workflow down"
    );
    assert!(s.watches.get(id).is_none());
}

/// T-057 (WEB-DEFECT fixed): an every-time watch that hears an update
/// while its run is queued or running runs again for it when that run
/// ends; the web dropped it.
#[tokio::test]
async fn an_every_time_watch_runs_again_for_the_update_it_heard_meanwhile() {
    let t = keyed_app();
    let s = side(&t);
    s.watches.attach();
    let watched = json!({"symbol": "SBIN", "status": "complete", "trigger": "every_time"});
    let id = save_graph(
        &t.ctx,
        logged_after(
            "orderUpdateTrigger",
            watched.clone(),
            "order {{webhook.orderid}}",
        ),
    );
    s.register(id, "orderUpdateTrigger", &data(watched))
        .await
        .unwrap();
    // Every worker busy: the first run waits, so the second update meets it.
    let busy = s.order_runs.clone().acquire_many_owned(4).await.unwrap();
    s.on_order_update(&update("1001"));
    s.on_order_update(&update("1002"));
    drop(busy);
    let done = finished(&t.ctx, id, 2).await;
    let mut seen: Vec<String> = done
        .iter()
        .flat_map(log_lines)
        .filter(|l| l.starts_with("[LOG] "))
        .collect();
    seen.sort();
    assert_eq!(seen, vec!["[LOG] order 1001", "[LOG] order 1002"]);
    assert!(active(&t.ctx, id), "every time: still active");
}

/// T-039, T-111: at most four price-alert runs at once (the web's worker
/// pool): with four holding it, a fifth waits for one to end.
#[tokio::test]
async fn a_fifth_monitor_run_waits_for_one_of_four_workers() {
    let t = app();
    let s = side(&t);
    assert_eq!(s.price_runs.available_permits(), 4);
    assert_eq!(s.order_runs.available_permits(), 4);
    let id = save(&t.ctx, node("priceAlert", alert_data()), true);
    let (_, alert) = fired_alert(&s, id, t.ctx.now());
    let busy = s.price_runs.clone().acquire_many_owned(4).await.unwrap();
    s.queue_price_run(alert, 812.0);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(runs(&t.ctx, id).is_empty(), "the fifth run waits");
    drop(busy);
    assert_eq!(finished(&t.ctx, id, 1).await.len(), 1);
}

/// T-039, X-157, T-112: a queued price-alert run whose alert was replaced
/// before it started runs nothing and leaves the new alert as it is; one
/// whose workflow was stood down runs nothing either.
#[tokio::test]
async fn a_queued_price_run_for_a_replaced_alert_or_a_stood_down_workflow_runs_nothing() {
    let t = app();
    let s = side(&t);
    let id = save(&t.ctx, node("priceAlert", alert_data()), true);
    let (_, old) = fired_alert(&s, id, t.ctx.now());
    let busy = s.price_runs.clone().acquire_many_owned(4).await.unwrap();
    s.queue_price_run(old, 812.0);
    let newer = s
        .alerts
        .add(Alert::from_node(id, &data(alert_data()), t.ctx.now()).unwrap());
    drop(busy);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        runs(&t.ctx, id).is_empty(),
        "the replaced alert's run is dropped"
    );
    let current = s.alerts.get(id).unwrap();
    assert_eq!(current.gen, newer);
    assert!(!current.triggered, "the new alert is untouched");

    let gone = save(&t.ctx, node("priceAlert", alert_data()), true);
    let (_, alert) = fired_alert(&s, gone, t.ctx.now());
    let busy = s.price_runs.clone().acquire_many_owned(4).await.unwrap();
    s.queue_price_run(alert, 812.0);
    workflows::set_active(&t.ctx.sqlite.conn().unwrap(), gone, false, t.ctx.now()).unwrap();
    drop(busy);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        runs(&t.ctx, gone).is_empty(),
        "an inactive workflow does not run"
    );
}

/// T-055, T-112: a queued order-update run whose watch was replaced, or
/// whose workflow was stood down, runs nothing.
#[tokio::test]
async fn a_queued_order_run_for_a_replaced_watch_or_a_stood_down_workflow_runs_nothing() {
    let t = app();
    let s = side(&t);
    s.watches.attach();
    let watched = json!({"symbol": "SBIN", "status": "complete"});
    let id = save(&t.ctx, node("orderUpdateTrigger", watched.clone()), true);
    let gone = save(&t.ctx, node("orderUpdateTrigger", watched.clone()), true);
    for w in [id, gone] {
        s.register(w, "orderUpdateTrigger", &data(watched.clone()))
            .await
            .unwrap();
    }
    let busy = s.order_runs.clone().acquire_many_owned(4).await.unwrap();
    s.on_order_update(&update("2001"));
    let newer = s
        .watches
        .add(Watch::from_node(id, &data(watched.clone())).unwrap());
    workflows::set_active(&t.ctx.sqlite.conn().unwrap(), gone, false, t.ctx.now()).unwrap();
    drop(busy);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        runs(&t.ctx, id).is_empty(),
        "the replaced watch's run is dropped"
    );
    assert!(
        runs(&t.ctx, gone).is_empty(),
        "an inactive workflow does not run"
    );
    let current = s.watches.get(id).unwrap();
    assert_eq!(current.gen, newer);
    assert!(!current.triggered, "the new watch is untouched");
}
