//! Broker Configuration and Select Broker: one key set per broker, for as
//! many brokers as the trader likes, and one broker logged in at a time
//! (`routes/broker_config.rs`, `services/broker_config_service.rs`).

use super::*;
use crate::brokers::catalog;
use crate::db::sqlite::credentials::{self, CredentialUpdate};
use crate::services::broker_auth_service::FormLogin;
use crate::services::broker_config_service::BrokerConfigService;

const LONG_KEY: &str = "KEYSENTINELq7Zx0123456789";
const LONG_SECRET: &str = "SECRETSENTINELw4Rv9876543210";
const SHORT_SECRET: &str = "S3cr3tQ7";

fn put_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::PUT)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn delete(path: &str) -> Request<Body> {
    Request::builder()
        .method(Method::DELETE)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

fn signed_in_harness() -> (H, String, String) {
    let h = H::new();
    h.setup();
    let (cookie, csrf) = h.session(true);
    (h, cookie, csrf)
}

async fn list(h: &H, cookie: &str) -> Value {
    let (s, headers, body) = h
        .send(with_session(get("/api/broker/list"), cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap(),
        "no-store, max-age=0",
        "masked keys are never cached"
    );
    serde_json::from_slice::<Value>(&body).unwrap()["data"].clone()
}

fn entry<'a>(data: &'a Value, broker: &str) -> &'a Value {
    data["brokers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == broker)
        .unwrap()
}

fn field<'a>(b: &'a Value, key: &str) -> &'a Value {
    b["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == key)
        .unwrap()
}

fn stored(h: &H, broker: &str) -> Option<credentials::BrokerCredentialSet> {
    let conn = h.ctx().sqlite.conn().unwrap();
    credentials::load(&conn, &h.ctx().security, broker).unwrap()
}

async fn save(h: &H, cookie: &str, csrf: &str, broker: &str, body: Value) -> (StatusCode, Value) {
    h.json(with_session(
        put_json(&format!("/api/broker/credentials/{}", broker), body),
        cookie,
        Some(csrf),
    ))
    .await
}

#[tokio::test]
async fn every_route_needs_the_signed_in_user() {
    let h = H::new();
    h.setup();
    let (anon, anon_csrf) = h.session(false);
    for req in [
        get("/api/broker/list"),
        put_json("/api/broker/credentials/zerodha", json!({"api_key": "k"})),
        delete("/api/broker/credentials/zerodha"),
        put_json("/api/broker/rate-plan/fyers", json!({"plan": "prime"})),
    ] {
        let (s, _) = h.json(with_session(req, &anon, Some(&anon_csrf))).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    assert!(stored(&h, "zerodha").is_none());
}

#[tokio::test]
async fn writes_need_the_csrf_token() {
    let (h, cookie, _csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    for req in [
        put_json(
            "/api/broker/credentials/zerodha",
            json!({"api_key": "other"}),
        ),
        delete("/api/broker/credentials/zerodha?sign_out=true"),
    ] {
        let (s, _) = h.json(with_session(req, &cookie, None)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "refused without the CSRF token");
    }
    assert_eq!(
        stored(&h, "zerodha").unwrap().api_key.expose(),
        "kiteapikey"
    );
    assert!(h.ctx().is_broker_connected(), "nothing ended");
}

#[tokio::test]
async fn lists_every_broker_with_its_sign_in_and_needs() {
    let (h, cookie, _) = signed_in_harness();
    let data = list(&h, &cookie).await;
    let brokers = data["brokers"].as_array().unwrap();
    assert_eq!(brokers.len(), catalog::ALL_BROKERS.len());
    assert_eq!(data["logged_in"], Value::Null);
    for b in brokers {
        let id = b["id"].as_str().unwrap();
        assert_eq!(b["sign_in"], json!(catalog::sign_in(id)), "{id}");
        assert_eq!(b["configured"], false, "{id}");
        assert_eq!(b["active"], false, "{id}");
        assert!(!b["banner"].as_str().unwrap().is_empty(), "{id}");
        // The redirect URL only for redirect brokers, automatic until set.
        if catalog::sign_in(id) == catalog::SignIn::Redirect {
            assert_eq!(b["redirect_url"], b["redirect_url_automatic"], "{id}");
            assert_eq!(b["redirect_url_is_default"], true, "{id}");
            assert!(b["redirect_url"]
                .as_str()
                .unwrap()
                .ends_with(&format!("/{}/callback", id)));
        } else {
            assert_eq!(b["redirect_url"], Value::Null, "{id}");
        }
    }
    let zerodha = entry(&data, "zerodha");
    assert_eq!(zerodha["name"], "Zerodha");
    assert_eq!(field(zerodha, "api_key")["saved"], Value::Null);
    // The mock adapter serves the supported exchanges.
    assert!(zerodha["exchanges"]
        .as_array()
        .unwrap()
        .contains(&json!("NSE")));
}

/// Masked display only: no saved key or secret ever comes back in full,
/// and at most six of its characters do.
#[tokio::test]
async fn masks_every_saved_key_and_never_returns_one_in_full() {
    let (h, cookie, csrf) = signed_in_harness();
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "fivepaisaxts",
        json!({
            "api_key": LONG_KEY,
            "api_secret": SHORT_SECRET,
            "api_key_market": LONG_SECRET,
            "api_secret_market": LONG_SECRET,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "arrow",
        json!({"api_key": LONG_KEY, "api_secret": LONG_SECRET, "client_id": "AB1234"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let (_, _, raw) = h
        .send(with_session(get("/api/broker/list"), &cookie, None))
        .await;
    let text = String::from_utf8(raw).unwrap();
    for secret in [LONG_KEY, LONG_SECRET, SHORT_SECRET] {
        assert!(!text.contains(secret), "{secret} returned in full");
        // Nor any seven characters of it in a row.
        for w in secret.as_bytes().windows(7) {
            assert!(!text.contains(std::str::from_utf8(w).unwrap()));
        }
    }
    let data = list(&h, &cookie).await;
    let xts = entry(&data, "fivepaisaxts");
    assert_eq!(xts["configured"], true);
    assert_eq!(field(xts, "api_key")["saved"], "KEY***789");
    assert_eq!(field(xts, "api_secret")["saved"], "***");
    assert_eq!(field(xts, "api_secret_market")["saved"], "SEC***210");
    // The client id is an account id, shown as saved.
    assert_eq!(field(entry(&data, "arrow"), "client_id")["saved"], "AB1234");
}

/// A key left empty keeps its saved value; only what is typed replaces.
#[tokio::test]
async fn a_blank_key_keeps_the_saved_value() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "zerodha",
        json!({"api_key": "", "api_secret": "newsecret"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = stored(&h, "zerodha").unwrap();
    assert_eq!(c.api_key.expose(), "kiteapikey", "kept");
    assert_eq!(c.api_secret.unwrap().expose(), "newsecret", "replaced");

    // Nothing typed is nothing to save.
    let (s, _) = save(&h, &cookie, &csrf, "zerodha", json!({"api_key": " "})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        stored(&h, "zerodha").unwrap().api_key.expose(),
        "kiteapikey"
    );
}

#[tokio::test]
async fn a_first_save_needs_every_required_key_in_its_format() {
    let (h, cookie, csrf) = signed_in_harness();
    let (s, v) = save(&h, &cookie, &csrf, "zerodha", json!({"api_key": "k1"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Enter the API Secret to save Zerodha.");
    assert!(stored(&h, "zerodha").is_none());
    // The web's `:::` formats are checked on this path too.
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "dhan",
        json!({"api_key": "noseparator", "api_secret": "s"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v["message"]
        .as_str()
        .unwrap()
        .contains("client_id:::api_key"));
    // A broker that needs no secret saves with its key alone.
    let (s, v) = save(&h, &cookie, &csrf, "angel", json!({"api_key": "smartkey"})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(stored(&h, "angel").unwrap().api_secret.is_none());
    let (s, _) = save(&h, &cookie, &csrf, "nosuchbroker", json!({"api_key": "k"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// CFG-01's rule for this page: an empty redirect URL means the automatic
/// one; a set one is a full address ending in `/<broker>/callback`.
#[tokio::test]
async fn an_empty_redirect_url_means_automatic() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    let custom = "https://abc.ngrok.app/zerodha/callback";
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "zerodha",
        json!({"redirect_url": custom}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let data = list(&h, &cookie).await;
    let z = entry(&data, "zerodha");
    assert_eq!(z["redirect_url"], custom);
    assert_eq!(z["redirect_url_is_default"], false);
    assert_eq!(
        BrokerConfigService::redirect_url(h.ctx(), "zerodha").unwrap(),
        custom,
        "a sign-in uses it"
    );
    for bad in [
        "ftp://abc.ngrok.app/zerodha/callback",
        "https://abc.ngrok.app/fyers/callback",
        "not a url/zerodha/callback",
        // Read one way here and another by the broker (CFG-02).
        "https://evil.example\\@127.0.0.1:5000/zerodha/callback",
        "https://u@evil.example/zerodha/callback",
        "https://127.0.0.1:5000/zerodha/callback?x=1/zerodha/callback",
        "https://abc.ngrok.app/zerodha/callback#x",
        "https://abc .ngrok.app/zerodha/callback",
    ] {
        let (s, _) = save(&h, &cookie, &csrf, "zerodha", json!({"redirect_url": bad})).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    // A broker that signs in without a redirect has no redirect URL.
    let (s, _) = save(
        &h,
        &cookie,
        &csrf,
        "angel",
        json!({"api_key": "k", "redirect_url": "http://127.0.0.1:5000/angel/callback"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = save(&h, &cookie, &csrf, "zerodha", json!({"redirect_url": ""})).await;
    assert_eq!(s, StatusCode::OK);
    let data = list(&h, &cookie).await;
    let z = entry(&data, "zerodha");
    assert_eq!(z["redirect_url_is_default"], true);
    assert_eq!(z["redirect_url"], z["redirect_url_automatic"]);
    assert_eq!(stored(&h, "zerodha").unwrap().redirect_url, None);
}

#[tokio::test]
async fn forget_clears_only_that_brokers_keys() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    let (s, _) = save(
        &h,
        &cookie,
        &csrf,
        "fyers",
        json!({"api_key": "APP-100", "api_secret": "fsecret"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = h
        .json(with_session(
            delete("/api/broker/credentials/fyers"),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out"], false);
    assert!(stored(&h, "fyers").is_none());
    assert!(
        stored(&h, "zerodha").is_some(),
        "another broker's keys stay"
    );
    let data = list(&h, &cookie).await;
    assert_eq!(entry(&data, "fyers")["configured"], false);
    assert_eq!(entry(&data, "zerodha")["configured"], true);
}

/// Forgetting the broker that is logged in asks first (409 without the
/// confirmation), then signs out of it: session, streaming and stored row.
#[tokio::test]
async fn forgetting_the_logged_in_broker_signs_out_of_it() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    let (s, v) = h
        .json(with_session(
            delete("/api/broker/credentials/zerodha"),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(h.ctx().is_broker_connected(), "nothing changed");
    assert!(stored(&h, "zerodha").is_some());

    let (s, v) = h
        .json(with_session(
            delete("/api/broker/credentials/zerodha?sign_out=true"),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out"], true);
    assert!(!h.ctx().is_broker_connected(), "the session ended");
    assert!(stored(&h, "zerodha").is_none());
    let conn = h.ctx().sqlite.conn().unwrap();
    assert!(
        crate::db::sqlite::auth::latest_active(&conn, &h.ctx().security)
            .unwrap()
            .is_none(),
        "no stored session to resume"
    );
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM auth WHERE broker_id = 'zerodha'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    drop(conn);
    assert_eq!(h.ctx().server_config().active_broker, None);
    // OpenAlgo itself stays signed in.
    let _ = list(&h, &cookie).await;
}

/// There is no explicit end of the session for a switch: the old session
/// ends only when the new sign-in completes, so one that is refused or
/// abandoned leaves the logged-in broker connected.
#[tokio::test]
async fn a_failed_or_abandoned_sign_in_to_another_broker_keeps_the_logged_in_one() {
    let (t, _angel, samco) = two_brokers();
    let ctx = t.ctx.clone();
    BrokerAuthService::login_with_form(&ctx, "angel", totp())
        .await
        .unwrap();
    // Refused by the broker (the mock refuses a sign-in with nothing typed).
    assert!(
        BrokerAuthService::login_with_form(&ctx, "samco", FormLogin::default())
            .await
            .is_err()
    );
    assert_eq!(*samco.logouts.lock(), 0);
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "angel");
    assert_eq!(ctx.runtime.active_broker().as_deref(), Some("angel"));
    assert_eq!(live_rows(&ctx), vec!["angel".to_string()]);
    // Started and never finished (the trader closed the broker's page).
    let _ = BrokerAuthService::start_oauth(&ctx, "samco", None).await;
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "angel");
    assert_eq!(live_rows(&ctx), vec!["angel".to_string()]);
    ctx.runtime.teardown(&ctx).await;
}

/// FLOW-04: a session belongs to the keys it signed in with. New keys for
/// the logged-in broker end its session, only once the trader confirms; a
/// redirect URL alone changes nothing live.
#[tokio::test]
async fn flow04_new_keys_for_the_logged_in_broker_end_its_session_after_confirming() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "zerodha",
        json!({"api_secret": "rotated"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(h.ctx().is_broker_connected(), "nothing ended");
    assert_eq!(
        stored(&h, "zerodha").unwrap().api_secret.unwrap().expose(),
        "kitesecret",
        "nothing saved"
    );

    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "zerodha",
        json!({"redirect_url": "https://abc.ngrok.app/zerodha/callback"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out"], false);
    assert!(
        h.ctx().is_broker_connected(),
        "a redirect URL alone ends nothing"
    );

    let (s, v) = h
        .json(with_session(
            put_json(
                "/api/broker/credentials/zerodha?sign_out=true",
                json!({"api_secret": "rotated"}),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out"], true);
    assert!(
        !h.ctx().is_broker_connected(),
        "the old keys' session ended"
    );
    assert_eq!(
        stored(&h, "zerodha").unwrap().api_secret.unwrap().expose(),
        "rotated"
    );
}

/// FLOW-04 on the web-shaped save too: new keys for the logged-in broker
/// end its session.
#[tokio::test]
async fn flow04_the_web_shaped_save_ends_the_session_of_rotated_keys() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    let (s, v) = h
        .json(with_session(
            post_json(
                "/api/broker/credentials",
                json!({"broker_api_secret": "rotated"}),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["signed_out_of"], "zerodha");
    assert!(!h.ctx().is_broker_connected());
}

/// FLOW-03: a save that names no broker (the tunnel settings) never writes
/// the active broker, so a snapshot older than another save that switched
/// cannot put the old broker back.
#[tokio::test]
async fn flow03_a_save_naming_no_broker_never_writes_a_stale_active_broker() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    // Another save switched to Fyers and committed; this process's settings
    // snapshot still says Zerodha.
    let conn = h.ctx().sqlite.conn().unwrap();
    crate::config::save(
        &conn,
        &crate::config::ServerConfigUpdate {
            active_broker: Some("fyers".into()),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        h.ctx().server_config().active_broker.as_deref(),
        Some("zerodha")
    );
    let (s, v) = h
        .json(with_session(
            post_json("/api/broker/credentials", json!({"websocket_url": ""})),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["broker_switched"], false);
    let conn = h.ctx().sqlite.conn().unwrap();
    assert_eq!(
        crate::config::ServerConfig::load(&conn)
            .unwrap()
            .active_broker
            .as_deref(),
        Some("fyers"),
        "the newer switch stands"
    );
}

/// FLOW-03: two saves at once, a switch and a tunnel-only save, always end
/// on the broker the switch named, with the old broker's session ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flow03_concurrent_saves_keep_the_switch_and_end_the_old_session() {
    for _ in 0..12 {
        let (h, cookie, csrf) = signed_in_harness();
        h.save_broker_credentials();
        h.connect_broker();
        let send = |body: Value| {
            let app = crate::server::app(h.ctx().clone());
            let req = with_session(
                post_json("/api/broker/credentials", body),
                &cookie,
                Some(&csrf),
            );
            tokio::spawn(async move { app.oneshot(req).await.unwrap().status() })
        };
        let switch = send(json!({"broker": "fyers"}));
        let tunnel = send(json!({"websocket_url": "", "ngrok_allow": "FALSE"}));
        assert_eq!(switch.await.unwrap(), StatusCode::OK);
        assert_eq!(tunnel.await.unwrap(), StatusCode::OK);
        let conn = h.ctx().sqlite.conn().unwrap();
        assert_eq!(
            crate::config::ServerConfig::load(&conn)
                .unwrap()
                .active_broker
                .as_deref(),
            Some("fyers")
        );
        drop(conn);
        assert_eq!(
            h.ctx().server_config().active_broker.as_deref(),
            Some("fyers")
        );
        assert!(!h.ctx().is_broker_connected(), "Zerodha's session ended");
    }
}

// ------------------------------------------------- one broker at a time

/// Two form brokers, both with saved keys, each signing in to its own
/// account through the mock.
fn two_brokers() -> (TestCtx, Arc<MockBroker>, Arc<MockBroker>) {
    let angel = Arc::new(MockBroker::new("angel"));
    let samco = Arc::new(MockBroker::new("samco"));
    *angel.auth_user_id.lock() = "A1".into();
    *samco.auth_user_id.lock() = "S1".into();
    let t = build(
        BrokerRegistry::with(vec![
            angel.clone() as Arc<dyn crate::brokers::Broker>,
            samco.clone() as Arc<dyn crate::brokers::Broker>,
        ]),
        ist(2026, 10, 5, 10, 0),
    );
    AuthService::setup(&t.ctx, USER, EMAIL, PASSWORD).unwrap();
    let conn = t.ctx.sqlite.conn().unwrap();
    for b in ["angel", "samco"] {
        credentials::save(
            &conn,
            &t.ctx.security,
            b,
            CredentialUpdate {
                api_key: Some(format!("{}key", b).into()),
                api_secret: Some("secret".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    drop(conn);
    (t, angel, samco)
}

fn totp() -> FormLogin {
    FormLogin {
        totp: Some("123456".into()),
        ..Default::default()
    }
}

/// The stored sessions not yet revoked, by broker.
fn live_rows(ctx: &AppState) -> Vec<String> {
    let conn = ctx.sqlite.conn().unwrap();
    let mut stmt = conn
        .prepare("SELECT broker_id FROM auth WHERE is_revoked = 0 ORDER BY broker_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows
}

/// Logging in to Y while X is logged in ends X first: its session, its
/// streaming and its stored row. X's keys stay saved.
#[tokio::test]
async fn a_sign_in_ends_any_other_brokers_session_first() {
    let (t, angel, _samco) = two_brokers();
    let ctx = t.ctx.clone();
    BrokerAuthService::login_with_form(&ctx, "angel", totp())
        .await
        .unwrap();
    assert_eq!(ctx.runtime.active_broker().as_deref(), Some("angel"));
    BrokerAuthService::login_with_form(&ctx, "samco", totp())
        .await
        .unwrap();
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "samco");
    assert_eq!(
        live_rows(&ctx),
        vec!["samco".to_string()],
        "angel's row revoked"
    );
    assert_eq!(ctx.runtime.active_broker().as_deref(), Some("samco"));
    assert_eq!(*angel.logouts.lock(), 1, "angel's streaming stopped");
    let conn = ctx.sqlite.conn().unwrap();
    assert!(credentials::load(&conn, &ctx.security, "angel")
        .unwrap()
        .is_some());
}

/// Two sign-ins that complete at the same moment never leave two sessions:
/// memory, the stored rows and streaming agree on exactly one broker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_sign_ins_completing_together_leave_one_session() {
    for _ in 0..10 {
        let (t, angel, samco) = two_brokers();
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        *angel.auth_gate.lock() = Some(gate.clone());
        *samco.auth_gate.lock() = Some(gate);
        let (a, s) = (t.ctx.clone(), t.ctx.clone());
        let ja = tokio::spawn(async move {
            BrokerAuthService::login_with_form(&a, "angel", totp())
                .await
                .map(|_| ())
        });
        let js = tokio::spawn(async move {
            BrokerAuthService::login_with_form(&s, "samco", totp())
                .await
                .map(|_| ())
        });
        ja.await.unwrap().unwrap();
        js.await.unwrap().unwrap();
        let ctx = &t.ctx;
        let live = ctx.get_broker_session().expect("one session").broker_id;
        assert_eq!(live_rows(ctx), vec![live.clone()], "one stored session");
        assert_eq!(ctx.runtime.active_broker(), Some(live.clone()));
        assert_eq!(ctx.server_config().active_broker, Some(live));
        ctx.runtime.teardown(ctx).await;
    }
}

// ------------------------------------- switching is enforced, not only confirmed
//
// Roadmap "Broker switching": one test per row. The counts come from the
// order-outcomes service through `AppState::switch_guard`; here a fake
// guard stands in for it.

use crate::services::broker_switch::{SwitchBlockers, SwitchGuard};

struct FakeGuard(std::result::Result<SwitchBlockers, ()>);

impl SwitchGuard for FakeGuard {
    fn blockers(&self, _: &AppState, _: &str) -> crate::error::Result<SwitchBlockers> {
        self.0
            .map_err(|_| crate::error::AppError::Internal("outcomes unavailable".into()))
    }
}

fn guard(ctx: &AppState, b: SwitchBlockers) {
    *ctx.switch_guard.write() = Arc::new(FakeGuard(Ok(b)));
}

/// Zerodha logged in (keys saved, session live) and Angel One configured:
/// logging in to Angel One is the switch.
fn switch_harness() -> (H, String, String) {
    let mock = Arc::new(MockBroker::new("zerodha"));
    let angel = Arc::new(MockBroker::new("angel"));
    let t = build(
        BrokerRegistry::with(vec![
            mock.clone() as Arc<dyn crate::brokers::Broker>,
            angel as Arc<dyn crate::brokers::Broker>,
        ]),
        ist(2026, 10, 5, 10, 0),
    );
    t.ctx.limiter.freeze(Some(std::time::Instant::now()));
    let h = H { t, mock };
    h.setup();
    h.save_broker_credentials();
    let conn = h.ctx().sqlite.conn().unwrap();
    credentials::save(
        &conn,
        &h.ctx().security,
        "angel",
        CredentialUpdate {
            api_key: Some("smartkey".into()),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    h.connect_broker();
    let (cookie, csrf) = h.session(true);
    (h, cookie, csrf)
}

/// Every way a switch could end the logged-in broker's session is refused,
/// with the same reason, and nothing changes.
async fn every_switch_is_refused(h: &H, cookie: &str, csrf: &str, says: &[&str]) {
    // The switch itself: a sign-in to another broker that completes.
    let e = BrokerAuthService::login_with_form(h.ctx(), "angel", totp())
        .await
        .unwrap_err();
    let why = e.client_message();
    for part in says {
        assert!(why.contains(part), "{why}");
    }
    assert!(why.contains("Nothing was changed."), "{why}");
    assert_eq!(
        h.ctx().get_broker_session().unwrap().broker_id,
        "zerodha",
        "the sign-in stored nothing"
    );
    let refused = [
        with_session(
            delete("/api/broker/credentials/zerodha?sign_out=true"),
            cookie,
            Some(csrf),
        ),
        with_session(
            put_json(
                "/api/broker/credentials/zerodha?sign_out=true",
                json!({"api_secret": "rotated"}),
            ),
            cookie,
            Some(csrf),
        ),
        with_session(
            post_json("/api/broker/credentials", json!({"broker": "fyers"})),
            cookie,
            Some(csrf),
        ),
        with_session(
            post_json(
                "/api/broker/credentials",
                json!({"broker_api_secret": "rotated"}),
            ),
            cookie,
            Some(csrf),
        ),
    ];
    for req in refused {
        let (s, v) = h.json(req).await;
        assert_eq!(s, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["message"], why.as_str());
    }
    assert!(h.ctx().is_broker_connected(), "the session stays");
    assert_eq!(
        h.ctx().server_config().active_broker.as_deref(),
        Some("zerodha")
    );
    let c = stored(h, "zerodha").expect("keys kept");
    assert_eq!(
        c.api_secret.unwrap().expose(),
        "kitesecret",
        "nothing saved"
    );
    // The page is told before it offers the confirm.
    assert_eq!(list(h, cookie).await["switch_refusal"], why.as_str());
}

#[tokio::test]
async fn switch_row1_running_live_automation_refuses_the_switch_and_names_it() {
    let (h, cookie, csrf) = switch_harness();
    guard(
        h.ctx(),
        SwitchBlockers {
            strategy_runs: 2,
            openscript_runs: 1,
            scalping_stops: 1,
            ..Default::default()
        },
    );
    every_switch_is_refused(
        &h,
        &cookie,
        &csrf,
        &[
            "Zerodha still has 2 running Live strategies",
            "1 running Live OpenScript run",
            "1 position under scalping protection",
            "stop the strategies and protection",
        ],
    )
    .await;
}

/// The same rule at the server's last line: a sign-in to another broker
/// that completes while the logged-in one still runs Live automation is
/// refused, and the logged-in session stays.
#[tokio::test]
async fn switch_row1_a_completed_sign_in_to_another_broker_is_refused_too() {
    let (t, _angel, _samco) = two_brokers();
    let ctx = t.ctx.clone();
    BrokerAuthService::login_with_form(&ctx, "angel", totp())
        .await
        .unwrap();
    guard(
        &ctx,
        SwitchBlockers {
            strategy_runs: 1,
            ..Default::default()
        },
    );
    let e = BrokerAuthService::login_with_form(&ctx, "samco", totp())
        .await
        .unwrap_err();
    assert!(
        e.client_message()
            .starts_with("Angel One still has 1 running Live strategy."),
        "{}",
        e.client_message()
    );
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "angel");
    assert_eq!(live_rows(&ctx), vec!["angel".to_string()]);
    ctx.runtime.teardown(&ctx).await;
}

/// A guard whose Live automation is recorded on `held` (one running Live
/// strategy there), whatever session is connected.
struct HeldGuard(&'static str);

impl SwitchGuard for HeldGuard {
    fn blockers(&self, _: &AppState, broker: &str) -> crate::error::Result<SwitchBlockers> {
        Ok(SwitchBlockers {
            strategy_runs: u32::from(broker == self.0),
            ..Default::default()
        })
    }

    fn held_brokers(&self, _: &AppState) -> crate::error::Result<Vec<String>> {
        Ok(vec![self.0.to_string()])
    }
}

/// Web marketcalls/openalgo#2337 (AUDIT R02): with no broker logged in (a
/// sign-out or the daily boundary ended the session), there is no session
/// to ask, and a sign-in to another broker went ahead under Live
/// automation recorded on the first. It is refused with the same reason a
/// switch gets, nothing is stored, and signing in to that broker again
/// goes ahead.
#[tokio::test]
async fn switch_row1_a_sign_in_with_no_session_is_refused_while_automation_trades_elsewhere() {
    let (t, _angel, _samco) = two_brokers();
    let ctx = t.ctx.clone();
    assert!(ctx.get_broker_session().is_none());
    *ctx.switch_guard.write() = Arc::new(HeldGuard("angel"));
    let e = BrokerAuthService::login_with_form(&ctx, "samco", totp())
        .await
        .unwrap_err();
    assert!(
        e.client_message()
            .starts_with("Angel One still has 1 running Live strategy."),
        "{}",
        e.client_message()
    );
    assert!(ctx.get_broker_session().is_none());
    assert!(live_rows(&ctx).is_empty(), "nothing stored");
    BrokerAuthService::login_with_form(&ctx, "angel", totp())
        .await
        .unwrap();
    assert_eq!(ctx.get_broker_session().unwrap().broker_id, "angel");
    ctx.runtime.teardown(&ctx).await;
}

#[tokio::test]
async fn switch_row2_orders_with_an_unknown_outcome_refuse_the_switch() {
    let (h, cookie, csrf) = switch_harness();
    guard(
        h.ctx(),
        SwitchBlockers {
            uncertain_placements: 2,
            ..Default::default()
        },
    );
    every_switch_is_refused(
        &h,
        &cookie,
        &csrf,
        &[
            "2 orders whose outcome is not known yet",
            "check the order book until every order is settled",
        ],
    )
    .await;
}

#[tokio::test]
async fn switch_row3_exits_still_filling_refuse_the_switch() {
    let (h, cookie, csrf) = switch_harness();
    guard(
        h.ctx(),
        SwitchBlockers {
            partial_exits: 1,
            ..Default::default()
        },
    );
    every_switch_is_refused(
        &h,
        &cookie,
        &csrf,
        &["1 exit still filling", "wait for the exits to finish"],
    )
    .await;
}

/// Working orders and positions placed by hand do not block: the sign-in
/// to the next broker completes, the old session ends then, and they stay
/// at the old broker untouched.
#[tokio::test]
async fn switch_row4_manual_orders_and_positions_stay_and_do_not_block() {
    let (h, _cookie, _csrf) = switch_harness();
    guard(h.ctx(), SwitchBlockers::default());
    BrokerAuthService::login_with_form(h.ctx(), "angel", totp())
        .await
        .unwrap();
    assert_eq!(h.ctx().get_broker_session().unwrap().broker_id, "angel");
    assert!(
        !h.mock.calls().iter().any(|c| matches!(
            c,
            crate::brokers::mock::MockCall::PlaceOrder(_)
                | crate::brokers::mock::MockCall::CancelOrder(_)
                | crate::brokers::mock::MockCall::CancelAll
                | crate::brokers::mock::MockCall::CloseAll
        )),
        "nothing is cancelled or closed at the old broker"
    );
    h.ctx().runtime.teardown(h.ctx()).await;
}

/// Sandbox runs are never counted: with analyzer mode on, the switch goes
/// ahead and analyzer mode stays on for the next broker's quotes.
#[tokio::test]
async fn switch_row5_sandbox_runs_do_not_block() {
    let (h, _cookie, _csrf) = switch_harness();
    h.ctx().sqlite.set_analyze_mode(true).unwrap();
    // The outcomes service counts Live items only.
    guard(h.ctx(), SwitchBlockers::default());
    BrokerAuthService::login_with_form(h.ctx(), "angel", totp())
        .await
        .unwrap();
    assert_eq!(h.ctx().get_broker_session().unwrap().broker_id, "angel");
    assert!(
        h.ctx().sqlite.get_analyze_mode().unwrap(),
        "sandbox mode stays on"
    );
    h.ctx().runtime.teardown(h.ctx()).await;
}

/// A guard that cannot answer never lets a switch through.
#[tokio::test]
async fn switch_refused_when_the_check_cannot_answer() {
    let (h, _cookie, _csrf) = switch_harness();
    *h.ctx().switch_guard.write() = Arc::new(FakeGuard(Err(())));
    let e = BrokerAuthService::login_with_form(h.ctx(), "angel", totp())
        .await
        .unwrap_err();
    assert!(e.client_message().contains("could not check"));
    assert_eq!(h.ctx().get_broker_session().unwrap().broker_id, "zerodha");
}

// ------------------------------------------------ review follow-ups

/// FLOW-04: new keys end the logged-in broker's session and forget the
/// account it was bound to, so the next login with the new account's keys
/// is not refused as "a different account".
#[tokio::test]
async fn flow04_new_keys_forget_the_old_account() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker(); // bound to AB1234
    let (s, v) = h
        .json(with_session(
            put_json(
                "/api/broker/credentials/zerodha?sign_out=true",
                json!({"api_key": "otheraccountkey"}),
            ),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let conn = h.ctx().sqlite.conn().unwrap();
    assert_eq!(
        crate::db::sqlite::auth::last_user_id(&conn, "zerodha").unwrap(),
        None
    );
}

/// A stored session not resumed yet (after a restart) is not brought back
/// once that broker's keys changed.
#[tokio::test]
async fn new_keys_revoke_a_stored_session_not_yet_resumed() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    // A restart: memory is empty, the stored row remains.
    h.ctx().set_broker_session(None);
    let (s, v) = save(
        &h,
        &cookie,
        &csrf,
        "zerodha",
        json!({"api_secret": "rotated"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(BrokerAuthService::try_resume(h.ctx())
        .await
        .unwrap()
        .is_none());
    assert!(!h.ctx().is_broker_connected());
}

/// A sign-in still waiting on the broker when the trader saves new keys
/// for it is not stored: the newer keys win.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sign_in_that_outlives_its_keys_is_not_stored() {
    let (t, angel, _samco) = two_brokers();
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    *angel.auth_gate.lock() = Some(gate.clone());
    let ctx = t.ctx.clone();
    let login =
        tokio::spawn(
            async move { BrokerAuthService::login_with_form(&ctx, "angel", totp()).await },
        );
    // The broker is answering; the trader saves new keys meanwhile.
    while angel.last_auth.lock().is_none() {
        tokio::task::yield_now().await;
    }
    let saved = BrokerConfigService::save(
        &t.ctx,
        "angel",
        crate::services::broker_config_service::KeySave {
            update: CredentialUpdate {
                api_key: Some("newsmartkey".into()),
                ..Default::default()
            },
            redirect_url: None,
        },
        false,
    )
    .await
    .unwrap();
    assert!(matches!(
        saved,
        crate::services::broker_config_service::SaveOutcome::Saved { .. }
    ));
    gate.wait().await;
    let e = login.await.unwrap().unwrap_err();
    assert!(
        e.client_message()
            .contains("keys changed while this login was in progress"),
        "{}",
        e.client_message()
    );
    assert!(t.ctx.get_broker_session().is_none());
    assert!(live_rows(&t.ctx).is_empty());
}

/// Forgetting the logged-in broker removes its keys first: when that
/// fails, the session and the keys stay as they were.
#[tokio::test]
async fn a_forget_that_fails_keeps_the_session() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    h.connect_broker();
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER test_fail_forget BEFORE DELETE ON broker_credentials
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    let (s, _) = h
        .json(with_session(
            delete("/api/broker/credentials/zerodha?sign_out=true"),
            &cookie,
            Some(&csrf),
        ))
        .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(h.ctx().is_broker_connected(), "the session stays");
    assert!(stored(&h, "zerodha").is_some(), "the keys stay");
}

/// One broker's keys that this install cannot decrypt do not hide the
/// others: that broker shows as saved but unreadable.
#[tokio::test]
async fn one_unreadable_broker_does_not_hide_the_list() {
    let (h, cookie, csrf) = signed_in_harness();
    h.save_broker_credentials();
    let (s, _) = save(
        &h,
        &cookie,
        &csrf,
        "fyers",
        json!({"api_key": "APP-100", "api_secret": "fsecret"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    h.ctx()
        .sqlite
        .conn()
        .unwrap()
        .execute(
            "UPDATE broker_credentials SET api_key_encrypted = 'AAAAAAAAAAAAAAAAAAAAAAAA'
             WHERE broker_id = 'fyers'",
            [],
        )
        .unwrap();
    let data = list(&h, &cookie).await;
    let fyers = entry(&data, "fyers");
    assert_eq!(fyers["configured"], true);
    assert!(field(fyers, "api_key")["saved"]
        .as_str()
        .unwrap()
        .contains("cannot be read"));
    assert_eq!(field(entry(&data, "zerodha"), "api_key")["saved"], "***");
}

fn fyers_harness() -> (H, String, String, Arc<crate::brokers::fyers::FyersBroker>) {
    let mock = Arc::new(MockBroker::new("zerodha"));
    let fyers = Arc::new(crate::brokers::fyers::FyersBroker::new(
        crate::brokers::common::symbols::SymbolResolver::new(),
    ));
    let t = build(
        BrokerRegistry::with(vec![
            mock.clone() as Arc<dyn crate::brokers::Broker>,
            fyers.clone() as Arc<dyn crate::brokers::Broker>,
        ]),
        ist(2026, 10, 5, 10, 0),
    );
    t.ctx.limiter.freeze(Some(std::time::Instant::now()));
    let h = H { t, mock };
    h.setup();
    let (cookie, csrf) = h.session(true);
    (h, cookie, csrf, fyers)
}

async fn set_plan(
    h: &H,
    cookie: &str,
    csrf: Option<&str>,
    broker: &str,
    plan: &str,
) -> (StatusCode, Value) {
    h.json(with_session(
        put_json(
            &format!("/api/broker/rate-plan/{}", broker),
            json!({"plan": plan}),
        ),
        cookie,
        csrf,
    ))
    .await
}

/// BRL-01: the Fyers API plan is listed with what each allows, saved and
/// applied at once (the market-data budget becomes Prime's), restored at
/// the next start, and only a plan the broker offers is accepted.
#[tokio::test]
async fn the_fyers_api_plan_is_listed_saved_applied_and_restored() {
    use crate::brokers::fyers::ratelimit::Category;
    use crate::brokers::Broker;
    let (h, cookie, csrf, fyers) = fyers_harness();
    let data = list(&h, &cookie).await;
    let f = entry(&data, "fyers");
    assert_eq!(f["rate_plan"], "standard");
    let ids: Vec<&str> = f["rate_plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["standard", "prime"]);
    assert!(f["rate_plans"][0]["hint"]
        .as_str()
        .unwrap()
        .starts_with("Market data: 5 requests a second, 50 a minute, 5,000 a day."));
    assert_eq!(entry(&data, "zerodha")["rate_plans"], json!([]));
    assert_eq!(entry(&data, "zerodha")["rate_plan"], Value::Null);

    // Refused without the CSRF token, for a plan Fyers does not sell and
    // for a broker without plans; nothing changes.
    let (s, _) = set_plan(&h, &cookie, None, "fyers", "prime").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = set_plan(&h, &cookie, Some(&csrf), "fyers", "gold").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["message"], "Fyers has no API plan called gold.");
    let (s, _) = set_plan(&h, &cookie, Some(&csrf), "zerodha", "prime").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(fyers.rate_plan(), Some("standard"));

    let (s, v) = set_plan(&h, &cookie, Some(&csrf), "fyers", "prime").await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert_eq!(
        v["message"],
        "Fyers requests are now paced for the Prime plan."
    );
    assert_eq!(fyers.rate_plan(), Some("prime"));
    assert_eq!(
        fyers.limiter().budget(Category::Data).limits().per_day,
        Some(500_000)
    );
    assert_eq!(
        entry(&list(&h, &cookie).await, "fyers")["rate_plan"],
        "prime"
    );

    // A restart builds the adapter with the default plan; the saved one is
    // applied again.
    fyers.set_rate_plan("standard");
    BrokerConfigService::apply_rate_plans(h.ctx());
    assert_eq!(fyers.rate_plan(), Some("prime"));
}
