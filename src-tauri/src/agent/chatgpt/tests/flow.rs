//! The device flow's states against a fake sign-in service (the web's
//! `test_agent_chatgpt_oauth.py`, start to sign-out).

use super::*;
use crate::agent::chatgpt::flow::{self, poll_wait, CLIENT_ID};
use crate::agent::chatgpt::signin::{LoginState, CANCELLED};
use crate::agent::outbound::fake::closed_port;

/// No trader sentence carries a status code or an error class (D-023).
fn trader_words(message: &str) {
    for word in [
        "HTTP",
        "Error",
        "error(",
        "403",
        "500",
        "Exception",
        "reqwest",
    ] {
        assert!(!message.contains(word), "{:?} in {:?}", word, message);
    }
}

#[tokio::test]
async fn start_returns_the_code_at_once_and_authorises_in_the_background() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 3).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let s = plan.start(&t.ctx, false).await.unwrap();
    assert_eq!(
        (s.state, s.user_code.as_str()),
        (LoginState::Pending, "ABCD-1234")
    );
    assert_eq!(s.verification_url, fake.url("/codex/device"));
    assert_eq!(s.expires_at.unwrap() - s.started_at.unwrap(), 30.0);
    assert_eq!(s.message, "");
    until("authorised", || {
        plan.snapshot().state == LoginState::Authorised
    })
    .await;
    let done = plan.snapshot();
    assert_eq!(done.message, "ChatGPT subscription authorised.");
    assert_eq!(done.user_code, "", "the code is gone once the sign-in ends");
    // What was sent, request by request, as LiteLLM sends it.
    let code = &fake.seen_at(CODE)[0];
    assert_eq!(code.json(), json!({"client_id": CLIENT_ID}));
    let polls = fake.seen_at(POLL);
    assert_eq!(polls.len(), 4);
    assert_eq!(
        polls[0].json(),
        json!({"device_auth_id": "dev-1", "user_code": "ABCD-1234"})
    );
    let exchange = &fake.seen_at(TOKEN)[0];
    assert_eq!(
        exchange.header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(
        exchange.text(),
        format!(
            "grant_type=authorization_code&code=auth-code-1&redirect_uri={}/deviceauth/callback&client_id={}&code_verifier=ver-1",
            fake.url(""),
            CLIENT_ID
        )
    );
    // Kept in the vault, nowhere else.
    let r = stored(&t).unwrap();
    assert_eq!(r.refresh(), Some("rt-one-0123456789"));
    assert_eq!(r.account_id.as_deref(), Some(ACCOUNT));
    assert!(r.expires_at.is_some());
}

#[tokio::test]
async fn a_second_start_hands_back_the_sign_in_in_flight() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let first = plan.start(&t.ctx, false).await.unwrap();
    let again = plan.start(&t.ctx, false).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(fake.seen_at(CODE).len(), 1, "one code, not two");
    plan.cancel().await;
}

#[tokio::test]
async fn concurrent_starts_request_one_code() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1000).await;
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d", "user_code": "ABCD-1234"}),
        )
        .after(Duration::from_millis(200))],
    );
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let (a, b) = tokio::join!(plan.start(&t.ctx, false), plan.start(&t.ctx, false));
    assert_eq!(fake.seen_at(CODE).len(), 1);
    let states = [a.unwrap(), b.unwrap()];
    assert!(states.iter().any(|s| s.user_code == "ABCD-1234"));
    assert!(
        states.iter().any(|s| s.message == flow::REQUESTING),
        "the double click saw the request in flight"
    );
    plan.cancel().await;
}

#[tokio::test]
async fn forcing_a_restart_replaces_the_sign_in() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let first = plan.start(&t.ctx, false).await.unwrap();
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d2", "user_code": "WXYZ-9999"}),
        )],
    );
    let second = plan.start(&t.ctx, true).await.unwrap();
    assert_eq!(second.user_code, "WXYZ-9999");
    assert_ne!(first.user_code, second.user_code);
    assert_eq!(fake.seen_at(CODE).len(), 2);
    let polled = fake.seen_at(POLL).len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(fake
        .seen_at(POLL)
        .iter()
        .skip(polled)
        .all(|p| p.json()["device_auth_id"] == "d2"));
    plan.cancel().await;
}

#[tokio::test]
async fn an_unapproved_code_reads_as_expired_not_failed() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1_000_000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_millis(150)));
    plan.start(&t.ctx, false).await.unwrap();
    until("expired", || plan.snapshot().state == LoginState::Expired).await;
    let s = plan.snapshot();
    assert_eq!(
        s.message,
        "The sign-in code expired before it was approved. Start again."
    );
    assert_eq!(s.user_code, "");
    assert!(stored(&t).is_none());
}

#[tokio::test]
async fn a_hard_poll_failure_stops_rather_than_retrying() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 0).await;
    fake.script(POLL, vec![Answer::json(500, json!({"detail": "boom"}))]);
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    plan.start(&t.ctx, false).await.unwrap();
    until("failed", || plan.snapshot().state == LoginState::Failed).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(fake.seen_at(POLL).len(), 1, "one refusal ends it");
    let m = plan.snapshot().message;
    assert_eq!(m, flow::POLL_REFUSED);
    trader_words(&m);
}

#[test]
fn the_endpoints_interval_is_a_floor_and_no_wait_is_ever_zero() {
    let ms = Duration::from_millis;
    assert_eq!(poll_wait(ms(5000), Some("1")), ms(5000));
    assert_eq!(poll_wait(ms(10), Some("0.2")), ms(200));
    assert_eq!(poll_wait(Duration::ZERO, Some("0")), ms(10));
    assert_eq!(poll_wait(Duration::ZERO, Some("soon")), ms(10));
    assert_eq!(poll_wait(Duration::ZERO, Some("-5")), ms(10));
    assert_eq!(poll_wait(Duration::ZERO, None), ms(10));
}

#[tokio::test]
async fn the_endpoints_own_interval_spaces_the_polls() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1000).await;
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d", "user_code": "C", "interval": "0.2"}),
        )],
    );
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    plan.start(&t.ctx, false).await.unwrap();
    until("two polls", || fake.seen_at(POLL).len() >= 2).await;
    let p = fake.seen_at(POLL);
    assert!(p[1].at - p[0].at >= Duration::from_millis(190));
    plan.cancel().await;
}

#[tokio::test]
async fn a_refused_code_fails_at_once_and_a_poller_sees_it() {
    let t = ctx();
    let fake = auth_server(&t, "X", 0).await;
    fake.script(CODE, vec![Answer::json(429, json!({"error": "slow down"}))]);
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let failed = plan.start(&t.ctx, false).await.unwrap_err();
    assert_eq!(failed.state, LoginState::Failed);
    assert_eq!(failed.message, flow::CODE_REFUSED);
    trader_words(&failed.message);
    assert_eq!(plan.snapshot(), failed);
    assert_eq!(failed.verification_url, fake.url("/codex/device"));
    assert!(fake.seen_at(POLL).is_empty());
    // The claim is released: the next start asks again.
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d", "user_code": "OK-1"}),
        )],
    );
    assert_eq!(plan.start(&t.ctx, false).await.unwrap().user_code, "OK-1");
    plan.cancel().await;
}

#[tokio::test]
async fn an_incomplete_code_or_exchange_is_refused() {
    let t = ctx();
    let fake = auth_server(&t, "X", 0).await;
    fake.script(
        CODE,
        vec![Answer::json(200, json!({"device_auth_id": "only"}))],
    );
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    assert_eq!(
        plan.start(&t.ctx, false).await.unwrap_err().message,
        flow::CODE_INCOMPLETE
    );
    fake.script(
        CODE,
        vec![Answer::json(
            200,
            json!({"device_auth_id": "d", "usercode": "ALT-1"}),
        )],
    );
    fake.script(TOKEN, vec![Answer::json(200, json!({"access_token": "a"}))]);
    let s = plan.start(&t.ctx, false).await.unwrap();
    assert_eq!(s.user_code, "ALT-1", "the alternative field name is read");
    until("failed", || plan.snapshot().state == LoginState::Failed).await;
    assert_eq!(plan.snapshot().message, flow::EXCHANGE_INCOMPLETE);
    assert!(stored(&t).is_none(), "nothing half-made is kept");
    fake.script(TOKEN, vec![Answer::json(401, json!({}))]);
    plan.start(&t.ctx, false).await.unwrap();
    until("refused", || {
        plan.snapshot().message == flow::EXCHANGE_REFUSED
    })
    .await;
    trader_words(flow::EXCHANGE_REFUSED);
}

#[tokio::test]
async fn an_unreachable_service_is_a_readable_failure() {
    let t = ctx();
    let port = closed_port().await;
    let plan = install(
        &t,
        Flow {
            endpoints: Endpoints::at(&format!("http://127.0.0.1:{}", port)),
            lifetime: Duration::from_secs(30),
            interval: Duration::from_millis(10),
        },
    );
    let failed = plan.start(&t.ctx, false).await.unwrap_err();
    assert_eq!(failed.message, flow::UNREACHABLE);
    trader_words(&failed.message);
}

#[tokio::test]
async fn cancel_stops_the_poll_promptly_and_cancelling_nothing_succeeds() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1_000_000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(60)));
    assert!(!plan.cancel().await, "nothing was running");
    plan.start(&t.ctx, false).await.unwrap();
    until("polling", || !fake.seen_at(POLL).is_empty()).await;
    let began = std::time::Instant::now();
    assert!(plan.cancel().await);
    assert!(began.elapsed() < Duration::from_secs(2));
    let s = plan.snapshot();
    assert_eq!(
        (s.state, s.message.as_str(), s.user_code.as_str()),
        (LoginState::Cancelled, CANCELLED, "")
    );
    let polled = fake.seen_at(POLL).len();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(fake.seen_at(POLL).len(), polled, "no poll after the cancel");
    assert!(!plan.cancel().await);
}

#[tokio::test]
async fn signing_out_locking_or_resetting_the_app_stops_the_sign_in() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 1_000_000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(60)));
    plan.start(&t.ctx, false).await.unwrap();
    until("polling", || !fake.seen_at(POLL).is_empty()).await;
    t.ctx.agent.stop_all();
    assert_eq!(plan.snapshot().state, LoginState::Cancelled);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let polled = fake.seen_at(POLL).len();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(fake.seen_at(POLL).len(), polled, "the poll task ended");
}

#[tokio::test]
async fn an_approval_that_lands_after_a_stop_is_not_kept() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 0).await;
    fake.script(
        TOKEN,
        vec![Answer::json(200, tokens(&t, "late")).after(Duration::from_millis(300))],
    );
    let plan = install(&t, flow_at(&fake, Duration::from_secs(60)));
    plan.start(&t.ctx, false).await.unwrap();
    until("exchanging", || !fake.seen_at(TOKEN).is_empty()).await;
    t.ctx.agent.stop_all();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(stored(&t).is_none());
    assert_eq!(plan.snapshot().state, LoginState::Cancelled);
}

#[tokio::test]
async fn forget_removes_the_credential_and_leaves_no_stale_snapshot() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 0).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    plan.start(&t.ctx, false).await.unwrap();
    until("authorised", || {
        plan.snapshot().state == LoginState::Authorised
    })
    .await;
    assert!(plan.forget(&t.ctx).await.unwrap());
    assert!(stored(&t).is_none());
    let s = plan.snapshot();
    assert_eq!(
        (s.state, s.message.as_str(), s.verification_url.as_str()),
        (LoginState::Idle, "", "")
    );
    assert!(!plan.forget(&t.ctx).await.unwrap(), "idempotent");
}

#[tokio::test]
async fn a_failed_save_is_never_reported_as_authorised() {
    let t = ctx();
    let fake = auth_server(&t, "ABCD-1234", 0).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    plan.start(&t.ctx, false).await.unwrap();
    // The keys go before the approval lands: nothing can be sealed.
    t.ctx.security.lock();
    until("ended", || plan.snapshot().state != LoginState::Pending).await;
    let s = plan.snapshot();
    assert_eq!(s.state, LoginState::Failed);
    assert_eq!(s.message, crate::agent::chatgpt::signin::NOT_SAVED);
}

#[tokio::test]
async fn the_code_and_the_tokens_never_reach_a_snapshot_debug_line() {
    let t = ctx();
    let fake = auth_server(&t, "SECRET-CODE", 1000).await;
    let plan = install(&t, flow_at(&fake, Duration::from_secs(30)));
    let s = plan.start(&t.ctx, false).await.unwrap();
    let shown = format!("{:?}", s);
    assert!(!shown.contains("SECRET-CODE"), "{}", shown);
    plan.cancel().await;
    let r = record_expiring(&t, 3600, Some("rt-very-secret-123456"));
    let shown = format!("{:?}", r);
    assert!(
        !shown.contains("rt-very-secret") && !shown.contains("eyJ"),
        "{}",
        shown
    );
}
