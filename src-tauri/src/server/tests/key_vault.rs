//! KEY-01 (maintainer decision 2026-10-13): no OS keychain; the data key and
//! the pepper wrapped with the login password in openalgo.db; unlocked at
//! sign-in and wiped at logout; existing keychain and vault.json installs
//! moved once. `docs/port/no-keychain.md` maps each test to its acceptance
//! item.

use super::*;
use crate::db::sqlite::key_vault::{self, KeychainCleanup, Source};
use crate::db::sqlite::{api_keys, credentials, user, SqliteDb};
use crate::security::keystore::{CountingKeyStore, KeyStore, KeyStoreError, DATA_KEY, PEPPER};
use crate::security::{totp, MoveStep, SignInFrom, Unlocked};
use crate::services::apikey_service::{ApiKeyService, KeyCheck};
use crate::services::auth_service::LoginOutcome;
use std::path::Path;

const NEW_PASSWORD: &str = "Fresh@1234";

/// The app opened on `dir` with `store` as the keychain earlier builds used.
fn open_with(dir: &Path, store: Arc<CountingKeyStore>) -> Arc<AppState> {
    open_on(dir, store)
}

fn open_on(dir: &Path, store: Arc<dyn KeyStore>) -> Arc<AppState> {
    AppState::open(
        dir,
        crate::state::OpenOptions {
            keystore: store,
            clock: Arc::new(crate::clock::SystemClock),
            brokers: Arc::new(BrokerRegistry::with(vec![
                Arc::new(MockBroker::new("zerodha")) as Arc<dyn crate::brokers::Broker>,
            ])),
        },
    )
    .unwrap()
}

/// Close the app as quitting does (DuckDB holds its file exclusively).
async fn close(ctx: Arc<AppState>) {
    ctx.shutdown().await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while Arc::strong_count(&ctx) > 1 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(Arc::strong_count(&ctx), 1, "app state outlives shutdown");
    drop(ctx);
}

/// A database an earlier build left: the account, its API key and saved
/// broker keys, encrypted with `keys`, and no vault (the vault migration
/// has not run yet). Returns the API key.
fn earlier_build_database(dir: &Path, keys: &Unlocked, password: &str) -> String {
    let with = keys.manager();
    let db = SqliteDb::new(&dir.join("openalgo.db")).unwrap();
    let conn = db.conn().unwrap();
    let hash = with.hash_password(password).unwrap();
    user::insert(&conn, &with, USER, EMAIL, &hash, &totp::generate_secret()).unwrap();
    let key = api_keys::generate_api_key();
    api_keys::upsert_for_user(&conn, &with, USER, &key).unwrap();
    credentials::save(
        &conn,
        &with,
        "zerodha",
        credentials::CredentialUpdate {
            api_key: Some("kiteapikey".into()),
            api_secret: Some("kitesecret".into()),
            client_id: Some("AB1234".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // As an earlier build left it: no key_vault table at all.
    conn.execute_batch(
        "DROP TABLE key_vault; DELETE FROM migrations WHERE name = '082_key_vault';",
    )
    .unwrap();
    key
}

/// An earlier build's keychain install: its database, and its keys in the
/// keychain (put there without counting).
fn keychain_install(dir: &Path, store: &CountingKeyStore) -> String {
    let keys = Unlocked::random().unwrap();
    let key = earlier_build_database(dir, &keys, PASSWORD);
    let (k, p) = crate::security::keychain_entries_for_test(&keys);
    store.seed(DATA_KEY, &k);
    store.seed(PEPPER, &p);
    key
}

/// Account setup, then the password step of a sign-in with the keys it
/// opened installed, as the session a sign-in issues installs them (setup
/// alone leaves them locked).
fn set_up_and_unlock(ctx: &AppState) {
    AuthService::setup(ctx, USER, EMAIL, PASSWORD).unwrap();
    assert!(!ctx.security.is_unlocked(), "setup left the keys in memory");
    ctx.security
        .install(&opened(ctx, PASSWORD).expect("the password was refused"));
}

/// The keys the password opened, from the app window (as the route checks
/// it), or the outcome when it opened none.
fn opened(ctx: &AppState, password: &str) -> Result<Unlocked, LoginOutcome> {
    match AuthService::verify_credentials(ctx, USER, password, SignInFrom::AppWindow) {
        LoginOutcome::Success(_, keys) | LoginOutcome::TotpRequired(_, keys) => Ok(keys),
        other => Err(other),
    }
}

/// A sign-in in the OpenAlgo Desktop window (with its proof).
async fn sign_in(ctx: &Arc<AppState>, password: &str) -> (StatusCode, Value, Option<String>) {
    let (s, h, v) = send_to(
        ctx,
        from_the_app_window(
            ctx,
            multipart("/auth/login", &[("username", USER), ("password", password)]),
        ),
    )
    .await;
    (s, v, cookie_from(&h))
}

async fn log_out(ctx: &Arc<AppState>, cookie: &str) {
    let id = cookie.trim_start_matches("session=");
    let csrf = ctx.sessions.get(id, ctx.now()).unwrap().csrf_token;
    let (s, _, v) = send_to(
        ctx,
        with_session(in_app(form("/auth/logout", &[])), cookie, Some(&csrf)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
}

fn vault(ctx: &AppState) -> Option<key_vault::VaultRow> {
    key_vault::read(&ctx.sqlite.conn().unwrap()).unwrap()
}

fn kite_key(ctx: &AppState) -> String {
    let conn = ctx.sqlite.conn().unwrap();
    credentials::load(&conn, &ctx.security, "zerodha")
        .unwrap()
        .unwrap()
        .api_key
        .expose()
        .to_string()
}

/// A route of `/api/v1` that needs only the key (no broker session).
async fn api_answer(ctx: &Arc<AppState>, key: &str) -> (StatusCode, Value) {
    let (s, _, v) = send_to(
        ctx,
        post_json("/api/v1/market/holidays", json!({"apikey": key})),
    )
    .await;
    (s, v)
}

fn locked_api_answer(s: StatusCode, v: &Value) -> bool {
    s == StatusCode::SERVICE_UNAVAILABLE
        && v == &json!({"status": "error", "message": crate::services::core::API_LOCKED})
}

// ------------------------------------------------------------- new install

/// KEY-01: a new install is in password mode on every system: no keychain
/// call while it is opened, the account created, signed in and out, the
/// password changed and the app reopened; the vault is a row of
/// openalgo.db and no key file is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_install_never_touches_the_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_with(dir.path(), store.clone());
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    let row = vault(&ctx).expect("the vault is in openalgo.db");
    assert_eq!(row.source, Source::Setup);
    let (s, v, cookie) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    log_out(&ctx, &cookie.unwrap()).await;
    assert!(!ctx.security.is_unlocked());
    sign_in(&ctx, PASSWORD).await;
    AuthService::change_password(&ctx, USER, PASSWORD, NEW_PASSWORD, NEW_PASSWORD).unwrap();
    close(ctx).await;
    let ctx = open_with(dir.path(), store.clone());
    let (s, v, _) = sign_in(&ctx, NEW_PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(store.calls(), 0, "the keychain was called");
    for f in [
        crate::security::VAULT_FILE,
        crate::security::VAULT_NEXT_FILE,
        "secrets.dat",
    ] {
        assert!(!dir.path().join(f).exists(), "{} written", f);
    }
    close(ctx).await;
}

// ----------------------------------------------------- the keychain move

/// KEY-01: an earlier build's keychain install. Opening reads nothing; the
/// first sign-in reads the two entries once, copies them into the vault,
/// verifies them and deletes the entries; the stored rows still decrypt;
/// no later sign-in, logout or restart reads the keychain again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keychain_keys_move_once_and_the_keychain_is_never_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let api_key = keychain_install(dir.path(), &store);
    let ctx = open_with(dir.path(), store.clone());
    assert_eq!(store.calls(), 0, "opening read the keychain");
    let (s, v) = api_answer(&ctx, &api_key).await;
    assert!(locked_api_answer(s, &v), "{} {}", s, v);

    let (s, v, cookie) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(store.reads(), 2, "one read of each entry");
    assert_eq!(store.peek(DATA_KEY), None, "entry left in the keychain");
    assert_eq!(store.peek(PEPPER), None, "entry left in the keychain");
    let row = vault(&ctx).unwrap();
    assert_eq!(row.source, Source::Keychain);
    assert_eq!(row.keychain_cleanup, KeychainCleanup::Done);
    assert_eq!(kite_key(&ctx), "kiteapikey");
    assert_eq!(
        ApiKeyService::check(&ctx, &api_key),
        KeyCheck::Valid(USER.into())
    );
    let calls = store.calls();

    log_out(&ctx, &cookie.unwrap()).await;
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK);
    close(ctx).await;
    let ctx = open_with(dir.path(), store.clone());
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(kite_key(&ctx), "kiteapikey");
    assert_eq!(store.calls(), calls, "the keychain was used again");
    close(ctx).await;
}

/// KEY-01: a wrong password, or a name that is not the account's, in the
/// app window moves nothing and deletes nothing (the keychain is read once
/// per sign-in whatever the name, so its prompt tells nothing about the
/// name). From anywhere else nothing reads the keychain at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_password_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    keychain_install(dir.path(), &store);
    let ctx = open_with(dir.path(), store.clone());
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    let (s, _) = sign_in_as(&ctx, Caller::Browser, USER, PASSWORD).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(store.calls(), 0, "a browser reached the keychain");
    let (s, _) = sign_in_as(&ctx, Caller::AppWindow, "someone", PASSWORD).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = sign_in_as(&ctx, Caller::AppWindow, USER, "Wrong@1234").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(store.reads(), 4, "two reads per sign-in in the window");
    assert!(vault(&ctx).is_none());
    assert!(store.peek(DATA_KEY).is_some() && store.peek(PEPPER).is_some());
    assert!(!ctx.security.is_unlocked());
    close(ctx).await;
}

/// KEY-01: the move stopped at every step (as a crash or a power cut would)
/// recovers at the next start's sign-in. The keychain entries are never
/// deleted before the vault row is committed and verified, and once the
/// row is committed the keychain is never read again: only its pending
/// deletion runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_at_every_step_of_the_move_recovers() {
    for step in [
        MoveStep::Read,
        MoveStep::BeforeCommit,
        MoveStep::AfterCommit,
        MoveStep::AfterVerify,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(CountingKeyStore::default());
        keychain_install(dir.path(), &store);
        let ctx = open_with(dir.path(), store.clone());
        ctx.security.fail_move_at(Some(step));
        let (s, v, _) = sign_in(&ctx, PASSWORD).await;
        assert_ne!(s, StatusCode::OK, "{:?}: {}", step, v);
        assert!(!ctx.security.is_unlocked(), "{:?}", step);
        close(ctx).await;

        let ctx = open_with(dir.path(), store.clone());
        let committed = matches!(step, MoveStep::AfterCommit | MoveStep::AfterVerify);
        assert_eq!(vault(&ctx).is_some(), committed, "{:?}", step);
        assert!(
            store.peek(DATA_KEY).is_some() && store.peek(PEPPER).is_some(),
            "{:?}: deleted before the copy was verified",
            step
        );
        if committed {
            assert_eq!(
                vault(&ctx).unwrap().keychain_cleanup,
                KeychainCleanup::Pending,
                "{:?}",
                step
            );
        }
        let reads = store.reads();
        let (s, v, _) = sign_in(&ctx, PASSWORD).await;
        assert_eq!(s, StatusCode::OK, "{:?}: {}", step, v);
        if committed {
            assert_eq!(
                store.reads(),
                reads,
                "{:?}: read again after the commit",
                step
            );
        } else {
            assert_eq!(store.reads(), reads + 2, "{:?}", step);
        }
        assert_eq!(store.peek(DATA_KEY), None, "{:?}", step);
        assert_eq!(store.peek(PEPPER), None, "{:?}", step);
        assert_eq!(
            vault(&ctx).unwrap().keychain_cleanup,
            KeychainCleanup::Done,
            "{:?}",
            step
        );
        assert_eq!(kite_key(&ctx), "kiteapikey", "{:?}", step);
        close(ctx).await;
    }
}

/// KEY-01: a keychain that is locked or refuses the one last read leaves
/// everything as it was and offers two ways out. Retrying once it allows
/// access moves the keys; starting fresh keeps the account, its settings
/// and history, drops only the encrypted values, and never touches the
/// keychain again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_keychain_offers_retry_and_start_fresh() {
    // Allow the keychain once and retry.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    keychain_install(dir.path(), &store);
    let ctx = open_with(dir.path(), store.clone());
    store.inner.set_unavailable(true);
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
    assert_eq!(v["code"], "keychain_unreadable");
    assert_eq!(v["recovery"], json!(["retry_keychain", "start_fresh"]));
    assert_eq!(v["message"], crate::error::KEYCHAIN_UNAVAILABLE_MESSAGE);
    assert!(vault(&ctx).is_none());
    assert!(!ctx.security.is_unlocked());
    store.inner.set_unavailable(false);
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(vault(&ctx).unwrap().source, Source::Keychain);
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;

    // Start fresh.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let old_api_key = keychain_install(dir.path(), &store);
    let ctx = open_with(dir.path(), store.clone());
    {
        let c = ctx.sqlite.conn().unwrap();
        c.execute("UPDATE api_keys SET order_mode = 'semi_auto'", [])
            .unwrap();
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', 'h', 'x', 'x')",
            [],
        )
        .unwrap();
    }
    let (mcp_row, mcp_token) = {
        let c = ctx.sqlite.conn().unwrap();
        crate::mcp::store::create_token(
            &c,
            "claude",
            crate::mcp::store::TokenScope::Read,
            ctx.now(),
        )
        .unwrap()
    };
    store.inner.set_unavailable(true);
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let calls = store.calls();
    AuthService::start_fresh_everywhere(&ctx, NEW_PASSWORD, NEW_PASSWORD)
        .await
        .unwrap();
    assert_eq!(store.calls(), calls, "starting fresh touched the keychain");
    let row = vault(&ctx).unwrap();
    assert_eq!(row.source, Source::Fresh);
    assert_eq!(row.keychain_cleanup, KeychainCleanup::Abandoned);
    assert!(!ctx.security.is_unlocked(), "the trader signs in next");
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "the old password still works");
    let (s, v, _) = sign_in(&ctx, NEW_PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        store.calls(),
        calls,
        "the new password's sign-in read the keychain"
    );
    // Kept: the account, its settings and history, AI client tokens.
    let c = ctx.sqlite.conn().unwrap();
    let account = user::find_first(&c).unwrap().unwrap();
    assert_eq!(
        (account.username.as_str(), account.email.as_deref()),
        (USER, Some(EMAIL))
    );
    assert_eq!(
        api_keys::get_order_mode(&c).unwrap().as_deref(),
        Some("semi_auto")
    );
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM sm_strategy", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        crate::mcp::store::find_token(&c, &mcp_token)
            .unwrap()
            .map(|r| r.id),
        Some(mcp_row.id)
    );
    // Dropped: broker keys (the client id stays), the old API key, TOTP.
    assert!(credentials::list_configured(&c).unwrap().is_empty());
    let creds = credentials::load(&c, &ctx.security, "zerodha")
        .unwrap()
        .unwrap();
    assert!(creds.api_key.is_empty());
    assert_eq!(creds.client_id.as_deref(), Some("AB1234"));
    assert!(!account.totp_enabled && !account.totp_required_for_login);
    drop(c);
    assert_eq!(ApiKeyService::check(&ctx, &old_api_key), KeyCheck::Invalid);
    let new_key = ApiKeyService::current(&ctx).unwrap().unwrap();
    assert_ne!(new_key.expose(), old_api_key);
    assert!(ApiKeyService::is_valid(&ctx, new_key.expose()));
    close(ctx).await;
}

/// KEY-01: the keychain holds no keys for this account (a folder from
/// another computer): nothing changes and the only way out offered is a
/// fresh start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_keychain_keys_offer_only_a_fresh_start() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    earlier_build_database(dir.path(), &Unlocked::random().unwrap(), PASSWORD);
    let ctx = open_with(dir.path(), store.clone());
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert_eq!(v["recovery"], json!(["start_fresh"]));
    // Another install's keys: they open nothing, so they are not moved.
    let (k, p) = crate::security::keychain_entries_for_test(&Unlocked::random().unwrap());
    store.seed(DATA_KEY, &k);
    store.seed(PEPPER, &p);
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert!(vault(&ctx).is_none());
    assert_eq!(
        store.peek(DATA_KEY),
        Some(k),
        "another install's entry deleted"
    );
    close(ctx).await;
}

// ------------------------------------------------------- the vault.json move

/// KEY-01: an earlier build's password-mode install moves `vault.json` into
/// openalgo.db at the first sign-in with the right password, then deletes
/// it; a wrong password leaves it. A half-done password change of that
/// build (`vault.next.json` for the password the stored hash names) is
/// finished the same way. No keychain call at any point.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_password_mode_vault_file_moves_into_the_database_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let keys = Unlocked::random().unwrap();
    let api_key = earlier_build_database(dir.path(), &keys, PASSWORD);
    let file = dir.path().join(crate::security::VAULT_FILE);
    crate::security::write_vault_file_for_test(&file, &keys, PASSWORD);
    let ctx = open_with(dir.path(), store.clone());
    let (s, _, _) = sign_in(&ctx, "Wrong@1234").await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(file.exists() && vault(&ctx).is_none());
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(vault(&ctx).unwrap().source, Source::VaultFile);
    assert!(!file.exists(), "vault.json left behind");
    assert_eq!(kite_key(&ctx), "kiteapikey");
    assert!(ApiKeyService::is_valid(&ctx, &api_key));
    close(ctx).await;
    let ctx = open_with(dir.path(), store.clone());
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(store.calls(), 0);
    close(ctx).await;

    // vault.json for the old password, vault.next.json for the new one,
    // and the stored hash already the new one's.
    let dir = tempfile::tempdir().unwrap();
    let keys = Unlocked::random().unwrap();
    earlier_build_database(dir.path(), &keys, NEW_PASSWORD);
    crate::security::write_vault_file_for_test(
        &dir.path().join(crate::security::VAULT_FILE),
        &keys,
        PASSWORD,
    );
    crate::security::write_vault_file_for_test(
        &dir.path().join(crate::security::VAULT_NEXT_FILE),
        &keys,
        NEW_PASSWORD,
    );
    let ctx = open_with(dir.path(), store.clone());
    let (s, _, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(
        s,
        StatusCode::UNAUTHORIZED,
        "the password the hash does not name"
    );
    let (s, v, _) = sign_in(&ctx, NEW_PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(!dir.path().join(crate::security::VAULT_FILE).exists());
    assert!(!dir.path().join(crate::security::VAULT_NEXT_FILE).exists());
    assert_eq!(kite_key(&ctx), "kiteapikey");
    close(ctx).await;
}

// ------------------------------------------------------------------ locked

/// KEY-01: before the first sign-in after a start, and after a logout,
/// every surface that needs the keys refuses in so many words and nothing
/// decrypts: `/api/v1` (the web's envelope, 503), MCP over HTTP, the strategy
/// and Chartink webhooks, the feed, and the broker resume. A sign-in opens
/// them; the logout closes them again and clears the caches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn locked_before_sign_in_and_after_logout_every_surface_refuses() {
    use crate::feed::auth::{AppAuth, AuthOutcome, FeedAuth};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_with(dir.path(), store.clone());
    set_up_and_unlock(&ctx);
    let api_key = ApiKeyService::current(&ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    let now = ctx.now();
    let webhook = crate::strategy::store::generate_webhook_token();
    let chartink = "11111111-1111-4111-8111-111111111111";
    let mcp_token = {
        let c = ctx.sqlite.conn().unwrap();
        c.execute(
            "INSERT INTO sm_strategy (user_id, name, universe_tab, underlying,
                 underlying_exchange, webhook_token_hash, created_at, updated_at)
             VALUES ('trader', 's1', 'index', 'NIFTY', 'NSE_INDEX', ?1, 'x', 'x')",
            [crate::strategy::store::hash_webhook_token(&webhook)],
        )
        .unwrap();
        c.execute(
            "INSERT INTO chartink_strategies (name, webhook_id) VALUES ('scan', ?1)",
            [chartink],
        )
        .unwrap();
        crate::mcp::store::create_token(&c, "claude", crate::mcp::store::TokenScope::Read, now)
            .unwrap()
            .1
    };
    BrokerAuthService::persist(
        &ctx,
        &BrokerSession {
            broker_id: "zerodha".into(),
            auth_token: "mock-access-token".into(),
            feed_token: None,
            user_id: "AB1234".into(),
            user_name: None,
            authenticated_at: now,
        },
    )
    .unwrap();
    close(ctx).await;

    let check_locked = |ctx: Arc<AppState>, when: &'static str| {
        let api_key = api_key.clone();
        let mcp_token = mcp_token.clone();
        let webhook = webhook.clone();
        async move {
            assert!(!ctx.security.is_unlocked(), "{}", when);
            let (s, v) = api_answer(&ctx, &api_key).await;
            assert!(locked_api_answer(s, &v), "{}: {} {}", when, s, v);
            let mut mcp = post_json(
                "/mcp",
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            );
            mcp.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {}", mcp_token).parse().unwrap(),
            );
            let (s, _, v) = send_to(&ctx, mcp).await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}: {}", when, v);
            assert_eq!(v["error"], "locked", "{}", when);
            let (s, _, v) = send_to(
                &ctx,
                post_json(
                    &format!("/strategy/webhook/{}", webhook),
                    json!({"action": "BUY"}),
                ),
            )
            .await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}: {}", when, v);
            assert_eq!(v["message"], crate::services::core::WEBHOOK_LOCKED);
            let (s, _, v) = send_to(
                &ctx,
                post_json(
                    &format!("/chartink/webhook/{}", chartink),
                    json!({"scan_name": "BUY"}),
                ),
            )
            .await;
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}: {}", when, v);
            assert_eq!(
                AppAuth::new(ctx.clone()).authenticate(&api_key).await,
                AuthOutcome::Locked,
                "{}",
                when
            );
            assert!(
                BrokerAuthService::try_resume(&ctx).await.unwrap().is_none(),
                "{}",
                when
            );
            assert!(ctx.get_broker_session().is_none(), "{}", when);
            let c = ctx.sqlite.conn().unwrap();
            let account = user::find_first(&c).unwrap().unwrap();
            assert!(
                matches!(
                    account.totp_secret(&ctx.security),
                    Err(crate::error::AppError::Locked)
                ),
                "{}: a stored secret decrypted",
                when
            );
            assert!(ctx.api_keys.is_empty(), "{}: a cache survived", when);
        }
    };

    let ctx = open_with(dir.path(), store.clone());
    check_locked(ctx.clone(), "after a start").await;
    let (s, v, cookie) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["redirect"], "/dashboard",
        "the broker session resumes: {}",
        v
    );
    let (s, v) = api_answer(&ctx, &api_key).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(!ctx.api_keys.is_empty());
    log_out(&ctx, &cookie.unwrap()).await;
    check_locked(ctx.clone(), "after a logout").await;
    assert_eq!(store.calls(), 0);
    close(ctx).await;
}

/// KEY-01: a sign-in still waiting for its authenticator code installs no
/// keys; the code, checked with the keys its password opened, installs them
/// with the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_waiting_for_its_code_unlocks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_with(dir.path(), store.clone());
    set_up_and_unlock(&ctx);
    let secret = {
        let c = ctx.sqlite.conn().unwrap();
        c.execute(
            "UPDATE users SET totp_enabled = 1, totp_required_for_login = 1",
            [],
        )
        .unwrap();
        user::find_first(&c)
            .unwrap()
            .unwrap()
            .totp_secret(&ctx.security)
            .unwrap()
            .unwrap()
    };
    let api_key = ApiKeyService::current(&ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    close(ctx).await;
    let ctx = open_with(dir.path(), store);
    let (s, v, cookie) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("totp_required"))
    );
    assert!(
        !ctx.security.is_unlocked(),
        "keys installed before the code"
    );
    let (s, v) = api_answer(&ctx, &api_key).await;
    assert!(locked_api_answer(s, &v));
    let code = totp::generate(secret.expose(), ctx.now().timestamp() as u64).unwrap();
    let cookie = cookie.unwrap();
    let csrf = ctx
        .sessions
        .get(cookie.trim_start_matches("session="), ctx.now())
        .unwrap()
        .csrf_token;
    let req = with_session(
        post_json("/auth/login/totp", json!({"totp_code": code})),
        &cookie,
        Some(&csrf),
    );
    let (s, _, v) = send_to(&ctx, req).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(ctx.security.is_unlocked());
    let (s, _) = api_answer(&ctx, &api_key).await;
    assert_eq!(s, StatusCode::OK);
    close(ctx).await;
}

// ------------------------------------------------------- password change

/// KEY-01: a password change writes the hash and the wrapped keys in one
/// transaction. A failure of the second write leaves both as they were
/// (the old password signs in, the new one does not, before and after a
/// restart); so does a crash with only the hash written (an uncommitted
/// transaction is rolled back by SQLite). A completed change moves both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_password_change_writes_hash_and_vault_together() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_with(dir.path(), store.clone());
    set_up_and_unlock(&ctx);
    let signs_in = |ctx: &AppState, p: &str| opened(ctx, p).is_ok();

    // The vault write fails after the hash was written.
    ctx.security.fail_rewrap(true);
    assert!(
        AuthService::change_password(&ctx, USER, PASSWORD, NEW_PASSWORD, NEW_PASSWORD).is_err()
    );
    ctx.security.fail_rewrap(false);
    assert!(signs_in(&ctx, PASSWORD) && !signs_in(&ctx, NEW_PASSWORD));

    // A crash with only the hash written: the transaction never commits.
    {
        let new_hash = ctx.security.hash_password(NEW_PASSWORD).unwrap();
        let raw = rusqlite::Connection::open(dir.path().join("openalgo.db")).unwrap();
        raw.execute_batch("BEGIN IMMEDIATE").unwrap();
        raw.execute("UPDATE users SET password_hash = ?1", [&new_hash])
            .unwrap();
        drop(raw); // the process dies here
    }
    close(ctx).await;
    let ctx = open_with(dir.path(), store.clone());
    assert!(signs_in(&ctx, PASSWORD), "the old password stopped working");
    assert!(!signs_in(&ctx, NEW_PASSWORD), "half a change took effect");

    // A completed change.
    let keys = opened(&ctx, PASSWORD).unwrap();
    ctx.security.install(&keys);
    AuthService::change_password(&ctx, USER, PASSWORD, NEW_PASSWORD, NEW_PASSWORD).unwrap();
    close(ctx).await;
    let ctx = open_with(dir.path(), store.clone());
    assert!(signs_in(&ctx, NEW_PASSWORD) && !signs_in(&ctx, PASSWORD));
    assert!(key_vault::unwrap(&vault(&ctx).unwrap(), NEW_PASSWORD)
        .unwrap()
        .is_some());
    assert_eq!(store.calls(), 0);
    close(ctx).await;
}

// --------------------------------------------------- forgotten password

/// KEY-01: a new password without the old one keeps the account, its
/// settings and history and drops only the values the old keys encrypted.
/// Nothing is left half decrypting: every encrypted value either opens
/// with the new keys or is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_password_without_the_old_one_drops_only_encrypted_values() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let ctx = open_with(dir.path(), store.clone());
    set_up_and_unlock(&ctx);
    let now = ctx.now();
    {
        let c = ctx.sqlite.conn().unwrap();
        credentials::save(
            &c,
            &ctx.security,
            "zerodha",
            credentials::CredentialUpdate {
                api_key: Some("kiteapikey".into()),
                api_secret: Some("kitesecret".into()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::messaging::telegram::db::update_bot_config(
            &c,
            &ctx.security,
            crate::messaging::telegram::db::ConfigUpdate {
                token: Some(Some("123:ABC".into())),
                ..Default::default()
            },
            now,
        )
        .unwrap();
        crate::messaging::telegram::db::create_or_update_user(
            &c,
            &ctx.security,
            &crate::messaging::telegram::db::Link {
                telegram_id: 42,
                username: USER,
                api_key: Some("linked-key"),
                host_url: None,
                first_name: "",
                last_name: "",
                telegram_username: "",
                broker: "zerodha",
            },
            now,
        )
        .unwrap();
        c.execute(
            "UPDATE users SET totp_enabled = 1, totp_required_for_login = 1",
            [],
        )
        .unwrap();
        c.execute("UPDATE api_keys SET order_mode = 'semi_auto'", [])
            .unwrap();
    }
    BrokerAuthService::persist(
        &ctx,
        &BrokerSession {
            broker_id: "zerodha".into(),
            auth_token: "mock-access-token".into(),
            feed_token: Some("feed".into()),
            user_id: "AB1234".into(),
            user_name: None,
            authenticated_at: now,
        },
    )
    .unwrap();
    let old_key = ApiKeyService::current(&ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    close(ctx).await;

    // Forgotten: locked, and the old password is not known.
    let ctx = open_with(dir.path(), store.clone());
    AuthService::start_fresh_everywhere(&ctx, NEW_PASSWORD, NEW_PASSWORD)
        .await
        .unwrap();
    assert!(!ctx.security.is_unlocked());
    let LoginOutcome::Success(_, keys) =
        AuthService::verify_credentials(&ctx, USER, NEW_PASSWORD, SignInFrom::AppWindow)
    else {
        panic!("TOTP is still required, or the new password was refused")
    };
    ctx.security.install(&keys);
    let c = ctx.sqlite.conn().unwrap();
    // Opens with the new keys.
    let account = user::find_first(&c).unwrap().unwrap();
    assert!(account.totp_secret(&ctx.security).unwrap().is_some());
    let new_key = api_keys::get_plaintext(&c, &ctx.security).unwrap().unwrap();
    assert_ne!(new_key.expose(), old_key);
    // Gone, and reading them is not an error.
    let creds = credentials::load(&c, &ctx.security, "zerodha")
        .unwrap()
        .unwrap();
    assert!(creds.api_key.is_empty() && creds.api_secret.is_none());
    assert!(crate::db::sqlite::auth::latest_active(&c, &ctx.security)
        .unwrap()
        .is_none());
    let bot = crate::messaging::telegram::db::get_bot_config(&c, &ctx.security).unwrap();
    assert!(bot.token.is_none() && !bot.is_active);
    let linked = crate::messaging::telegram::db::user_credentials(&c, &ctx.security, 42).unwrap();
    assert!(linked.map(|l| l.api_key.is_none()).unwrap_or(true));
    // Nothing still encrypted under the old keys anywhere.
    if let Some(w) = key_vault::witness(&c).unwrap() {
        assert!(ctx
            .security
            .decrypt(&w.ciphertext, &w.nonce, &w.aad)
            .is_ok());
    }
    // Kept.
    assert_eq!(account.email.as_deref(), Some(EMAIL));
    assert_eq!(
        api_keys::get_order_mode(&c).unwrap().as_deref(),
        Some("semi_auto")
    );
    let n: i64 = c
        .query_row("SELECT COUNT(*) FROM telegram_users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "the Telegram link stays; only its saved key goes");
    drop(c);
    assert!(matches!(
        AuthService::verify_credentials(&ctx, USER, PASSWORD, SignInFrom::AppWindow),
        LoginOutcome::Invalid
    ));
    assert_eq!(store.calls(), 0);
    close(ctx).await;
}

/// KEY-01: quitting wipes the keys as a logout does: the shutdown path
/// locks them, so nothing decrypts with what a quitting app held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quitting_wipes_the_keys() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_with(dir.path(), Arc::new(CountingKeyStore::default()));
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    let (s, v, _) = sign_in(&ctx, PASSWORD).await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(ctx.security.is_unlocked());
    // What quitting runs (`close` without its checks of what is left).
    ctx.shutdown().await;
    assert!(
        !ctx.security.is_unlocked(),
        "quitting left the keys in memory"
    );
    assert!(matches!(
        ctx.security.api_key_lookup("k"),
        Err(crate::error::AppError::Locked)
    ));
}

// ----------------------------------------------------------------- caches

/// KEY-01: a logout clears every cache the web's `invalidate_user_cache`
/// clears, and the keys.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_logout_clears_every_cache() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_with(dir.path(), Arc::new(CountingKeyStore::default()));
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    let (_, _, cookie) = sign_in(&ctx, PASSWORD).await;
    let key = ApiKeyService::current(&ctx)
        .unwrap()
        .unwrap()
        .expose()
        .to_string();
    assert!(ApiKeyService::is_valid(&ctx, &key));
    assert!(!ApiKeyService::is_valid(&ctx, "not-the-key"));
    ApiKeyService::order_mode(&ctx).unwrap();
    ctx.load_symbol_cache(vec![crate::brokers::common::symbols::tests::row(
        "SBIN", "SBIN-EQ", "NSE", "3045",
    )]);
    ctx.trading.facts.get(
        &ctx,
        "SBIN",
        "NSE",
        chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
    );
    assert!(!ctx.api_keys.is_empty() && !ctx.trading.facts.is_empty());
    log_out(&ctx, &cookie.unwrap()).await;
    assert!(
        ctx.api_keys.is_empty(),
        "API key or order mode cache survived"
    );
    assert!(ctx.trading.facts.is_empty(), "facts survived");
    assert_eq!(ctx.symbol_count(), 0, "the master survived");
    assert!(!ctx.security.is_unlocked());
    close(ctx).await;
}

// ------------------------------------------- what a sign-in gives away

/// Who sends a sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caller {
    /// The OpenAlgo Desktop window, with the proof the shell hands it.
    AppWindow,
    /// A browser tab or any program on this computer: no proof.
    Browser,
    /// A program on this computer with a made-up proof.
    WrongProof,
    /// A browser on another device of the network.
    AnotherDevice,
}

/// Every caller that is not the app window.
const OTHERS: [Caller; 3] = [Caller::Browser, Caller::WrongProof, Caller::AnotherDevice];

/// The request as a browser on another device of the network sends it from
/// the app's page.
fn from_another_device(mut r: Request<Body>) -> Request<Body> {
    r.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::new(
            "192.168.1.50".parse().unwrap(),
            50000,
        )));
    let port = if crate::config::dev_ports_enabled() {
        crate::config::DEV_HTTP_PORT
    } else {
        crate::config::DEFAULT_HTTP_PORT
    };
    r.headers_mut()
        .insert(header::HOST, format!("127.0.0.1:{}", port).parse().unwrap());
    in_app(r)
}

fn sent_by(ctx: &AppState, caller: Caller, r: Request<Body>) -> Request<Body> {
    match caller {
        Caller::AppWindow => from_the_app_window(ctx, r),
        Caller::Browser => r,
        Caller::WrongProof => {
            let mut r = r;
            r.headers_mut().insert(
                crate::security::window::HEADER,
                "0".repeat(64).parse().unwrap(),
            );
            r
        }
        Caller::AnotherDevice => from_another_device(r),
    }
}

/// A sign-in as `caller`. The limiter's clock moves on six minutes first,
/// past the longest wait and the per-address minute limit, so neither
/// answers instead of the sign-in itself; the failures stay counted (they
/// are forgotten only after an hour without attempts).
async fn sign_in_as(
    ctx: &Arc<AppState>,
    caller: Caller,
    name: &str,
    password: &str,
) -> (StatusCode, Value) {
    let now = ctx.limiter.now();
    ctx.limiter
        .freeze(Some(now + std::time::Duration::from_secs(6 * 60)));
    let (s, _, v) = send_to(
        ctx,
        sent_by(
            ctx,
            caller,
            multipart("/auth/login", &[("username", name), ("password", password)]),
        ),
    )
    .await;
    (s, v)
}

fn failures(ctx: &AppState, caller: Caller) -> u32 {
    use crate::server::middleware::{sign_in_source, Source};
    let source = match caller {
        Caller::AnotherDevice => Source::Lan("192.168.1.50".parse().unwrap()),
        _ => Source::Local,
    };
    ctx.limiter.backoff.failures(sign_in_source(source))
}

/// The answers `caller` gets to the right password, a wrong one and a name
/// that is not the account's, each of which must count against its budget.
async fn three_answers(ctx: &Arc<AppState>, caller: Caller) -> [(StatusCode, Value); 3] {
    let before = failures(ctx, caller);
    let answers = [
        sign_in_as(ctx, caller, USER, PASSWORD).await,
        sign_in_as(ctx, caller, USER, "Wrong@1234").await,
        sign_in_as(ctx, caller, "someone", PASSWORD).await,
    ];
    assert_eq!(
        failures(ctx, caller),
        before + 3,
        "{:?}: an attempt was not counted",
        caller
    );
    answers
}

fn invalid_credentials() -> (StatusCode, Value) {
    (
        StatusCode::UNAUTHORIZED,
        json!({"status": "error", "message": "Invalid credentials"}),
    )
}

/// KEY-01 security review: nobody but the OpenAlgo Desktop window can tell
/// a right password from a wrong one, or the account's name from another,
/// by how a sign-in fails: a browser or a program on this computer, one
/// with a made-up window proof, and another device each get one answer
/// for all three, and each attempt spends their budget. Checked while an
/// earlier build's keys still have to move (nothing reads the keychain or
/// moves them), with the vault in place when a proven password then fails
/// (its TOTP secret cannot be written), and with a damaged vault. The app
/// window still hears why: a proven password that failed is not "Invalid
/// credentials", and a damaged vault offers the fresh start whatever was
/// typed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nobody_but_the_app_window_can_tell_a_right_password_from_a_wrong_one() {
    // Keys still in the keychain of an earlier build.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    keychain_install(dir.path(), &store);
    let ctx = open_with(dir.path(), store.clone());
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    for caller in OTHERS {
        let [right, wrong, unknown] = three_answers(&ctx, caller).await;
        assert_eq!(
            right, wrong,
            "{:?}: a right password answered differently",
            caller
        );
        assert_eq!(
            right, unknown,
            "{:?}: the account's name answered differently",
            caller
        );
        assert_eq!(right.0, StatusCode::FORBIDDEN, "{:?}: {}", caller, right.1);
        assert_eq!(
            right.1["message"],
            crate::server::routes::auth::SIGN_IN_IN_APP_WINDOW_FIRST
        );
    }
    assert_eq!(
        store.calls(),
        0,
        "a caller other than the window reached the keychain"
    );
    assert!(vault(&ctx).is_none() && !ctx.security.is_unlocked());
    close(ctx).await;

    // The vault in place; the account's TOTP secret is missing and cannot
    // be written, so a proven password fails after its check.
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_with(dir.path(), store.clone());
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    ctx.sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "UPDATE users SET totp_secret_encrypted = NULL, totp_nonce = NULL;
             CREATE TRIGGER test_no_user_writes BEFORE UPDATE ON users
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    for caller in OTHERS {
        let [right, wrong, unknown] = three_answers(&ctx, caller).await;
        assert_eq!(right, invalid_credentials(), "{:?}: {}", caller, right.1);
        assert_eq!(wrong, right, "{:?}", caller);
        assert_eq!(unknown, right, "{:?}", caller);
    }
    assert_eq!(ctx.sessions.authenticated_count(), 0);
    // The app window sees that something is wrong once the password is
    // proven, and only then.
    let (s, v) = sign_in_as(&ctx, Caller::AppWindow, USER, PASSWORD).await;
    assert!(
        s != StatusCode::UNAUTHORIZED && s != StatusCode::OK,
        "{} {}",
        s,
        v
    );
    assert_eq!(
        sign_in_as(&ctx, Caller::AppWindow, USER, "Wrong@1234").await,
        invalid_credentials()
    );
    assert!(!ctx.security.is_unlocked());
    close(ctx).await;

    // A damaged vault: unreadable whatever is typed.
    let dir = tempfile::tempdir().unwrap();
    let ctx = open_with(dir.path(), store.clone());
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    AuthService::setup(&ctx, USER, EMAIL, PASSWORD).unwrap();
    ctx.sqlite
        .conn()
        .unwrap()
        .execute("UPDATE key_vault SET nonce = 'AAAA'", [])
        .unwrap();
    for caller in OTHERS {
        for answer in three_answers(&ctx, caller).await {
            assert_eq!(answer, invalid_credentials(), "{:?}", caller);
        }
    }
    let window = [
        sign_in_as(&ctx, Caller::AppWindow, USER, PASSWORD).await,
        sign_in_as(&ctx, Caller::AppWindow, USER, "Wrong@1234").await,
        sign_in_as(&ctx, Caller::AppWindow, "someone", PASSWORD).await,
    ];
    for answer in &window {
        assert_eq!(
            answer, &window[0],
            "the window's answer depends on what was typed"
        );
    }
    assert_eq!(window[0].0, StatusCode::CONFLICT, "{}", window[0].1);
    assert_eq!(window[0].1["recovery"], json!(["start_fresh"]));
    close(ctx).await;
}

/// KEY-01 security review: the ways out that follow a proven password
/// reach the app window only, and only for that password. An earlier
/// build's `vault.json` that the password opens and whose pepper the stored
/// hash verifies, but whose data key opens none of the data: the window
/// is offered a fresh start for the right password and told "Invalid
/// credentials" for a wrong one; every other caller hears the same for both
/// and nothing is moved or deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ways_out_need_the_password_and_the_app_window() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(CountingKeyStore::default());
    let keys = Unlocked::random().unwrap();
    earlier_build_database(dir.path(), &keys, PASSWORD);
    let file = dir.path().join(crate::security::VAULT_FILE);
    crate::security::write_vault_file_for_test(&file, &keys.with_another_data_key(), PASSWORD);
    let ctx = open_with(dir.path(), store.clone());
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    for caller in OTHERS {
        let [right, wrong, unknown] = three_answers(&ctx, caller).await;
        assert_eq!(right, wrong, "{:?}", caller);
        assert_eq!(right, unknown, "{:?}", caller);
        assert!(
            right.1.get("recovery").is_none(),
            "{:?}: {}",
            caller,
            right.1
        );
    }
    assert_eq!(
        sign_in_as(&ctx, Caller::AppWindow, USER, "Wrong@1234").await,
        invalid_credentials()
    );
    let (s, v) = sign_in_as(&ctx, Caller::AppWindow, USER, PASSWORD).await;
    assert_eq!(s, StatusCode::CONFLICT, "{}", v);
    assert_eq!(v["recovery"], json!(["start_fresh"]));
    assert!(file.exists() && vault(&ctx).is_none());
    assert_eq!(store.calls(), 0);
    close(ctx).await;
}

/// A keychain that fails the way an OS keychain can, saying more than the
/// trader should be shown: its item names and a file path.
#[derive(Default)]
struct OutspokenKeychain {
    inner: crate::security::keystore::MemoryKeyStore,
    refuse_reads: std::sync::atomic::AtomicBool,
    refuse_deletes: std::sync::atomic::AtomicBool,
}

const KEYCHAIN_SAID: &str =
    "/Users/trader/Library/Keychains/login.keychain-db: com.openalgo.desktop";

impl KeyStore for OutspokenKeychain {
    fn get(&self, name: &str) -> Result<Option<crate::security::Secret>, KeyStoreError> {
        if self.refuse_reads.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(KeyStoreError::Unavailable(format!(
                "{} {} denied",
                KEYCHAIN_SAID, name
            )));
        }
        self.inner.get(name)
    }

    fn delete(&self, name: &str) -> Result<(), KeyStoreError> {
        if self
            .refuse_deletes
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(KeyStoreError::Failure(format!(
                "{} {} is read-only",
                KEYCHAIN_SAID, name
            )));
        }
        self.inner.delete(name)
    }

    fn kind(&self) -> &'static str {
        "outspoken test keychain"
    }
}

/// KEY-01 security review: what the system keychain says when it refuses
/// (its item names, file paths, the system's own text) reaches neither a
/// sign-in's answer nor the error log the Diagnostics page serves over
/// HTTP; both carry OpenAlgo's own sentences. Checked for a refused read
/// and for a refused deletion after a move.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_error_log_never_carries_what_the_keychain_said() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(OutspokenKeychain::default());
    let keys = Unlocked::random().unwrap();
    earlier_build_database(dir.path(), &keys, PASSWORD);
    let (k, p) = crate::security::keychain_entries_for_test(&keys);
    store.inner.set(DATA_KEY, &k).unwrap();
    store.inner.set(PEPPER, &p).unwrap();
    let ctx = open_on(dir.path(), store.clone());

    store.refuse_reads.store(true, SeqCst);
    let (refused, mut kept) = crate::services::error_log::kept_while(|| {
        AuthService::verify_credentials(&ctx, USER, PASSWORD, SignInFrom::AppWindow)
    });
    assert!(
        matches!(
            refused,
            LoginOutcome::Unreadable(crate::error::AppError::KeychainUnavailable)
        ),
        "{:?}",
        refused
    );
    assert!(!kept.is_empty(), "the refusal was not logged at all");
    let (s, v) = sign_in_as(&ctx, Caller::AppWindow, USER, PASSWORD).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{}", v);
    kept.push(v.to_string());

    store.refuse_reads.store(false, SeqCst);
    store.refuse_deletes.store(true, SeqCst);
    let (moved, more) = crate::services::error_log::kept_while(|| {
        AuthService::verify_credentials(&ctx, USER, PASSWORD, SignInFrom::AppWindow)
    });
    assert!(moved.signed_in(), "{:?}", moved);
    assert_eq!(
        vault(&ctx).unwrap().keychain_cleanup,
        KeychainCleanup::Failed
    );
    kept.extend(more);

    for line in &kept {
        for word in [
            "/Users/",
            "login.keychain-db",
            "com.openalgo.desktop",
            DATA_KEY,
            PEPPER,
        ] {
            assert!(!line.contains(word), "{:?} reached: {}", word, line);
        }
    }
    close(ctx).await;
}
