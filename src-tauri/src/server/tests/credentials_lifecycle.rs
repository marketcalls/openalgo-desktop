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

/// Whether the stored broker session is still un-revoked (read without the
/// keys, which a logout locks).
fn stored_session_active(h: &H) -> bool {
    let conn = h.ctx().sqlite.conn().unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM auth WHERE is_revoked = 0", [], |r| {
            r.get(0)
        })
        .unwrap();
    n > 0
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

/// The API answers that OpenAlgo is locked (after a logout, KEY-01): the
/// key is not even checked.
async fn api_locked(h: &H, key: &str) -> bool {
    let (s, v) = h
        .json(post_json("/api/v1/ping", json!({"apikey": key})))
        .await;
    s == StatusCode::SERVICE_UNAVAILABLE
        && v == json!({"status": "error", "message": crate::services::core::API_LOCKED})
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
    for r in [get("/auth/logout"), {
        let mut r = get("/auth/logout");
        r.headers_mut()
            .insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        r
    }] {
        let (s, headers, _) = h.send(r).await;
        assert_eq!(s, StatusCode::FOUND, "still answers like the web");
        assert_eq!(headers[header::LOCATION], "/login");
        assert!(
            signed_in(&h, &trader).await,
            "trader signed out by a stranger"
        );
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
    assert!(
        signed_in(&h, &trader).await,
        "trader signed out by a stranger"
    );
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
    assert_eq!(
        r.status().as_u16(),
        400,
        "a POST without a session is refused"
    );
    assert!(
        signed_in(&h, &trader).await,
        "trader signed out by a stranger"
    );
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
    assert!(
        h.ctx().runtime.active_broker().is_none(),
        "streaming stopped"
    );
    assert!(api_locked(&h, &key).await, "the API key still trades");
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
    assert_eq!(
        v,
        json!({"status": "success", "redirect": "/broker"}),
        "not resumed"
    );
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
    assert!(
        h.ctx().runtime.active_broker().is_none(),
        "streaming stopped"
    );
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
    assert!(
        h.ctx().runtime.active_broker().is_none(),
        "streaming stopped"
    );
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
    assert!(
        h.ctx().runtime.active_broker().is_none(),
        "streaming stopped"
    );
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

/// SEC-01: a signed-in browser's cookie rides on a GET from a link on
/// another site (SameSite=Lax), and a browser without Fetch Metadata does
/// not say so. Such a GET ends nothing; the app's own sign-out (a page of
/// OpenAlgo, or the address typed in) still does.
#[tokio::test]
async fn sec01_a_logout_link_from_elsewhere_ends_nothing() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let (trader, _) = h.session(true);
    for r in [with_session(get("/auth/logout"), &trader, None), {
        let mut r = with_session(get("/auth/logout"), &trader, None);
        r.headers_mut().insert(
            header::REFERER,
            "https://evil.example/page".parse().unwrap(),
        );
        r
    }] {
        let (s, _, _) = h.send(r).await;
        assert_eq!(s, StatusCode::FOUND);
        assert!(signed_in(&h, &trader).await, "signed out by a link");
        assert!(h.ctx().is_broker_connected(), "broker ended by a link");
    }
    let (s, headers, _) = h
        .send(in_app(with_session(get("/auth/logout"), &trader, None)))
        .await;
    assert_eq!(s, StatusCode::FOUND);
    assert_eq!(headers[header::LOCATION], "/login");
    assert!(!signed_in(&h, &trader).await);
    assert!(!h.ctx().is_broker_connected());
}

/// SEC-05b: the trader logs out while the database refuses the write, then
/// OpenAlgo is closed and opened again on the same data folder before the
/// retry ran. The process's memory of the pending revoke is gone, but the
/// stored session must still not come back: neither a resume nor a sign-in
/// brings it back, and the first poll after the database recovers revokes
/// it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sec05b_a_failed_revoke_survives_a_restart() {
    use crate::services::broker_auth_service::REVOKE_MARKER;
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn crate::security::keystore::KeyStore> =
        Arc::new(crate::security::keystore::MemoryKeyStore::new());
    let open = || {
        AppState::open(
            dir.path(),
            crate::state::OpenOptions {
                keystore: store.clone(),
                clock: Arc::new(crate::clock::SystemClock),
                brokers: Arc::new(BrokerRegistry::with(vec![
                    Arc::new(MockBroker::new("zerodha")) as Arc<dyn crate::brokers::Broker>,
                ])),
            },
        )
        .unwrap()
    };
    let active = |ctx: &AppState| {
        let conn = ctx.sqlite.conn().unwrap();
        crate::db::sqlite::auth::latest_active(&conn, &ctx.security)
            .unwrap()
            .is_some()
    };

    let ctx = open();
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    BrokerAuthService::persist(
        &ctx,
        &BrokerSession {
            broker_id: "zerodha".into(),
            auth_token: "mock-access-token".into(),
            feed_token: None,
            user_id: "AB1234".into(),
            user_name: None,
            authenticated_at: ctx.now(),
        },
    )
    .unwrap();
    ctx.sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER test_fail_auth_update BEFORE UPDATE ON auth
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    BrokerAuthService::revoke(&ctx, SessionEndReason::Logout)
        .await
        .unwrap_err();
    assert!(active(&ctx), "the write really failed");
    assert!(dir.path().join(REVOKE_MARKER).exists());
    close(ctx).await;

    // The next start, the database still refusing.
    let ctx = open();
    assert!(BrokerAuthService::try_resume(&ctx).await.unwrap().is_none());
    let (s, _, v) = send_to(
        &ctx,
        multipart("/auth/login", &[("username", USER), ("password", PASSWORD)]),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_ne!(
        v["redirect"], "/dashboard",
        "resumed after a restart: {}",
        v
    );
    assert!(ctx.get_broker_session().is_none());
    assert!(active(&ctx));

    // The database recovers: one poll tick writes the revoke.
    ctx.sqlite
        .conn()
        .unwrap()
        .execute_batch("DROP TRIGGER test_fail_auth_update")
        .unwrap();
    crate::session::poll_tick(&ctx, ctx.now()).await;
    assert!(!active(&ctx));
    assert!(!BrokerAuthService::revoke_is_pending(&ctx));
    assert!(!dir.path().join(REVOKE_MARKER).exists());
    close(ctx).await;
}

/// SEC-05c: a resume waiting on the broker's session check while the
/// trader logs out. When the check answers, the ended session must not be
/// installed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sec05c_a_resume_racing_a_logout_installs_nothing() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    // As after a restart: the stored row only.
    h.ctx().set_broker_session(None);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *h.mock.funds_gate.lock() = Some(gate.clone());
    let calls_before = *h.mock.funds_calls.lock();
    let ctx = h.ctx().clone();
    let resume = tokio::spawn(async move { BrokerAuthService::try_resume(&ctx).await });
    // The resume is waiting on the broker's answer.
    let mut waited = 0;
    while *h.mock.funds_calls.lock() == calls_before {
        assert!(waited < 500, "the resume never asked the broker");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        waited += 1;
    }
    BrokerAuthService::revoke(h.ctx(), SessionEndReason::Logout)
        .await
        .unwrap();
    gate.add_permits(1);
    let resumed = resume.await.unwrap().unwrap();
    assert!(resumed.is_none(), "a resume brought an ended session back");
    assert!(h.ctx().get_broker_session().is_none());
    assert!(h.ctx().runtime.active_broker().is_none());
    assert!(!stored_session_active(&h));
}

// ------------------------------- SEC-03, SEC-04, SEC-09 under KEY-01

/// The app opened on `dir` with `store` as the keychain of earlier builds,
/// as at a start.
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
/// A request's own short task may still hold the state for a moment.
async fn close(ctx: Arc<AppState>) {
    ctx.shutdown().await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&ctx) > 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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

/// Sign in as the route does: check the password and, when it opens the
/// keys, install them (with the session, in the route).
fn signs_in(ctx: &AppState, password: &str) -> bool {
    use crate::services::auth_service::LoginOutcome;
    match AuthService::verify_credentials(
        ctx,
        USER,
        password,
        crate::security::SignInFrom::AppWindow,
    ) {
        LoginOutcome::Success(_, keys) | LoginOutcome::TotpRequired(_, keys) => {
            ctx.security.install(&keys);
            true
        }
        _ => false,
    }
}

/// SEC-03 under KEY-01: no sign-in ever makes new keys for an existing
/// install. The keys of an earlier build's keychain install stay where they
/// are while the keychain is locked (nothing is written, the account is
/// told how to go on), and once it opens the same password moves them and
/// every saved secret still decrypts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sec03_a_locked_keychain_never_gets_new_keys() {
    use crate::security::keystore::{CountingKeyStore, DATA_KEY, PEPPER};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let keys = crate::security::Unlocked::random().unwrap();
    {
        // An earlier build's install: its rows, its keys in the keychain.
        let with = keys.manager();
        let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
        let conn = db.conn().unwrap();
        let hash = with.hash_password(PASSWORD).unwrap();
        crate::db::sqlite::user::insert(
            &conn,
            &with,
            USER,
            EMAIL,
            &hash,
            &crate::security::totp::generate_secret(),
        )
        .unwrap();
        crate::db::sqlite::credentials::save(
            &conn,
            &with,
            "zerodha",
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some("kiteapikey".into()),
                ..Default::default()
            },
        )
        .unwrap();
        conn.execute_batch(
            "DROP TABLE key_vault; DELETE FROM migrations WHERE name = '082_key_vault';",
        )
        .unwrap();
        let (k, p) = crate::security::keychain_entries_for_test(&keys);
        store.seed(DATA_KEY, &k);
        store.seed(PEPPER, &p);
    }
    store.inner.set_unavailable(true);
    let ctx = open_at(dir.path(), store.clone());
    let r = AuthService::verify_credentials(
        &ctx,
        USER,
        PASSWORD,
        crate::security::SignInFrom::AppWindow,
    );
    assert!(
        matches!(
            r,
            crate::services::auth_service::LoginOutcome::Unreadable(
                crate::error::AppError::KeychainUnavailable
            )
        ),
        "{:?}",
        r
    );
    let (s, _, v) = send_to(
        &ctx,
        from_the_app_window(
            &ctx,
            multipart("/auth/login", &[("username", USER), ("password", PASSWORD)]),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
    assert_eq!(v["message"], crate::error::KEYCHAIN_UNAVAILABLE_MESSAGE);
    let vault = crate::db::sqlite::key_vault::read(&ctx.sqlite.conn().unwrap()).unwrap();
    assert!(vault.is_none(), "a sign-in made new keys");
    assert!(!ctx.security.is_unlocked());
    close(ctx).await;

    store.inner.set_unavailable(false);
    let ctx = open_at(dir.path(), store);
    assert!(signs_in(&ctx, PASSWORD));
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;
}

/// SEC-03 under KEY-01: keys moved from an old `secrets.dat` are wrapped
/// into the vault only with a password that was verified; a wrong password
/// leaves no vault behind for the right one to fail on.
#[tokio::test]
async fn sec03_legacy_keys_are_wrapped_only_with_a_verified_password() {
    use crate::security::keystore::CountingKeyStore;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_at(dir.path(), store.clone());
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    assert!(signs_in(&ctx, PASSWORD));
    save_kite_key(&ctx);
    // The keys setup made, as an earlier build's secrets.dat held them.
    let (key, pepper) = {
        let row = crate::db::sqlite::key_vault::read(&ctx.sqlite.conn().unwrap())
            .unwrap()
            .unwrap();
        let (k, p) = crate::db::sqlite::key_vault::unwrap(&row, PASSWORD)
            .unwrap()
            .unwrap();
        (k.expose().to_vec(), p.expose().to_vec())
    };
    close(ctx).await;
    // As a build that had not yet wrapped the moved keys left it.
    {
        let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("openalgo.db")).unwrap();
        crate::db::sqlite::key_vault::delete(&db.conn().unwrap()).unwrap();
    }
    crate::security::legacy::write_for_test(dir.path(), &key, &pepper);

    let ctx = open_at(dir.path(), store.clone());
    let vault = |ctx: &AppState| {
        crate::db::sqlite::key_vault::read(&ctx.sqlite.conn().unwrap())
            .unwrap()
            .map(|r| r.source)
    };
    assert!(!signs_in(&ctx, "Wrong@1234"));
    assert_eq!(vault(&ctx), None, "wrapped with a wrong password");
    assert!(signs_in(&ctx, PASSWORD));
    assert_eq!(
        vault(&ctx),
        Some(crate::db::sqlite::key_vault::Source::LegacyFile)
    );
    assert!(!crate::security::legacy::path(dir.path()).exists());
    close(ctx).await;

    let ctx = open_at(dir.path(), store.clone());
    assert!(signs_in(&ctx, PASSWORD));
    assert_eq!(kite_key(&ctx), "kiteapikey");
    assert_eq!(store.calls(), 0, "the keychain was called");
    close(ctx).await;
}

/// SEC-04 under KEY-01: an account reset signs every window out, locks the
/// keys and deletes the vault with the account; no keychain is involved,
/// so nothing it refuses can leave a window signed in.
#[tokio::test]
async fn sec04_a_reset_signs_everyone_out_and_forgets_the_keys() {
    use crate::security::keystore::CountingKeyStore;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_at(dir.path(), store.clone());
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    let s = ctx.sessions.create(ctx.now());
    ctx.sessions.update(&s.id, |x| x.user = Some(USER.into()));
    assert_eq!(ctx.sessions.authenticated_count(), 1);
    AuthService::reset_account_everywhere(&ctx).await.unwrap();
    assert!(AuthService::needs_setup(&ctx).unwrap(), "account removed");
    assert_eq!(
        ctx.sessions.authenticated_count(),
        0,
        "a window stayed signed in to a removed account"
    );
    assert!(!ctx.security.is_unlocked(), "the keys outlived the account");
    assert!(
        crate::db::sqlite::key_vault::read(&ctx.sqlite.conn().unwrap())
            .unwrap()
            .is_none(),
        "the vault outlived the account"
    );
    // A new account gets new keys.
    AuthService::setup(&ctx, USER, EMAIL, "Second@123").unwrap();
    assert!(signs_in(&ctx, "Second@123"));
    assert_eq!(store.calls(), 0, "the keychain was called");
    close(ctx).await;
}

// ------------------------------- SEC-02 password change under KEY-01

const NEW_PASSWORD: &str = "Changed@456";

/// An install with an account and a saved broker key, its keys in the
/// vault (an OS-keychain-like store: the app opens locked).
async fn locked_install() -> (
    tempfile::TempDir,
    Arc<crate::security::keystore::CountingKeyStore>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::security::keystore::CountingKeyStore::default());
    let ctx = open_at(dir.path(), store.clone());
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    assert!(signs_in(&ctx, PASSWORD));
    save_kite_key(&ctx);
    close(ctx).await;
    (dir, store)
}

fn reopen(
    dir: &tempfile::TempDir,
    store: &Arc<crate::security::keystore::CountingKeyStore>,
) -> Arc<AppState> {
    open_at(dir.path(), store.clone())
}

/// After a restart exactly `works` signs in (not `fails`), and the saved
/// key still decrypts.
async fn only_password_signs_in(
    dir: &tempfile::TempDir,
    store: &Arc<crate::security::keystore::CountingKeyStore>,
    works: &str,
    fails: &str,
) {
    let ctx = reopen(dir, store);
    assert!(!signs_in(&ctx, fails), "{} signs in", fails);
    assert!(!ctx.security.is_unlocked(), "{} unlocked the keys", fails);
    assert!(signs_in(&ctx, works), "{} does not sign in", works);
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;
    let ctx = reopen(dir, store);
    assert!(signs_in(&ctx, works), "{} after a second restart", works);
    close(ctx).await;
    assert_eq!(store.calls(), 0, "the keychain was called");
}

/// SEC-02: a change that completes moves the hash and the vault to the new
/// password together.
#[tokio::test]
async fn sec02_a_completed_change_moves_to_the_new_password() {
    let (dir, store) = locked_install().await;
    let ctx = reopen(&dir, &store);
    assert!(signs_in(&ctx, PASSWORD));
    AuthService::change_password(&ctx, USER, PASSWORD, NEW_PASSWORD, NEW_PASSWORD).unwrap();
    assert!(ctx.security.is_unlocked(), "the keys stay with the trader");
    close(ctx).await;
    only_password_signs_in(&dir, &store, NEW_PASSWORD, PASSWORD).await;
}

/// SEC-02 (review): the trader changes a leaked password while someone
/// signs in with the old one. That sign-in read the account (and the old
/// hash) just before the change, and checks the password after the change
/// signed every window out: it must not get a session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sec02_a_sign_in_racing_a_password_change_gets_no_session() {
    let h = H::new();
    h.setup();
    let (reached, release) = crate::services::auth_service::test_hooks::pause_next_sign_in(h.ctx());
    let ctx = h.ctx().clone();
    let old_password_sign_in = tokio::spawn(async move {
        send_to(
            &ctx,
            multipart("/auth/login", &[("username", USER), ("password", PASSWORD)]),
        )
        .await
    });
    // It has read the account, with the old hash.
    tokio::task::spawn_blocking(move || {
        reached
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the sign-in reached the account read")
    })
    .await
    .unwrap();
    let ctx = h.ctx().clone();
    tokio::task::spawn_blocking(move || {
        AuthService::change_password(&ctx, USER, PASSWORD, NEW_PASSWORD, NEW_PASSWORD)
    })
    .await
    .unwrap()
    .unwrap();
    release.send(()).unwrap();
    let (s, _, v) = old_password_sign_in.await.unwrap();
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{}", v);
    assert_eq!(
        h.ctx().sessions.authenticated_count(),
        0,
        "a session was issued for the old password"
    );
    // The new password signs in as usual.
    let (s, _, v) = send_to(
        h.ctx(),
        multipart(
            "/auth/login",
            &[("username", USER), ("password", NEW_PASSWORD)],
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

/// SEC-02b: two password changes both validated against the old password.
/// The first finishes; the second must not overwrite it (its hash and its
/// vault write are refused together, in one transaction). Only the first
/// change's password signs in.
#[tokio::test]
async fn sec02b_a_second_change_validated_against_a_stale_hash_is_refused() {
    let (dir, store) = locked_install().await;
    let ctx = reopen(&dir, &store);
    assert!(signs_in(&ctx, PASSWORD));
    let (id, original): (i64, String) = ctx
        .sqlite
        .conn()
        .unwrap()
        .query_row("SELECT id, password_hash FROM users", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    // A: validated and finished.
    AuthService::set_password(&ctx, id, &original, NEW_PASSWORD).unwrap();
    // B: validated against the same old password, finishes second.
    let r = AuthService::set_password(&ctx, id, &original, "Other@789");
    assert!(
        matches!(&r, Err(crate::error::AppError::Validation(m))
            if m == crate::services::auth_service::PASSWORD_CHANGED_MEANWHILE),
        "{:?}",
        r
    );
    close(ctx).await;
    only_password_signs_in(&dir, &store, NEW_PASSWORD, PASSWORD).await;
    let ctx = reopen(&dir, &store);
    assert!(!signs_in(&ctx, "Other@789"));
    close(ctx).await;
}

// ------------------------------------------ WI-3 broker settings save

async fn post_settings(h: &H, cookie: &str, csrf: &str, body: Value) -> (StatusCode, Value) {
    h.json(with_session(
        post_json("/api/broker/credentials", body),
        cookie,
        Some(csrf),
    ))
    .await
}

async fn settings(h: &H, cookie: &str) -> Value {
    let (_, v) = h
        .json(with_session(get("/api/broker/credentials"), cookie, None))
        .await;
    v["data"].clone()
}

async fn feed_address(h: &H, cookie: &str) -> Value {
    let (_, v) = h
        .json(with_session(get("/api/websocket/config"), cookie, None))
        .await;
    v["websocket_url"].clone()
}

/// FLOW-01: correcting only the client id (the account a sign-in is bound
/// to) is saved.
#[tokio::test]
async fn flow01_a_client_id_alone_is_saved() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let (cookie, csrf) = h.session(true);
    let (s, v) = post_settings(&h, &cookie, &csrf, json!({"client_id": "AB9999"})).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(v["updated_fields"]
        .as_array()
        .unwrap()
        .contains(&json!("CLIENT_ID")));
    assert_eq!(settings(&h, &cookie).await["client_id"], "AB9999");
}

/// CFG-02: an address must be a full URL with the right scheme and a host.
#[tokio::test]
async fn cfg02_addresses_must_be_full_urls() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let (cookie, csrf) = h.session(true);
    for (field, value) in [
        ("host_server", "http-not-a-url"),
        ("host_server", "https//x.ngrok.app"),
        ("host_server", "ws://x.ngrok.app"),
        ("host_server", "http://"),
        ("websocket_url", "ws-not-a-url"),
        ("websocket_url", "ws:/x.ngrok.app"),
        ("websocket_url", "https://x.ngrok.app"),
        // Read one way here and another by a broker or a browser.
        ("host_server", "https://x.ngrok.app\\@evil.example"),
        ("host_server", "https://user@x.ngrok.app"),
        ("host_server", "https://x.ngrok.app/?next=evil"),
        ("host_server", "https://x.ngrok.app/#evil"),
        ("host_server", "https://x.ngrok.app/somewhere"),
        ("host_server", "https://x.ngrok .app"),
        ("websocket_url", "wss://user:pass@x.ngrok.app/ws"),
        ("websocket_url", "wss://x.ngrok.app/ws?k=v"),
        (
            "redirect_url",
            "https://evil.example\\@127.0.0.1:5000/zerodha/callback",
        ),
        ("redirect_url", "https://u@evil.example/zerodha/callback"),
        (
            "redirect_url",
            "https://127.0.0.1:5000/zerodha/callback?x=1/zerodha/callback",
        ),
    ] {
        let (s, v) = post_settings(&h, &cookie, &csrf, json!({ field: value })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{} = {}: {}", field, value, v);
    }
    for (field, value) in [
        ("host_server", "https://x.ngrok.app"),
        ("host_server", "https://x.ngrok.app/"),
        ("websocket_url", "wss://x.ngrok.app/ws"),
        ("redirect_url", "https://x.ngrok.app/zerodha/callback"),
    ] {
        let (s, v) = post_settings(&h, &cookie, &csrf, json!({ field: value })).await;
        assert_eq!(s, StatusCode::OK, "{} = {}: {}", field, value, v);
    }
}

/// CFG-01: a saved address sent back empty returns to the automatic one,
/// which follows a later port change.
#[tokio::test]
async fn cfg01_an_emptied_address_goes_back_to_automatic() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    let (cookie, csrf) = h.session(true);
    let body =
        json!({"websocket_url": "wss://x.ngrok.app/ws", "host_server": "https://x.ngrok.app"});
    let (s, v) = post_settings(&h, &cookie, &csrf, body).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(feed_address(&h, &cookie).await, "wss://x.ngrok.app/ws");
    let saved = settings(&h, &cookie).await;
    assert_eq!(saved["websocket_url_is_default"], false);
    assert_eq!(saved["host_server_is_default"], false);

    let body = json!({"websocket_url": "", "host_server": ""});
    let (s, v) = post_settings(&h, &cookie, &csrf, body).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let cfg = h.ctx().server_config();
    assert_eq!(
        feed_address(&h, &cookie).await,
        format!("ws://127.0.0.1:{}", cfg.ws_port)
    );
    let saved = settings(&h, &cookie).await;
    assert_eq!(saved["websocket_url_is_default"], true);
    assert_eq!(saved["host_server_is_default"], true);
    assert_eq!(
        saved["host_server"],
        format!("http://127.0.0.1:{}", cfg.http_port)
    );
    // A later port change is followed.
    h.ctx().pin_listener_ports(cfg.http_port, 18766);
    assert_eq!(feed_address(&h, &cookie).await, "ws://127.0.0.1:18766");
}

/// The database refuses every change to the settings row.
fn fail_settings_writes(h: &H) {
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER test_fail_settings_update BEFORE UPDATE ON settings
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
}

/// FLOW-03 (server half): a switch whose save fails changes nothing: the
/// live session keeps running and the other broker's keys are not stored.
#[tokio::test]
async fn flow03_a_failed_switch_save_keeps_the_live_session() {
    let h = H::new();
    h.setup();
    h.save_broker_credentials();
    connect_and_stream(&h).await;
    fail_settings_writes(&h);
    let (cookie, csrf) = h.session(true);
    let body = json!({"broker": "upstox", "broker_api_key": "upstoxkey"});
    let (s, v) = post_settings(&h, &cookie, &csrf, body).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{}", v);
    assert_eq!(
        v["message"],
        "Your broker settings were not saved, and nothing was changed. Try again."
    );
    assert_eq!(
        h.ctx().get_broker_session().map(|s| s.broker_id).as_deref(),
        Some("zerodha"),
        "the live session was ended by a save that failed"
    );
    assert_eq!(h.ctx().runtime.active_broker().as_deref(), Some("zerodha"));
    assert_eq!(
        h.ctx().server_config().active_broker.as_deref(),
        Some("zerodha")
    );
    let conn = h.ctx().sqlite.conn().unwrap();
    assert!(
        crate::db::sqlite::credentials::load(&conn, &h.ctx().security, "upstox")
            .unwrap()
            .is_none(),
        "keys saved by a switch that failed"
    );
    drop(conn);
    h.ctx().runtime.teardown(h.ctx()).await;
}

/// FLOW-03: a new broker session and the active broker are stored together
/// or not at all.
#[tokio::test]
async fn flow03_a_session_and_the_active_broker_are_stored_together() {
    let h = H::new();
    h.setup();
    fail_settings_writes(&h);
    let r = BrokerAuthService::persist(
        h.ctx(),
        &BrokerSession {
            broker_id: "zerodha".into(),
            auth_token: "mock-access-token".into(),
            feed_token: None,
            user_id: "AB1234".into(),
            user_name: None,
            authenticated_at: h.ctx().now(),
        },
    );
    assert!(r.is_err());
    assert!(!stored_session_active(&h), "a half-stored session");
    assert!(!h.ctx().is_broker_connected());
}

// ------------------------------------- SEC-06 key-bearing responses

/// SEC-06: every answer that carries the API key or the TOTP enrollment
/// seed to the signed-in session is marked never to be cached.
#[tokio::test]
async fn sec06_key_bearing_answers_are_never_cached() {
    let h = H::new();
    h.setup();
    h.connect_broker();
    let (cookie, csrf) = h.session(true);
    let json_get = |path: &str| {
        let mut r = with_session(get(path), &cookie, None);
        r.headers_mut()
            .insert(header::ACCEPT, "application/json".parse().unwrap());
        r
    };
    let mut answers = Vec::new();
    for path in [
        "/apikey",
        "/auth/profile-data",
        "/auth/session-status",
        "/playground/api-key",
        "/api/websocket/apikey",
    ] {
        let (s, headers, body) = h.send(json_get(path)).await;
        answers.push((path.to_string(), s, headers, body));
    }
    let (s, headers, body) = h
        .send(with_session(
            post_json("/apikey", json!({"user_id": USER})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    answers.push(("POST /apikey".into(), s, headers, body));
    // The AI-client token, shown once when it is created.
    let (s, headers, body) = h
        .send(with_session(
            post_json(
                "/api/mcp/tokens",
                json!({"name": "Claude", "scope": "read"}),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    answers.push(("POST /api/mcp/tokens".into(), s, headers, body));
    for (what, s, headers, body) in answers {
        assert_eq!(s, StatusCode::OK, "{}", what);
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            v.to_string().contains("api_key")
                || v.to_string().contains("totp_secret")
                || v.to_string().contains("oamcp_"),
            "{}: {}",
            what,
            v
        );
        let cache = headers
            .get(header::CACHE_CONTROL)
            .and_then(|c| c.to_str().ok())
            .unwrap_or_default();
        assert!(cache.contains("no-store"), "{}: {:?}", what, cache);
    }
}
