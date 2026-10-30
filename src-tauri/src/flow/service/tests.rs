//! The service's rules that HTTP tests do not reach easily: the run status
//! table, body reading, trigger re-registration and its rollback, and a
//! delete that waits for the feed.

use super::*;
use crate::flow::contract::subscriptions::FeedRelease;
use crate::flow::contract::triggers::{TriggerError, TriggerHooks};
use crate::security::Unlocked;
use async_trait::async_trait;
use parking_lot::Mutex;

/// R-35, one row per branch.
#[test]
fn run_results_map_to_the_webs_status_codes() {
    for (result, code) in [
        (
            json!({"status": "error", "message": "Workflow is already running", "already_running": true}),
            409,
        ),
        (json!({"status": "accepted", "accepted": true}), 202),
        (json!({"status": "error", "busy": true}), 429),
        (json!({"status": "error", "errors": [{"node": "a"}]}), 502),
        (json!({"status": "error", "errors": []}), 500),
        (
            json!({"status": "error", "message": "No trigger node found"}),
            500,
        ),
        (json!({"status": "success"}), 200),
        (json!([]), 200),
    ] {
        assert_eq!(run_status(&result), code, "{}", result);
    }
}

#[test]
fn bodies_read_as_flask_reads_them() {
    assert_eq!(Body::parse(b""), Body::Nothing);
    assert_eq!(Body::parse(b"  \n"), Body::Nothing);
    assert_eq!(Body::parse(b"{}"), Body::Nothing);
    assert_eq!(Body::parse(b"false"), Body::Nothing);
    assert_eq!(Body::parse(b"{\"a\": NaN}"), Body::NonFinite);
    assert_eq!(Body::parse(b"[Infinity]"), Body::NonFinite);
    // Not JSON at all is the web's "No data provided" (decision 24).
    assert_eq!(Body::parse(b"BUY NIFTY"), Body::Nothing);
    assert_eq!(Body::parse(b"{\"a\": 1"), Body::Nothing);
    assert_eq!(Body::parse(&[0xff, 0xfe]), Body::Nothing);
    assert_eq!(Body::parse(b"[1]"), Body::Json(json!([1])));
}

/// Hooks that record what they were asked and fail on demand.
#[derive(Default)]
struct Hooks {
    calls: Mutex<Vec<String>>,
    refuse_register: bool,
    feed_confirms: bool,
}

#[async_trait]
impl FeedRelease for Hooks {
    async fn unsubscribe(&self, symbol: &str, _: &str, _: &str) -> bool {
        self.calls.lock().push(format!("unsubscribe {}", symbol));
        self.feed_confirms
    }
}

#[async_trait]
impl TriggerHooks for Hooks {
    async fn register(
        &self,
        id: i64,
        t: &str,
        data: &Map<String, Value>,
    ) -> std::result::Result<(), TriggerError> {
        self.calls.lock().push(format!(
            "register {} {} {}",
            id,
            t,
            data.get("time").map(pyfmt::str).unwrap_or_default()
        ));
        if self.refuse_register {
            return Err(TriggerError::Invalid("Order ID is required".into()));
        }
        Ok(())
    }

    async fn unregister(&self, id: i64) -> std::result::Result<(), String> {
        self.calls.lock().push(format!("unregister {}", id));
        Ok(())
    }
}

fn app() -> crate::state::testing::TestCtx {
    let t = crate::state::testing::build(
        crate::brokers::BrokerRegistry::new(),
        "2026-10-30T03:45:00Z".parse().unwrap(),
    );
    t.ctx.security.install(&Unlocked::random().unwrap());
    crate::services::auth_service::AuthService::setup(
        &t.ctx,
        "trader",
        "trader@example.com",
        "Secret@123",
    )
    .unwrap();
    t
}

fn scheduled(time: &str) -> Value {
    json!([{"id": "s", "type": "start", "position": {"x": 0, "y": 0},
             "data": {"scheduleType": "daily", "time": time, "days": [1, 2, 3, 4, 5]}}])
}

async fn active_workflow(app: &Arc<AppState>) -> i64 {
    let w = create_workflow(app, "A".into(), None, scheduled("09:15"), json!([]))
        .await
        .unwrap();
    let conn = app.sqlite.conn().unwrap();
    workflows::set_active(&conn, w.id, true, app.now()).unwrap();
    w.id
}

/// R-21: a saved change to an active trigger re-registers it; a save that
/// leaves it alone does not touch the registration.
#[tokio::test]
async fn a_changed_trigger_of_an_active_workflow_is_registered_again() {
    let t = app();
    let hooks = Arc::new(Hooks::default());
    t.ctx.flow.set_triggers(hooks.clone());
    let id = active_workflow(&t.ctx).await;
    let body = |v: Value| Body::Json(v);
    let r = update(&t.ctx, id, body(json!({"name": "Renamed"}))).await;
    assert_eq!(r.body["needs_reactivate"], false);
    assert!(hooks.calls.lock().is_empty());
    let r = update(&t.ctx, id, body(json!({"nodes": scheduled("09:20")}))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["needs_reactivate"], false);
    assert_eq!(
        *hooks.calls.lock(),
        vec![
            format!("unregister {}", id),
            format!("register {} start 09:20", id)
        ]
    );
}

/// A registration that fails stands the workflow down instead of leaving
/// it on a stale schedule, and the editor is told.
#[tokio::test]
async fn a_trigger_that_cannot_be_registered_deactivates_the_workflow() {
    let t = app();
    t.ctx.flow.set_triggers(Arc::new(Hooks {
        refuse_register: true,
        ..Default::default()
    }));
    let id = active_workflow(&t.ctx).await;
    let r = update(&t.ctx, id, Body::Json(json!({"nodes": scheduled("09:30")}))).await;
    assert_eq!(r.body["needs_reactivate"], true);
    assert_eq!(r.body["is_active"], false, "the answer shows it stood down");
    assert!(!load(&t.ctx, id).await.unwrap().unwrap().is_active);
    let r = transfer::replace(
        &t.ctx,
        id,
        Body::Json(json!({"nodes": scheduled("09:45"), "edges": []})),
    )
    .await;
    assert_eq!(
        r.body["needs_reactivate"], false,
        "an inactive workflow is not re-registered"
    );
}

/// R-23: a feed that does not confirm the release keeps the workflow, so
/// the delete can be retried.
#[tokio::test]
async fn a_delete_waits_for_the_feed_to_let_go() {
    let t = app();
    let hooks = Arc::new(Hooks::default());
    t.ctx.flow.set_triggers(hooks.clone());
    let id = active_workflow(&t.ctx).await;
    t.ctx
        .flow
        .subscriptions
        .record(Some(id), "SBIN", "NSE", "LTP");
    let r = delete(&t.ctx, id).await;
    assert_eq!(
        (r.status, r.body.clone()),
        (
            503,
            json!({"error": "The workflow feed is still connected. Try again shortly."})
        )
    );
    assert!(load(&t.ctx, id).await.unwrap().is_some());
    t.ctx.flow.set_triggers(Arc::new(Hooks {
        feed_confirms: true,
        ..Default::default()
    }));
    assert_eq!(delete(&t.ctx, id).await.status, 200);
    assert!(load(&t.ctx, id).await.unwrap().is_none());
}

/// T-086 (decision 24): a delete tears down the workflow's registrations
/// whatever its row says, so one a failed deactivate left behind never
/// fires for a deleted workflow.
#[tokio::test]
async fn a_delete_tears_down_an_inactive_workflows_triggers() {
    let t = app();
    let hooks = Arc::new(Hooks {
        feed_confirms: true,
        ..Default::default()
    });
    t.ctx.flow.set_triggers(hooks.clone());
    let w = create_workflow(&t.ctx, "A".into(), None, scheduled("09:15"), json!([]))
        .await
        .unwrap();
    assert!(!w.is_active);
    assert_eq!(delete(&t.ctx, w.id).await.status, 200);
    assert_eq!(*hooks.calls.lock(), vec![format!("unregister {}", w.id)]);
}

/// R-19 / V-132: the fields a PUT may not change are logged as the web
/// logs its sorted list, in Python's text.
#[tokio::test]
async fn ignored_fields_are_logged_as_a_python_list() {
    let t = app();
    let w = create_workflow(&t.ctx, "A".into(), None, scheduled("09:15"), json!([]))
        .await
        .unwrap();
    let logs = crate::services::tunnel::fake::capture_logs();
    let r = update(
        &t.ctx,
        w.id,
        Body::Json(json!({"webhook_enabled": true, "name": "B", "is_active": true})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let want = format!(
        "Ignoring non-editable field(s) ['is_active', 'webhook_enabled'] in PUT for workflow {}",
        w.id
    );
    assert!(logs.text().contains(&want), "{}", logs.text());
}

/// IE-15 / V-122: replacing an active workflow's trigger registers it again;
/// when that fails the workflow stands down and the answer says so.
#[tokio::test]
async fn replacing_an_active_trigger_registers_it_again_or_stands_down() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app();
    let hooks = Arc::new(Hooks::default());
    t.ctx.flow.set_triggers(hooks.clone());
    let id = active_workflow(&t.ctx).await;
    {
        let conn = t.ctx.sqlite.conn().unwrap();
        crate::flow::triggers::store::set_schedule_job_id(
            &conn,
            id,
            Some("flow_workflow_kept"),
            t.ctx.now(),
        )
        .unwrap();
    }
    let graph = |time: &str| Body::Json(json!({"nodes": scheduled(time), "edges": []}));
    let r = transfer::replace(&t.ctx, id, graph("09:20")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["needs_reactivate"], false);
    assert_eq!(
        r.body["message"],
        "Workflow replaced. Changes apply from the next run."
    );
    assert_eq!(
        *hooks.calls.lock(),
        vec![
            format!("unregister {}", id),
            format!("register {} start 09:20", id)
        ]
    );
    // IE-14: replace leaves the scheduler's job id alone.
    assert_eq!(
        load(&t.ctx, id)
            .await
            .unwrap()
            .unwrap()
            .schedule_job_id
            .as_deref(),
        Some("flow_workflow_kept")
    );
    t.ctx.flow.set_triggers(Arc::new(Hooks {
        refuse_register: true,
        ..Default::default()
    }));
    let r = transfer::replace(&t.ctx, id, graph("09:30")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["needs_reactivate"], true);
    assert_eq!(
        r.body["message"],
        "Trigger changed and could not be re-registered, so the workflow was deactivated. Activate it again when the trigger is valid."
    );
    assert!(!load(&t.ctx, id).await.unwrap().unwrap().is_active);
    // The web's lines, its booleans written as Python writes them.
    let text = logs.text();
    for line in [
        format!(
            "Replaced workflow {} from JSON (nodes=1 edges=0 reactivate=False)",
            id
        ),
        format!(
            "Replaced workflow {} from JSON (nodes=1 edges=0 reactivate=True)",
            id
        ),
        format!(
            "Could not re-register the trigger for workflow {}; deactivating it so it cannot run a stale configuration",
            id
        ),
    ] {
        assert!(text.contains(&line), "{}\n{}", line, text);
    }
}

/// IE-13: a replace the validator refuses is logged with its first error,
/// as the web's warning reads.
#[tokio::test]
async fn a_refused_replace_is_logged_with_its_first_error() {
    let logs = crate::services::tunnel::fake::capture_logs();
    let t = app();
    let id = active_workflow(&t.ctx).await;
    let bad = json!({"nodes": [{"id": "x", "type": "Nope", "position": {"x": 0, "y": 0}, "data": {}}],
                     "edges": []});
    let r = transfer::replace(&t.ctx, id, Body::Json(bad)).await;
    assert_eq!(r.status, 400, "{}", r.body);
    let first = &r.body["errors"][0];
    let line = format!(
        "Rejected replace of workflow {}: {} {} - {}",
        id,
        first["path"].as_str().unwrap(),
        first["code"].as_str().unwrap(),
        first["message"].as_str().unwrap()
    );
    assert!(logs.text().contains(&line), "{}\n{}", line, logs.text());
}
