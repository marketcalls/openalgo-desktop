//! Web `test/test_websocket_session_expiry.py` (e2bdd061e): a login from
//! before the most recent daily boundary (03:00 IST) is not a current
//! login. On the web the `/api/websocket/*` routes and the `/market`
//! socket answered any cookie that carried a name, so a cookie from before
//! the boundary still read the live API key.
//!
//! The desktop serves the two helper routes its pages use
//! (`/api/websocket/config`, `/api/websocket/apikey`) behind the signed-in
//! user, and already measured a login against the most recent boundary
//! (`session::boundary`), but a browser session was ended only by the
//! expiry poll: for up to 30 s after the boundary, or after a wake from
//! sleep, its cookie still read the API key and kept the live-update
//! socket. Here the clock is past the boundary and the poll has not run.

use super::security::{closed_within, live_updates, serve};
use super::*;

/// The helper routes the market data pages call.
const ROUTES: [&str; 2] = ["/api/websocket/apikey", "/api/websocket/config"];

/// A browser session signed in at `login`; the clock is left at `now`.
fn signed_in_at(h: &H, login: DateTime<Utc>, now: DateTime<Utc>) -> String {
    h.t.clock.set(login);
    let s = h.ctx().sessions.create(login);
    h.ctx().sessions.update(&s.id, |x| {
        x.user = Some(USER.into());
        x.authenticated_at = Some(login);
    });
    h.t.clock.set(now);
    format!("session={}", s.id)
}

fn json_get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut r = get(path);
    r.headers_mut()
        .insert(header::ACCEPT, "application/json".parse().unwrap());
    match cookie {
        Some(c) => with_session(r, c, None),
        None => r,
    }
}

/// The web test's stale cases: (now, login).
fn stale() -> [(DateTime<Utc>, DateTime<Utc>); 2] {
    [
        // After the boundary, a login from the day before.
        (ist(2026, 10, 30, 10, 0), ist(2026, 10, 29, 10, 0)),
        // Between midnight and 03:00, a login three days old.
        (ist(2026, 10, 30, 1, 0), ist(2026, 10, 27, 10, 0)),
    ]
}

/// Web `test_a_session_is_valid_only_since_the_last_rollover`: between
/// midnight and 03:00 the last boundary was yesterday's.
#[tokio::test]
async fn a_login_is_current_only_since_the_last_boundary() {
    for (now, login, current) in [
        (ist(2026, 10, 30, 1, 0), ist(2026, 10, 27, 10, 0), false),
        (ist(2026, 10, 30, 1, 0), ist(2026, 10, 29, 2, 0), false),
        (ist(2026, 10, 30, 1, 0), ist(2026, 10, 29, 10, 0), true),
        (ist(2026, 10, 30, 10, 0), ist(2026, 10, 29, 10, 0), false),
        (ist(2026, 10, 30, 10, 0), ist(2026, 10, 30, 9, 0), true),
    ] {
        let h = H::new();
        h.setup();
        let cookie = signed_in_at(&h, login, now);
        let (_, v) = h
            .json(json_get("/auth/session-status", Some(&cookie)))
            .await;
        assert_eq!(
            v["authenticated"],
            json!(current),
            "login {} checked at {}",
            login,
            now
        );
    }
}

/// Web `test_a_cookie_from_before_the_rollover_is_not_signed_in`: a stale
/// cookie gets exactly what no cookie gets, and never the API key.
#[tokio::test]
async fn a_cookie_from_before_the_boundary_is_not_signed_in() {
    for (now, login) in stale() {
        let h = H::new();
        let key = h.setup();
        let cookie = signed_in_at(&h, login, now);
        for path in ROUTES {
            let (s, _, body) = h.send(json_get(path, Some(&cookie))).await;
            let (anon_s, _, anon_body) = h.send(json_get(path, None)).await;
            assert!(
                !String::from_utf8_lossy(&body).contains(&key),
                "{} handed the API key to a login from {} at {}",
                path,
                login,
                now
            );
            assert_eq!((s, body), (anon_s, anon_body), "{} at {}", path, now);
        }
    }
}

/// Web `test_a_current_session_still_reads_its_api_key`: the control.
#[tokio::test]
async fn a_current_session_still_reads_its_api_key() {
    let h = H::new();
    let key = h.setup();
    let cookie = signed_in_at(&h, ist(2026, 10, 30, 9, 0), ist(2026, 10, 30, 10, 0));
    let (s, v) = h
        .json(json_get("/api/websocket/apikey", Some(&cookie)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"status": "success", "api_key": key}));
}

/// Web `test_a_stale_session_cannot_open_the_market_socket`: the desktop's
/// live-update socket refuses it, or closes it before any push.
#[tokio::test]
async fn a_stale_session_cannot_keep_the_live_update_socket() {
    let h = H::new();
    h.setup();
    let cookie = signed_in_at(&h, ist(2026, 10, 29, 10, 0), ist(2026, 10, 30, 10, 0));
    let s = serve(&h).await;
    if let Some(mut ws) = live_updates(&s, &cookie).await {
        h.ctx()
            .ui
            .emit_signed_in(None, "order_event", &json!({"probe": true}));
        assert!(
            closed_within(&mut ws, 10).await,
            "a login from yesterday kept the live-update socket"
        );
    }
}

/// Web `test_an_expired_session_revokes_nothing_from_these_routes`: open
/// tabs poll these routes; a stale one ends nothing (the expiry poll does).
#[tokio::test]
async fn a_stale_request_ends_nothing() {
    let h = H::new();
    h.setup();
    let cookie = signed_in_at(&h, ist(2026, 10, 29, 10, 0), ist(2026, 10, 30, 10, 0));
    let before = h.ctx().sessions.len();
    for path in ROUTES {
        h.send(json_get(path, Some(&cookie))).await;
    }
    assert_eq!(h.ctx().sessions.len(), before);
}
