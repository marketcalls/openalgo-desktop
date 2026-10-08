//! Tauri commands: only what the shell alone can do. Everything else is HTTP.
//!
//! | Command          | Who may call          | Why                                         |
//! |------------------|-----------------------|---------------------------------------------|
//! | `startup_status` | anyone                | the start-up page shows why the server is not running (no secrets) |
//! | `retry_server`   | anyone, only while the server is NOT running | recover from a taken port before any page can load |
//! | `restart_server` | signed-in user        | apply new port / LAN settings               |
//! | `open_external`  | signed-in user        | open an http(s) link in the system browser  |
//! | `reset_account`  | the main window on the app's own page, after a native confirmation | last-resort recovery when the password and authenticator are both lost |

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

/// Who may start an account reset: the main window, showing the app's own
/// page (the local server on its listening port, or the Vite dev server in
/// development builds). A hidden runner window, a broker page the main
/// window was sent to, or any other origin is refused.
pub fn reset_caller_allowed(label: &str, url: &url::Url, listening_port: u16) -> bool {
    if label != "main" || url.scheme() != "http" {
        return false;
    }
    let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    let port = url.port_or_known_default();
    loopback && (port == Some(listening_port) || (cfg!(debug_assertions) && port == Some(5173)))
}

/// The answer the reset page gets back.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResetOutcome {
    /// `reset` or `cancelled`.
    pub status: &'static str,
    pub message: String,
}

/// Last-resort recovery when the password and the authenticator are both
/// lost. Only the person at this computer can do it: the call must come
/// from the main window on the app's own page, and the trader confirms in a
/// native dialog that page scripts cannot click. No HTTP route resets the
/// account, so nothing reaching the server over the network (a LAN device,
/// a tunnel, a web page in a browser) can.
#[tauri::command]
pub async fn reset_account(
    app: AppHandle,
    window: tauri::WebviewWindow,
    shell: State<'_, ShellState>,
) -> Result<ResetOutcome> {
    let url = window
        .url()
        .map_err(|e| AppError::Internal(format!("window address: {}", e)))?;
    if !reset_caller_allowed(window.label(), &url, shell.ctx.listening_port()) {
        tracing::warn!("Account reset refused: not requested from the OpenAlgo window");
        return Err(AppError::Auth(
            "Reset account works only from the OpenAlgo Desktop window on this computer.".into(),
        ));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        app.dialog()
            .message(
                "This removes your OpenAlgo login, API key and saved broker keys, ends your \
broker session, stops every AI client (MCP) token, unlinks Telegram and WhatsApp, and gives \
your strategy and Chartink webhooks new addresses. Your trade and strategy history stays. You \
then create a new account.",
            )
            .title("Reset your OpenAlgo account?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Reset account".into(),
                "Keep my account".into(),
            ))
            .parent(&window)
            .show(move |confirmed| {
                let _ = tx.send(confirmed);
            });
    }
    if !rx.await.unwrap_or(false) {
        return Ok(ResetOutcome {
            status: "cancelled",
            message: "Your account was not changed.".into(),
        });
    }
    crate::services::auth_service::AuthService::reset_account_everywhere(&shell.ctx).await?;
    Ok(ResetOutcome {
        status: "reset",
        message: "Your account was removed. Create a new account to continue.".into(),
    })
}

#[cfg(test)]
mod reset_tests {
    use super::reset_caller_allowed;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn only_the_main_window_on_the_app_page_may_reset() {
        assert!(reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:5000/reset-password"),
            5000
        ));
        assert!(reset_caller_allowed(
            "main",
            &u("http://localhost:5000/"),
            5000
        ));
        // Another program on loopback, a broker page, the bundled page, a
        // rebinding host.
        assert!(!reset_caller_allowed(
            "main",
            &u("http://127.0.0.1:8080/"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("https://kite.zerodha.com/connect"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("tauri://localhost/index.html"),
            5000
        ));
        assert!(!reset_caller_allowed(
            "main",
            &u("http://evil.example:5000/"),
            5000
        ));
        // A hidden OpenScript runner window on the right page.
        assert!(!reset_caller_allowed(
            "runner-1",
            &u("http://127.0.0.1:5000/"),
            5000
        ));
    }
}
