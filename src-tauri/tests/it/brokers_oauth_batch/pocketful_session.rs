//! Pocketful's sign-in through the app's own sign-in service (web
//! `test/test_pocketful_auth.py`, the callback cases), and what a session
//! stored by 1.0.9 does after the update to the new host: it resumes when
//! Pocketful still takes its token, with the account it was stored with;
//! otherwise it is not resumed and the trader is sent to log in again.

use axum::body::Bytes;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use openalgo_desktop_lib::brokers::common::symbols::SymbolResolver;
use openalgo_desktop_lib::brokers::pocketful::{auth, PocketfulBroker};
use openalgo_desktop_lib::brokers::{Broker, BrokerRegistry};
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::db::sqlite::credentials::{self, CredentialUpdate};
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::security::Secret;
use openalgo_desktop_lib::services::broker_auth_service::{BrokerAuthService, CallbackOrigin};
use openalgo_desktop_lib::state::{AppState, BrokerSession, OpenOptions};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// A fake Pocketful: the code exchange, the account and the funds view (the
/// resume check). `funds_status` is what the funds view answers.
#[derive(Default)]
struct Pocketful {
    /// Path and query of every request.
    seen: Mutex<Vec<String>>,
    funds_status: Mutex<u16>,
}

fn answer(status: u16, body: serde_json::Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn serve(p: Arc<Pocketful>) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().fallback(move |method: Method, uri: Uri, _body: Bytes| {
        let p = p.clone();
        async move {
            p.seen.lock().push(uri.to_string());
            match (method.as_str(), uri.path()) {
                ("POST", "/oauth2/token") => answer(
                    200,
                    json!({"access_token": "acc-pf-9", "token_type": "bearer", "expires_in": 3599}),
                ),
                ("GET", "/api/v1/user/trading_info") => answer(
                    200,
                    json!({"status": "success", "message": "", "data": {"client_id": "YA0002"}}),
                ),
                ("GET", "/api/v2/funds/view") => {
                    let status = *p.funds_status.lock();
                    if status == 200 {
                        answer(
                            200,
                            json!({"status": "success", "message": "", "data": {"values": [["Available", "39.99"]]}}),
                        )
                    } else {
                        answer(
                            status,
                            json!({"status": "error", "message": "Request Unauthorised", "error_code": 40000, "data": {}}),
                        )
                    }
                }
                _ => answer(404, json!({"status": "error", "message": "not mocked"})),
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, task)
}

/// The app with Pocketful pointed at the fake and its keys saved.
async fn app(dir: &std::path::Path, base: &str) -> Arc<AppState> {
    let symbols = SymbolResolver::new();
    let broker = PocketfulBroker::with_base_url(
        symbols.clone(),
        base,
        "ws://127.0.0.1:9",
        format!("{}/contract", base),
    );
    let registry = BrokerRegistry::with_symbols(symbols, vec![Arc::new(broker) as Arc<dyn Broker>]);
    let ctx = AppState::open(
        dir,
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(registry),
        },
    )
    .unwrap();
    {
        let conn = ctx.sqlite.conn().unwrap();
        credentials::save(
            &conn,
            &ctx.security,
            "pocketful",
            CredentialUpdate {
                api_key: Some(Secret::new("APPID7Vf")),
                api_secret: Some(Secret::new("app-secret")),
                ..Default::default()
            },
        )
        .unwrap();
    }
    ctx
}

/// The `state` on a Pocketful sign-in address.
fn state_of(url: &str) -> String {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string()
}

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn callback(ctx: &AppState, query: HashMap<String, String>) -> Result<BrokerSession, String> {
    BrokerAuthService::complete_oauth(
        ctx,
        "pocketful",
        &query,
        CallbackOrigin::Redirect { session_id: None },
    )
    .await
    .map_err(|e| e.client_message())
}

/// Web ::test_an_answer_without_a_code_is_refused_in_plain_words and
/// ::test_an_answer_for_no_login_started_here_never_reaches_pocketful: a
/// sign-in Pocketful sent back with an error reads as the trader's
/// sentence once its state matched a sign-in started here, and a state
/// this app never issued is refused before anything else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_callback_without_a_code_says_why_after_its_state_matched() {
    let dir = tempfile::tempdir().unwrap();
    let pocketful = Arc::new(Pocketful::default());
    let (base, task) = serve(pocketful.clone()).await;
    let ctx = app(dir.path(), &base).await;

    let url = BrokerAuthService::start_oauth(&ctx, "pocketful", None)
        .await
        .unwrap();
    let declined = params(&[
        ("error", "access_denied"),
        (
            "error_description",
            "The resource owner denied the request.",
        ),
        ("state", &state_of(&url)),
    ]);
    assert_eq!(
        callback(&ctx, declined).await.unwrap_err(),
        auth::LOGIN_CANCELLED
    );

    let url = BrokerAuthService::start_oauth(&ctx, "pocketful", None)
        .await
        .unwrap();
    let blank = params(&[("state", &state_of(&url))]);
    assert_eq!(callback(&ctx, blank).await.unwrap_err(), auth::MISSING_CODE);

    let forged = params(&[("code", "c9"), ("state", "forged")]);
    let e = callback(&ctx, forged).await.unwrap_err();
    assert!(e.contains("was not started from OpenAlgo"), "{}", e);
    assert!(
        pocketful.seen.lock().is_empty(),
        "nothing reached Pocketful"
    );
    // The sign-in starts on the new host.
    assert!(
        url.starts_with("https://algo.pocketful.in/oauth2/auth?client_id=APPID7Vf&"),
        "{}",
        url
    );

    ctx.shutdown().await;
    task.abort();
}

/// The upgrade path: a stored Pocketful session is checked against the new
/// host with the account it was stored with. Pocketful taking the token
/// resumes it; refusing it, or a row without its account (as a damaged or
/// much older row could be), resumes nothing, so the sign-in page sends
/// the trader to Select Broker to log in again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_session_resumes_only_with_its_account_and_a_token_pocketful_takes() {
    let dir = tempfile::tempdir().unwrap();
    let pocketful = Arc::new(Pocketful::default());
    *pocketful.funds_status.lock() = 200;
    let (base, task) = serve(pocketful.clone()).await;
    let ctx = app(dir.path(), &base).await;

    // Sign in: the session carries the account Pocketful reported.
    let url = BrokerAuthService::start_oauth(&ctx, "pocketful", None)
        .await
        .unwrap();
    let session = callback(&ctx, params(&[("code", "c9"), ("state", &state_of(&url))]))
        .await
        .unwrap();
    assert_eq!(session.user_id, "YA0002");
    assert_eq!(session.auth_token.expose(), "acc-pf-9");

    // The app closes without logging out and comes back: the funds check
    // names the stored account, and the session resumes.
    ctx.runtime.teardown(&ctx).await;
    ctx.set_broker_session(None);
    pocketful.seen.lock().clear();
    let resumed = BrokerAuthService::try_resume(&ctx).await.unwrap();
    assert_eq!(resumed.map(|s| s.user_id).as_deref(), Some("YA0002"));
    let funds: Vec<String> = pocketful
        .seen
        .lock()
        .iter()
        .filter(|u| u.starts_with("/api/v2/funds/view"))
        .cloned()
        .collect();
    assert!(!funds.is_empty());
    assert!(
        funds.iter().all(|u| u.contains("client_id=YA0002")),
        "{:?}",
        funds
    );

    // The new host refuses the stored token: nothing is resumed.
    ctx.runtime.teardown(&ctx).await;
    ctx.set_broker_session(None);
    *pocketful.funds_status.lock() = 401;
    assert!(BrokerAuthService::try_resume(&ctx).await.unwrap().is_none());

    // A stored row without its account is not resumed either, and nothing
    // is sent with an empty account.
    *pocketful.funds_status.lock() = 200;
    BrokerAuthService::persist(
        &ctx,
        &BrokerSession {
            user_id: String::new(),
            ..session
        },
    )
    .unwrap();
    ctx.runtime.teardown(&ctx).await;
    ctx.set_broker_session(None);
    pocketful.seen.lock().clear();
    assert!(BrokerAuthService::try_resume(&ctx).await.unwrap().is_none());
    let seen = pocketful.seen.lock().clone();
    assert!(
        !seen.iter().any(|u| u.starts_with("/api/v2/funds/view")),
        "{:?}",
        seen
    );

    ctx.shutdown().await;
    task.abort();
}
