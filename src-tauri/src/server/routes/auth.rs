//! `/auth/*` and `/setup` (web `blueprints/auth.py`, `blueprints/core.py`).

use crate::events::Event;
use crate::security::{totp, Secret};
use crate::server::envelope::{error, json_response, EarlyReply};
use crate::server::form::FormData;
use crate::server::middleware::{
    claim_sign_in, clear_session_cookie, login_limited, redirect, same_origin_navigation,
    sign_in_from, sign_in_refused, with_cookie, Sess, Src, User,
};
use crate::server::routes::webui::no_store;
use crate::services::apikey_service::ApiKeyService;
use crate::services::auth_service::{AuthService, LoginOutcome, Refusal};
use crate::services::broker_auth_service::BrokerAuthService;
use crate::services::security_service::record_login;
use crate::session::web::{WebSession, PENDING_TOTP_SECS};
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn ok(v: Value) -> Response {
    json_response(StatusCode::OK, v)
}

/// GET /auth/csrf-token -> `{"csrf_token": ...}`. Creates the session (and
/// its cookie) when the browser has none.
pub async fn csrf_token(State(ctx): Ctx, Sess(sess): Sess) -> Response {
    match sess {
        Some(s) => ok(json!({"csrf_token": s.csrf_token})),
        None => {
            let s = ctx.sessions.create(ctx.now());
            with_cookie(ok(json!({"csrf_token": s.csrf_token})), &s.id)
        }
    }
}

/// GET /auth/check-setup
pub async fn check_setup(State(ctx): Ctx) -> Response {
    match AuthService::needs_setup(&ctx) {
        Ok(n) => ok(json!({"status": "success", "needs_setup": n})),
        Err(e) => e.into_response(),
    }
}

/// GET /auth/app-info
pub async fn app_info() -> Response {
    ok(json!({"status": "success", "version": env!("CARGO_PKG_VERSION"), "name": "OpenAlgo"}))
}

/// POST /setup (form: username, email, password[, confirm_password]).
/// The session layer requires the CSRF token and a same-origin request;
/// from anywhere but this computer (a tunnel, a proxy, another device) the
/// account cannot be created at all.
pub async fn setup(State(ctx): Ctx, Src(source): Src, form: FormData) -> Response {
    if !source.is_local() {
        tracing::warn!("Account setup refused: request did not come from this computer");
        return error(
            StatusCode::FORBIDDEN,
            "Create your account on the computer where OpenAlgo Desktop is installed.",
        );
    }
    let username = form.non_empty("username").unwrap_or_default();
    let email = form.non_empty("email").unwrap_or_default();
    let password = Secret::new(form.get("password").unwrap_or_default());
    if let Some(c) = form.get("confirm_password") {
        if c != password.expose() {
            return error(StatusCode::BAD_REQUEST, "Passwords do not match.");
        }
    }
    let c2 = ctx.clone();
    let r = tokio::task::spawn_blocking(move || {
        AuthService::setup(&c2, &username, &email, password.expose())
    })
    .await;
    match r {
        Ok(Ok(())) => ok(json!({
            "status": "success",
            "message": "Account created successfully. Sign in to continue.",
            "redirect": "/login",
        })),
        Ok(Err(e)) => {
            let already = matches!(&e, crate::error::AppError::Validation(m) if m.starts_with("An account already exists"));
            let mut body = json!({"status": "error", "message": e.client_message()});
            if already {
                body["redirect"] = json!("/login");
            }
            json_response(StatusCode::BAD_REQUEST, body)
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Setup failed. Please try again.",
        ),
    }
}

/// What a sign-in answers when the password was changed, or every window
/// was signed out, while it was being checked.
pub const PASSWORD_CHANGED_DURING_SIGN_IN: &str =
    "You were signed out, or your password was changed, while signing in. Sign in again.";

/// Issue the signed-in session and install the keys the password opened,
/// together (KEY-01). `epoch` is the credentials generation read before the
/// account was read: if a password change or reset, an account reset or a
/// logout happened since, no session is issued and nothing is installed,
/// even though the password checked out against the hash read before the
/// change (`SecurityManager::issue_if_current`; security review of SEC-02).
async fn finish_sign_in(
    ctx: &Arc<AppState>,
    old_session: Option<&str>,
    username: &str,
    epoch: u64,
    keys: crate::security::Unlocked,
) -> Response {
    let now = ctx.now();
    let issued = ctx.security.issue_if_current(epoch, || {
        ctx.security.install(&keys);
        let s = match old_session {
            Some(id) => ctx.sessions.rotate(id, now),
            None => ctx.sessions.create(now),
        };
        ctx.sessions.update(&s.id, |x| {
            x.user = Some(username.to_string());
            x.authenticated_at = Some(now);
            x.forget_pending_sign_in();
        });
        s
    });
    let Some(s) = issued else {
        tracing::warn!("Sign-in refused: the password changed while it was being checked");
        if let Some(id) = old_session {
            ctx.sessions.remove(id);
        }
        return error(StatusCode::UNAUTHORIZED, PASSWORD_CHANGED_DURING_SIGN_IN);
    };
    // The one routing rule after a sign-in: the dashboard when the saved
    // broker session was resumed, otherwise Select Broker. The web leaves
    // the second implied (its page defaults to /broker); the desktop says
    // it, so the page and the server cannot disagree.
    let body = match BrokerAuthService::try_resume(ctx).await {
        Ok(Some(b)) => json!({
            "status": "success",
            "message": "Broker session resumed",
            "redirect": "/dashboard",
            "broker": b.broker_id,
        }),
        Ok(None) => json!({"status": "success", "redirect": "/broker"}),
        Err(e) => {
            tracing::warn!("Could not resume the broker session: {}", e);
            json!({"status": "success", "redirect": "/broker"})
        }
    };
    // Telegram and WhatsApp come back with the keys, as the web's start-up
    // brings them back (they were stopped at the logout).
    crate::messaging::autostart(ctx);
    // The built-in tunnel opens with them when it is switched on.
    crate::services::tunnel::autostart(ctx);
    with_cookie(ok(body), &s.id)
}

/// What a sign-in from anywhere but the OpenAlgo Desktop window answers
/// while the keys of an earlier build still have to move into openalgo.db
/// (KEY-01): the move happens at a sign-in in the app window only. The same
/// answer, after the same work, for every name and password.
pub const SIGN_IN_IN_APP_WINDOW_FIRST: &str = "Sign in once in the OpenAlgo Desktop window on \
the computer where it is installed: OpenAlgo is moving your saved keys into its own database. \
After that you can sign in here too.";

/// Answer a sign-in that issued no session as the auth service decided
/// (`LoginOutcome::refusal`), spending the caller's budget unless the app
/// window is being told why.
fn refuse(
    ctx: &AppState,
    attempt: crate::server::middleware::SignInAttempt<'_>,
    refusal: Refusal,
    username: &str,
    ip: std::net::IpAddr,
) -> Response {
    match refusal {
        Refusal::InvalidCredentials => {
            attempt.failed();
            tracing::info!("Sign-in failed: invalid credentials");
            record_login(
                ctx,
                username,
                ip,
                "failed",
                "password",
                Some("invalid_credentials"),
            );
            error(StatusCode::UNAUTHORIZED, "Invalid credentials")
        }
        Refusal::MoveInAppWindow => {
            attempt.failed();
            tracing::info!("Sign-in refused: the keys move at a sign-in in the app window");
            error(StatusCode::FORBIDDEN, SIGN_IN_IN_APP_WINDOW_FIRST)
        }
        Refusal::Explain(e) => {
            attempt.released();
            e.into_response()
        }
        Refusal::TryAgain => {
            attempt.released();
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Login failed. Please try again.",
            )
        }
    }
}

/// A sign-in that is not needed or not possible yet: initial setup comes
/// first, and a signed-in session is already signed in.
fn setup_or_signed_in(ctx: &AppState, sess: Option<&WebSession>) -> Option<Response> {
    match AuthService::needs_setup(ctx) {
        Ok(true) => {
            return Some(json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": "Please complete initial setup first.", "redirect": "/setup"}),
            ))
        }
        Ok(false) => {}
        Err(e) => return Some(e.into_response()),
    }
    if sess.is_some_and(|s| s.user.is_some()) {
        let to = if ctx.is_broker_connected() {
            "/dashboard"
        } else {
            "/broker"
        };
        return Some(ok(
            json!({"status": "success", "message": "Already logged in", "redirect": to}),
        ));
    }
    None
}

/// The password was right and the account has 2FA: a session (rotated, or
/// new) holds the pending sign-in until the code step.
fn ask_for_code(
    ctx: &AppState,
    sess: Option<&WebSession>,
    user: String,
    epoch: u64,
    keys: crate::security::Unlocked,
) -> Response {
    let now = ctx.now();
    let s = match sess {
        Some(s) => ctx.sessions.rotate(&s.id, now),
        None => ctx.sessions.create(now),
    };
    ctx.sessions.update(&s.id, |x| {
        x.user = None;
        x.pending_totp_user = Some(user);
        x.pending_totp_started = Some(now);
        // The code step issues the session only if the password is
        // still the one checked here.
        x.pending_epoch = Some(epoch);
        // The keys the password opened, used only to check the
        // code and installed only with the session (KEY-01).
        x.pending_keys = Some(keys);
    });
    with_cookie(
        ok(
            json!({"status": "totp_required", "message": "Enter the 6-digit code from your authenticator app."}),
        ),
        &s.id,
    )
}

/// POST /auth/login (form: username, password)
pub async fn login(
    State(ctx): Ctx,
    Sess(sess): Sess,
    Src(source): Src,
    headers: HeaderMap,
    form: FormData,
) -> Response {
    // A post that is not from the app's own page (a cross-site form, or a
    // program elsewhere) is refused before anything counts or is checked,
    // so a web page cannot spend the trader's budget (security review S-02).
    if let Some(r) = sign_in_refused(&ctx, &headers, source) {
        return r;
    }
    let ip = source.ip();
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    let username = form.non_empty("username").unwrap_or_default();
    if let Some(r) = setup_or_signed_in(&ctx, sess.as_ref()) {
        return r;
    }
    // The failure budget of this source, whatever name was typed (so the
    // delays never tell which name is the account's), claimed and counted
    // as a failure before the password is checked: no password is checked
    // while its wait runs, and parallel requests cannot all pass.
    let attempt = match claim_sign_in(&ctx, source) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    // KEY-01 security review: only the app window moves keys and hears why
    // they could not be opened (`LoginOutcome::refusal`).
    let from = sign_in_from(&ctx, &headers, source);
    let password = Secret::new(form.get("password").unwrap_or_default());
    // Read before the account is: a password change that lands while the
    // password is checked voids this sign-in (`finish_sign_in`).
    let epoch = ctx.security.credentials_epoch();
    let c2 = ctx.clone();
    let u2 = username.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        AuthService::verify_credentials(&c2, &u2, password.expose(), from)
    })
    .await;
    match outcome {
        Ok(LoginOutcome::Success(user, keys)) => {
            attempt.succeeded();
            tracing::info!("Sign-in succeeded");
            record_login(&ctx, &user, ip, "success", "password", None);
            let old = sess.as_ref().map(|s| s.id.as_str());
            finish_sign_in(&ctx, old, &user, epoch, keys).await
        }
        Ok(LoginOutcome::TotpRequired(user, keys)) => {
            // The password was right: this attempt is given back. Earlier failures stay
            // until the code is right too, so the password cannot clear them between guesses.
            attempt.released();
            ask_for_code(&ctx, sess.as_ref(), user, epoch, keys)
        }
        Ok(other) => refuse(&ctx, attempt, other.refusal(from), &username, ip),
        Err(_) => refuse(&ctx, attempt, Refusal::sign_in_broke(from), &username, ip),
    }
}

/// GET /auth/login: where the browser should go.
pub async fn login_page(State(ctx): Ctx, Sess(sess): Sess) -> Response {
    if AuthService::needs_setup(&ctx).unwrap_or(false) {
        return redirect("/setup");
    }
    match sess.and_then(|s| s.user) {
        Some(_) if ctx.is_broker_connected() => redirect("/dashboard"),
        Some(_) => redirect("/broker"),
        None => redirect("/login"),
    }
}

/// The sign-in a code completes: this session's pending one, while it is
/// fresh (an expired one is forgotten), with the user and the keys its
/// password opened.
fn pending_code_step(
    ctx: &AppState,
    sess: Option<WebSession>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(WebSession, String, crate::security::Unlocked), EarlyReply> {
    let none = || {
        Box::new(error(
            StatusCode::UNAUTHORIZED,
            "No pending login. Sign in first.",
        ))
    };
    let Some(s) = sess else {
        return Err(none());
    };
    let fresh = s
        .pending_totp_started
        .map(|t| (now - t).num_seconds() <= PENDING_TOTP_SECS)
        .unwrap_or(false);
    if !fresh {
        ctx.sessions.update(&s.id, |x| x.forget_pending_sign_in());
        return Err(Box::new(json_response(
            StatusCode::UNAUTHORIZED,
            json!({"status": "error", "message": "Login session expired. Please sign in again.", "redirect": "/login"}),
        )));
    }
    let (Some(user), Some(keys)) = (s.pending_totp_user.clone(), s.pending_keys.clone()) else {
        return Err(none());
    };
    Ok((s, user, keys))
}

/// POST /auth/login/totp (json: totp_code)
pub async fn login_totp(
    State(ctx): Ctx,
    Sess(sess): Sess,
    Src(source): Src,
    headers: HeaderMap,
    form: FormData,
) -> Response {
    if let Some(r) = sign_in_refused(&ctx, &headers, source) {
        return r;
    }
    let ip = source.ip();
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    let now = ctx.now();
    let (s, user, keys) = match pending_code_step(&ctx, sess, now) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let code = form.non_empty("totp_code").unwrap_or_default();
    if code.is_empty() {
        return error(StatusCode::BAD_REQUEST, "TOTP code is required.");
    }
    // The account's budget from this source, the same one its password
    // spends, claimed before the code is checked.
    let attempt = match claim_sign_in(&ctx, source) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match AuthService::verify_totp_with(&ctx, &user, &code, &keys) {
        Ok(true) => {
            attempt.succeeded();
            ctx.sessions
                .update(&s.id, |x| x.totp_verified_at = Some(now));
            record_login(&ctx, &user, ip, "success", "totp", None);
            // The generation read at the password step: a password change
            // since then voids this sign-in. A pending sign-in without one
            // never issues a session.
            let epoch = s.pending_epoch.unwrap_or(u64::MAX);
            finish_sign_in(&ctx, Some(&s.id), &user, epoch, keys).await
        }
        Ok(false) => {
            attempt.failed();
            record_login(&ctx, &user, ip, "failed", "totp", Some("invalid_totp"));
            error(StatusCode::UNAUTHORIZED, "Invalid TOTP code.")
        }
        Err(e) => {
            attempt.released();
            e.into_response()
        }
    }
}

/// GET /auth/session-status. Carries the API key to the signed-in session
/// (as on the web), so it is never cached (security review SEC-06).
pub async fn session_status(State(ctx): Ctx, Sess(sess): Sess) -> Response {
    no_store(session_status_body(&ctx, sess))
}

fn session_status_body(ctx: &AppState, sess: Option<crate::session::web::WebSession>) -> Response {
    // The keys are always locked with the password now (KEY-01); `locked`
    // says whether they are in memory.
    let key_mode = "password";
    let locked = !ctx.security.is_unlocked();
    let Some(user) = sess.and_then(|s| s.user) else {
        return ok(json!({
            "status": "success", "message": "Not authenticated",
            "authenticated": false, "logged_in": false, "key_mode": key_mode, "locked": locked,
        }));
    };
    let active = ctx.sessions.authenticated_count();
    match ctx.get_broker_session() {
        Some(b) => {
            let api_key = ApiKeyService::current(ctx).ok().flatten();
            ok(json!({
                "status": "success", "authenticated": true, "logged_in": true,
                "user": user, "broker": b.broker_id,
                "api_key": api_key.as_ref().map(|k| k.expose()),
                "active_sessions": active, "key_mode": key_mode, "locked": locked,
            }))
        }
        None if BrokerAuthService::had_revoked_session(ctx) => ok(json!({
            "status": "success", "authenticated": true, "logged_in": false,
            "user": user, "broker": Value::Null, "broker_session_expired": true,
            "active_sessions": active, "key_mode": key_mode, "locked": locked,
        })),
        None => ok(json!({
            "status": "success", "authenticated": true, "logged_in": false,
            "user": user, "broker": Value::Null, "active_sessions": active, "key_mode": key_mode, "locked": locked,
        })),
    }
}

fn foreign(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .map(|s| s != "same-origin" && s != "none")
        .unwrap_or(false)
}

/// What a POST logout answers when the broker session ended in memory but
/// its stored record could not be updated yet (the session poll retries).
pub const LOGOUT_REVOKE_PENDING: &str = "Logged out. Your broker session has ended, but \
OpenAlgo could not record that on this computer yet and will keep trying in the background.";

/// GET|POST /auth/logout. The signed-in trader's logout revokes the broker
/// session, ends every browser session and tells every window. A caller
/// without a signed-in session ends only its own browser session: signing
/// the trader out everywhere needs the trader's own session (security
/// review SEC-01; the web, too, clears only the caller's session).
pub async fn logout(
    State(ctx): Ctx,
    Sess(sess): Sess,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if foreign(&headers) {
        return error(StatusCode::FORBIDDEN, "Request blocked.");
    }
    let signed_in = sess.as_ref().and_then(|s| s.user.as_ref()).is_some();
    // The session cookie is SameSite=Lax, so a link on another site opened
    // in a browser without Fetch Metadata carries it on this GET. Ending
    // every session and the broker's from a GET therefore needs proof the
    // navigation came from OpenAlgo itself (or was typed); the app's own
    // sign-out is a CSRF-checked POST.
    if signed_in && method == Method::GET && !same_origin_navigation(&ctx, &headers) {
        tracing::warn!("Sign-out link not opened from OpenAlgo; nothing was ended");
        return redirect("/");
    }
    let mut revoke_pending = false;
    if signed_in {
        ctx.sessions.clear();
        if let Err(e) =
            BrokerAuthService::revoke(&ctx, crate::events::SessionEndReason::Logout).await
        {
            tracing::error!(
                "Broker session ended on logout; its stored row is still to be revoked: {}",
                e
            );
            revoke_pending = true;
        }
        // The keys and everything decrypted with them leave memory, and
        // every cache is cleared (KEY-01).
        AuthService::lock_everything(&ctx).await;
        ctx.bus.publish(Event::ForceLogout {
            message: "You have been logged out from another device.".into(),
        });
        tracing::info!("Signed out");
    } else if let Some(s) = &sess {
        ctx.sessions.remove(&s.id);
    }
    let mut resp = if method == Method::POST {
        if revoke_pending {
            ok(json!({
                "status": "success",
                "message": LOGOUT_REVOKE_PENDING,
                "broker_revoke_pending": true,
            }))
        } else {
            ok(json!({"status": "success", "message": "Logged out successfully"}))
        }
    } else {
        redirect("/login")
    };
    resp.headers_mut()
        .append(header::SET_COOKIE, clear_session_cookie());
    resp
}

/// POST /auth/change-password (form: old_password, new_password, confirm_password)
pub async fn change_password_api(
    State(ctx): Ctx,
    User(u): User,
    Src(source): Src,
    headers: HeaderMap,
    form: FormData,
) -> Response {
    if let Some(r) = sign_in_refused(&ctx, &headers, source) {
        return r;
    }
    let (Some(old), Some(new), Some(confirm)) = (
        form.non_empty("old_password"),
        form.non_empty("new_password"),
        form.non_empty("confirm_password"),
    ) else {
        return error(StatusCode::BAD_REQUEST, "All fields are required");
    };
    change_password_common(
        &ctx,
        source,
        &u.username,
        &old,
        &new,
        &confirm,
        "Password changed successfully",
    )
    .await
}

/// POST /auth/change (json or form: old_password|current_password, new_password[, confirm_password])
pub async fn change_password_legacy(
    State(ctx): Ctx,
    User(u): User,
    Src(source): Src,
    headers: HeaderMap,
    form: FormData,
) -> Response {
    if let Some(r) = sign_in_refused(&ctx, &headers, source) {
        return r;
    }
    let old = form
        .non_empty("old_password")
        .or_else(|| form.non_empty("current_password"))
        .unwrap_or_default();
    let new = form.get("new_password").unwrap_or_default().to_string();
    let confirm = form
        .get("confirm_password")
        .map(String::from)
        .unwrap_or_else(|| new.clone());
    change_password_common(
        &ctx,
        source,
        &u.username,
        &old,
        &new,
        &confirm,
        "Your password has been changed successfully.",
    )
    .await
}

async fn change_password_common(
    ctx: &Arc<AppState>,
    source: crate::server::middleware::Source,
    username: &str,
    old: &str,
    new: &str,
    confirm: &str,
    success: &str,
) -> Response {
    // Checking the current password spends the account's budget from this
    // source, so a signed-in session cannot be used to guess it. (The
    // handler has already refused a request from another site.)
    let attempt = match claim_sign_in(ctx, source) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let (c2, u, o, n, cf) = (
        ctx.clone(),
        username.to_string(),
        Secret::new(old),
        Secret::new(new),
        Secret::new(confirm),
    );
    let r = tokio::task::spawn_blocking(move || {
        AuthService::change_password(&c2, &u, o.expose(), n.expose(), cf.expose())
    })
    .await;
    match r {
        Ok(Ok(())) => {
            attempt.succeeded();
            ctx.bus.publish(Event::ForceLogout {
                message: "Your password was changed. Please log in again with the new password."
                    .into(),
            });
            let mut resp = ok(json!({"status": "success", "message": success}));
            resp.headers_mut()
                .append(header::SET_COOKIE, clear_session_cookie());
            resp
        }
        Ok(Err(e)) => {
            if matches!(&e, crate::error::AppError::Validation(m) if m == crate::services::auth_service::WRONG_CURRENT_PASSWORD)
            {
                attempt.failed();
            } else {
                attempt.released();
            }
            error(StatusCode::BAD_REQUEST, e.client_message())
        }
        Err(_) => {
            attempt.released();
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to change password",
            )
        }
    }
}

/// GET /auth/analyzer-mode
pub async fn analyzer_mode(State(ctx): Ctx) -> Response {
    match ctx.sqlite.get_analyze_mode() {
        Ok(m) => ok(
            json!({"status": "success", "data": {"mode": if m {"analyze"} else {"live"}, "analyze_mode": m}}),
        ),
        Err(e) => e.into_response(),
    }
}

/// POST /auth/analyzer-toggle
pub async fn analyzer_toggle(State(ctx): Ctx) -> Response {
    if !ctx.is_broker_connected() {
        return error(StatusCode::UNAUTHORIZED, "Broker not connected");
    }
    // The mode stored; the switch does not count the analyzer log (web
    // `toggle_analyzer_mode_session`).
    match crate::services::AnalyzerService::toggle_mode(&ctx).await {
        Ok(on) => ok(json!({"status": "success", "data": {
            "mode": if on { "analyze" } else { "live" },
            "analyze_mode": on,
            "message": format!("Switched to {} mode", if on {"Analyze"} else {"Live"}),
        }})),
        Err(e) => e.into_response(),
    }
}

/// GET /auth/dashboard-data
pub async fn dashboard_data(State(ctx): Ctx) -> Response {
    if !ctx.is_broker_connected() {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"status": "error", "code": "BROKER_SESSION_EXPIRED", "message": "Broker not connected - please connect your broker"}),
        );
    }
    // Web: the funds service's own status and message on failure.
    let r = crate::services::account_service::funds(&ctx).await;
    if !r.is_success() {
        let msg = r.message();
        return json_response(
            StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            json!({"status": "error", "message": if msg.is_empty() { "Failed to get funds".to_string() } else { msg }}),
        );
    }
    match r
        .body
        .get("data")
        .filter(|d| d.as_object().is_some_and(|m| !m.is_empty()))
    {
        Some(data) => ok(json!({"status": "success", "data": data})),
        None => {
            tracing::error!("Dashboard funds came back empty");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get margin data",
            )
        }
    }
}

/// GET /auth/profile-data
pub async fn profile_data(State(ctx): Ctx, User(u): User) -> Response {
    match AuthService::profile(&ctx, &u.username) {
        Ok(Some((row, secret))) => {
            let account = row.email.clone().unwrap_or_else(|| row.username.clone());
            let (qr, sec) = match secret {
                Some(s) => (
                    totp::qr_png_base64(&totp::provisioning_uri(s.expose(), &account)).ok(),
                    Some(s.expose().to_string()),
                ),
                None => (None, None),
            };
            // The enrollment seed goes to the signed-in session, as on the
            // web, and is never cached (security review SEC-06).
            no_store(ok(json!({"status": "success", "data": {
                "username": row.username, "smtp_settings": Value::Null,
                "qr_code": qr, "totp_secret": sec,
            }})))
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "User not found."),
        Err(e) => {
            tracing::error!("Profile data failed: {}", e);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get profile data",
            )
        }
    }
}

/// GET /auth/2fa/status
pub async fn two_factor_status(State(ctx): Ctx, User(u): User, Sess(sess): Sess) -> Response {
    match AuthService::profile(&ctx, &u.username) {
        Ok(Some((row, _))) => ok(json!({
            "status": "success",
            "totp_enabled": row.totp_enabled,
            "totp_required_for_login": row.totp_required_for_login,
            "totp_required_for_mcp": row.totp_required_for_mcp,
            "totp_required_for_password_reset": row.totp_required_for_password_reset,
            "last_totp_verified_at": sess.and_then(|s| s.totp_verified_at).map(|t| t.to_rfc3339()),
        })),
        Ok(None) => error(StatusCode::NOT_FOUND, "User not found."),
        Err(e) => e.into_response(),
    }
}

/// POST /auth/2fa/configure (json: totp_code, totp_enabled, totp_required_for_*)
pub async fn two_factor_configure(
    State(ctx): Ctx,
    User(u): User,
    Sess(sess): Sess,
    Src(source): Src,
    headers: HeaderMap,
    form: FormData,
) -> Response {
    if let Some(r) = sign_in_refused(&ctx, &headers, source) {
        return r;
    }
    let code = form.non_empty("totp_code").unwrap_or_default();
    if code.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "TOTP code is required to change 2FA settings.",
        );
    }
    // A signed-in session still cannot guess the code: it spends the
    // source's budget like a sign-in.
    let attempt = match claim_sign_in(&ctx, source) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    match AuthService::verify_totp_for(&ctx, &u.username, &code) {
        Ok(true) => attempt.released(),
        Ok(false) => {
            attempt.failed();
            return error(StatusCode::UNAUTHORIZED, "Invalid TOTP code.");
        }
        Err(e) => {
            attempt.released();
            return e.into_response();
        }
    }
    let f = two_factor_flags(&form);
    let row = match AuthService::profile(&ctx, &u.username) {
        Ok(Some((r, _))) => r,
        Ok(None) => return error(StatusCode::NOT_FOUND, "User not found."),
        Err(e) => return e.into_response(),
    };
    if let Err(e) = AuthService::set_two_factor(&ctx, row.id, f) {
        return e.into_response();
    }
    if let Some(s) = sess {
        let now = ctx.now();
        ctx.sessions
            .update(&s.id, |x| x.totp_verified_at = Some(now));
    }
    ok(json!({
        "status": "success", "totp_enabled": f.enabled,
        "totp_required_for_login": f.login, "totp_required_for_mcp": f.mcp,
        "totp_required_for_password_reset": f.password_reset,
    }))
}

/// The two-factor choices the form asks for; each requirement needs 2FA on.
fn two_factor_flags(form: &FormData) -> crate::db::sqlite::user::TwoFactorFlags {
    let flag = |k: &str| matches!(form.get(k), Some("true") | Some("1") | Some("on"));
    let enabled = flag("totp_enabled");
    crate::db::sqlite::user::TwoFactorFlags {
        enabled,
        login: enabled && flag("totp_required_for_login"),
        password_reset: enabled && flag("totp_required_for_password_reset"),
        mcp: enabled && flag("totp_required_for_mcp"),
    }
}

/// GET /auth/active-sessions
pub async fn active_sessions(State(ctx): Ctx) -> Response {
    ok(json!({
        "status": "success",
        "count": ctx.sessions.authenticated_count(),
        "current_session_id": Value::Null,
        "sessions": [],
    }))
}
