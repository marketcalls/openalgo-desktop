//! Web `blueprints/settings.py` (`/settings/analyze-mode`) and the desktop
//! Server Settings page (`/settings/api/server`, also served at the older
//! `/api/desktop/settings`): the listen addresses that OpenAlgo web reads
//! from `.env`.

use crate::config::{validate_bind_host, ServerConfigUpdate};
use crate::server::envelope::error;
use crate::server::routes::webui::{failed, ok, JsonBody};
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// GET /settings/analyze-mode
pub async fn analyze_mode(State(ctx): Ctx) -> Response {
    match ctx.sqlite.get_analyze_mode() {
        Ok(m) => ok(json!({"analyze_mode": m})),
        Err(e) => {
            tracing::error!("Reading analyzer mode failed: {}", e);
            crate::server::envelope::json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "Failed to get analyze mode"}),
            )
        }
    }
}

fn server_data(ctx: &AppState) -> Value {
    let cfg = ctx.server_config();
    let ws = ctx.feed_status.read().clone();
    json!({
        "http_host": cfg.bind_host,
        "http_port": cfg.http_port,
        "ws_host": cfg.bind_host,
        "ws_port": cfg.ws_port,
        "lan_enabled": !cfg.is_loopback(),
        // The market data listener's state; a taken port carries the fix.
        "ws_status": ws_status(&ws),
    })
}

/// `{state, message}` of the market data listener for the settings page
/// (`message` is null while it runs).
pub fn ws_status(st: &crate::state::ServerStatus) -> Value {
    use crate::state::ServerStatus as S;
    match st {
        S::Running { host, port } => json!({
            "state": "running", "host": host, "port": port, "message": null,
        }),
        S::Starting => json!({"state": "starting", "message": null}),
        S::PortInUse { port, message } => json!({
            "state": "port_in_use", "port": port, "message": message,
        }),
        S::Failed { message } => json!({"state": "failed", "message": message}),
    }
}

/// GET /settings/api/server
pub async fn get_server(State(ctx): Ctx) -> Response {
    ok(json!({"status": "success", "data": server_data(&ctx)}))
}

fn is_loopback_host(h: &str) -> bool {
    matches!(h, "127.0.0.1" | "localhost" | "::1")
}

/// Trader-facing reason a port cannot be used, or None when it is free.
/// The ports this app itself holds are free to keep.
pub fn port_problem(host: &str, port: u16, own: &[u16]) -> Option<String> {
    if own.contains(&port) {
        return None;
    }
    let bind_host = if is_loopback_host(host) {
        "127.0.0.1"
    } else {
        host
    };
    // Address reuse lets a bind succeed next to a listener on another
    // address of the same port on some systems, so also see whether
    // anything answers there.
    let answering = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let taken =
        std::net::TcpStream::connect_timeout(&answering, std::time::Duration::from_millis(300))
            .is_ok();
    let bound = if taken {
        Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))
    } else {
        std::net::TcpListener::bind((bind_host, port)).map(drop)
    };
    match bound {
        Ok(()) => None,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let hint = if cfg!(target_os = "macos") && port == 5000 {
                " On a Mac this is usually AirPlay Receiver: turn it off in System Settings, General, AirDrop and Handoff, or choose another port."
            } else {
                " Close the program using it (for example OpenAlgo web), or choose another port."
            };
            Some(format!(
                "Port {} is already used by another program.{}",
                port, hint
            ))
        }
        Err(_) => Some(format!(
            "OpenAlgo cannot use port {} on this computer. Choose another port between 1024 and 65535.",
            port
        )),
    }
}

#[allow(clippy::result_large_err)]
fn parse_port(body: &JsonBody, k: &str, label: &str) -> Result<u16, Response> {
    match body.int(k) {
        Ok(Some(p)) if (1024..=65535).contains(&p) => Ok(p as u16),
        _ => Err(error(
            StatusCode::BAD_REQUEST,
            format!("Enter {} between 1024 and 65535.", label),
        )),
    }
}

/// What a saved change does and when (security review CFG-03). The market
/// data feed follows the saved address within seconds (its watcher rebinds
/// it, closing every streaming connection); the app's own listener moves
/// only when OpenAlgo restarts.
#[derive(Debug, PartialEq)]
pub struct SaveEffect {
    /// Changes in use within seconds.
    pub applied: Vec<&'static str>,
    /// Changes that wait for a restart.
    pub pending: Vec<&'static str>,
    pub message: String,
}

/// The effect of going from `before` to `after` (both as in use, with the
/// development override applied) while the app listens on `listening`.
pub fn save_effect(
    before: &crate::config::ServerConfig,
    after: &crate::config::ServerConfig,
    listening: (&str, u16),
) -> SaveEffect {
    let feed = |c: &crate::config::ServerConfig| {
        let host = if c.is_loopback() {
            "127.0.0.1".to_string()
        } else {
            c.bind_host.clone()
        };
        (host, c.ws_port)
    };
    let (was, now) = (feed(before), feed(after));
    let mut applied = Vec::new();
    if was.1 != now.1 {
        applied.push("ws_port");
    }
    if was.0 != now.0 {
        applied.push("ws_host");
    }
    let mut pending = Vec::new();
    if after.http_port != listening.1 {
        pending.push("http_port");
    }
    if after.bind_host != listening.0 {
        pending.push("http_host");
    }
    let mut message = String::from("Saved.");
    if !applied.is_empty() {
        message.push_str(&format!(
            " Live market data moves to {}:{} within a few seconds: trading platforms and SDK scripts that stream from OpenAlgo are disconnected and must reconnect to the new address.",
            now.0, now.1
        ));
    }
    if !pending.is_empty() {
        message.push_str(&format!(
            " The app address changes to {}:{} when you restart OpenAlgo.",
            after.bind_host, after.http_port
        ));
    }
    SaveEffect {
        applied,
        pending,
        message,
    }
}

/// POST /settings/api/server (json: http_host, http_port, ws_host, ws_port,
/// lan_enabled). The market data feed moves at once; the app's listener
/// after a restart. The answer lists which is which (`applied`, `pending`).
pub async fn save_server(State(ctx): Ctx, body: JsonBody) -> Response {
    let cfg = ctx.server_config();
    let http_port = match parse_port(&body, "http_port", "an app port") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let ws_port = match parse_port(&body, "ws_port", "a market data port") {
        Ok(p) => p,
        Err(r) => return r,
    };
    if http_port == ws_port {
        return error(
            StatusCode::BAD_REQUEST,
            "The app port and the market data port must be different.",
        );
    }
    let lan = body.bool("lan_enabled");
    let http_host = body
        .non_empty("http_host")
        .unwrap_or_else(|| cfg.bind_host.clone());
    let ws_host = body
        .non_empty("ws_host")
        .unwrap_or_else(|| http_host.clone());
    for h in [&http_host, &ws_host] {
        if validate_bind_host(h).is_err() {
            return error(
                StatusCode::BAD_REQUEST,
                "Enter an IP address such as 127.0.0.1 for this computer only, or 0.0.0.0 for every network.",
            );
        }
    }
    if http_host != ws_host {
        return error(
            StatusCode::BAD_REQUEST,
            "The market data address must be the same as the app address.",
        );
    }
    if !lan && !is_loopback_host(&http_host) {
        return error(
            StatusCode::BAD_REQUEST,
            "Turn on access from other devices to listen on an address other than 127.0.0.1.",
        );
    }
    let bind_host = if lan && is_loopback_host(&http_host) {
        "0.0.0.0".to_string()
    } else {
        http_host
    };
    let own = [ctx.listening_port(), cfg.http_port, cfg.ws_port];
    for p in [http_port, ws_port] {
        if let Some(msg) = port_problem(&bind_host, p, &own) {
            return error(StatusCode::BAD_REQUEST, msg);
        }
    }
    let update = ServerConfigUpdate {
        http_port: Some(http_port),
        ws_port: Some(ws_port),
        bind_host: Some(bind_host),
        ..Default::default()
    };
    let res = ctx
        .sqlite
        .conn()
        .and_then(|c| crate::config::save(&c, &update));
    match res {
        Ok(()) => {
            let _ = ctx.reload_config();
            let after = ctx.server_config();
            let listening = match &*ctx.server_status.read() {
                crate::state::ServerStatus::Running { host, port } => (host.clone(), *port),
                _ => (cfg.bind_host.clone(), cfg.http_port),
            };
            let effect = save_effect(&cfg, &after, (&listening.0, listening.1));
            tracing::info!(
                "Server settings saved (in use now: {:?}; after a restart: {:?})",
                effect.applied,
                effect.pending
            );
            ok(json!({
                "status": "success",
                "message": effect.message,
                "applied": effect.applied,
                "pending": effect.pending,
                "restart_required": !effect.pending.is_empty(),
                "data": server_data(&ctx),
            }))
        }
        Err(crate::error::AppError::Validation(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(e) => failed(
            "Saving server settings",
            e,
            "The server settings were not saved. Try again.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(host: &str, http: u16, ws: u16) -> crate::config::ServerConfig {
        crate::config::ServerConfig {
            bind_host: host.into(),
            http_port: http,
            ws_port: ws,
            ..Default::default()
        }
    }

    /// CFG-03: the answer says what moves now (the feed) and what waits for
    /// a restart (the app's listener).
    #[test]
    fn the_answer_separates_what_applies_now_from_after_a_restart() {
        let before = cfg("127.0.0.1", 5000, 8765);
        let listening = ("127.0.0.1", 5000);

        let e = save_effect(&before, &cfg("127.0.0.1", 5000, 8770), listening);
        assert_eq!((e.applied, e.pending), (vec!["ws_port"], vec![]));
        assert!(e.message.contains("moves to 127.0.0.1:8770 within a few seconds"));
        assert!(!e.message.contains("restart"));

        let e = save_effect(&before, &cfg("127.0.0.1", 5100, 8765), listening);
        assert_eq!((e.applied, e.pending), (vec![], vec!["http_port"]));
        assert!(e.message.contains("changes to 127.0.0.1:5100 when you restart"));

        let e = save_effect(&before, &cfg("0.0.0.0", 5000, 8765), listening);
        assert_eq!((e.applied, e.pending), (vec!["ws_host"], vec!["http_host"]));

        let e = save_effect(&before, &before, listening);
        assert_eq!(
            e,
            SaveEffect {
                applied: vec![],
                pending: vec![],
                message: "Saved.".into()
            }
        );
    }

    #[test]
    fn a_held_port_is_reported_and_an_own_port_is_not() {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let msg = port_problem("127.0.0.1", port, &[]).unwrap();
        assert!(msg.contains(&port.to_string()));
        assert!(msg.contains("already used"));
        assert!(port_problem("127.0.0.1", port, &[port]).is_none());
    }
}
