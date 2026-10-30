//! The key a run uses (parity D-05): the desktop stores no key with a
//! workflow; every run reads the trader's current key when it starts. The
//! recorded difference from the web: after the key is regenerated, an
//! active workflow's next run uses the new key, from its webhook, its
//! schedule or Run Now (the web's active workflows kept the old, refused,
//! key until they were activated again).

use super::*;
use crate::brokers::mock::MockCall;
use crate::flow::contract::node::RunTrigger;
use crate::flow::triggers::runs::Fired;
use crate::flow::triggers::TriggerSide;

/// A webhook workflow whose second node reads the funds: a call the
/// `/api/v1` handlers refuse on a stale key.
fn funds_graph() -> Value {
    let mut g = webhook_graph("Funds check");
    g["nodes"][1] = json!({"id": "l", "type": "funds", "position": {"x": 0, "y": 100},
                           "data": {"outputVariable": "funds"}});
    g
}

fn funds_calls(f: &F) -> usize {
    f.h.mock
        .calls()
        .iter()
        .filter(|c| matches!(c, MockCall::Funds))
        .count()
}

#[tokio::test]
async fn a_run_after_the_key_is_regenerated_uses_the_new_key() {
    let f = F::new();
    let ctx = f.h.ctx();
    // The app's own side restores the active workflows on its own task;
    // stopped first, only the routes below register anything.
    ctx.flow.triggers().stop().await;
    let id = f.create(funds_graph()).await;
    let (_, hook_info) = f
        .post(&format!("/flow/api/workflows/{}/webhook/enable", id))
        .await;
    let (s, v) = f
        .post(&format!("/flow/api/workflows/{}/activate", id))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let old = crate::services::apikey_service::ApiKeyService::current(ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let (s, v) = f
        .call(
            Method::POST,
            "/apikey",
            Some(&json!({"user_id": USER}).to_string()),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_ne!(v["api_key"], json!(old));
    let (s, v) =
        f.h.json(post_json("/api/v1/funds", json!({"apikey": old})))
            .await;
    assert_eq!(
        (s, v["message"].clone()),
        (StatusCode::FORBIDDEN, json!("Invalid openalgo apikey")),
        "the old key is refused"
    );

    let url = format!(
        "/flow/webhook/{}",
        hook_info["webhook_token"].as_str().unwrap()
    );
    let secret = hook_info["webhook_secret"].as_str().unwrap();
    let (s, _, v) = f
        .send_hook(&url, &json!({"secret": secret}).to_string(), "127.0.0.1")
        .await;
    assert_eq!(
        (s, v["status"].clone()),
        (StatusCode::OK, json!("success")),
        "{}",
        v
    );
    assert_eq!(funds_calls(&f), 1, "the webhook's run reached the broker");

    let side = TriggerSide::new(Arc::downgrade(ctx), ctx.clock.clone());
    let fired = side.fire(id, RunTrigger::Schedule, None).await;
    assert!(
        matches!(&fired, Fired::Ran(r) if r["status"] == "success"),
        "{:?}",
        fired
    );
    assert_eq!(funds_calls(&f), 2, "the schedule's run reached the broker");

    let (s, v) = f.post(&format!("/flow/api/workflows/{}/execute", id)).await;
    assert_eq!(
        (s, v["status"].clone()),
        (StatusCode::OK, json!("success")),
        "{}",
        v
    );
    assert_eq!(funds_calls(&f), 3, "Run Now reached the broker");
}
