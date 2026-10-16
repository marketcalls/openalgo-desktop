//! Quitting from the desktop window (ARCH-03): closing the main window, or
//! Quit in the macOS app menu, first checks what is still running and asks
//! the trader in a native dialog when Live or sandbox work would stop being
//! watched (`services::lifecycle::quit`). Nothing is closed at the broker.

use super::{confirmed, Question, ShellState};
use crate::services::lifecycle::quit::{quit_warning, CloseRequest};
use tauri::{AppHandle, Manager};

/// The app menu's Quit, which goes through the same check (macOS).
#[cfg(target_os = "macos")]
const QUIT_MENU_ID: &str = "openalgo-quit";

/// The main window was asked to close. True when the close must wait:
/// the check is started (or already running) and closes the window itself
/// once the trader confirms.
pub fn hold_close(handle: &AppHandle) -> bool {
    let Some(shell) = handle.try_state::<ShellState>() else {
        return false;
    };
    match shell.quit.on_close() {
        CloseRequest::Close => false,
        CloseRequest::Asking => true,
        CloseRequest::Ask => {
            let check = tauri::async_runtime::spawn(ask_then_quit(handle.clone()));
            *shell.quit_check.lock() = Some(check);
            true
        }
    }
}

/// Read what quitting would leave unwatched; quit at once when nothing,
/// otherwise when the trader confirms.
async fn ask_then_quit(app: AppHandle) {
    let (Some(shell), Some(window)) = (
        app.try_state::<ShellState>(),
        app.get_webview_window("main"),
    ) else {
        return;
    };
    let quit = match quit_warning(&shell.ctx).await {
        None => true,
        Some(w) => {
            let message = w.dialog_text();
            let question = Question {
                title: &w.title,
                message: &message,
                yes: "Quit",
                no: "Keep OpenAlgo Open",
            };
            confirmed(&app, &window, question).await
        }
    };
    shell.quit.answered(quit);
    if quit {
        tracing::info!("Quitting OpenAlgo at the trader's request");
        if let Err(e) = window.destroy() {
            tracing::warn!("The OpenAlgo window could not be closed: {}", e);
        }
    }
}

/// Stop a check still waiting for its answer (the app is exiting).
pub fn stop(shell: &ShellState) {
    if let Some(check) = shell.quit_check.lock().take() {
        check.abort();
    }
}

/// macOS quits from the app menu (Cmd+Q) without closing a window, so the
/// default Quit item is replaced by one that closes the main window, which
/// asks first. (Quit from the Dock still ends the app at once: the window
/// system gives no chance to ask.)
#[cfg(target_os = "macos")]
pub fn route_menu_quit(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app.handle())?;
    if let Some(app_menu) = menu.items()?.first().and_then(MenuItemKind::as_submenu) {
        // The default app menu ends with the system Quit item.
        if let Some(MenuItemKind::Predefined(quit)) = app_menu.items()?.last() {
            app_menu.remove(quit)?;
        }
        let quit = MenuItem::with_id(
            app,
            QUIT_MENU_ID,
            "Quit OpenAlgo Desktop",
            true,
            Some("CmdOrCtrl+Q"),
        )?;
        app_menu.append(&quit)?;
    }
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id() == QUIT_MENU_ID {
            match app.get_webview_window("main") {
                Some(w) => {
                    let _ = w.close();
                }
                None => app.exit(0),
            }
        }
    });
    Ok(())
}
