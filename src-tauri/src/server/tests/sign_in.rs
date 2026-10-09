//! Every catalogue broker's sign-in, end to end against a mock adapter,
//! started the way the broker page starts it: from the `sign_in` the server
//! reports for that broker (`catalog::sign_in`), never from a list kept in
//! the page.

use super::*;
use crate::brokers::catalog::{self, SignIn};
use crate::events::SessionEndReason;

const ACCOUNT: &str = "U1";

/// One mock per catalogue broker, each configured with keys that name the
/// account the way that broker's settings do: `U1:::appkey` (Dhan, the
/// Noren family), the Profile client id `U1` (Arrow, the HDFC pair,
/// AliceBlue), the mobile number as the key (Nubra).
fn every_broker() -> (TestCtx, Vec<Arc<MockBroker>>) {
    let mocks: Vec<Arc<MockBroker>> = catalog::ALL_BROKERS
        .iter()
        .map(|b| Arc::new(MockBroker::new(b)))
        .collect();
    for m in &mocks {
        *m.auth_user_id.lock() = ACCOUNT.into();
        // Only the brokers the page signs in from the saved keys accept a
        // sign-in with nothing typed.
        *m.saved_keys_sign_in.lock() = catalog::sign_in(m.id) == SignIn::SavedKeys;
    }
    // Dhan builds its consent address in `begin_login`.
    let dhan = catalog::ALL_BROKERS
        .iter()
        .position(|b| *b == "dhan")
        .unwrap();
    *mocks[dhan].login_url.lock() =
        Some("https://auth.dhan.co/login/consentApp-login?consentAppId=CA1".into());
    let t = build(
        BrokerRegistry::with(
            mocks
                .iter()
                .map(|m| m.clone() as Arc<dyn crate::brokers::Broker>)
                .collect(),
        ),
        ist(2026, 10, 5, 10, 0),
    );
    AuthService::setup(&t.ctx, USER, EMAIL, PASSWORD).unwrap();
    let conn = t.ctx.sqlite.conn().unwrap();
    for b in catalog::ALL_BROKERS {
        crate::db::sqlite::credentials::save(
            &conn,
            &t.ctx.security,
            b,
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some(if *b == "nubra" {
                    ACCOUNT.into()
                } else {
                    format!("{}:::appkey", ACCOUNT).into()
                }),
                api_secret: Some("appsecret".into()),
                client_id: catalog::CLIENT_ID_BROKERS
                    .contains(b)
                    .then(|| ACCOUNT.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    drop(conn);
    (t, mocks)
}

fn activate(ctx: &AppState, broker: &str) {
    let conn = ctx.sqlite.conn().unwrap();
    crate::config::save(
        &conn,
        &crate::config::ServerConfigUpdate {
            active_broker: Some(broker.into()),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    ctx.reload_config().unwrap();
}

/// A page navigation made from inside OpenAlgo.
fn navigate(path: &str, cookie: &str) -> Request<Body> {
    let mut r = with_session(get(path), cookie, None);
    r.headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    r
}

/// The broker's redirect back to `/<broker>/callback`, as the broker sends
/// it: the code under the broker's own parameter name, `state` only when
/// the broker returns it.
fn broker_redirect(broker: &str, authorize: &str, cookie: &str) -> Request<Body> {
    let mut q: Vec<(String, String)> = match broker {
        "zerodha" => vec![("request_token".into(), "c1".into())],
        "fyers" => vec![("auth_code".into(), "c1".into())],
        "dhan" => vec![
            ("tokenId".into(), "c1".into()),
            ("consentAppId".into(), "CA1".into()),
        ],
        "paytm" => vec![("requestToken".into(), "c1".into())],
        "arrow" | "hdfcsky" | "hdfcsecurities" => vec![("request-token".into(), "c1".into())],
        "iiflcapital" => vec![
            ("authCode".into(), "c1".into()),
            ("clientId".into(), ACCOUNT.into()),
        ],
        "aliceblue" => vec![
            ("authCode".into(), "c1".into()),
            ("userId".into(), ACCOUNT.into()),
        ],
        "compositedge" | "rmoney" => vec![],
        _ => vec![("code".into(), "c1".into())],
    };
    if catalog::callback_carries_state(broker) {
        q.push(("state".into(), state_on(authorize)));
    }
    if catalog::posts_callback(broker) {
        // XTS third-party login: `session` in a cross-site form POST,
        // `state` on the return address's query.
        let qs = serde_urlencoded::to_string(&q).unwrap();
        return Request::builder()
            .method(Method::POST)
            .uri(format!("/{}/callback?{}", broker, qs))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, cookie)
            .header("sec-fetch-site", "cross-site")
            .body(Body::from("session=%7B%22token%22%3A%22t1%22%7D"))
            .unwrap();
    }
    let qs = serde_urlencoded::to_string(&q).unwrap();
    let mut r = with_session(get(&format!("/{}/callback?{}", broker, qs)), cookie, None);
    r.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    r
}

/// What a trader types into the broker's in-app form (the page's generic
/// form when the catalogue lists no fields).
fn form_fields(broker: &str) -> Value {
    let fields = catalog::login_fields(broker);
    if fields.is_empty() {
        return json!({"userid": ACCOUNT, "password": "pin1", "totp": "123456"});
    }
    let mut o = serde_json::Map::new();
    for f in fields {
        let v = match f.name {
            "dob" => "01/01/1990",
            "userid" | "clientid" | "client_id" => ACCOUNT,
            _ => "123456",
        };
        o.insert(f.name.into(), json!(v));
    }
    Value::Object(o)
}

#[tokio::test]
async fn every_broker_signs_in_the_way_the_server_tells_the_broker_page() {
    let (t, mocks) = every_broker();
    let ctx = &t.ctx;
    let (cookie, csrf, _) = user_session(ctx);
    let base = std::time::Instant::now();
    assert_eq!(mocks.len(), catalog::ALL_BROKERS.len());
    let mut seen = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for (i, (broker, mock)) in catalog::ALL_BROKERS.iter().zip(&mocks).enumerate() {
        // A fresh sign-in attempt budget per broker: this drives 36.
        ctx.limiter.freeze(Some(
            base + std::time::Duration::from_secs(7_200 * i as u64),
        ));
        activate(ctx, broker);
        // The broker page reads the sign-in from the server.
        let (_, _, v) = send_to(
            ctx,
            with_session(get("/api/broker/configured"), &cookie, None),
        )
        .await;
        let entry = v["data"]["brokers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == *broker)
            .cloned()
            .unwrap_or_else(|| panic!("{} not listed: {}", broker, v));
        let kind = entry["sign_in"].clone();
        assert_eq!(kind, json!(catalog::sign_in(broker)), "{}", broker);
        let (_, _, cfg) =
            send_to(ctx, with_session(get("/auth/broker-config"), &cookie, None)).await;
        assert_eq!(cfg["sign_in"], kind, "{}", broker);
        match catalog::sign_in(broker) {
            SignIn::Redirect => {
                seen.entry("redirect").or_default().push(broker);
                // The callback opened with no answer from the broker (where
                // the page used to send four of these) starts the sign-in.
                let (_, h, _) =
                    send_to(ctx, navigate(&format!("/{}/callback", broker), &cookie)).await;
                assert_eq!(
                    location(&h),
                    format!("/{}/initiate-oauth", broker),
                    "{}",
                    broker
                );
                let (s, h, _) = send_to(
                    ctx,
                    navigate(&format!("/{}/initiate-oauth", broker), &cookie),
                )
                .await;
                assert_eq!(s, StatusCode::FOUND, "{}", broker);
                let authorize = location(&h);
                assert!(
                    authorize.starts_with("https://"),
                    "{}: {}",
                    broker,
                    authorize
                );
                let (_, h, v) = send_to(ctx, broker_redirect(broker, &authorize, &cookie)).await;
                assert_eq!(location(&h), "/dashboard", "{}: {}", broker, v);
                let a = mock.last_auth.lock().clone().unwrap();
                assert!(a.request_token.is_some(), "{}", broker);
            }
            SignIn::SavedKeys => {
                seen.entry("saved_keys").or_default().push(broker);
                // No form to fill: opening the callback or the sign-in start
                // goes back to the broker page, and signs in nothing.
                for path in [
                    format!("/{}/callback", broker),
                    format!("/{}/initiate-oauth", broker),
                ] {
                    let (_, h, _) = send_to(ctx, navigate(&path, &cookie)).await;
                    assert_eq!(location(&h), "/broker", "{} {}", broker, path);
                }
                assert!(mock.last_auth.lock().is_none(), "{}", broker);
                // The page's one action: the CSRF-checked POST, as the page
                // sends it (multipart, the token and no login fields).
                let (s, _, v) = send_to(
                    ctx,
                    with_session(
                        multipart(
                            &format!("/{}/callback", broker),
                            &[("csrf_token", csrf.as_str())],
                        ),
                        &cookie,
                        Some(&csrf),
                    ),
                )
                .await;
                assert_eq!(s, StatusCode::OK, "{}: {}", broker, v);
                assert_eq!(v["redirect"], "/dashboard", "{}", broker);
                let a = mock.last_auth.lock().clone().unwrap();
                assert_eq!(a.api_secret.as_deref(), Some("appsecret"), "{}", broker);
                assert!(
                    a.password.is_none() && a.totp.is_none() && a.request_token.is_none(),
                    "{}",
                    broker
                );
            }
            SignIn::Form => {
                seen.entry("form").or_default().push(broker);
                let (_, h, _) =
                    send_to(ctx, navigate(&format!("/{}/callback", broker), &cookie)).await;
                let page = if *broker == "samco" {
                    "/broker/samco/auth".to_string()
                } else {
                    format!("/broker/{}/totp", broker)
                };
                assert_eq!(location(&h), page, "{}", broker);
                let (s, _, v) = send_to(
                    ctx,
                    with_session(
                        post_json(&format!("/{}/callback", broker), form_fields(broker)),
                        &cookie,
                        Some(&csrf),
                    ),
                )
                .await;
                assert_eq!(s, StatusCode::OK, "{}: {}", broker, v);
                assert_eq!(v["redirect"], "/dashboard", "{}", broker);
            }
        }
        let session = ctx
            .get_broker_session()
            .unwrap_or_else(|| panic!("{} did not sign in", broker));
        assert_eq!(session.broker_id, *broker);
        assert_eq!(session.user_id, ACCOUNT, "{}", broker);
        BrokerAuthService::revoke(ctx, SessionEndReason::Logout)
            .await
            .unwrap();
        assert!(ctx.get_broker_session().is_none(), "{}", broker);
    }
    // The four whose Connect the release review found broken start at the
    // broker.
    for b in ["shoonya", "zebu", "tradesmart", "rmoney"] {
        assert!(seen["redirect"].contains(&b), "{}", b);
    }
    assert_eq!(
        seen["saved_keys"],
        [
            "deltaexchange",
            "dhan_sandbox",
            "fivepaisaxts",
            "ibulls",
            "iifl",
            "jainamxts",
            "wisdom"
        ]
    );
    // State-less redirects and client-id brokers are among those covered.
    for b in catalog::ALL_BROKERS
        .iter()
        .filter(|b| !catalog::callback_carries_state(b))
        .chain(catalog::CLIENT_ID_BROKERS)
    {
        assert!(seen["redirect"].contains(b), "{}", b);
    }
    assert_eq!(
        seen.values().map(Vec::len).sum::<usize>(),
        catalog::ALL_BROKERS.len()
    );
    ctx.runtime.teardown(ctx).await;
}

/// The broker page's Connect for a redirect broker, as it was before the
/// page read the sign-in from the server: a bare `GET /<broker>/callback`.
/// It used to be refused ("not started from OpenAlgo"); it now starts the
/// sign-in. Without a signed-in trader it still starts nothing.
#[tokio::test]
async fn a_bare_callback_starts_a_redirect_sign_in_only_for_the_signed_in_trader() {
    let (t, mocks) = every_broker();
    let ctx = &t.ctx;
    ctx.limiter.freeze(Some(std::time::Instant::now()));
    let (cookie, _, _) = user_session(ctx);
    for b in ["shoonya", "zebu", "tradesmart", "rmoney"] {
        activate(ctx, b);
        let (_, h, _) = send_to(ctx, navigate(&format!("/{}/callback", b), &cookie)).await;
        assert_eq!(location(&h), format!("/{}/initiate-oauth", b), "{}", b);
        let (_, h, _) = send_to(ctx, get(&format!("/{}/callback", b))).await;
        assert!(location(&h).starts_with("/broker?error="), "{}", b);
    }
    assert!(mocks.iter().all(|m| m.last_auth.lock().is_none()));
    assert!(ctx.get_broker_session().is_none());
    ctx.runtime.teardown(ctx).await;
}
