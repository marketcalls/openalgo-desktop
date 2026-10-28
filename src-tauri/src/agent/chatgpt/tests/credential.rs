//! The stored credential: custody in the vault, readiness without the
//! network, renewal with its skew (D-025), the status the panel reads, and
//! billing decided by the resolved row alone (D-026).

use super::*;
use crate::agent::chatgpt::flow::CLIENT_ID;
use crate::agent::chatgpt::refresh::{SIGN_IN_AGAIN, SKEW_SECONDS};
use crate::agent::chatgpt::service;
use crate::agent::contract::model::ModelError;
use crate::agent::outbound::fake::closed_port;
use crate::agent::plan::NOT_SIGNED_IN;

fn ciphertext(t: &TestCtx) -> String {
    t.ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT ciphertext FROM ag_secret WHERE name = 'oauth:chatgpt'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn the_record_is_canonical_json_and_an_unchanged_one_is_not_rewritten() {
    let t = ctx();
    let r = record_expiring(&t, 3600, Some("rt-abc-0123456789"));
    let text = r.canonical();
    let keys: Vec<String> = serde_json::from_str::<serde_json::Map<String, Value>>(text.expose())
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        [
            "access_token",
            "account_id",
            "expires_at",
            "id_token",
            "refresh_token"
        ]
    );
    assert!(
        !text.expose().contains(": ") && !text.expose().contains(", "),
        "compact"
    );
    put(&t, &r);
    let first = ciphertext(&t);
    for _ in 0..5 {
        put(&t, &AuthRecord::parse(text.expose()).unwrap());
    }
    assert_eq!(ciphertext(&t), first, "five re-stores write once");
    let back = stored(&t).unwrap();
    assert_eq!(back.canonical().expose(), text.expose());
}

#[tokio::test]
async fn readiness_reads_the_vault_and_never_the_network() {
    let t = ctx();
    let port = closed_port().await;
    let flow = Flow {
        endpoints: Endpoints::at(&format!("http://127.0.0.1:{}", port)),
        lifetime: Duration::from_secs(1),
        interval: Duration::from_millis(10),
    };
    t.ctx.agent.set_plan(Arc::new(ChatGptPlan::new(flow)));
    let plan = t.ctx.agent.plan();
    assert!(!plan.is_authorised(&t.ctx));
    assert_eq!(plan.ensure_ready(&t.ctx), Err(NOT_SIGNED_IN.to_string()));
    // An expired access token with a refresh token is still ready.
    put(&t, &record_expiring(&t, -600, Some("rt-abc-0123456789")));
    assert!(plan.is_authorised(&t.ctx));
    assert_eq!(plan.ensure_ready(&t.ctx), Ok(()));
    // An expired access token alone is not.
    put(&t, &record_expiring(&t, -600, None));
    assert!(!plan.is_authorised(&t.ctx));
    // A live access token alone is.
    put(&t, &record_expiring(&t, 600, None));
    assert!(plan.is_authorised(&t.ctx));
    // Locked keys: nothing can be read, so nothing is ready.
    t.ctx.security.lock();
    assert!(!plan.is_authorised(&t.ctx));
}

/// A renewal service: `/oauth/token` answers `body`.
async fn renewal(t: &TestCtx, body: Value) -> (FakeServer, Arc<SignIn>) {
    let fake = FakeServer::start().await;
    fake.script(TOKEN, vec![Answer::json(200, body)]);
    let plan = install(t, flow_at(&fake, Duration::from_secs(30)));
    (fake, plan)
}

#[tokio::test]
async fn a_token_inside_the_skew_is_renewed_before_it_is_sent() {
    let t = ctx();
    let fresh = tokens(&t, "new");
    let (fake, plan) = renewal(&t, fresh.clone()).await;
    // 30 s left: inside the 60 s skew, so it is renewed first (D-025).
    put(&t, &record_expiring(&t, 30, Some("rt-old-0123456789")));
    let access = plan.access(&t.ctx).await.unwrap();
    assert_eq!(
        access.token.expose(),
        fresh["access_token"].as_str().unwrap()
    );
    assert_eq!(access.account_id.as_deref(), Some(ACCOUNT));
    let sent = &fake.seen_at(TOKEN)[0];
    assert_eq!(
        sent.json(),
        json!({"client_id": CLIENT_ID, "grant_type": "refresh_token",
               "refresh_token": "rt-old-0123456789", "scope": "openid profile email"})
    );
    // The rotated refresh token is kept in the same step.
    let r = stored(&t).unwrap();
    assert_eq!(r.refresh(), Some("rt-new-0123456789"));
    assert_eq!(r.access(), fresh["access_token"].as_str());
}

#[tokio::test]
async fn a_token_outside_the_skew_is_sent_as_it_is() {
    let t = ctx();
    let (fake, plan) = renewal(&t, tokens(&t, "new")).await;
    let r = record_expiring(&t, (SKEW_SECONDS as i64) + 60, Some("rt-old-0123456789"));
    let token = r.access().unwrap().to_string();
    put(&t, &r);
    assert_eq!(plan.access(&t.ctx).await.unwrap().token.expose(), token);
    assert!(fake.seen_at(TOKEN).is_empty(), "no renewal");
}

#[tokio::test]
async fn a_renewal_without_a_new_refresh_token_keeps_the_old_one() {
    let t = ctx();
    let mut body = tokens(&t, "new");
    body.as_object_mut().unwrap().remove("refresh_token");
    let (_fake, plan) = renewal(&t, body).await;
    put(&t, &record_expiring(&t, -10, Some("rt-old-0123456789")));
    plan.access(&t.ctx).await.unwrap();
    assert_eq!(stored(&t).unwrap().refresh(), Some("rt-old-0123456789"));
}

#[tokio::test]
async fn a_refused_renewal_asks_for_a_sign_in_and_changes_nothing() {
    let t = ctx();
    let fake = FakeServer::start().await;
    fake.script(
        TOKEN,
        vec![Answer::json(401, json!({"error": "invalid_grant"}))],
    );
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let before = record_expiring(&t, -10, Some("rt-old-0123456789"));
    put(&t, &before);
    let e = plan.access(&t.ctx).await.unwrap_err();
    assert_eq!(e, ModelError::MissingCredential(SIGN_IN_AGAIN.into()));
    assert_eq!(
        stored(&t).unwrap().canonical().expose(),
        before.canonical().expose()
    );
}

#[tokio::test]
async fn a_request_with_nothing_stored_is_refused_before_anything_is_sent() {
    let t = ctx();
    let fake = FakeServer::start().await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let e = plan.access(&t.ctx).await.unwrap_err();
    assert_eq!(e, ModelError::MissingCredential(NOT_SIGNED_IN.into()));
    assert!(fake.seen().is_empty());
}

#[tokio::test]
async fn two_requests_at_once_renew_once() {
    let t = ctx();
    let (fake, plan) = renewal(&t, tokens(&t, "new")).await;
    put(&t, &record_expiring(&t, -10, Some("rt-old-0123456789")));
    let (a, b) = tokio::join!(plan.access(&t.ctx), plan.access(&t.ctx));
    assert_eq!(a.unwrap().token, b.unwrap().token);
    assert_eq!(fake.seen_at(TOKEN).len(), 1);
}

#[tokio::test]
async fn the_status_describes_the_credential_and_shows_none_of_it() {
    let t = ctx();
    let fake = FakeServer::start().await;
    install(&t, flow_at(&fake, Duration::from_secs(30)));
    let empty = service::status(&t.ctx).await.unwrap();
    assert_eq!(
        empty["data"],
        json!({"provider": "chatgpt", "authorised": false, "fingerprint": "...????",
               "account_id": null, "access_token_expires_at": null, "access_token_expired": false,
               "stored_in_database": false, "token_dir": null,
               "login": {"state": "idle", "user_code": "", "verification_url": "",
                         "started_at": null, "expires_at": null, "message": ""}})
    );
    let r = record_expiring(&t, -10, Some("rt-old-0123456789"));
    put(&t, &r);
    let s = service::status(&t.ctx).await.unwrap();
    let d = &s["data"];
    assert_eq!(d["authorised"], json!(true));
    assert_eq!(
        d["fingerprint"],
        json!(crate::agent::models::secrets::fingerprint(
            "rt-old-0123456789"
        ))
    );
    assert_eq!(d["account_id"], json!(ACCOUNT));
    assert_eq!(d["access_token_expired"], json!(true));
    assert_eq!(d["access_token_expires_at"], json!((now_s(&t) - 10) as f64));
    assert_eq!(
        (d["stored_in_database"].clone(), d["token_dir"].clone()),
        (json!(true), Value::Null)
    );
    // The row's own fingerprint is over the JSON blob: not the one shown.
    let row = crate::agent::models::secrets::describe(
        &t.ctx.sqlite.conn().unwrap(),
        &["oauth:chatgpt".to_string()],
    )
    .unwrap();
    assert_ne!(json!(row["oauth:chatgpt"].fingerprint), d["fingerprint"]);
    let text = s.to_string();
    for secret in [r.refresh().unwrap(), r.access().unwrap()] {
        assert!(!text.contains(secret));
    }
}

#[tokio::test]
async fn an_abandoned_sign_in_reports_no_expired_token_it_never_had() {
    let t = ctx();
    let fake = FakeServer::start().await;
    install(&t, flow_at(&fake, Duration::from_secs(30)));
    put(
        &t,
        &AuthRecord {
            refresh_token: Some(Secret::new("rt-only-0123456789")),
            ..Default::default()
        },
    );
    let d = &service::status(&t.ctx).await.unwrap()["data"];
    assert_eq!(
        (d["authorised"].clone(), d["access_token_expired"].clone()),
        (json!(true), json!(false))
    );
}

#[test]
fn billing_is_the_resolved_rows_prefix_alone() {
    use crate::agent::chatgpt::codex::is_plan;
    use crate::agent::contract::model::{Prices, Pricing};
    use crate::agent::run::usage::{Totals, UsageMeter};
    use crate::agent::testing::fake_model;
    let mut plan = fake_model(
        "http://127.0.0.1:1/",
        genai::adapter::AdapterKind::OpenAIResp,
    );
    plan.catalog_id = "chatgpt/gpt-5.3-instant".into();
    plan.model_name = "gpt-5.3-instant".into();
    plan.pricing = Pricing::Subscription;
    assert!(
        is_plan(&plan),
        "a plan-only model with no bare-name entry is still the plan's"
    );
    let mut api = plan.clone();
    api.catalog_id = "openai/gpt-5.4".into();
    api.pricing = Pricing::Metered(Some(Prices {
        input: Some(1e-6),
        output: Some(2e-6),
        cache_read: None,
    }));
    assert!(
        !is_plan(&api),
        "the same bare name under openai/ is metered"
    );
    let mut meter = UsageMeter::new(&plan);
    let totals = Totals {
        input: 1000,
        output: 10,
        total: 1010,
        cached: 0,
        reasoning: 0,
    };
    let frame = serde_json::to_value(meter.record(totals, &plan).unwrap()).unwrap();
    assert_eq!(
        (frame["billing"].clone(), frame["cost_usd"].clone()),
        (json!("subscription"), Value::Null)
    );
    assert_eq!(
        frame["model"],
        json!("chatgpt/gpt-5.3-instant"),
        "never re-priced by a bare name"
    );
    let frame = serde_json::to_value(UsageMeter::new(&api).record(totals, &api).unwrap()).unwrap();
    assert_eq!(frame["billing"], json!("metered"));
    assert!(frame["cost_usd"].as_f64().unwrap() > 0.0);
}
