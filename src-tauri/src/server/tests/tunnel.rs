//! The built-in tunnel in the app: it opens at the sign-in when switched on
//! and closes at the sign-out, a fresh start and an account reset; the
//! settings page saves and applies at once and never gets the authtoken
//! back; while it is online the Host Server URL and the automatic broker
//! redirect address follow its public address. Against the fake provider:
//! nothing reaches ngrok.

use super::*;
use crate::services::broker_config_service::BrokerConfigService;
use crate::services::tunnel::fake::{capture_logs, wait_for, FakeProvider};
use crate::services::tunnel::provider::OpenError;
use crate::services::tunnel::TunnelState;

const TOKEN: &str = "2abcDEFghiJKLmnoPQRstu_vwxYZ0123456789";

fn harness() -> (H, Arc<FakeProvider>) {
    let h = H::new();
    let fake = FakeProvider::new();
    h.ctx().tunnel.use_provider(fake.clone());
    h.setup();
    (h, fake)
}

/// A sign-in in the OpenAlgo Desktop window; the session cookie.
async fn sign_in(ctx: &Arc<AppState>) -> String {
    let (s, headers, v) = send_to(
        ctx,
        from_the_app_window(
            ctx,
            multipart("/auth/login", &[("username", USER), ("password", PASSWORD)]),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    cookie_from(&headers).expect("a session")
}

fn csrf(ctx: &AppState, cookie: &str) -> String {
    let id = cookie.trim_start_matches("session=");
    ctx.sessions.get(id, ctx.now()).unwrap().csrf_token
}

async fn log_out(ctx: &Arc<AppState>, cookie: &str) {
    let token = csrf(ctx, cookie);
    let (s, _, v) = send_to(
        ctx,
        with_session(in_app(form("/auth/logout", &[])), cookie, Some(&token)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// POST /api/tunnel from the settings page; the status and the raw body.
async fn save(ctx: &Arc<AppState>, cookie: &str, body: Value) -> (StatusCode, Value, String) {
    let token = csrf(ctx, cookie);
    let resp = crate::server::app(ctx.clone())
        .oneshot(with_session(
            in_app(post_json("/api/tunnel", body)),
            cookie,
            Some(&token),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    )
}

async fn get_json(ctx: &Arc<AppState>, cookie: &str, path: &str) -> (Value, String) {
    let resp = crate::server::app(ctx.clone())
        .oneshot(with_session(get(path), cookie, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", path);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (serde_json::from_str(&text).unwrap(), text)
}

fn online(s: &crate::services::tunnel::TunnelStatus) -> bool {
    s.state == TunnelState::Online
}

fn stopped(s: &crate::services::tunnel::TunnelStatus) -> bool {
    s.state == TunnelState::Stopped
}

#[tokio::test]
async fn the_tunnel_opens_at_sign_in_and_closes_at_sign_out() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    let (s, v, _) = save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": "https://Desk.ngrok-free.app/"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["domain"], "desk.ngrok-free.app");
    let st = wait_for(&ctx.tunnel, online).await;
    assert_eq!(st.url.as_deref(), Some("https://desk.ngrok-free.app"));
    let opened = fake.opens();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].authtoken.expose(), TOKEN);
    assert_eq!(opened[0].domain.as_deref(), Some("desk.ngrok-free.app"));

    log_out(&ctx, &cookie).await;
    let st = wait_for(&ctx.tunnel, stopped).await;
    assert!(st.message.contains("when you sign in"), "{}", st.message);
    assert_eq!(fake.closed(), 1);
    assert_eq!(
        ctx.server_config().host_server,
        None,
        "back to the saved one"
    );

    // The next sign-in brings it back, with the same settings.
    let cookie = sign_in(&ctx).await;
    wait_for(&ctx.tunnel, online).await;
    assert_eq!(fake.opens().len(), 2);
    assert_eq!(fake.opens()[1], fake.opens()[0]);
    log_out(&ctx, &cookie).await;
    wait_for(&ctx.tunnel, stopped).await;
}

#[tokio::test]
async fn switched_off_it_stays_closed_at_sign_in_and_closes_when_turned_off() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    let (s, v, _) = save(&ctx, &cookie, json!({"authtoken": TOKEN})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["enabled"], false);
    assert_eq!(v["data"]["tunnel"]["state"], "stopped");
    assert_eq!(v["data"]["tunnel"]["message"], "The tunnel is off.");
    log_out(&ctx, &cookie).await;
    let cookie = sign_in(&ctx).await;
    assert_eq!(fake.opens().len(), 0);

    save(&ctx, &cookie, json!({"enabled": true})).await;
    wait_for(&ctx.tunnel, online).await;
    let (s, v, _) = save(&ctx, &cookie, json!({"enabled": false})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["data"]["tunnel"]["state"], "stopped");
    assert_eq!(fake.closed(), 1);
    // On with no authtoken: says what to do.
    save(
        &ctx,
        &cookie,
        json!({"enabled": true, "forget_authtoken": true}),
    )
    .await;
    let st = wait_for(&ctx.tunnel, stopped).await;
    assert_eq!(st.message, "Add your ngrok authtoken to start the tunnel.");
    assert_eq!(fake.opens().len(), 1);
}

#[tokio::test]
async fn the_host_server_url_and_the_redirect_address_follow_the_tunnel() {
    let (h, _fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    let port = ctx.server_config().http_port;
    let local = format!("http://127.0.0.1:{}", port);
    let (v, _) = get_json(&ctx, &cookie, "/api/config/host").await;
    assert_eq!(v["host_server"], local.as_str());
    assert_eq!(v["is_localhost"], true);

    save(&ctx, &cookie, json!({"enabled": true, "authtoken": TOKEN})).await;
    let url = wait_for(&ctx.tunnel, online).await.url.unwrap();
    assert_eq!(url, "https://fake-1.ngrok-free.app");
    let (v, _) = get_json(&ctx, &cookie, "/api/config/host").await;
    assert_eq!(v["host_server"], url.as_str(), "webhook addresses");
    assert_eq!(v["is_localhost"], false);
    assert_eq!(
        BrokerConfigService::redirect_url(&ctx, "zerodha").unwrap(),
        format!("{}/zerodha/callback", url)
    );
    let (v, _) = get_json(&ctx, &cookie, "/api/broker/credentials").await;
    assert_eq!(v["data"]["host_server"], url.as_str());

    save(&ctx, &cookie, json!({"enabled": false})).await;
    let (v, _) = get_json(&ctx, &cookie, "/api/config/host").await;
    assert_eq!(v["host_server"], local.as_str());
    assert_eq!(
        BrokerConfigService::redirect_url(&ctx, "zerodha").unwrap(),
        format!("{}/zerodha/callback", local)
    );
}

#[tokio::test]
async fn the_authtoken_is_never_sent_back_or_logged() {
    let logs = capture_logs();
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;

    let typo = "2abcDEF ghi!";
    let (s, v, text) = save(&ctx, &cookie, json!({"authtoken": typo})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{}", v);
    assert!(v["message"].as_str().unwrap().contains("ngrok authtoken"));
    assert!(!text.contains(typo), "{}", text);

    fake.fail_next(OpenError::Authtoken, 1);
    let (s, _, text) = save(&ctx, &cookie, json!({"enabled": true, "authtoken": TOKEN})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!text.contains(TOKEN), "{}", text);
    let st = wait_for(&ctx.tunnel, |s| s.state == TunnelState::Error).await;
    assert!(st.message.contains("did not accept the authtoken"));
    let (v, text) = get_json(&ctx, &cookie, "/api/tunnel").await;
    assert_eq!(v["data"]["authtoken_saved"], true);
    assert!(!text.contains(TOKEN), "{}", text);
    assert!(!text.contains("v1:"), "not even sealed: {}", text);

    // Stored sealed, bound to its row and column.
    let stored: String = ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row(
            "SELECT ngrok_authtoken FROM settings WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(stored.starts_with("v1:") && !stored.contains(TOKEN));

    // Saving again retries at once (the trader fixed the account).
    save(&ctx, &cookie, json!({"authtoken": TOKEN})).await;
    wait_for(&ctx.tunnel, online).await;
    log_out(&ctx, &cookie).await;
    wait_for(&ctx.tunnel, stopped).await;
    assert!(!logs.text().contains(TOKEN), "{}", logs.text());
}

#[tokio::test]
async fn a_domain_that_is_not_one_is_refused_and_nothing_is_saved() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    for bad in [
        "https://desk.ngrok-free.app/webhook",
        "desk.ngrok-free.app:8080",
        "https://user@desk.ngrok-free.app",
        "localhost",
        "desk ngrok app",
        "-desk.ngrok.app",
    ] {
        let (s, v, _) = save(
            &ctx,
            &cookie,
            json!({"enabled": true, "authtoken": TOKEN, "domain": bad}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{}: {}", bad, v);
        assert!(
            v["message"].as_str().unwrap().contains("Domains"),
            "{}",
            bad
        );
    }
    let (v, _) = get_json(&ctx, &cookie, "/api/tunnel").await;
    assert_eq!(v["data"]["enabled"], false);
    assert_eq!(v["data"]["authtoken_saved"], false);
    assert_eq!(fake.opens().len(), 0);
    // Empty clears it.
    save(&ctx, &cookie, json!({"domain": "a.ngrok.app"})).await;
    let (_, v, _) = save(&ctx, &cookie, json!({"domain": ""})).await;
    assert_eq!(v["data"]["domain"], Value::Null);
}

/// The web's NGROK_ALLOW, saved with the server addresses, is the same
/// switch.
#[tokio::test]
async fn the_web_switch_on_the_broker_settings_starts_and_stops_the_tunnel() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    save(&ctx, &cookie, json!({"authtoken": TOKEN})).await;
    let token = csrf(&ctx, &cookie);
    let (s, _, v) = send_to(
        &ctx,
        with_session(
            in_app(post_json(
                "/api/broker/credentials",
                json!({"ngrok_allow": "TRUE"}),
            )),
            &cookie,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    wait_for(&ctx.tunnel, online).await;
    let (s, _, v) = send_to(
        &ctx,
        with_session(
            in_app(post_json(
                "/api/broker/credentials",
                json!({"ngrok_allow": "FALSE"}),
            )),
            &cookie,
            Some(&token),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    wait_for(&ctx.tunnel, stopped).await;
    assert_eq!(fake.closed(), 1);
}

/// An account reset closes the tunnel and drops the authtoken with the
/// keys that sealed it; the switch and the domain stay.
#[tokio::test]
async fn an_account_reset_closes_the_tunnel_and_forgets_the_authtoken() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": "desk.ngrok.app"}),
    )
    .await;
    wait_for(&ctx.tunnel, online).await;
    AuthService::reset_account_everywhere(&ctx).await.unwrap();
    wait_for(&ctx.tunnel, stopped).await;
    assert_eq!(fake.closed(), 1);
    let s = crate::db::sqlite::tunnel::read(&ctx.sqlite.conn().unwrap()).unwrap();
    assert_eq!(s.authtoken, None);
    assert!(s.enabled);
    assert_eq!(s.domain.as_deref(), Some("desk.ngrok.app"));
}
