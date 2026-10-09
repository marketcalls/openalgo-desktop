//! Web `blueprints/security.py`: blocked addresses, 404 and invalid API
//! key tracking, thresholds, login activity and active sessions.
//! Errors keep the web's `{"error": ...}` shape; successes `{"success": true}`.

use crate::db::sqlite::monitor as store;
use crate::db::sqlite::webui::{self, SecuritySettings};
use crate::server::envelope::json_response;
use crate::server::middleware::User;
use crate::server::routes::webui::{failed, ok, JsonBody};
use crate::services::security_service;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Shown when a trader tries to ban the identity every tunnel caller shares.
const TUNNEL_IDENTITY: &str = "This address stands for every request that comes through your tunnel, including your own alerts, so it cannot be banned. Ban the caller's own address instead.";

type Ctx = State<Arc<AppState>>;

fn err(status: StatusCode, msg: &str) -> Response {
    json_response(status, json!({"error": msg}))
}

fn internal(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("{} failed: {}", what, e);
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong. Try again.",
    )
}

fn valid_hostname(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

/// POST /security/ban (json: ip_address, reason, duration_hours, permanent)
pub async fn ban(State(ctx): Ctx, body: JsonBody) -> Response {
    let ip = body.str("ip_address").unwrap_or_default();
    let reason = body
        .non_empty("reason")
        .unwrap_or_else(|| "Manual ban".into());
    let hours = match body.int("duration_hours") {
        Ok(h) => h.unwrap_or(24),
        Err(_) => return err(StatusCode::BAD_REQUEST, "Enter the ban length in hours."),
    };
    let permanent = body.bool("permanent");
    if ip.is_empty() {
        return err(StatusCode::BAD_REQUEST, "IP address is required");
    }
    // One spelling per address, as every ban check compares.
    let Some(ip) = crate::server::addr::canonical_text(&ip) else {
        return err(StatusCode::BAD_REQUEST, "Invalid IP address format");
    };
    if store::is_loopback_ip(&ip) {
        return err(StatusCode::BAD_REQUEST, "Cannot ban localhost");
    }
    if store::never_banned(&ip) {
        return err(StatusCode::BAD_REQUEST, TUNNEL_IDENTITY);
    }
    let now = ctx.now();
    let limit = ctx.monitor.security_settings(&ctx).repeat_offender_limit;
    let r = ctx.logs.conn().and_then(|c| {
        store::ban_ip(
            &c,
            &ip,
            &reason,
            Some(hours.clamp(1, 8_760)),
            permanent,
            "manual",
            now,
            limit,
        )
    });
    match r {
        Ok(true) => {
            ctx.monitor.reload_bans(&ctx);
            tracing::info!("Address blocked by the trader");
            ok(json!({"success": true, "message": format!("IP {} has been banned", ip)}))
        }
        Ok(false) => err(StatusCode::INTERNAL_SERVER_ERROR, "Failed to ban IP"),
        Err(e) => internal("Blocking an address", e),
    }
}

/// POST /security/unban (json: ip_address)
pub async fn unban(State(ctx): Ctx, body: JsonBody) -> Response {
    let ip = body.str("ip_address").unwrap_or_default();
    if ip.is_empty() {
        return err(StatusCode::BAD_REQUEST, "IP address is required");
    }
    let ip = crate::server::addr::canonical_text(&ip).unwrap_or(ip);
    match ctx.logs.conn().and_then(|c| store::unban_ip(&c, &ip)) {
        Ok(true) => {
            ctx.monitor.reload_bans(&ctx);
            ok(json!({"success": true, "message": format!("IP {} has been unbanned", ip)}))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "IP not found in ban list"),
        Err(e) => internal("Unblocking an address", e),
    }
}

/// POST /security/ban-host (json: host, reason, permanent)
pub async fn ban_host(State(ctx): Ctx, body: JsonBody) -> Response {
    let host = body.str("host").unwrap_or_default();
    if host.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Host is required");
    }
    let reason = body
        .non_empty("reason")
        .unwrap_or_else(|| format!("Host ban: {}", host));
    let permanent = body.bool("permanent");
    let hours = (!permanent).then_some(24);
    let now = ctx.now();
    let limit = ctx.monitor.security_settings(&ctx).repeat_offender_limit;
    if let Some(host) = crate::server::addr::canonical_text(&host) {
        if store::is_loopback_ip(&host) {
            return err(StatusCode::BAD_REQUEST, "Cannot ban localhost");
        }
        if store::never_banned(&host) {
            return err(StatusCode::BAD_REQUEST, TUNNEL_IDENTITY);
        }
        let r = ctx.logs.conn().and_then(|c| {
            store::ban_ip(
                &c,
                &host,
                &format!("Manual ban: {}", reason),
                hours,
                permanent,
                "manual",
                now,
                limit,
            )
        });
        return match r {
            Ok(true) => {
                ctx.monitor.reload_bans(&ctx);
                ok(json!({"success": true, "message": format!("Banned IP: {}", host)}))
            }
            Ok(false) => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Failed to ban IP: {}", host),
            ),
            Err(e) => internal("Blocking an address", e),
        };
    }
    if !valid_hostname(&host) {
        return err(StatusCode::BAD_REQUEST, "Invalid hostname format");
    }
    ctx.monitor.drain_now(&ctx);
    let r = ctx.logs.conn().and_then(|c| {
        let ips = store::traffic_ips_for_host(&c, &host)?;
        let mut n = 0;
        for ip in &ips {
            if !store::never_banned(ip)
                && store::ban_ip(
                    &c,
                    ip,
                    &format!("Host ban: {} - {}", host, reason),
                    hours,
                    permanent,
                    "host_ban",
                    now,
                    limit,
                )?
            {
                n += 1;
            }
        }
        Ok((ips.len(), n))
    });
    match r {
        Ok((0, _)) => json_response(
            StatusCode::NOT_FOUND,
            json!({
                "error": format!("No traffic found from host: {}. To ban specific IPs, use the IP ban form instead.", host),
                "suggestion": "Use the Manual IP Ban form above to ban specific IP addresses directly.",
            }),
        ),
        Ok((_, n)) => {
            ctx.monitor.reload_bans(&ctx);
            ok(
                json!({"success": true, "message": format!("Banned {} IPs associated with host: {}", n, host)}),
            )
        }
        Err(e) => internal("Blocking a host", e),
    }
}

/// POST /security/clear-404 (json: ip_address)
pub async fn clear_404(State(ctx): Ctx, body: JsonBody) -> Response {
    let ip = body.str("ip_address").unwrap_or_default();
    if ip.is_empty() {
        return err(StatusCode::BAD_REQUEST, "IP address is required");
    }
    ctx.monitor.drain_now(&ctx);
    match ctx.logs.conn().and_then(|c| store::clear_404(&c, &ip)) {
        Ok(true) => {
            ok(json!({"success": true, "message": format!("404 tracker cleared for {}", ip)}))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "No tracker found for this IP"),
        Err(e) => internal("Clearing the 404 tracker", e),
    }
}

/// GET /security/api/data
pub async fn data(State(ctx): Ctx) -> Response {
    ctx.monitor.drain_now(&ctx);
    match security_service::dashboard_data(&ctx) {
        Ok(v) => ok(v),
        Err(e) => {
            tracing::error!("Security data failed: {}", e);
            ok(json!({
                "banned_ips": [], "suspicious_ips": [], "api_abuse_ips": [],
                "security_settings": SecuritySettings::default(),
            }))
        }
    }
}

/// GET /security/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    ctx.monitor.drain_now(&ctx);
    match security_service::stats(&ctx) {
        Ok(v) => ok(v),
        Err(e) => internal("Security stats", e),
    }
}

/// POST /security/settings
pub async fn settings(State(ctx): Ctx, body: JsonBody) -> Response {
    let get = |k: &str, d: i64| body.int(k).map(|v| v.unwrap_or(d));
    let (t404, d404, tapi, dapi, rep) = match (
        get("threshold_404", 100),
        get("ban_duration_404", 0),
        get("threshold_api", 100),
        get("ban_duration_api", 0),
        get("repeat_offender_limit", 2),
    ) {
        (Ok(a), Ok(b), Ok(c), Ok(d), Ok(e)) => (a, b, c, d, e),
        _ => return err(StatusCode::BAD_REQUEST, "Invalid numeric value provided"),
    };
    if !(1..=1000).contains(&t404) {
        return err(
            StatusCode::BAD_REQUEST,
            "404 threshold must be between 1 and 1000",
        );
    }
    let dur_ok = |d: i64| d == 0 || (1..=8_760).contains(&d);
    if !dur_ok(d404) || !dur_ok(dapi) {
        return err(
            StatusCode::BAD_REQUEST,
            "Ban duration must be Permanent (0) or between 1 hour and 1 year",
        );
    }
    if !(1..=100).contains(&tapi) {
        return err(
            StatusCode::BAD_REQUEST,
            "API threshold must be between 1 and 100",
        );
    }
    if !(1..=10).contains(&rep) {
        return err(
            StatusCode::BAD_REQUEST,
            "Repeat offender limit must be between 1 and 10",
        );
    }
    let s = SecuritySettings {
        auto_ban_enabled: body.bool("auto_ban_enabled"),
        threshold_404: t404,
        ban_duration_404: d404,
        api_threshold: tapi,
        api_ban_duration: dapi,
        repeat_offender_limit: rep,
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| webui::set_security_settings(&c, &s))
    {
        Ok(()) => {
            ctx.monitor.invalidate_settings();
            tracing::info!("Security settings updated");
            ok(json!({
                "success": true,
                "message": "Security settings updated successfully",
                "settings": s,
            }))
        }
        Err(e) => internal("Saving security settings", e),
    }
}

/// GET /security/api/login-activity?status=&limit=
pub async fn login_activity(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let status = q
        .get("status")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty());
    match security_service::login_activity(&ctx, limit, status) {
        Ok(v) => ok(json!({"status": "success", "attempts": v})),
        Err(e) => {
            tracing::error!("Login activity failed: {}", e);
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"status": "error", "attempts": []}),
            )
        }
    }
}

/// POST /security/api/login-activity/clear
pub async fn clear_login_activity(State(ctx): Ctx) -> Response {
    match ctx
        .logs
        .conn()
        .and_then(|c| store::clear_login_attempts(&c))
    {
        Ok(_) => ok(json!({"status": "success", "message": "Login history cleared"})),
        Err(e) => failed("Clearing login history", e, "Failed to clear history"),
    }
}

/// GET /security/api/active-sessions
pub async fn active_sessions(State(ctx): Ctx, User(u): User) -> Response {
    ok(security_service::active_sessions(&ctx, &u.session_id))
}
