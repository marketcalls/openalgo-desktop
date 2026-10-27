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
use crate::services::tunnel::{Off, TunnelState, TunnelStatus};
use std::time::Duration;

const TOKEN: &str = "2abcDEFghiJKLmnoPQRstu_vwxYZ0123456789";
const DOMAIN: &str = "desk.ngrok-free.app";

fn harness() -> (H, Arc<FakeProvider>) {
    let h = H::new();
    let fake = FakeProvider::new();
    h.ctx().tunnel.use_provider(fake.clone());
    h.setup();
    (h, fake)
}

/// A sign-in in the OpenAlgo Desktop window, once its tunnel autostart
/// has applied the settings (its own task: a save racing it would open the
/// tunnel twice); the session cookie.
async fn sign_in(ctx: &Arc<AppState>) -> String {
    let cookie = sign_in_only(ctx).await;
    let signed_out = TunnelStatus::stopped(Off::SignedOut);
    wait_for(&ctx.tunnel, |s| *s != signed_out).await;
    cookie
}

async fn sign_in_only(ctx: &Arc<AppState>) -> String {
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

fn online(s: &TunnelStatus) -> bool {
    s.state == TunnelState::Online
}

fn stopped(s: &TunnelStatus) -> bool {
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
    assert_eq!(opened[0].domain, "desk.ngrok-free.app");

    log_out(&ctx, &cookie).await;
    let st = wait_for(&ctx.tunnel, stopped).await;
    assert!(st.message.contains("when you sign in"), "{}", st.message);
    assert_eq!(fake.closed(), 1);
    assert_eq!(
        ctx.server_config().host_server.as_deref(),
        Some("https://desk.ngrok-free.app"),
        "the same address while it is closed"
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
    let (s, v, _) = save(&ctx, &cookie, json!({"authtoken": TOKEN, "domain": DOMAIN})).await;
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

/// The zerodha sign-in redirect as built (`BrokerConfigService`), as the
/// Broker Configuration page shows it (default and automatic), and the
/// Host Server URL the webhook addresses use.
async fn addresses(ctx: &Arc<AppState>, cookie: &str) -> (String, String, String, String) {
    let (host, _) = get_json(ctx, cookie, "/api/config/host").await;
    let (list, _) = get_json(ctx, cookie, "/api/broker/list").await;
    let zerodha = list["data"]["brokers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == "zerodha")
        .unwrap()
        .clone();
    (
        BrokerConfigService::redirect_url(ctx, "zerodha").unwrap(),
        zerodha["redirect_url"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        zerodha["redirect_url_automatic"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        host["host_server"].as_str().unwrap_or_default().to_string(),
    )
}

/// With the switch on and a domain saved, the stored Host Server URL is
/// `https://<domain>` and every address uses it, the tunnel online or not
/// (maintainer, 2026-10-27): webhook addresses, the automatic broker
/// redirect (as a web user registers it), the address a sign-in starts
/// with.
#[tokio::test]
async fn with_the_switch_on_every_address_is_the_domain_online_or_offline() {
    let (h, fake) = harness();
    h.save_broker_credentials();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    let site = format!("https://{}", DOMAIN);
    let callback = format!("{}/zerodha/callback", site);
    let expected = (
        callback.clone(),
        callback.clone(),
        callback.clone(),
        site.clone(),
    );

    // Offline: ngrok cannot be reached yet.
    fake.fail_next(OpenError::Unreachable, 1);
    save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": DOMAIN}),
    )
    .await;
    wait_for(&ctx.tunnel, |s| s.state == TunnelState::Error).await;
    assert_eq!(
        ctx.server_config().host_server.as_deref(),
        Some(site.as_str())
    );
    assert_eq!(addresses(&ctx, &cookie).await, expected, "offline");

    // Online, on the next attempt.
    let st = wait_for(&ctx.tunnel, online).await;
    assert_eq!(st.url.as_deref(), Some(site.as_str()));
    assert_eq!(addresses(&ctx, &cookie).await, expected, "online");
    BrokerAuthService::start_oauth(&ctx, "zerodha", None)
        .await
        .unwrap();
    let sent: String = ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row("SELECT redirect_uri FROM pending_oauth", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sent, callback, "the address a sign-in starts with");
}

/// A redirect address saved with a broker's keys still wins over the
/// automatic one.
#[tokio::test]
async fn a_redirect_saved_with_the_brokers_keys_still_wins() {
    let (h, _fake) = harness();
    h.save_broker_credentials();
    let ctx = h.ctx().clone();
    let fixed = "http://127.0.0.1:5000/zerodha/callback";
    ctx.sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE broker_credentials SET redirect_url = ?1 WHERE broker_id = 'zerodha'",
            [fixed],
        )
        .unwrap();
    let cookie = sign_in(&ctx).await;
    save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": DOMAIN}),
    )
    .await;
    wait_for(&ctx.tunnel, online).await;
    let (built, shown, automatic, _) = addresses(&ctx, &cookie).await;
    assert_eq!((built.as_str(), shown.as_str()), (fixed, fixed));
    assert_eq!(automatic, format!("https://{}/zerodha/callback", DOMAIN));
}

/// With the switch off the automatic redirect is this computer's address
/// (master's rule), and the stored Host Server URL stays the domain.
#[tokio::test]
async fn with_the_switch_off_the_redirect_is_this_computer() {
    let (h, _fake) = harness();
    h.save_broker_credentials();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": DOMAIN}),
    )
    .await;
    wait_for(&ctx.tunnel, online).await;
    save(&ctx, &cookie, json!({"enabled": false})).await;
    wait_for(&ctx.tunnel, stopped).await;
    let local = format!(
        "http://127.0.0.1:{}/zerodha/callback",
        ctx.server_config().http_port
    );
    let (built, shown, automatic, host) = addresses(&ctx, &cookie).await;
    assert_eq!(
        (built, shown, automatic),
        (local.clone(), local.clone(), local)
    );
    assert_eq!(host, format!("https://{}", DOMAIN));
}

/// No domain, no tunnel: it never opens on another address.
#[tokio::test]
async fn without_a_domain_there_is_no_tunnel() {
    let (h, fake) = harness();
    let ctx = h.ctx().clone();
    let cookie = sign_in(&ctx).await;
    let (s, v, _) = save(&ctx, &cookie, json!({"enabled": true, "authtoken": TOKEN})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["data"]["tunnel"]["message"],
        "Add your ngrok domain: the free domain listed under Domains in your ngrok dashboard."
    );
    assert_eq!(v["data"]["tunnel"]["state"], "stopped");
    assert!(fake.opens().is_empty());
    assert_eq!(ctx.server_config().host_server, None);
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
    let (s, _, text) = save(
        &ctx,
        &cookie,
        json!({"enabled": true, "authtoken": TOKEN, "domain": DOMAIN}),
    )
    .await;
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
    save(&ctx, &cookie, json!({"authtoken": TOKEN, "domain": DOMAIN})).await;
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
    let closed = fake.closed();
    AuthService::reset_account_everywhere(&ctx).await.unwrap();
    wait_for(&ctx.tunnel, stopped).await;
    assert_eq!(fake.closed(), closed + 1);
    let s = crate::db::sqlite::tunnel::read(&ctx.sqlite.conn().unwrap()).unwrap();
    assert_eq!(s.authtoken, None);
    assert!(s.enabled);
    assert_eq!(s.domain.as_deref(), Some("desk.ngrok.app"));
}
