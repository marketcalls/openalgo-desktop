//! The trigger side's service paths and the lines they log, on a real app
//! context: a PUT that re-registers (or cannot), activation's guard order
//! and failures, deactivation's failures, the webhook settings' failures,
//! the public webhook's lookup and run lines, the order-update monitor's
//! lines, the skips while signed out, and a background run in flight at
//! shutdown (parity R-21, R-25, R-26, R-30..R-32, R-41..R-43, R-45, R-54,
//! R-55, S-15..S-20, V-133, V-136, T-051, T-054..T-056, T-071, T-081,
//! T-083..T-085, X-344, D-06).

use super::support::*;
use super::*;
use crate::flow::service::{activation, webhooks, Body};
use crate::flow::triggers::order_monitor::{After, Watch};
use crate::services::core::Reply;

/// The app's own trigger side stopped (its start-up restore cannot race a
/// save) and `hooks` installed in its place.
async fn install(t: &crate::state::testing::TestCtx, hooks: Arc<dyn TriggerHooks>) {
    t.ctx.flow.triggers().stop().await;
    t.ctx.flow.set_triggers(hooks);
}

/// A fresh trigger side installed as the app's hooks.
async fn hooked(t: &crate::state::testing::TestCtx) -> Arc<TriggerSide> {
    let s = side(t);
    install(t, s.clone()).await;
    s
}

/// Refuse every update of `column` on the workflows table (only those that
/// set it to `when`, if given), as a database that cannot be written
/// answers.
fn refuse(app: &Arc<AppState>, column: &str, when: Option<&str>) {
    let guard = when
        .map(|v| format!("WHEN NEW.{column} = {v}"))
        .unwrap_or_default();
    app.sqlite
        .conn()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse_{column} BEFORE UPDATE OF {column} ON flow_workflows {guard}
             BEGIN SELECT RAISE(ABORT, 'refused'); END;"
        ))
        .unwrap();
}

fn allow(app: &Arc<AppState>, column: &str) {
    app.sqlite
        .conn()
        .unwrap()
        .execute_batch(&format!("DROP TRIGGER refuse_{column}"))
        .unwrap();
}

fn reply(r: Reply) -> (u16, Value) {
    (r.status, r.body)
}

fn daily(time: &str) -> Value {
    node("start", json!({"scheduleType": "daily", "time": time}))
}

/// A webhook trigger and a Log node the trigger never reaches: the strict
/// validator refuses it.
fn incomplete() -> Value {
    json!([
        {"id": "w", "type": "webhookTrigger", "position": {"x": 0, "y": 0}, "data": {}},
        {"id": "l", "type": "log", "position": {"x": 0, "y": 100}, "data": {"message": "x"}}
    ])
}

fn id_of(app: &Arc<AppState>, token: &str) -> i64 {
    workflows::get_by_token(&app.sqlite.conn().unwrap(), &app.security, token)
        .unwrap()
        .unwrap()
        .id
}

/// The newest finished run of `id`, waiting (bounded) for it.
async fn last_run(app: &Arc<AppState>, id: i64) -> crate::flow::contract::model::Execution {
    for _ in 0..500 {
        let runs =
            crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 1).unwrap();
        if let Some(r) = runs.into_iter().next().filter(|r| r.status != "running") {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("workflow {} never finished a run", id);
}

/// Hooks whose registry is down: every registration and removal fails.
struct Unavailable;

const DOWN: &str = "the job store is unavailable";

#[async_trait]
impl FeedRelease for Unavailable {
    async fn unsubscribe(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
}

#[async_trait]
impl TriggerHooks for Unavailable {
    async fn register(&self, _: i64, _: &str, _: &Map<String, Value>) -> Result<(), TriggerError> {
        Err(TriggerError::Failed(DOWN.into()))
    }

    async fn unregister(&self, _: i64) -> Result<(), String> {
        Err(DOWN.into())
    }
}

/// R-21, T-085: a PUT that changes an active workflow's trigger logs the
/// web's line and re-registers it with the new settings; a PUT that leaves
/// the trigger alone does neither. Decision 34: a PUT whose write fails
/// answers 500.
#[tokio::test]
async fn a_put_re_registers_a_changed_trigger_and_answers_500_when_unwritten() {
    let t = keyed();
    let s = hooked(&t).await;
    let id = save(&t.ctx, daily("09:15"), true);
    s.register(
        id,
        "start",
        &data(json!({"scheduleType": "daily", "time": "09:15"})),
    )
    .await
    .unwrap();
    let (_guard, logs) = logged();
    let r = crate::flow::service::update(&t.ctx, id, Body::Json(json!({"nodes": daily("10:30")})))
        .await;
    assert_eq!(
        (
            r.status,
            r.body["needs_reactivate"].clone(),
            r.body["is_active"].clone()
        ),
        (200, json!(false), json!(true)),
        "{}",
        r.body
    );
    let line = format!(
        "Workflow {} trigger configuration changed while active; re-registering it",
        id
    );
    assert!(logs.has(&line), "{:?}", logs.0.lock());
    assert_eq!(
        store::next_run(&t.ctx.sqlite.conn().unwrap(), id)
            .unwrap()
            .as_deref(),
        Some("2026-11-03T05:00:00"),
        "the job follows 10:30 IST"
    );
    let r = crate::flow::service::update(&t.ctx, id, Body::Json(json!({"name": "Renamed"}))).await;
    assert_eq!(r.status, 200);
    assert_eq!(logs.starting_with(&line).len(), 1, "no trigger change");
    refuse(&t.ctx, "name", None);
    let r = crate::flow::service::update(&t.ctx, id, Body::Json(json!({"name": "Again"}))).await;
    assert_eq!(
        reply(r),
        (500, json!({"error": "Failed to update workflow"}))
    );
    assert_eq!(
        logs.starting_with("Flow could not update workflow: ").len(),
        1
    );
}

/// R-30, T-085: re-registration refuses a graph whose trigger was removed
/// and a run with no API key, with the web's sentences; a PUT that meets
/// either stands the workflow down and says so.
#[tokio::test]
async fn re_registration_refuses_no_trigger_and_no_key() {
    use crate::flow::service::triggers::reregister;
    for (t, nodes, refusal) in [
        (
            keyed(),
            node("log", json!({"message": "x"})),
            TriggerError::Invalid("Workflow has no trigger node".into()),
        ),
        (
            app(),
            daily("10:30"),
            TriggerError::Failed("API key not configured".into()),
        ),
    ] {
        let s = hooked(&t).await;
        let id = save(&t.ctx, daily("09:15"), true);
        s.register(
            id,
            "start",
            &data(json!({"scheduleType": "daily", "time": "09:15"})),
        )
        .await
        .unwrap();
        let mut changed = crate::flow::service::load(&t.ctx, id)
            .await
            .unwrap()
            .unwrap();
        changed.nodes = nodes.clone();
        assert_eq!(reregister(&t.ctx, &changed).await, Err(refusal));
        let (_guard, logs) = logged();
        let r = crate::flow::service::update(&t.ctx, id, Body::Json(json!({"nodes": nodes}))).await;
        assert_eq!(
            (
                r.status,
                r.body["needs_reactivate"].clone(),
                r.body["is_active"].clone()
            ),
            (200, json!(true), json!(false)),
            "{}",
            r.body
        );
        assert!(logs.has(&format!(
            "Could not re-register the trigger for workflow {}; deactivating it so it cannot run a stale configuration",
            id
        )));
        assert!(!active(&t.ctx, id) && !s.scheduler.has(id));
    }
}

/// R-25, V-133, T-081: the guards in the web's order, each pinned against
/// the next with two failing at once: already active before the key, the
/// key before the strict validator. A graph with no trigger is refused by
/// the validator, so the `No trigger node found in workflow` 400 after it
/// is unreachable, on the web as here.
#[tokio::test]
async fn activation_checks_its_guards_in_the_webs_order() {
    let t = app();
    hooked(&t).await;
    let running = save(&t.ctx, incomplete(), true);
    assert_eq!(
        reply(activation::activate(&t.ctx, running).await),
        (
            200,
            json!({"status": "already_active", "message": "Workflow is already active"})
        ),
        "already active, with no key and a graph the validator refuses"
    );
    let stopped = save(&t.ctx, incomplete(), false);
    assert_eq!(
        reply(activation::activate(&t.ctx, stopped).await),
        (400, json!({"error": "API key not configured"})),
        "the key, before the validator"
    );
    let t = keyed();
    hooked(&t).await;
    let no_trigger = save(&t.ctx, node("log", json!({"message": "x"})), false);
    let (status, body) = reply(activation::activate(&t.ctx, no_trigger).await);
    assert_eq!(
        (status, &body["error"]),
        (400, &json!("Workflow cannot be activated"))
    );
    assert!(
        body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["code"] == "no_trigger"
                && e["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("Workflow has no trigger node, so it can never run.")),
        "{}",
        body
    );
    assert!(!active(&t.ctx, no_trigger));
}

/// R-26, S-15, T-081: activation stores no API key (decision 3: a run
/// trades with the current key); a flag that cannot be written and a
/// registration that fails are logged with the web's lines.
#[tokio::test]
async fn activation_stores_no_key_and_logs_its_failures() {
    let t = keyed();
    hooked(&t).await;
    let id = save(&t.ctx, daily("09:15"), false);
    let (_guard, logs) = logged();
    let r = activation::activate(&t.ctx, id).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let key: Option<String> = t
        .ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT api_key FROM flow_workflows WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(key, None);
    let persist = save(&t.ctx, daily("09:15"), false);
    refuse(&t.ctx, "is_active", None);
    assert_eq!(
        reply(activation::activate(&t.ctx, persist).await),
        (500, json!({"error": "Could not activate workflow"}))
    );
    assert!(logs.has(&format!(
        "Failed to persist active state for workflow {}",
        persist
    )));
    allow(&t.ctx, "is_active");
    refuse(&t.ctx, "schedule_job_id", None);
    let record = "Could not record the scheduler job id for this workflow";
    assert_eq!(
        reply(activation::activate(&t.ctx, persist).await),
        (500, json!({"error": record}))
    );
    assert!(logs.has(&format!(
        "Failed to activate workflow {}: {}",
        persist, record
    )));
    assert!(!active(&t.ctx, persist), "rolled back");
}

/// R-31, T-083: a rollback that cannot undo the registration or the
/// active flag says so in the web's words (the one unregister call removes
/// every kind, so its line is the scheduler job's).
#[tokio::test]
async fn a_rollback_that_cannot_undo_says_so() {
    let t = keyed();
    install(&t, Arc::new(Unavailable)).await;
    let id = save(&t.ctx, daily("09:15"), false);
    refuse(&t.ctx, "is_active", Some("0"));
    let (_guard, logs) = logged();
    assert_eq!(
        reply(activation::activate(&t.ctx, id).await),
        (500, json!({"error": DOWN}))
    );
    assert!(logs.has(&format!("Failed to activate workflow {}: {}", id, DOWN)));
    let manual = "after a failed activation; it may need to be deactivated manually: ";
    assert!(logs.has(&format!(
        "Could not roll back the scheduler job for workflow {} {}{}",
        id, manual, DOWN
    )));
    assert_eq!(
        logs.starting_with(&format!(
            "Could not roll back the active flag for workflow {} {}",
            id, manual
        ))
        .len(),
        1,
        "{:?}",
        logs.0.lock()
    );
    assert!(active(&t.ctx, id), "the flag could not be cleared");
}

/// R-32, T-084: a registration that cannot be removed answers its error
/// and leaves the workflow active; an inactive flag that cannot be written
/// answers the web's 500; both logged with the web's lines.
#[tokio::test]
async fn deactivation_logs_and_answers_its_failures() {
    let t = app();
    install(&t, Arc::new(Unavailable)).await;
    let id = save(&t.ctx, daily("09:15"), true);
    let (_guard, logs) = logged();
    assert_eq!(
        reply(activation::deactivate(&t.ctx, id).await),
        (500, json!({"error": DOWN}))
    );
    assert!(logs.has(&format!("Failed to deactivate workflow {}: {}", id, DOWN)));
    assert!(active(&t.ctx, id));
    hooked(&t).await;
    refuse(&t.ctx, "is_active", None);
    assert_eq!(
        reply(activation::deactivate(&t.ctx, id).await),
        (500, json!({"error": "Could not deactivate workflow"}))
    );
    assert!(logs.has(&format!(
        "Failed to persist inactive state for workflow {}",
        id
    )));
    assert!(active(&t.ctx, id));
}

/// R-41..R-43: each settings write that fails answers the web's 500; a
/// regenerate whose secret fails keeps the new token, as the web's (the
/// token was committed first).
#[tokio::test]
async fn the_settings_writes_answer_the_webs_500s() {
    let t = app();
    let id = save(&t.ctx, incomplete(), false);
    refuse(&t.ctx, "webhook_enabled", None);
    assert_eq!(
        reply(webhooks::enable(&t.ctx, id).await),
        (500, json!({"error": "Failed to enable webhook"}))
    );
    assert_eq!(
        reply(webhooks::disable(&t.ctx, id).await),
        (500, json!({"error": "Failed to disable webhook"}))
    );
    let before = crate::flow::service::load(&t.ctx, id)
        .await
        .unwrap()
        .unwrap();
    refuse(&t.ctx, "webhook_secret", None);
    let secret_failed = (500, json!({"error": "Failed to regenerate secret"}));
    assert_eq!(reply(webhooks::regenerate(&t.ctx, id).await), secret_failed);
    let after = crate::flow::service::load(&t.ctx, id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(after.webhook_token, before.webhook_token);
    assert_eq!(
        after.webhook_secret.map(|s| s.expose().to_string()),
        before.webhook_secret.map(|s| s.expose().to_string())
    );
    assert_eq!(
        reply(webhooks::regenerate_secret(&t.ctx, id).await),
        secret_failed
    );
}

/// S-20 (decision 36): the settings go on whatever minting the missing
/// credentials did, as the web's: a failed mint was rolled back whole
/// (the token with the secret), so the answer carries neither.
#[tokio::test]
async fn the_settings_answer_what_is_stored_when_minting_fails() {
    let t = app();
    let id = save(&t.ctx, incomplete(), false);
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE flow_workflows SET webhook_token = NULL, webhook_secret = NULL,
                 webhook_secret_nonce = NULL WHERE id = ?1",
            [id],
        )
        .unwrap();
    refuse(&t.ctx, "webhook_secret", None);
    let (status, v) = reply(webhooks::info(&t.ctx, id).await);
    assert_eq!(
        (
            status,
            v["webhook_token"].clone(),
            v["webhook_secret"].clone()
        ),
        (200, Value::Null, Value::Null),
        "{}",
        v
    );
    allow(&t.ctx, "webhook_secret");
    let (status, v) = reply(webhooks::info(&t.ctx, id).await);
    assert_eq!(status, 200);
    assert_eq!(v["webhook_token"].as_str().map(str::len), Some(43));
    assert_eq!(v["webhook_secret"].as_str().map(str::len), Some(64));
}

/// S-17, S-18, S-19, R-45 (decision 36): the service writes none of the
/// store's lines itself, so each is written once, where the web writes it
/// (the store tests count that one); an auth type that is not text never
/// reaches the store and is logged here, as the web's store logs it.
#[tokio::test]
async fn the_rotation_and_auth_type_lines_are_written_once() {
    let t = app();
    let id = save(&t.ctx, incomplete(), false);
    let (_guard, logs) = logged();
    assert_eq!(webhooks::regenerate(&t.ctx, id).await.status, 200);
    assert_eq!(webhooks::regenerate_secret(&t.ctx, id).await.status, 200);
    let invalid = (400, json!({"error": "Invalid auth type"}));
    for given in [json!("header"), json!(123), Value::Null] {
        let body = Body::Json(json!({"auth_type": given}));
        assert_eq!(
            reply(webhooks::set_auth_type(&t.ctx, id, body).await),
            invalid
        );
    }
    for line in [
        format!("Regenerated webhook token for workflow {}", id),
        format!("Regenerated webhook secret for workflow {}", id),
        "Invalid webhook auth type: header".to_string(),
    ] {
        assert!(!logs.has(&line), "{} was written by the service too", line);
    }
    assert!(logs.has("Invalid webhook auth type: 123"));
    assert!(logs.has("Invalid webhook auth type: None"));
}

/// S-16 (decision 36): a token lookup that fails answers the web's 404 for
/// an unknown token, never a sentence about the failure; the failure is
/// logged.
#[tokio::test]
async fn a_failed_token_lookup_answers_404_and_is_logged() {
    let t = app();
    let (token, secret) = live_hook(&t.ctx, incomplete(), json!([]));
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("ALTER TABLE flow_workflows RENAME TO flow_workflows_gone")
        .unwrap();
    let (_guard, logs) = logged();
    let a = webhooks::trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(
        (a.reply.status, a.reply.body, a.wrong_secret),
        (404, json!({"error": "Invalid webhook token"}), false)
    );
    assert_eq!(
        logs.starting_with("Error getting workflow by webhook token: ")
            .len(),
        1,
        "{:?}",
        logs.0.lock()
    );
}

/// R-54, V-136, T-071, R-55: a webhook the strict validator refuses logs
/// the web's line with the validator's first message; one that runs logs
/// the web's line naming the workflow.
#[tokio::test]
async fn the_webhook_logs_its_refusal_and_its_run() {
    let t = keyed();
    let (token, secret) = live_hook(&t.ctx, incomplete(), json!([]));
    let blocked = id_of(&t.ctx, &token);
    let (_guard, logs) = logged();
    let a = webhooks::trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(a.reply.status, 400, "{}", a.reply.body);
    let message = a.reply.body["message"].as_str().unwrap().to_string();
    assert!(!message.is_empty());
    assert!(
        logs.has(&format!(
            "Webhook for workflow {} rejected: {}",
            blocked, message
        )),
        "{:?}",
        logs.0.lock()
    );
    assert!(!logs.has(&format!("Webhook triggered for workflow {}: Hook", blocked)));
    let (token, secret) = live_hook(
        &t.ctx,
        incomplete(),
        json!([{"id": "e", "source": "w", "target": "l"}]),
    );
    let runs = id_of(&t.ctx, &token);
    let a = webhooks::trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(a.reply.status, 200, "{}", a.reply.body);
    assert!(logs.has(&format!("Webhook triggered for workflow {}: Hook", runs)));
}

fn update(orderid: &str) -> crate::events::OrderUpdate {
    crate::events::OrderUpdate {
        orderid: orderid.into(),
        symbol: "SBIN".into(),
        exchange: "NSE".into(),
        order_status: "complete".into(),
        ..Default::default()
    }
}

/// T-051, T-054, T-055: a watch added, an update that fires it and the
/// run's result are logged with the web's lines (the desktop has one
/// account, so the web's ` user=` has nothing to name: T-052).
#[tokio::test]
async fn the_order_update_monitor_logs_a_watch_its_fire_and_its_result() {
    let t = app();
    let s = side(&t);
    s.watches.attach();
    let id = save(
        &t.ctx,
        node("orderUpdateTrigger", json!({"symbol": "SBIN"})),
        true,
    );
    let other = save(&t.ctx, node("orderUpdateTrigger", json!({})), false);
    let (_guard, logs) = logged();
    s.register(id, "orderUpdateTrigger", &data(json!({"symbol": "SBIN"})))
        .await
        .unwrap();
    s.register(
        other,
        "orderUpdateTrigger",
        &data(json!({"orderId": "2511030009", "status": "Complete"})),
    )
    .await
    .unwrap();
    assert!(logs.has(&format!(
        "Added order-update watch for workflow {}: orderid=None symbol=SBIN status=complete",
        id
    )));
    assert!(logs.has(&format!(
        "Added order-update watch for workflow {}: orderid=2511030009 symbol=None status=complete",
        other
    )));
    s.on_order_update(&update("2511030001"));
    assert!(logs.has(&format!(
        "Order-update trigger fired for workflow {}: orderid=2511030001 status=complete",
        id
    )));
    assert_eq!(logs.starting_with("Order-update trigger fired").len(), 1);
    // No API key in this app: the run is skipped and the result says so.
    wait_for(&logs, &format!("Workflow {} execution result: skipped", id)).await;
}

/// T-055, T-039: a queued order-update run whose watch was replaced, whose
/// workflow was stood down, or whose active flag cannot be read, is
/// dropped with the web's lines.
#[tokio::test]
async fn a_dropped_order_update_run_says_why() {
    let t = app();
    let s = side(&t);
    s.watches.attach();
    let watched = json!({"symbol": "SBIN", "status": "complete"});
    let ids: Vec<i64> = (0..3)
        .map(|_| save(&t.ctx, node("orderUpdateTrigger", watched.clone()), true))
        .collect();
    for id in &ids {
        s.register(*id, "orderUpdateTrigger", &data(watched.clone()))
            .await
            .unwrap();
    }
    let (replaced, stood_down, unreadable) = (ids[0], ids[1], ids[2]);
    let (_guard, logs) = logged();
    let busy = s.order_runs.clone().acquire_many_owned(4).await.unwrap();
    s.on_order_update(&update("2511030002"));
    s.watches
        .add(Watch::from_node(replaced, &data(watched.clone())).unwrap());
    let conn = t.ctx.sqlite.conn().unwrap();
    workflows::set_active(&conn, stood_down, false, t.ctx.now()).unwrap();
    conn.execute(
        "UPDATE flow_workflows SET is_active = 'unreadable' WHERE id = ?1",
        [unreadable],
    )
    .unwrap();
    drop(conn);
    drop(busy);
    wait_for(
        &logs,
        &format!(
            "Dropping queued order-update run for workflow {}: the watch was removed or replaced before this run claimed it.",
            replaced
        ),
    )
    .await;
    for id in [stood_down, unreadable] {
        wait_for(
            &logs,
            &format!(
                "Dropping queued order-update run for workflow {}: the workflow is no longer active.",
                id
            ),
        )
        .await;
    }
    assert_eq!(
        logs.starting_with(&format!(
            "Could not confirm workflow {} is active: ",
            unreadable
        ))
        .len(),
        1,
        "{:?}",
        logs.0.lock()
    );
}

/// T-056: after its run a one-shot watch that was replaced is left alone,
/// one whose run executed is removed, one whose run did not is re-armed,
/// each with the web's line.
#[tokio::test]
async fn a_one_shot_watch_after_its_run_logs_the_webs_lines() {
    let w = order_monitor::Watches::default();
    w.attach();
    let watch = || Watch::from_node(7, &data(json!({"symbol": "SBIN"}))).unwrap();
    let old = w.add(watch());
    let current = w.add(watch());
    let (_guard, logs) = logged();
    assert!(matches!(w.after_run(7, old, true), After::Done));
    assert!(logs.has(
        "One-shot order-update watch for workflow 7 was already replaced; leaving the current registration alone."
    ));
    assert_eq!(w.claim(&update("1")), vec![(7, current)]);
    assert!(matches!(w.after_run(7, current, false), After::Rearmed));
    assert!(
        logs.has("Re-arming one-shot order-update watch for workflow 7: the run did not execute.")
    );
    assert!(!w.get(7).unwrap().triggered, "re-armed");
    assert!(!logs.has("Removed order-update watch for workflow 7"));
    assert_eq!(w.claim(&update("2")), vec![(7, current)]);
    assert!(matches!(w.after_run(7, current, true), After::Retire));
    assert!(logs.has("Removed order-update watch for workflow 7"));
    assert!(w.get(7).is_none());
}

/// D-06 (decision 16): a price alert and an order update that fire while
/// OpenAlgo is signed out are written to the history as skipped with the
/// reason naming the trigger; the poll checks no price meanwhile, and says
/// so once.
#[tokio::test]
async fn price_alerts_and_order_updates_while_signed_out_are_skipped_with_the_reason() {
    let t = app();
    // Only this side acts: the app's own restores these workflows at start-up
    // and would poll and log the pause line too.
    t.ctx.flow.triggers().stop().await;
    let s = side(&t);
    s.watches.attach();
    let alert_data = json!({"symbol": "SBIN", "condition": "above", "price": 800});
    let alert = save(&t.ctx, node("priceAlert", alert_data.clone()), true);
    let watch = save(
        &t.ctx,
        node("orderUpdateTrigger", json!({"symbol": "SBIN"})),
        true,
    );
    s.register(
        watch,
        "orderUpdateTrigger",
        &data(json!({"symbol": "SBIN"})),
    )
    .await
    .unwrap();
    s.register(alert, "priceAlert", &data(alert_data))
        .await
        .unwrap();
    let gen = s.alerts.get(alert).unwrap().gen;
    let price_monitor::Observed::Fire(fired) = s.alerts.observe(alert, gen, 812.0) else {
        panic!("the alert should fire at 812");
    };
    t.ctx.security.lock_with(|| {});
    let (_guard, logs) = logged();
    s.queue_price_run(*fired, 812.0);
    s.on_order_update(&update("2511030003"));
    for (id, noun) in [(alert, "price alert"), (watch, "order update")] {
        let reason = format!(
            "Skipped: OpenAlgo was signed out when this {} fired, so the workflow did not run. Its triggers run again after you sign in.",
            noun
        );
        let run = last_run(&t.ctx, id).await;
        assert_eq!(
            (run.status.as_str(), run.error.as_deref()),
            ("failed", Some(reason.as_str()))
        );
        wait_for(&logs, &format!("Workflow {}: {}", id, reason)).await;
        assert!(active(&t.ctx, id), "a skip never deactivates");
    }
    assert!(s.check_prices(false).await, "paused");
    assert!(s.check_prices(true).await);
    assert_eq!(
        logs.starting_with("Flow price alerts are not checked for now. Skipped: OpenAlgo was signed out when this price alert fired")
            .len(),
        1
    );
}

/// X-344: background runs in flight when the app shuts down (one a
/// webhook started, one a schedule started, each waiting a minute) are
/// ended by the shutdown, which returns in bounded time: each run is
/// recorded failed, its workflow is free, and nothing of Flow is left
/// running.
#[tokio::test]
async fn the_apps_shutdown_ends_the_background_runs_in_flight() {
    let t = keyed();
    let s = hooked(&t).await;
    let waits = |trigger: Value| {
        json!([
            trigger,
            {"id": "d", "type": "delay", "position": {"x": 0, "y": 100}, "data": {"delayValue": 60}}
        ])
    };
    let edge = json!([{"id": "e", "source": "t", "target": "d"}]);
    let (token, secret) = live_hook(
        &t.ctx,
        waits(
            json!({"id": "t", "type": "webhookTrigger", "position": {"x": 0, "y": 0}, "data": {}}),
        ),
        edge.clone(),
    );
    let hooked_run = id_of(&t.ctx, &token);
    let a = webhooks::trigger(&t.ctx, hook_call(&token, &secret)).await;
    assert_eq!(a.reply.status, 202, "{}", a.reply.body);
    let scheduled = {
        let conn = t.ctx.sqlite.conn().unwrap();
        let w = workflows::create(
            &conn,
            &t.ctx.security,
            &NewWorkflow {
                name: "Waits",
                description: None,
                nodes: &waits(daily("09:15")[0].clone()),
                edges: &edge,
            },
            t.ctx.now(),
        )
        .unwrap();
        workflows::set_active(&conn, w.id, true, t.ctx.now()).unwrap();
        w.id
    };
    s.scheduled_run(scheduled, false).await;
    assert_eq!(t.ctx.flow.running(), 2, "both runs wait in the background");
    let (_guard, logs) = logged();
    tokio::time::timeout(std::time::Duration::from_secs(30), t.ctx.shutdown())
        .await
        .expect("the shutdown returned");
    assert_eq!(t.ctx.flow.running(), 0);
    assert_eq!(s.task_count(), 0);
    for id in [hooked_run, scheduled] {
        assert_eq!(
            last_run(&t.ctx, id).await.status,
            "failed",
            "workflow {}",
            id
        );
        assert!(
            t.ctx.flow.try_claim(id).is_some(),
            "workflow {} is free",
            id
        );
    }
    // T-017, T-045, T-060: the web's three shutdown lines (schedule_logs pins
    // their order and that each is written once).
    for line in [
        "Flow Scheduler shutdown",
        "FlowPriceMonitor shutdown",
        "FlowOrderUpdateMonitor shutdown",
    ] {
        assert!(logs.has(line), "{}", line);
    }
}
