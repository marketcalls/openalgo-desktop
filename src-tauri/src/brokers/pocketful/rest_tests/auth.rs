//! Sign-in (web `test/test_pocketful_auth.py`): the code exchange, the
//! trading account, every refusal as a sentence, and nothing secret in a
//! log. Answers are shaped as RFC 6749 and the OAuth server Pocketful runs
//! (the error texts are fosite's) return them.

use super::{capture, success, Fake, Reply};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::pocketful::auth::{self, *};
use crate::brokers::{Broker, BrokerCredentials};
use base64::Engine;
use serde_json::json;
use std::collections::HashMap;

const APP_ID: &str = "APPID7Vf";
const CODE: &str = "CODE_SENTINEL_5f1e";
const SECRET: &str = "SECRET_SENTINEL_9a2c";
const ACCESS: &str = "ACCESS_SENTINEL_77c0";
const REDIRECT: &str = "http://127.0.0.1:5000/pocketful/callback";
const SENTINELS: [&str; 3] = [CODE, SECRET, ACCESS];

const TOKEN: &str = "/oauth2/token";
const ACCOUNT: &str = "/api/v1/user/trading_info";

const GRANT_DESCRIPTION: &str = "The provided authorization grant (e.g., authorization code, resource owner credentials) or refresh token is invalid, expired, revoked, does not match the redirection URI used in the authorization request, or was issued to another client.";
const REDIRECT_HINT: &str =
    "The 'redirect_uri' from this request does not match the one from the authorize request.";
const CLIENT_DESCRIPTION: &str = "Client authentication failed (e.g., unknown client, no client authentication included, or unsupported authentication method).";

fn creds() -> BrokerCredentials {
    BrokerCredentials {
        api_key: APP_ID.into(),
        api_secret: Some(SECRET.into()),
        auth_code: Some(CODE.into()),
        redirect_uri: Some(REDIRECT.into()),
        ..Default::default()
    }
}

fn token_ok() -> Reply {
    Reply::Json(
        200,
        json!({"access_token": ACCESS, "token_type": "bearer", "expires_in": 3599, "scope": "orders holdings"}),
    )
}

fn account_ok() -> Reply {
    success(json!({"client_id": "YA0002"}))
}

async fn fake() -> Fake {
    let f = Fake::start().await;
    f.on(TOKEN, vec![token_ok()]);
    f.on(ACCOUNT, vec![account_ok()]);
    f
}

fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

/// Web ::test_the_code_is_exchanged_on_the_new_host_with_the_apps_keys and
/// ::test_the_account_is_read_with_the_new_access_token
#[tokio::test]
async fn the_code_is_exchanged_with_the_apps_keys_and_the_account_read_with_the_token() {
    let f = fake().await;
    let b = f.broker(SymbolResolver::new());

    b.authenticate(creds()).await.unwrap();

    let sent = f.to(TOKEN);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].method, "POST");
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", APP_ID, SECRET));
    assert_eq!(sent[0].header("authorization"), format!("Basic {}", basic));
    assert_eq!(
        sent[0].header("content-type"),
        "application/x-www-form-urlencoded"
    );
    let fields = form(&sent[0].body);
    assert_eq!(fields.len(), 3);
    assert_eq!(fields["grant_type"], "authorization_code");
    assert_eq!(fields["code"], CODE);
    assert_eq!(fields["redirect_uri"], REDIRECT);

    let read = f.to(ACCOUNT);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].method, "GET");
    assert_eq!(
        read[0].header("authorization"),
        format!("Bearer {}", ACCESS)
    );
    assert_eq!(read[0].header("x-authorization-token"), ACCESS);
}

/// Web ::test_the_token_carries_the_account_and_the_account_is_handed_back.
/// The desktop's session keeps the account beside the token (the web packs
/// them into one string because its session row has no account).
#[tokio::test]
async fn the_account_is_handed_back_with_the_session() {
    let f = fake().await;
    let r = f
        .broker(SymbolResolver::new())
        .authenticate(creds())
        .await
        .unwrap();
    assert_eq!(r.auth_token, ACCESS);
    assert_eq!(r.user_id, "YA0002");
    assert!(r.feed_token.is_none());
}

/// Web ::test_the_keys_and_redirect_are_read_at_each_login
#[tokio::test]
async fn each_login_sends_its_own_keys_and_redirect() {
    let f = fake().await;
    let b = f.broker(SymbolResolver::new());
    b.authenticate(creds()).await.unwrap();
    b.authenticate(BrokerCredentials {
        api_key: "OTHERAPP".into(),
        api_secret: Some("other-secret".into()),
        redirect_uri: Some("http://127.0.0.1:5500/pocketful/callback".into()),
        ..creds()
    })
    .await
    .unwrap();

    let sent = f.to(TOKEN);
    let other = base64::engine::general_purpose::STANDARD.encode("OTHERAPP:other-secret");
    assert_eq!(sent[1].header("authorization"), format!("Basic {}", other));
    assert_ne!(
        sent[0].header("authorization"),
        sent[1].header("authorization")
    );
    assert_eq!(
        form(&sent[1].body)["redirect_uri"],
        "http://127.0.0.1:5500/pocketful/callback"
    );
}

/// One refusal case: what is changed, the sentence, and how many requests
/// reach Pocketful (None where a lost answer may be read once more).
struct Case {
    name: &'static str,
    creds: fn() -> BrokerCredentials,
    token: Option<Reply>,
    account: Option<Reply>,
    message: &'static str,
    calls: Option<usize>,
}

fn refusals() -> Vec<Case> {
    let none = |name: &'static str, creds: fn() -> BrokerCredentials, message: &'static str| Case {
        name,
        creds,
        token: None,
        account: None,
        message,
        calls: Some(0),
    };
    let token = |name: &'static str, reply: Reply, message: &'static str| Case {
        name,
        creds,
        token: Some(reply),
        account: None,
        message,
        calls: Some(1),
    };
    let account = |name: &'static str, reply: Reply, message: &'static str| Case {
        name,
        creds,
        token: None,
        account: Some(reply),
        message,
        calls: Some(2),
    };
    vec![
        none(
            "no-code",
            || BrokerCredentials {
                auth_code: None,
                ..creds()
            },
            MISSING_CODE,
        ),
        none(
            "blank-code",
            || BrokerCredentials {
                auth_code: Some("   ".into()),
                ..creds()
            },
            MISSING_CODE,
        ),
        none(
            "no-app-id",
            || BrokerCredentials {
                api_key: String::new(),
                ..creds()
            },
            MISSING_KEYS,
        ),
        none(
            "no-secret",
            || BrokerCredentials {
                api_secret: None,
                ..creds()
            },
            MISSING_KEYS,
        ),
        token("token-unreachable", Reply::Cut, UNREACHABLE),
        token(
            "keys-refused",
            Reply::Json(
                401,
                json!({"error": "invalid_client", "error_description": CLIENT_DESCRIPTION}),
            ),
            KEYS_REFUSED,
        ),
        token(
            "redirect-mismatch",
            Reply::Json(
                400,
                json!({"error": "invalid_grant", "error_description": format!("{} {}", GRANT_DESCRIPTION, REDIRECT_HINT)}),
            ),
            REDIRECT_MISMATCH,
        ),
        token(
            "redirect-mismatch-legacy-hint",
            Reply::Json(
                400,
                json!({"error": "invalid_grant", "error_description": GRANT_DESCRIPTION, "error_hint": REDIRECT_HINT}),
            ),
            REDIRECT_MISMATCH,
        ),
        token(
            "code-expired",
            Reply::Json(
                400,
                json!({"error": "invalid_grant", "error_description": GRANT_DESCRIPTION}),
            ),
            CODE_REFUSED,
        ),
        token(
            "broker-down",
            Reply::Text(503, "<html>upstream unavailable</html>"),
            BROKER_UNAVAILABLE,
        ),
        token(
            "other-refusal",
            Reply::Json(
                400,
                json!({"error": "invalid_request", "error_description": "malformed"}),
            ),
            LOGIN_REFUSED,
        ),
        token(
            "no-access-token",
            Reply::Json(200, json!({"token_type": "bearer", "expires_in": 3599})),
            NO_SESSION,
        ),
        Case {
            calls: None,
            ..account("account-unreachable", Reply::Cut, UNREACHABLE)
        },
        account(
            "account-refused",
            Reply::Json(
                401,
                json!({"status": "error", "message": "Request Unauthorised", "error_code": 40000, "data": {}}),
            ),
            NO_ACCOUNT,
        ),
        account("account-missing", success(json!({})), NO_ACCOUNT),
        account(
            "account-breaks-the-token-layout",
            success(json!({"client_id": "YA:::0002"})),
            NO_ACCOUNT,
        ),
        account(
            "account-not-an-id",
            success(json!({"client_id": "../orders"})),
            NO_ACCOUNT,
        ),
        account(
            "account-not-json",
            Reply::Text(200, "<html>maintenance</html>"),
            NO_ACCOUNT,
        ),
    ]
}

/// Web ::test_every_failure_is_a_sentence_and_stores_nothing
#[tokio::test]
async fn every_refusal_is_a_sentence_and_logs_no_secret() {
    for case in refusals() {
        let f = fake().await;
        if let Some(r) = case.token {
            f.on(TOKEN, vec![r]);
        }
        if let Some(r) = case.account {
            f.on(ACCOUNT, vec![r]);
        }
        let b = f.broker(SymbolResolver::new());
        let (log, guard) = capture();
        let e = b.authenticate((case.creds)()).await.unwrap_err();
        drop(guard);

        assert_eq!(e.client_message(), case.message, "{}", case.name);
        if let Some(calls) = case.calls {
            assert_eq!(f.seen().len(), calls, "{}", case.name);
        }
        for word in [
            "OAuth",
            "HTTP",
            "status",
            "error_code",
            "invalid_",
            "Exception",
        ] {
            assert!(!case.message.contains(word), "{}: {}", case.name, word);
        }
        let shown = format!("{} {:?} {}", e, e, log.text());
        let leaked: Vec<_> = SENTINELS.iter().filter(|s| shown.contains(*s)).collect();
        assert!(leaked.is_empty(), "{}: {:?}", case.name, leaked);
    }
}

/// Web ::test_a_successful_login_logs_no_secret
#[tokio::test]
async fn a_successful_login_logs_no_secret() {
    let f = fake().await;
    let b = f.broker(SymbolResolver::new());
    let (log, guard) = capture();
    b.authenticate(creds()).await.unwrap();
    drop(guard);
    let text = log.text();
    assert!(!text.is_empty(), "the login logs that it completed");
    for s in SENTINELS {
        assert!(!text.contains(s), "{}", s);
    }
}

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Web ::test_a_sign_in_sent_back_with_an_error_reads_as_a_sentence and
/// ::test_an_answer_without_a_code_is_refused_in_plain_words
#[test]
fn a_callback_without_a_code_reads_as_a_sentence() {
    let (log, guard) = capture();
    let cases = [
        (
            params(&[
                ("error", "access_denied"),
                (
                    "error_description",
                    "The resource owner denied the request.",
                ),
                ("state", "st-1"),
            ]),
            Some(LOGIN_CANCELLED),
        ),
        (
            params(&[("error", "invalid_scope"), ("state", "st-1")]),
            Some(SCOPE_REFUSED),
        ),
        (
            params(&[("error", "server_error"), ("state", "st-1")]),
            Some(LOGIN_REFUSED),
        ),
        (params(&[("state", "st-1")]), Some(MISSING_CODE)),
        (
            params(&[("code", "  "), ("state", "st-1")]),
            Some(MISSING_CODE),
        ),
        (params(&[("code", "c9"), ("state", "st-1")]), None),
    ];
    for (query, want) in cases {
        assert_eq!(auth::callback_refusal(&query), want, "{:?}", query);
        assert_eq!(
            crate::brokers::catalog::callback_refusal("pocketful", &query),
            want
        );
    }
    drop(guard);
    // The broker's own words go to the log, for whoever reads it.
    assert!(log.text().contains("denied the request"));
    // Another broker's callback is not read this way.
    assert_eq!(
        crate::brokers::catalog::callback_refusal("zerodha", &params(&[("error", "x")])),
        None
    );
}

/// Web `_CLIENT_ID`: an account goes into request paths, so only the shape
/// of a client id is stored.
#[test]
fn only_a_client_id_shaped_account_is_kept() {
    for ok in ["DEMO1", "YA0002", "SB3179", "a_b-c"] {
        assert!(auth::is_client_id(ok), "{}", ok);
    }
    let long = "x".repeat(65);
    for bad in ["", "YA:::0002", "../orders", "a b", long.as_str()] {
        assert!(!auth::is_client_id(bad), "{}", bad);
    }
}
