//! Session routes for MCP: the admin page's `/admin/api/mcp/*` and
//! `/admin/api/oauth/clients*` (web `blueprints/admin.py`), and the desktop's
//! token control on the API key page (`/api/mcp/tokens`) with the
//! ready-to-paste client configuration (`/api/mcp/client-config`).
//!
//! Every route here requires the signed-in user (declared in the route
//! table); the writes also pass the session layer's CSRF check.

use super::store::{self, AuditQuery, Settings, TokenScope};
use crate::server::envelope::json_response;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// Placeholder for the token in the client configuration.
pub const TOKEN_PLACEHOLDER: &str = "<MCP_TOKEN>";
const AUDIT_MAX_LIMIT: i64 = 500;
const KILL_CONFIRM: &str = "REVOKE_ALL_MCP_TOKENS";

fn ok(v: Value) -> Response {
    json_response(StatusCode::OK, v)
}

fn err(status: StatusCode, msg: &str) -> Response {
    json_response(status, json!({"status": "error", "message": msg}))
}

fn failed(e: impl std::fmt::Display, what: &str, msg: &str) -> Response {
    tracing::error!("MCP {}: {}", what, e);
    err(StatusCode::INTERNAL_SERVER_ERROR, msg)
}

fn body_object(b: &Bytes) -> Option<Map<String, Value>> {
    if b.is_empty() {
        return Some(Map::new());
    }
    match serde_json::from_slice::<Value>(b) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// GET /admin/api/mcp/audit
pub async fn audit(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, AUDIT_MAX_LIMIT);
    let cut = |k: &str, n: usize| {
        q.get(k)
            .map(|v| v.trim().chars().take(n).collect())
            .unwrap_or_default()
    };
    let query = AuditQuery {
        limit,
        tool: cut("tool", 100),
        scope: cut("scope", 50),
        outcome: cut("outcome", 50),
    };
    match ctx.logs.conn().and_then(|c| store::audit_tail(&c, &query)) {
        Ok((rows, scanned, total)) => {
            let data: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
            let mut r = ok(json!({
                "status": "success",
                "mcp_enabled": true,
                "count": data.len(),
                "data": data,
                "scanned": scanned,
                "total_in_window": total,
            }));
            r.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0"),
            );
            r
        }
        Err(e) => failed(e, "audit read failed", "Failed to read audit log."),
    }
}

/// POST /admin/api/mcp/kill-switch: revoke every token and withdraw the
/// write scope, so no AI client can place, modify or cancel orders until it
/// is turned back on in the settings.
pub async fn kill_switch(State(ctx): Ctx, body: Bytes) -> Response {
    let data = body_object(&body).unwrap_or_default();
    if data.get("confirm").and_then(Value::as_str) != Some(KILL_CONFIRM) {
        return err(
            StatusCode::BAD_REQUEST,
            "Kill switch requires confirm=\"REVOKE_ALL_MCP_TOKENS\".",
        );
    }
    let r = ctx.sqlite.conn().and_then(|c| {
        let n = store::revoke_all(&c, ctx.now())?;
        let mut s = store::settings(&c)?;
        s.write_scope_enabled = false;
        store::save_settings(&c, &s)?;
        Ok(n)
    });
    match r {
        Ok(n) => {
            tracing::warn!("MCP kill switch: {} tokens revoked, order tools off", n);
            ok(json!({"status": "success", "tokens_revoked": n}))
        }
        Err(e) => failed(e, "kill switch failed", "Failed to execute kill switch."),
    }
}

/// GET /admin/api/mcp/settings
pub async fn settings_get(State(ctx): Ctx) -> Response {
    match ctx.sqlite.conn().and_then(|c| store::settings(&c)) {
        Ok(s) => ok(json!({"status": "success", "settings": s.to_json()})),
        Err(e) => failed(e, "settings read failed", "Failed to read MCP settings."),
    }
}

fn https_url_ok(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    let hostport = &rest[..host_end];
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (hostport, None),
    };
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && port.is_none_or(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// PUT /admin/api/mcp/settings: saved and applied at once (the desktop has
/// no service to restart).
pub async fn settings_put(State(ctx): Ctx, body: Bytes) -> Response {
    let Some(data) = body_object(&body) else {
        return err(StatusCode::BAD_REQUEST, "Body must be JSON object.");
    };
    for k in ["http_enabled", "require_approval", "write_scope_enabled"] {
        if data.get(k).is_some_and(|v| !v.is_boolean()) {
            return err(StatusCode::BAD_REQUEST, &format!("{} must be boolean.", k));
        }
    }
    let public_url = match data.get("public_url") {
        None => None,
        Some(Value::String(s)) => {
            let u = s.trim().trim_end_matches('/').to_string();
            if !u.is_empty() && !https_url_ok(&u) {
                return err(
                    StatusCode::BAD_REQUEST,
                    "public_url must be HTTPS (e.g. https://yourdomain.com).",
                );
            }
            Some(u)
        }
        Some(_) => return err(StatusCode::BAD_REQUEST, "public_url must be string."),
    };
    let b = |k: &str| data.get(k).and_then(Value::as_bool);
    let r = ctx.sqlite.conn().and_then(|c| {
        let cur = store::settings(&c)?;
        let next = Settings {
            http_enabled: b("http_enabled").unwrap_or(cur.http_enabled),
            public_url: public_url.clone().unwrap_or(cur.public_url),
            require_approval: b("require_approval").unwrap_or(cur.require_approval),
            write_scope_enabled: b("write_scope_enabled").unwrap_or(cur.write_scope_enabled),
        };
        store::save_settings(&c, &next)?;
        Ok(next)
    });
    match r {
        Ok(s) => {
            tracing::info!(
                "MCP settings saved: remote={} write_scope={}",
                s.http_enabled,
                s.write_scope_enabled
            );
            ok(
                json!({"status": "success", "restart_required": false, "settings_pending": s.to_json()}),
            )
        }
        Err(e) => failed(e, "settings save failed", "Failed to save MCP settings."),
    }
}

/// GET /admin/api/oauth/clients: hosted-client sign-in (OAuth) is not in
/// the desktop yet, so there are no clients to list.
pub async fn oauth_clients() -> Response {
    ok(json!({
        "status": "success",
        "mcp_enabled": true,
        "clients": [],
        "summary": {"pending": 0, "approved": 0, "revoked": 0},
    }))
}

/// POST /admin/api/oauth/clients/{id}/approve and .../revoke
pub async fn oauth_client_action(Path(_id): Path<String>) -> Response {
    err(StatusCode::NOT_FOUND, "Client not found.")
}

// ----------------------------------------------------------------------
// Tokens (API key page)
// ----------------------------------------------------------------------

/// GET /api/mcp/tokens
pub async fn tokens_list(State(ctx): Ctx) -> Response {
    match ctx.sqlite.conn().and_then(|c| store::list_tokens(&c)) {
        Ok(rows) => ok(json!({
            "status": "success",
            "data": rows.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        })),
        Err(e) => failed(
            e,
            "token list failed",
            "Could not load your AI client tokens. Try again.",
        ),
    }
}

/// POST /api/mcp/tokens {name, scope: "read" | "read_write"}: the token is
/// in this reply only.
pub async fn tokens_create(State(ctx): Ctx, body: Bytes) -> Response {
    let Some(data) = body_object(&body) else {
        return err(
            StatusCode::BAD_REQUEST,
            "Send the token name and access level.",
        );
    };
    let name: String = data
        .get("name")
        .and_then(Value::as_str)
        .map(|s| s.trim().chars().take(64).collect())
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "AI client".into());
    let Some(scope) = data
        .get("scope")
        .and_then(Value::as_str)
        .and_then(TokenScope::parse)
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "Choose an access level: read only, or read and place orders.",
        );
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::create_token(&c, &name, scope, ctx.now()))
    {
        Ok((row, token)) => {
            tracing::info!("MCP token {} created ({})", row.id, scope.as_str());
            ok(json!({
                "status": "success",
                "token": token,
                "data": row.to_json(),
                "client_config": client_config(&ctx, &token),
            }))
        }
        Err(e) => failed(
            e,
            "token create failed",
            "Could not create the token. Try again.",
        ),
    }
}

/// DELETE /api/mcp/tokens/{id}
pub async fn tokens_revoke(State(ctx): Ctx, Path(id): Path<i64>) -> Response {
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::revoke_token(&c, id, ctx.now()))
    {
        Ok(true) => {
            tracing::info!("MCP token {} revoked", id);
            ok(json!({"status": "success"}))
        }
        Ok(false) => err(StatusCode::NOT_FOUND, "That token was already revoked."),
        Err(e) => failed(
            e,
            "token revoke failed",
            "Could not revoke the token. Try again.",
        ),
    }
}

/// GET /api/mcp/client-config
pub async fn client_config_get(State(ctx): Ctx) -> Response {
    let mut v = client_config(&ctx, TOKEN_PLACEHOLDER);
    v["status"] = json!("success");
    ok(v)
}

fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else if s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The environment variable the AppImage runtime sets to the `.AppImage`
/// file being run (set by the OS packaging runtime, never by the trader; it
/// carries no secret).
const APPIMAGE_ENV: &str = "APPIMAGE";

/// The program an MCP client should launch. Run from a Linux AppImage, the
/// binary sits in a temporary mount (`/tmp/.mount_XXXX/...`) that changes at
/// every launch, so a client configured with it stops working after the
/// next start; the `.AppImage` file itself stays put and runs the same
/// binary. `appimage` is used only when it is an absolute path to a file.
fn mcp_executable(
    current_exe: Option<&std::path::Path>,
    appimage: Option<&std::path::Path>,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> String {
    if let Some(a) = appimage.filter(|a| a.is_absolute() && is_file(a)) {
        return a.to_string_lossy().into_owned();
    }
    current_exe
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Ready-to-paste configuration for Claude Desktop and Claude Code with the
/// running executable's real path (the `.AppImage` file on a Linux
/// AppImage).
pub fn client_config(ctx: &AppState, token: &str) -> Value {
    let current = std::env::current_exe().ok();
    let appimage = if cfg!(target_os = "linux") {
        std::env::var_os(APPIMAGE_ENV).map(std::path::PathBuf::from)
    } else {
        None
    };
    let exe = mcp_executable(current.as_deref(), appimage.as_deref(), |p| p.is_file());
    let server_url = format!("http://127.0.0.1:{}", ctx.listening_port());
    let mcp_url = format!("{}/mcp", server_url);
    // The token travels in the client's `env` block, never in the command
    // line (visible in the process list and shell history).
    let env_var = super::stdio::TOKEN_ENV;
    let args = vec!["mcp", "--url", server_url.as_str()];
    let stdio_cmd = format!(
        "claude mcp add openalgo -e {} -- {} {}",
        shell_quote(&format!("{}={}", env_var, token)),
        shell_quote(&exe),
        args.iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    );
    json!({
        "executable": exe,
        "server_url": server_url,
        "mcp_url": mcp_url,
        "claude_desktop": {
            "mcpServers": {
                "openalgo": {"command": exe, "args": args, "env": {env_var: token}},
            },
        },
        "claude_code": stdio_cmd,
        "http": {
            "type": "http",
            "url": mcp_url,
            "headers": {"Authorization": format!("Bearer {}", token)},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls() {
        assert!(https_url_ok("https://example.com"));
        assert!(https_url_ok("https://a.example.com:8443/x"));
        assert!(!https_url_ok("http://example.com"));
        assert!(!https_url_ok("https://exa mple.com"));
        assert!(!https_url_ok("https://"));
    }

    #[test]
    fn appimage_file_is_launched_instead_of_its_temporary_mount() {
        use std::path::Path;
        let mount = Path::new("/tmp/.mount_OpenAlXYZ/usr/bin/openalgo-desktop");
        let image = Path::new("/home/trader/Apps/OpenAlgo_1.0.0_amd64.AppImage");
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        if cfg!(windows) {
            // Unix paths are not absolute on Windows; the rule is the same.
            return;
        }
        assert_eq!(
            mcp_executable(Some(mount), Some(image), yes),
            image.to_string_lossy()
        );
        // Not an AppImage (deb, macOS, Windows): the running binary.
        assert_eq!(
            mcp_executable(Some(mount), None, yes),
            mount.to_string_lossy()
        );
        // A stale or relative value is ignored.
        assert_eq!(
            mcp_executable(Some(mount), Some(image), no),
            mount.to_string_lossy()
        );
        assert_eq!(
            mcp_executable(Some(mount), Some(Path::new("OpenAlgo.AppImage")), yes),
            mount.to_string_lossy()
        );
        assert_eq!(mcp_executable(None, None, yes), "");
    }

    #[test]
    fn quoting() {
        if !cfg!(windows) {
            assert_eq!(shell_quote("/usr/bin/x"), "/usr/bin/x");
            assert_eq!(
                shell_quote("/Applications/Open Algo"),
                "'/Applications/Open Algo'"
            );
        }
    }
}
