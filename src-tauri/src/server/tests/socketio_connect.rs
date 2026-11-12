//! Web `test/test_socketio_connect_auth.py` (471e1ddf7): the default
//! Socket.IO namespace admits only a signed-in session. On the web a client
//! with no cookie connected and received every broadcast: live order
//! events, analyzer updates, the session count, the WhatsApp pairing code.
//!
//! The desktop refused such a connection from the start
//! (`server::socketio::on_connect`), and every push checks the session
//! again as it goes out (`SocketEmitter::emit_signed_in`). These tests pin
//! the connect check by itself: a refused connection must be closed before
//! any push is sent, since a push would close it too.
//!
//! One rule differs on purpose. The web also asks for a broker login, the
//! rule its `/market` namespace and `/api/websocket/*` routes use. A broker
//! session on the desktop belongs to the app, not to a browser session, and
//! those helper routes ask for the signed-in user only
//! (`session_rollover.rs`), so the socket asks the same. Both apps' pages
//! open the socket only once a broker is logged in (`SocketProvider`).
//! A sign-in from before the daily boundary is covered in
//! `session_rollover.rs`.

use super::security::{closed_within, live_updates, receives, serve, session_id, Served};
use super::*;

/// Whether the server turns the connection away by itself: refused at the
/// namespace join, or closed right after it with no push sent.
async fn turned_away(s: &Served, cookie: &str) -> bool {
    match live_updates(s, cookie).await {
        None => true,
        Some(mut ws) => closed_within(&mut ws, 10).await,
    }
}

fn push_an_order_update(h: &H) {
    h.ctx()
        .ui
        .emit_signed_in(None, "order_update", &json!({"symbol": "SBIN"}));
}

/// Web `test_a_signed_in_broker_session_connects_and_is_pushed_to`. No
/// broker is logged in: the desktop asks for the signed-in user only.
#[tokio::test]
async fn a_signed_in_session_connects_and_is_pushed_to() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let s = serve(&h).await;
    let mut ws = live_updates(&s, &cookie).await.expect("connected");
    push_an_order_update(&h);
    assert!(receives(&mut ws, "order_update", 10).await);
}

/// Web `test_no_cookie_is_refused_and_receives_nothing`: no session cookie,
/// one the app never issued, and another site's cookie are each turned
/// away, while the trader's own page keeps its pushes.
#[tokio::test]
async fn no_session_is_refused_and_hears_nothing() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    let s = serve(&h).await;
    let mut trader = live_updates(&s, &cookie).await.expect("trader connected");
    for stranger in ["", "session=never-issued", "theme=dark"] {
        assert!(turned_away(&s, stranger).await, "{:?} was let in", stranger);
    }
    push_an_order_update(&h);
    assert!(receives(&mut trader, "order_update", 10).await);
}

/// Web `test_a_password_sign_in_with_no_broker_login_is_refused` and
/// `test_a_broker_login_with_no_user_is_refused`, in the desktop's terms:
/// a browser session with no user (the one any page load gets) and a
/// sign-in whose password was accepted but whose authenticator code has
/// not been entered are not signed in.
#[tokio::test]
async fn a_session_without_a_signed_in_user_is_refused() {
    let h = H::new();
    h.setup();
    let s = serve(&h).await;
    let (anonymous, _) = h.session(false);
    assert!(turned_away(&s, &anonymous).await, "anonymous session");
    let (waiting, _) = h.session(false);
    let now = h.ctx().now();
    h.ctx().sessions.update(&session_id(&waiting), |x| {
        x.pending_totp_user = Some(USER.into());
        x.pending_totp_started = Some(now);
    });
    assert!(
        turned_away(&s, &waiting).await,
        "a sign-in waiting for its code"
    );
}

/// Web `test_a_sign_in_ended_on_another_device_is_refused`: the cookie of a
/// session that was signed out (by a logout anywhere, a password change or
/// an account reset) opens no connection afterwards.
#[tokio::test]
async fn an_ended_sign_in_is_refused() {
    let h = H::new();
    h.setup();
    let s = serve(&h).await;
    let (cookie, _) = h.session(true);
    h.ctx().sessions.remove(&session_id(&cookie));
    assert!(turned_away(&s, &cookie).await);
}
