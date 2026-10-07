//! Tauri commands: only what the shell alone can do. Everything else is HTTP.
//!
//! | Command          | Who may call          | Why                                         |
//! |------------------|-----------------------|---------------------------------------------|
//! | `startup_status` | anyone                | the start-up page shows why the server is not running (no secrets) |
//! | `retry_server`   | anyone, only while the server is NOT running | recover from a taken port before any page can load |
//! | `restart_server` | signed-in user        | apply new port / LAN settings               |
//! | `open_external`  | signed-in user        | open an http(s) link in the system browser  |

use crate::error::{AppError, Result};
use crate::server::{self, ServerHandle};
use crate::state::{AppState, ServerStatus};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

pub struct ShellState {
    pub ctx: Arc<AppState>,
    pub server: tokio::sync::Mutex<Option<ServerHandle>>,
}

/// The guard every non-bootstrap command calls first.
pub fn require_user(ctx: &AppState) -> Result<String> {
    ctx.signed_in_user()
        .ok_or_else(|| AppError::Auth("Sign in to OpenAlgo first.".into()))
}

/// URL the main window should show for the current server state.
pub fn window_url(ctx: &AppState) -> Option<url::Url> {
    match &*ctx.server_status.read() {
        ServerStatus::Running { port, .. } => {
            url::Url::parse(&format!("http://127.0.0.1:{}/", port)).ok()
        }
        _ => None,
    }
}

fn navigate(app: &AppHandle, ctx: &AppState) {
    if cfg!(debug_assertions) {
        // Development: the window stays on the Vite dev server.
        return;
    }
    if let (Some(w), Some(u)) = (app.get_webview_window("main"), window_url(ctx)) {
        if let Err(e) = w.navigate(u) {
            tracing::error!("Could not reload the window: {}", e);
        }
    }
}

/// The HTTP listener's state (the start-up page reads it as before) plus
/// the market data listener's under `ws`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StartupStatus {
    #[serde(flatten)]
    pub http: ServerStatus,
    pub ws: ServerStatus,
}

pub fn startup_status_of(ctx: &AppState) -> StartupStatus {
    StartupStatus {
        http: ctx.server_status.read().clone(),
        ws: ctx.feed_status.read().clone(),
    }
}

#[tauri::command]
pub fn startup_status(shell: State<'_, ShellState>) -> StartupStatus {
    startup_status_of(&shell.ctx)
}

#[tauri::command]
pub async fn retry_server(
    app: AppHandle,
    shell: State<'_, ShellState>,
    port: Option<u16>,
) -> Result<ServerStatus> {
    let mut guard = shell.server.lock().await;
    if guard.is_some() {
        return Err(AppError::Validation("OpenAlgo is already running.".into()));
    }
    if let Some(p) = port {
        let conn = shell.ctx.sqlite.conn()?;
        crate::config::save(
            &conn,
            &crate::config::ServerConfigUpdate {
                http_port: Some(p),
                ..Default::default()
            },
        )?;
        drop(conn);
        shell.ctx.reload_config()?;
    }
    match server::start(shell.ctx.clone()).await {
        Ok(h) => {
            *guard = Some(h);
            navigate(&app, &shell.ctx);
        }
        Err(st) => return Ok(st),
    }
    Ok(shell.ctx.server_status.read().clone())
}

#[tauri::command]
pub async fn restart_server(app: AppHandle, shell: State<'_, ShellState>) -> Result<ServerStatus> {
    require_user(&shell.ctx)?;
    shell.ctx.reload_config()?;
    let mut guard = shell.server.lock().await;
    if let Some(h) = guard.take() {
        h.stop().await;
    }
    match server::start(shell.ctx.clone()).await {
        Ok(h) => {
            *guard = Some(h);
            navigate(&app, &shell.ctx);
            Ok(shell.ctx.server_status.read().clone())
        }
        Err(st) => {
            if let Some(w) = app.get_webview_window("main") {
                if let Ok(u) = url::Url::parse("tauri://localhost/index.html") {
                    let _ = w.navigate(u);
                }
            }
            Ok(st)
        }
    }
}

#[tauri::command]
pub fn open_external(app: AppHandle, shell: State<'_, ShellState>, url: String) -> Result<()> {
    require_user(&shell.ctx)?;
    let parsed = url::Url::parse(&url)
        .map_err(|_| AppError::Validation("That link is not a web address.".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(AppError::Validation("Only web links can be opened.".into()));
    }
    #[allow(deprecated)]
    tauri_plugin_shell::ShellExt::shell(&app)
        .open(parsed.as_str(), None)
        .map_err(|e| AppError::Internal(e.to_string()))
}
