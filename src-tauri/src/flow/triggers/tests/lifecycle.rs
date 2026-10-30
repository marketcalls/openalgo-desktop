//! A workflow's triggers across its life: a fire for a workflow that is
//! gone, inactive or running; delete; activation without a key; the app's
//! own start-up; the feed release that completes on its retry; the logs
//! the web writes (parity T-019, T-034, T-081, T-086, R-25, S-20, V-133,
//! X-252, X-316, X-318).

use super::*;
use std::sync::atomic::Ordering;

/// Every message logged on this thread while the guard lives (a
/// current-thread test runtime keeps its tasks on it).
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Logged {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{:?}", v);
                }
            }
        }
        let mut m = Message(String::new());
        event.record(&mut m);
        self.0.lock().push(m.0);
    }
}

fn logged() -> (tracing::subscriber::DefaultGuard, Logged) {
    use tracing_subscriber::layer::SubscriberExt;
    let l = Logged::default();
    let guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(l.clone()));
    (guard, l)
}

impl Logged {
    fn has(&self, line: &str) -> bool {
        self.0.lock().iter().any(|m| m == line)
    }
}

fn run_count(app: &Arc<AppState>, id: i64) -> usize {
    crate::flow::store::executions::list(&app.sqlite.conn().unwrap(), id, 10)
        .unwrap()
        .len()
}

fn alert_node() -> Value {
    node(
        "priceAlert",
        json!({"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800}),
    )
}

/// T-019: a scheduled fire for a workflow that is gone or inactive is
/// skipped with the web's warning and no run; one that meets a run already
/// holding the workflow runs nothing either.
#[tokio::test]
async fn a_scheduled_fire_for_a_gone_inactive_or_running_workflow_runs_nothing() {
    let t = app();
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    let s = side(&t);
    let daily = || node("start", json!({"scheduleType": "daily", "time": "09:15"}));
    let inactive = save(&t.ctx, daily(), false);
    let busy = save(&t.ctx, daily(), true);
    let (_guard, logs) = logged();
    s.scheduled_run(inactive, false).await;
    s.scheduled_run(9999, false).await;
    let claim = t.ctx.flow.try_claim(busy);
    s.scheduled_run(busy, false).await;
    drop(claim);
    assert_eq!(run_count(&t.ctx, inactive), 0);
    assert_eq!(
        run_count(&t.ctx, busy),
        0,
        "already running: nothing recorded"
    );
    assert!(logs.has(&format!(
        "Skipping scheduled workflow {}: it is not active",
        inactive
    )));
    assert!(logs.has("Skipping scheduled workflow 9999: it no longer exists"));
    assert!(logs.has(&format!(
        "Scheduled execution result for workflow {}: error",
        busy
    )));
    s.scheduled_run(busy, false).await;
    assert_eq!(run_count(&t.ctx, busy), 1, "free again: it runs");
}

/// T-086 (decision 24): delete tears down the workflow's triggers whether
/// it is active or not; a registration left behind would keep matching for
/// a workflow that no longer exists.
#[tokio::test]
async fn delete_tears_down_the_triggers_of_an_inactive_workflow_too() {
    let t = app();
    let hooks = t.ctx.flow.triggers();
    let id = save(&t.ctx, alert_node(), false);
    hooks
        .register(
            id,
            "priceAlert",
            &data(json!({"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800})),
        )
        .await
        .unwrap();
    let watch = save(
        &t.ctx,
        node("orderUpdateTrigger", json!({"symbol": "SBIN"})),
        false,
    );
    hooks
        .register(
            watch,
            "orderUpdateTrigger",
            &data(json!({"symbol": "SBIN"})),
        )
        .await
        .unwrap();
    assert_eq!(hooks.status()["alerts_count"], 1);
    for w in [id, watch] {
        let r = crate::flow::service::delete(&t.ctx, w).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    assert_eq!(hooks.status()["alerts_count"], 0);
    assert_eq!(hooks.status()["order_updates"]["watches_count"], 0);
}

/// R-25, T-081, V-133: activation without an API key answers the web's 400
/// and leaves the workflow inactive with nothing registered.
#[tokio::test]
async fn activation_without_an_api_key_is_refused() {
    let t = app();
    let id = save(&t.ctx, alert_node(), false);
    let r = crate::flow::service::activation::activate(&t.ctx, id).await;
    assert_eq!(
        (r.status, r.body),
        (400, json!({"error": "API key not configured"}))
    );
    assert!(!active(&t.ctx, id));
    assert_eq!(t.ctx.flow.triggers().status()["alerts_count"], 0);
}

/// T-034: with no API key the poll checks nothing and warns per alert, as
/// the web does.
#[tokio::test]
async fn the_poll_without_a_key_warns_for_each_alert() {
    let t = app();
    let s = side(&t);
    let id = save(&t.ctx, alert_node(), true);
    s.register(
        id,
        "priceAlert",
        &data(json!({"symbol": "SBIN", "exchange": "NSE", "condition": "above", "price": 800})),
    )
    .await
    .unwrap();
    let (_guard, logs) = logged();
    assert!(!s.check_prices(false).await);
    assert!(logs.has(&format!("No API key for alert workflow {}", id)));
    assert!(!s.alerts.get(id).unwrap().triggered);
    assert_eq!(run_count(&t.ctx, id), 0);
}

/// S-20: minting a missing webhook credential is logged as the web logs it;
/// a workflow that has both logs nothing.
#[tokio::test]
async fn minting_webhook_credentials_is_logged() {
    let t = app();
    let id = save(&t.ctx, alert_node(), false);
    let conn = t.ctx.sqlite.conn().unwrap();
    conn.execute(
        "UPDATE flow_workflows SET webhook_token = NULL WHERE id = ?1",
        [id],
    )
    .unwrap();
    let (_guard, logs) = logged();
    assert!(
        workflows::ensure_webhook_credentials(&conn, &t.ctx.security, id, t.ctx.now()).unwrap()
    );
    let line = format!("Generated webhook credentials for workflow {}", id);
    assert!(logs.has(&line));
    logs.0.lock().clear();
    assert!(
        workflows::ensure_webhook_credentials(&conn, &t.ctx.security, id, t.ctx.now()).unwrap()
    );
    assert!(!logs.has(&line), "nothing was missing");
}

/// X-316: the entry a partial release kept is cleared by the retry once the
/// feed gives it back.
#[tokio::test]
async fn the_retry_clears_what_a_partial_release_kept() {
    let subs = crate::flow::contract::subscriptions::Subscriptions::default();
    let f = feed(false);
    subs.record(Some(1), "PASS", "NSE", "LTP");
    subs.record(Some(1), "FAIL", "NSE", "LTP");
    assert_eq!(subs.release(1, &f).await, 1);
    assert!(subs.has(1));
    f.refuse_fail.store(false, Ordering::SeqCst);
    assert_eq!(subs.release(1, &f).await, 1);
    assert!(!subs.has(1));
}

/// X-318: a feed two workflows read is given back once, by the release of
/// the last of them.
#[tokio::test]
async fn a_shared_feed_is_given_back_once_by_its_last_holder() {
    let subs = crate::flow::contract::subscriptions::Subscriptions::default();
    let f = feed(false);
    subs.record(Some(1), "SBIN", "NSE", "LTP");
    subs.record(Some(2), "SBIN", "NSE", "LTP");
    assert_eq!(subs.release(1, &f).await, 1);
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        0,
        "workflow 2 still reads it"
    );
    assert_eq!(subs.release(2, &f).await, 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert!(!subs.has(1) && !subs.has(2));
}

/// X-252: opening the app restores the active workflows' triggers (its own
/// start-up path, `AppState::open` -> `flow::start` -> the restore).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_the_app_restores_its_active_triggers() {
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn crate::security::keystore::KeyStore> =
        Arc::new(crate::security::keystore::MemoryKeyStore::new());
    let open = || {
        AppState::open(
            dir.path(),
            crate::state::OpenOptions {
                keystore: keys.clone(),
                clock: Arc::new(crate::clock::SystemClock),
                brokers: Arc::new(crate::brokers::BrokerRegistry::new()),
            },
        )
        .unwrap()
    };
    let first = open();
    first.security.install(&Unlocked::random().unwrap());
    let id = save(&first, alert_node(), true);
    assert_eq!(
        first.flow.triggers().status()["alerts_count"],
        0,
        "saved, not registered"
    );
    first.shutdown().await;
    for _ in 0..1000 {
        if Arc::strong_count(&first) == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(Arc::strong_count(&first), 1, "app state outlives shutdown");
    drop(first);
    let again = open();
    let mut restored = Value::Null;
    for _ in 0..500 {
        restored = again.flow.triggers().status();
        if restored["alerts_count"] == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(restored["alerts_count"], 1, "{}", restored);
    assert_eq!(restored["alerts"][0]["workflow_id"], id);
    again.shutdown().await;
}
