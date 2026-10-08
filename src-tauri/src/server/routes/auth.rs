//! `/auth/*` and `/setup` (web `blueprints/auth.py`, `blueprints/core.py`).

use crate::events::Event;
use crate::security::{totp, Secret};
use crate::server::envelope::{error, json_response};
use crate::server::form::FormData;
use crate::server::middleware::{
    clear_session_cookie, login_limited, redirect, with_cookie, ClientIp, Sess, User,
};
use crate::server::ratelimit::Bucket;
use crate::services::apikey_service::ApiKeyService;
use crate::services::auth_service::{AuthService, LoginOutcome};
use crate::services::broker_auth_service::BrokerAuthService;
use crate::session::web::{random_token, PENDING_TOTP_SECS};
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;

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

/// POST /setup (form: username, email, password[, confirm_password])
pub async fn setup(State(ctx): Ctx, form: FormData) -> Response {
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

async fn finish_sign_in(
    ctx: &Arc<AppState>,
    old_session: Option<&str>,
    username: &str,
) -> Response {
    let now = ctx.now();
    let s = match old_session {
        Some(id) => ctx.sessions.rotate(id, now),
        None => ctx.sessions.create(now),
    };
    ctx.sessions.update(&s.id, |x| {
        x.user = Some(username.to_string());
        x.authenticated_at = Some(now);
        x.pending_totp_user = None;
        x.pending_totp_started = None;
    });
    let body = match BrokerAuthService::try_resume(ctx).await {
        Ok(Some(b)) => json!({
            "status": "success",
            "message": "Broker session resumed",
            "redirect": "/dashboard",
            "broker": b.broker_id,
        }),
        Ok(None) => json!({"status": "success"}),
        Err(e) => {
            tracing::warn!("Could not resume the broker session: {}", e);
            json!({"status": "success"})
        }
    };
    with_cookie(ok(body), &s.id)
}

/// POST /auth/login (form: username, password)
pub async fn login(
    State(ctx): Ctx,
    Sess(sess): Sess,
    ClientIp(ip): ClientIp,
    form: FormData,
) -> Response {
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    match AuthService::needs_setup(&ctx) {
        Ok(true) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": "Please complete initial setup first.", "redirect": "/setup"}),
            )
        }
        Ok(false) => {}
        Err(e) => return e.into_response(),
    }
    if let Some(s) = &sess {
        if s.user.is_some() {
            let to = if ctx.is_broker_connected() {
                "/dashboard"
            } else {
                "/broker"
            };
            return ok(
                json!({"status": "success", "message": "Already logged in", "redirect": to}),
            );
        }
    }
    let username = form.non_empty("username").unwrap_or_default();
    let password = Secret::new(form.get("password").unwrap_or_default());
    let c2 = ctx.clone();
    let u2 = username.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        AuthService::verify_credentials(&c2, &u2, password.expose())
    })
    .await;
    match outcome {
        Ok(Ok(LoginOutcome::Success(user))) => {
            tracing::info!("Sign-in succeeded");
            crate::services::security_service::record_login(
                &ctx, &user, ip, "success", "password", None,
            );
            finish_sign_in(&ctx, sess.as_ref().map(|s| s.id.as_str()), &user).await
        }
        Ok(Ok(LoginOutcome::TotpRequired(user))) => {
            let now = ctx.now();
            let s = match &sess {
                Some(s) => ctx.sessions.rotate(&s.id, now),
                None => ctx.sessions.create(now),
            };
            ctx.sessions.update(&s.id, |x| {
                x.user = None;
                x.pending_totp_user = Some(user);
                x.pending_totp_started = Some(now);
            });
            with_cookie(
                ok(
                    json!({"status": "totp_required", "message": "Enter the 6-digit code from your authenticator app."}),
                ),
                &s.id,
            )
        }
        Ok(Ok(LoginOutcome::Invalid)) => {
            tracing::info!("Sign-in failed: invalid credentials");
            crate::services::security_service::record_login(
                &ctx,
                &username,
                ip,
                "failed",
                "password",
                Some("invalid_credentials"),
            );
            error(StatusCode::UNAUTHORIZED, "Invalid credentials")
        }
        Ok(Err(e)) => e.into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Login failed. Please try again.",
        ),
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

/// POST /auth/login/totp (json: totp_code)
pub async fn login_totp(
    State(ctx): Ctx,
    Sess(sess): Sess,
    ClientIp(ip): ClientIp,
    form: FormData,
) -> Response {
    if let Some(r) = login_limited(&ctx, ip) {
        return r;
    }
    let now = ctx.now();
    let Some(s) = sess else {
        return error(StatusCode::UNAUTHORIZED, "No pending login. Sign in first.");
    };
    let fresh = s
        .pending_totp_started
        .map(|t| (now - t).num_seconds() <= PENDING_TOTP_SECS)
        .unwrap_or(false);
    if !fresh {
        ctx.sessions.update(&s.id, |x| {
            x.pending_totp_user = None;
            x.pending_totp_started = None;
        });
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"status": "error", "message": "Login session expired. Please sign in again.", "redirect": "/login"}),
        );
    }
    let Some(user) = s.pending_totp_user.clone() else {
        return error(StatusCode::UNAUTHORIZED, "No pending login. Sign in first.");
    };
    let code = form.non_empty("totp_code").unwrap_or_default();
    if code.is_empty() {
        return error(StatusCode::BAD_REQUEST, "TOTP code is required.");
    }
    match AuthService::verify_totp_for(&ctx, &user, &code) {
        Ok(true) => {
            ctx.sessions
                .update(&s.id, |x| x.totp_verified_at = Some(now));
            crate::services::security_service::record_login(
                &ctx, &user, ip, "success", "totp", None,
            );
            finish_sign_in(&ctx, Some(&s.id), &user).await
        }
        Ok(false) => {
            crate::services::security_service::record_login(
                &ctx,
                &user,
                ip,
                "failed",
                "totp",
                Some("invalid_totp"),
            );
            error(StatusCode::UNAUTHORIZED, "Invalid TOTP code.")
        }
        Err(e) => e.into_response(),
    }
}

/// GET /auth/session-status
pub async fn session_status(State(ctx): Ctx, Sess(sess): Sess) -> Response {
    let key_mode = ctx.security.mode();
    let Some(user) = sess.and_then(|s| s.user) else {
        return ok(json!({
            "status": "success", "message": "Not authenticated",
            "authenticated": false, "logged_in": false, "key_mode": key_mode,
        }));
    };
    let active = ctx.sessions.authenticated_count();
    match ctx.get_broker_session() {
        Some(b) => {
            let api_key = ApiKeyService::current(&ctx).ok().flatten();
            ok(json!({
                "status": "success", "authenticated": true, "logged_in": true,
                "user": user, "broker": b.broker_id,
                "api_key": api_key.as_ref().map(|k| k.expose()),
                "active_sessions": active, "key_mode": key_mode,
            }))
        }
        None if BrokerAuthService::had_revoked_session(&ctx) => ok(json!({
            "status": "success", "authenticated": true, "logged_in": false,
            "user": user, "broker": Value::Null, "broker_session_expired": true,
            "active_sessions": active, "key_mode": key_mode,
        })),
        None => ok(json!({
            "status": "success", "authenticated": true, "logged_in": false,
            "user": user, "broker": Value::Null, "active_sessions": active, "key_mode": key_mode,
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

/// GET|POST /auth/logout. Revokes the stored broker token, ends every
/// browser session, tells every window.
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
    ctx.sessions.clear();
    if signed_in {
        if let Err(e) =
            BrokerAuthService::revoke(&ctx, crate::events::SessionEndReason::Logout).await
        {
            tracing::error!("Could not revoke the broker session on logout: {}", e);
        }
        ctx.bus.publish(Event::ForceLogout {
            message: "You have been logged out from another device.".into(),
        });
        tracing::info!("Signed out");
    }
    let mut resp = if method == Method::POST {
        ok(json!({"status": "success", "message": "Logged out successfully"}))
    } else {
        redirect("/login")
    };
    resp.headers_mut()
        .append(header::SET_COOKIE, clear_session_cookie());
    resp
}

fn hash_token(t: &str) -> String {
    hex::encode(Sha256::digest(t.as_bytes()))
}

/// POST /auth/reset-password (steps: email, select_totp, select_email, totp, password)
pub async fn reset_password(
    State(ctx): Ctx,
    Sess(sess): Sess,
    ClientIp(ip): ClientIp,
    form: FormData,
) -> Response {
    if ctx
        .limiter
        .check(Bucket::Reset, ip, Instant::now())
        .is_err()
    {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many password reset attempts. Please wait and try again later.",
        );
    }
    let Some(s) = sess else {
        return error(StatusCode::BAD_REQUEST, "Invalid or expired reset token.");
    };
    let step = form.get("step").unwrap_or_default().to_string();
    let email = form.non_empty("email").unwrap_or_default();
    match step.as_str() {
        "email" => ok(json!({"status": "success", "message": "Email verified"})),
        "select_totp" => ok(json!({"status": "success", "method": "totp"})),
        "select_email" => error(
            StatusCode::BAD_REQUEST,
            "Email reset is not available. Please use TOTP authentication.",
        ),
        "totp" => {
            if !ctx.security.is_unlocked() {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Password reset with an authenticator is not available on this computer because OpenAlgo protects its keys with your password. Use Reset account instead.",
                );
            }
            let code = form.non_empty("totp_code").unwrap_or_default();
            match AuthService::verify_totp_by_email(&ctx, &email, &code) {
                Ok(true) => {
                    let token = random_token();
                    let h = hash_token(&token);
                    ctx.sessions.update(&s.id, |x| {
                        x.reset_token_hash = Some(h);
                        x.reset_email = Some(email.clone());
                    });
                    ok(json!({"status": "success", "message": "TOTP verified", "token": token}))
                }
                _ => error(
                    StatusCode::BAD_REQUEST,
                    "Invalid TOTP code. Please try again.",
                ),
            }
        }
        "password" => {
            let token = form.get("token").unwrap_or_default();
            let valid = s
                .reset_token_hash
                .as_deref()
                .map(|h| crate::server::form::tokens_match(&hash_token(token), h))
                .unwrap_or(false)
                && s.reset_email.as_deref() == Some(email.as_str());
            if token.is_empty() || !valid {
                return error(StatusCode::BAD_REQUEST, "Invalid or expired reset token.");
            }
            let password = Secret::new(form.get("password").unwrap_or_default());
            match AuthService::reset_password(&ctx, &email, password.expose()) {
                Ok(()) => {
                    ctx.bus.publish(Event::ForceLogout {
                        message:
                            "Your password was reset. Please log in again with the new password."
                                .into(),
                    });
                    ok(
                        json!({"status": "success", "message": "Your password has been reset successfully."}),
                    )
                }
                Err(e) => error(StatusCode::BAD_REQUEST, e.client_message()),
            }
        }
        _ => error(StatusCode::BAD_REQUEST, "Invalid step"),
    }
}

/// POST /auth/change-password (form: old_password, new_password, confirm_password)
pub async fn change_password_api(State(ctx): Ctx, User(u): User, form: FormData) -> Response {
    let (Some(old), Some(new), Some(confirm)) = (
        form.non_empty("old_password"),
        form.non_empty("new_password"),
        form.non_empty("confirm_password"),
    ) else {
        return error(StatusCode::BAD_REQUEST, "All fields are required");
    };
    change_password_common(
        &ctx,
        &u.username,
        &old,
        &new,
        &confirm,
        "Password changed successfully",
    )
    .await
}

/// POST /auth/change (json or form: old_password|current_password, new_password[, confirm_password])
pub async fn change_password_legacy(State(ctx): Ctx, User(u): User, form: FormData) -> Response {
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
    username: &str,
    old: &str,
    new: &str,
    confirm: &str,
    success: &str,
) -> Response {
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
            ctx.bus.publish(Event::ForceLogout {
                message: "Your password was changed. Please log in again with the new password."
                    .into(),
            });
            let mut resp = ok(json!({"status": "success", "message": success}));
            resp.headers_mut()
                .append(header::SET_COOKIE, clear_session_cookie());
            resp
        }
        Ok(Err(e)) => error(StatusCode::BAD_REQUEST, e.client_message()),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to change password",
        ),
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
    let new_mode = match ctx.sqlite.get_analyze_mode() {
        Ok(m) => !m,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = crate::services::AnalyzerService::toggle_mode(&ctx, new_mode) {
        return e.into_response();
    }
    ok(json!({"status": "success", "data": {
        "mode": if new_mode {"analyze"} else {"live"},
        "analyze_mode": new_mode,
        "message": format!("Switched to {} mode", if new_mode {"Analyze"} else {"Live"}),
    }}))
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
            ok(json!({"status": "success", "data": {
                "username": row.username, "smtp_settings": Value::Null,
                "qr_code": qr, "totp_secret": sec,
            }}))
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
    form: FormData,
) -> Response {
    let code = form.non_empty("totp_code").unwrap_or_default();
    if code.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "TOTP code is required to change 2FA settings.",
        );
    }
    match AuthService::verify_totp_for(&ctx, &u.username, &code) {
        Ok(true) => {}
        Ok(false) => return error(StatusCode::UNAUTHORIZED, "Invalid TOTP code."),
        Err(e) => return e.into_response(),
    }
    let flag = |k: &str| matches!(form.get(k), Some("true") | Some("1") | Some("on"));
    let enabled = flag("totp_enabled");
    let f = crate::db::sqlite::user::TwoFactorFlags {
        enabled,
        login: enabled && flag("totp_required_for_login"),
        password_reset: enabled && flag("totp_required_for_password_reset"),
        mcp: enabled && flag("totp_required_for_mcp"),
    };
    let row = match AuthService::profile(&ctx, &u.username) {
        Ok(Some((r, _))) => r,
        Ok(None) => return error(StatusCode::NOT_FOUND, "User not found."),
        Err(e) => return e.into_response(),
    };
    let saved = ctx
        .sqlite
        .conn()
        .and_then(|c| crate::db::sqlite::user::set_two_factor(&c, row.id, f));
    if let Err(e) = saved {
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

/// GET /auth/active-sessions
pub async fn active_sessions(State(ctx): Ctx) -> Response {
    ok(json!({
        "status": "success",
        "count": ctx.sessions.authenticated_count(),
        "current_session_id": Value::Null,
        "sessions": [],
    }))
}
