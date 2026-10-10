//! Regression tests for evaluation B of the Codex review
//! (`docs/codex-evaluation/B-security-credentials-limits-config.md`):
//! sign-out, broker revocation, key material and broker settings. Each test
//! names its finding.

use super::*;
use crate::events::SessionEndReason;

/// The trader's broker session, streaming (the runtime active), as after a
/// sign-in.
async fn connect_and_stream(h: &H) {
    h.connect_broker();
    let session = h.ctx().get_broker_session().expect("connected");
    h.ctx().runtime.activate(h.ctx(), &session).await;
    assert_eq!(
        h.ctx().runtime.active_broker().as_deref(),
        Some("zerodha"),
        "streaming started"
    );
}

/// The database refuses every change to a stored broker session (a full
/// disk, a lock held past the timeout).
fn fail_auth_writes(h: &H) {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER test_fail_auth_update BEFORE UPDATE ON auth
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
}

fn allow_auth_writes(h: &H) {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER test_fail_auth_update")
        .unwrap();
}

/// Whether the stored broker session is still un-revoked.
fn stored_session_active(h: &H) -> bool {
    let conn = h.ctx().sqlite.conn().unwrap();
    crate::db::sqlite::auth::latest_active(&conn, &h.ctx().security)
        .unwrap()
        .is_some()
}

fn csrf_of(h: &H, cookie: &str) -> String {
    h.ctx()
        .sessions
        .get(cookie.trim_start_matches("session="), h.ctx().now())
        .unwrap()
        .csrf_token
}

async fn signed_in(h: &H, cookie: &str) -> bool {
    let (_, v) = h
        .json(with_session(get("/auth/session-status"), cookie, None))
        .await;
    v["authenticated"] == true
}

/// The API key no longer trades: no broker session behind it.
async fn api_key_refused(h: &H, key: &str) -> bool {
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    let (s2, _) = h
        .json(post_json(
            "/api/v1/placeorder",
            json!({"apikey": key, "strategy": "t", "symbol": "SBIN", "exchange": "NSE",
                   "action": "BUY", "quantity": "1", "pricetype": "MARKET", "product": "MIS"}),
        ))
        .await;
    s == StatusCode::FORBIDDEN && v["message"] == "Invalid openalgo apikey" && s2 == s
}

// ------------------------------------------- SEC-01 anonymous logout

/// SEC-01: a caller with no signed-in session (a local program, a device on
/// the network, an internet caller through the tunnel) cannot sign the
/// trader out. It ends only its own browser session; the trader's session,
/// the broker session and the API key stay.
#[tokio::test]
async fn sec01_anonymous_logout_ends_only_its_own_session() {
    let h = H::new();
    let key = h.setup();
    h.connect_broker();
    let (trader, _) = h.session(true);

    // GET with no cookie and no Fetch Metadata, in process and through
    // the tunnel.
    for r in [
        get("/auth/logout"),
        {
            let mut r = get("/auth/logout");
            r.headers_mut()
                .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
            r
        },
    ] {
        let (s, headers, _) = h.send(r).await;
        assert_eq!(s, StatusCode::FOUND, "still answers like the web");
        assert_eq!(headers[header::LOCATION], "/login");
        assert!(signed_in(&h, &trader).await, "trader signed out by a stranger");
    }

    // POST with an anonymous session and its CSRF token (anyone can get
    // both from the public token endpoint): only that session ends.
    let (anon, anon_csrf) = h.session(false);
    let (s, v) = h
        .json(with_session(
            post_json("/auth/logout", json!({})),
            &anon,
            Some(&anon_csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["status"], "success");
    assert!(
        h.ctx()
            .sessions
            .get(anon.trim_start_matches("session="), h.ctx().now())
            .is_none(),
        "the caller's own session ends"
    );
    assert!(signed_in(&h, &trader).await, "trader signed out by a stranger");
    assert!(h.ctx().is_broker_connected(), "broker session kept");
    assert!(stored_session_active(&h), "stored broker session kept");
    let (s, _) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    assert_eq!(s, StatusCode::OK, "API key keeps working");

    // The trader's own logout still ends every session and the broker's.
    let (other, _) = h.session(true);
    let csrf = csrf_of(&h, &trader);
    let (s, v) = h
        .json(with_session(
            post_json("/auth/logout", json!({})),
            &trader,
            Some(&csrf),
        ))
        .await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"status": "success", "message": "Logged out successfully"})
        )
    );
    assert!(!signed_in(&h, &trader).await);
    assert!(!signed_in(&h, &other).await, "every session ends");
    assert!(!h.ctx().is_broker_connected());
    assert!(!stored_session_active(&h));
}

/// SEC-01 over a real connection: the request a program on this computer
/// (or a tunnel) sends, with the app's own Host and no cookie.
#[tokio::test]
async fn sec01_anonymous_logout_over_a_real_connection() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let (trader, _) = h.session(true);
    let served = super::security::serve(&h).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{}/auth/logout", served.addr.port());
    let r = client.get(&url).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let r = client.post(&url).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 400, "a POST without a session is refused");
    assert!(signed_in(&h, &trader).await, "trader signed out by a stranger");
    assert!(h.ctx().is_broker_connected());
}

// ------------------------------- SEC-05 memory first, then the stored row

/// SEC-05: logout while the database refuses the write. The session still
/// ends in this process (memory, API key, streaming), is never resumed, the
/// answer says the record is still to be written, and the session poll
/// writes it once the database recovers.
#[tokio::test]
async fn sec05_logout_ends_the_session_even_when_the_write_fails() {
    let h = H::new();
    let key = h.setup();
    connect_and_stream(&h).await;
    fail_auth_writes(&h);
    let (trader, csrf) = h.session(true);

    let (s, v) = h
        .json(with_session(
            post_json("/auth/logout", json!({})),
            &trader,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["message"],
        crate::server::routes::auth::LOGOUT_REVOKE_PENDING
    );
    assert_eq!(v["broker_revoke_pending"], true);
    assert!(stored_session_active(&h), "the write really failed");

    assert!(h.ctx().get_broker_session().is_none(), "session in memory");
    assert!(h.ctx().runtime.active_broker().is_none(), "streaming stopped");
    assert!(api_key_refused(&h, &key).await, "the API key still trades");
    assert!(BrokerAuthService::revoke_is_pending(h.ctx()));
    // A sign-in (or a restart's first sign-in) does not bring it back.
    assert!(BrokerAuthService::try_resume(h.ctx())
        .await
        .unwrap()
        .is_none());
    let (_, v) = h
        .json(multipart(
            "/auth/login",
            &[("username", USER), ("password", PASSWORD)],
        ))
        .await;
    assert_eq!(v, json!({"status": "success"}), "not resumed");
    assert!(!h.ctx().is_broker_connected());

    // One poll tick after the database recovers writes the revoke.
    allow_auth_writes(&h);
    crate::session::poll_tick(h.ctx(), h.ctx().now()).await;
    assert!(!stored_session_active(&h));
    assert!(!BrokerAuthService::revoke_is_pending(h.ctx()));
}

/// SEC-05: the daily boundary with the write failing stops streaming and
/// blocks resume the same way.
#[tokio::test]
async fn sec05_daily_expiry_with_a_failed_write_stops_streaming() {
    let h = H::new();
    h.setup();
    connect_and_stream(&h).await;
    fail_auth_writes(&h);
    let before = ist(2026, 10, 6, 2, 59);
    h.t.clock.set(ist(2026, 10, 6, 3, 0));
    assert!(crate::session::expire_if_crossed(h.ctx(), before).await);
    assert!(h.ctx().runtime.active_broker().is_none(), "streaming stopped");
    assert!(h.ctx().get_broker_session().is_none());
    assert!(BrokerAuthService::try_resume(h.ctx())
        .await
        .unwrap()
        .is_none());
    allow_auth_writes(&h);
    crate::session::poll_tick(h.ctx(), h.ctx().now()).await;
    assert!(!stored_session_active(&h));
}

/// SEC-05: switching broker while the old session's write fails still
/// ends the old session (memory and streaming); the record follows.
#[tokio::test]
async fn sec05_broker_switch_with_a_failed_write_ends_the_old_session() {
    let h = H::new();
    let key = h.setup();
    h.save_broker_credentials();
    connect_and_stream(&h).await;
    fail_auth_writes(&h);
    let (trader, csrf) = h.session(true);
    let (s, v) = h
        .json(with_session(
            post_json("/api/broker/credentials", json!({"broker": "upstox"})),
            &trader,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(v["signed_out_of"], "zerodha");
    assert!(h.ctx().get_broker_session().is_none());
    assert!(h.ctx().runtime.active_broker().is_none(), "streaming stopped");
    assert!(api_key_refused(&h, &key).await);
    assert!(BrokerAuthService::try_resume(h.ctx())
        .await
        .unwrap()
        .is_none());
    allow_auth_writes(&h);
    crate::session::poll_tick(h.ctx(), h.ctx().now()).await;
    assert!(!stored_session_active(&h));
}

/// SEC-05: account reset always stops the broker session's streaming,
/// even when the stored row cannot be revoked first.
#[tokio::test]
async fn sec05_account_reset_always_stops_streaming() {
    let h = H::new();
    h.setup();
    connect_and_stream(&h).await;
    fail_auth_writes(&h);
    AuthService::reset_account_everywhere(h.ctx())
        .await
        .unwrap();
    assert!(h.ctx().runtime.active_broker().is_none(), "streaming stopped");
    assert!(!h.ctx().is_broker_connected());
    assert!(!stored_session_active(&h), "the reset deleted the row");
    assert!(
        !BrokerAuthService::revoke_is_pending(h.ctx()),
        "nothing left to revoke"
    );
}

/// SEC-05: a new sign-in while a revoke is pending writes that revoke
/// first, so the later retry never revokes the new session.
#[tokio::test]
async fn sec05_a_new_sign_in_finishes_the_pending_revoke_first() {
    let h = H::new();
    h.setup();
    connect_and_stream(&h).await;
    fail_auth_writes(&h);
    BrokerAuthService::revoke(h.ctx(), SessionEndReason::Logout)
        .await
        .unwrap_err();
    assert!(BrokerAuthService::revoke_is_pending(h.ctx()));
    allow_auth_writes(&h);
    h.connect_broker();
    assert!(!BrokerAuthService::revoke_is_pending(h.ctx()));
    crate::session::poll_tick(h.ctx(), h.ctx().now()).await;
    assert!(h.ctx().is_broker_connected());
    assert!(stored_session_active(&h), "the new session's row is kept");
    h.ctx().runtime.teardown(h.ctx()).await;
}

// ---------------------------------- SEC-03 never new keys for an install

/// The app opened on `dir` with `store` as its keychain, as at a start.
fn open_at(
    dir: &std::path::Path,
    store: Arc<dyn crate::security::keystore::KeyStore>,
) -> Arc<AppState> {
    AppState::open(
        dir,
        crate::state::OpenOptions {
            keystore: store,
            clock: Arc::new(crate::clock::SystemClock),
            brokers: Arc::new(BrokerRegistry::with(vec![])),
        },
    )
    .unwrap()
}

/// Close the app as a restart does (DuckDB holds its file exclusively).
async fn close(ctx: Arc<AppState>) {
    ctx.shutdown().await;
    assert_eq!(Arc::strong_count(&ctx), 1, "app state outlives shutdown");
    drop(ctx);
}

fn save_kite_key(ctx: &AppState) {
    let conn = ctx.sqlite.conn().unwrap();
    crate::db::sqlite::credentials::save(
        &conn,
        &ctx.security,
        "zerodha",
        crate::db::sqlite::credentials::CredentialUpdate {
            api_key: Some("kiteapikey".into()),
            ..Default::default()
        },
    )
    .unwrap();
}

fn kite_key(ctx: &AppState) -> String {
    let conn = ctx.sqlite.conn().unwrap();
    crate::db::sqlite::credentials::load(&conn, &ctx.security, "zerodha")
        .unwrap()
        .unwrap()
        .api_key
        .expose()
        .to_string()
}

fn signs_in(ctx: &AppState, password: &str) -> bool {
    matches!(
        AuthService::verify_credentials(ctx, USER, password),
        Ok(crate::services::auth_service::LoginOutcome::Success(_))
            | Ok(crate::services::auth_service::LoginOutcome::TotpRequired(_))
    )
}

/// SEC-03: the keychain is locked (or access was denied) when the app
/// starts. Signing in with the right password says so, for any name, and
/// creates no vault; once the keychain is back the same password signs in
/// and every saved secret still decrypts.
#[tokio::test]
async fn sec03_a_locked_keychain_never_gets_new_keys() {
    use crate::security::keystore::FaultyKeyStore;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultyKeyStore::default());
    let ctx = open_at(dir.path(), store.clone());
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    save_kite_key(&ctx);
    close(ctx).await;

    store.set_unavailable(true);
    let ctx = open_at(dir.path(), store.clone());
    for name in [USER, "someone-else"] {
        let r = AuthService::verify_credentials(&ctx, name, PASSWORD);
        assert!(
            matches!(r, Err(crate::error::AppError::KeychainUnavailable)),
            "{}: {:?}",
            name,
            r
        );
        let (s, _, v) = send_to(
            &ctx,
            multipart(
                "/auth/login",
                &[("username", name), ("password", PASSWORD)],
            ),
        )
        .await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
        assert_eq!(v["message"], crate::error::KEYCHAIN_UNAVAILABLE_MESSAGE);
    }
    assert!(
        !dir.path().join(crate::security::VAULT_FILE).exists(),
        "a sign-in created new keys"
    );
    close(ctx).await;

    store.set_unavailable(false);
    let ctx = open_at(dir.path(), store);
    assert!(signs_in(&ctx, PASSWORD));
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;
}

/// SEC-03: in password mode, keys moved from an old `secrets.dat` are
/// wrapped into the vault only with a password that was verified; a wrong
/// password leaves no vault behind for the right one to fail on.
#[tokio::test]
async fn sec03_legacy_keys_are_wrapped_only_with_a_verified_password() {
    use crate::security::keystore::UnavailableKeyStore;
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join(crate::security::VAULT_FILE);
    crate::security::legacy::write_for_test(dir.path(), &[5u8; 32], &[6u8; 32]);
    let ctx = open_at(dir.path(), Arc::new(UnavailableKeyStore));
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    save_kite_key(&ctx);
    close(ctx).await;
    // As a build that had not yet wrapped the moved keys left it.
    std::fs::remove_file(&vault).unwrap();
    crate::security::legacy::write_for_test(dir.path(), &[5u8; 32], &[6u8; 32]);

    let ctx = open_at(dir.path(), Arc::new(UnavailableKeyStore));
    assert!(!signs_in(&ctx, "Wrong@1234"));
    assert!(!vault.exists(), "wrapped with a wrong password");
    assert!(signs_in(&ctx, PASSWORD));
    assert!(vault.exists());
    close(ctx).await;

    let ctx = open_at(dir.path(), Arc::new(UnavailableKeyStore));
    assert!(signs_in(&ctx, PASSWORD));
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;
}
