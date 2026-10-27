//! OpenAlgo Desktop: a single-user desktop port of OpenAlgo web.
//!
//! The Rust process owns everything: one HTTP server (UI, session routes,
//! broker callbacks, `/api/v1`, Socket.IO), the services, the event bus and
//! the databases. The Tauri window is a browser pointed at that server.

// Code health (CLAUDE.md): no unwrap, expect, todo or print macros in
// production code. Tests may use them. CI runs clippy with -D warnings, so
// these warnings fail the build.
#![cfg_attr(
    not(test),
    warn(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::dbg_macro,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::todo,
        clippy::unimplemented
    )
)]

pub mod agent;
pub mod analytics;
pub mod brokers;
pub mod cache;
pub mod chartink;
pub mod clock;
pub mod commands;
pub mod config;
pub mod db;
// CI's native E2E test build only; see src/e2e.rs. Its features do not
// build outside a debug build unless build.rs saw OPENALGO_E2E_BUILD=1.
#[cfg(all(
    any(feature = "e2e-debug-port", feature = "e2e-broker"),
    not(any(e2e_build_allowed, debug_assertions))
))]
compile_error!(
    "The e2e-debug-port and e2e-broker features (a WebView2 debugging port, a scripted broker) \
     are only for CI's native E2E test build: build it with OPENALGO_E2E_BUILD=1, never for a \
     release."
);
#[cfg(any(all(windows, feature = "e2e-debug-port"), feature = "e2e-broker"))]
mod e2e;
pub mod error;
pub mod events;
pub mod feed;
pub mod historify;
pub mod mcp;
pub mod messaging;
pub mod risk;
pub mod sandbox;
pub mod scalping;
pub mod security;
pub mod server;
pub mod services;
pub mod session;
pub mod state;
pub mod strategy;
pub mod trading;
pub mod updater;
pub mod websocket;

use commands::ShellState;
use state::AppState;
use std::sync::Arc;
use tauri::webview::{PermissionKind, PermissionResponse};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// The fixed log filter (no environment read): info in release, debug in
/// development. The socket libraries are pinned at info whatever the
/// catch-all says: tungstenite traces the whole handshake request, path and
/// query included, and some brokers (mStock, Kite) carry their API key and
/// session token on the socket address.
pub(crate) fn log_filter(development: bool) -> &'static str {
    if development {
        "openalgo_desktop_lib=debug,openalgo_desktop=debug,tauri=info,tungstenite=info,tokio_tungstenite=info,warn"
    } else {
        "openalgo_desktop_lib=info,openalgo_desktop=info,tauri=warn,tungstenite=info,tokio_tungstenite=info,warn"
    }
}

/// The data folder was not opened: show the start-up page with the reason
/// (the `startup_status` command reads it) and start nothing else.
fn show_data_refusal(
    app: &mut tauri::App,
    refusal: db::upgrade::Refusal,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing::error!(
        "OpenAlgo did not open its data folder: {}",
        refusal.detail()
    );
    app.manage(commands::DataRefusal(refusal.message()));
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("OpenAlgo Desktop")
        .inner_size(900.0, 640.0)
        .min_inner_size(640.0, 480.0)
        .center()
        .build()?;
    Ok(())
}

/// Open the data folder for this version (REL-02, `db::upgrade`): one
/// running copy per folder; a folder from a newer version is refused; an
/// older one is backed up before its migrations and put back if they fail.
/// `None` when it was refused: the start-up page then shows why and nothing
/// else starts. The folder lock is held for the life of the app.
fn open_data_folder(
    app: &mut tauri::App,
) -> std::result::Result<Option<Arc<AppState>>, Box<dyn std::error::Error>> {
    let data_dir = app.path().app_data_dir()?;
    let lock = match db::upgrade::DataFolderLock::acquire(&data_dir) {
        Ok(lock) => lock,
        Err(refusal) => {
            show_data_refusal(app, refusal)?;
            return Ok(None);
        }
    };
    // `AppState::open` spawns its subscribers, so it runs on the runtime.
    let opened = tauri::async_runtime::block_on(async {
        AppState::open_upgrading(&data_dir, open_options(), env!("CARGO_PKG_VERSION"))
    });
    match opened {
        Ok(ctx) => {
            app.manage(commands::DataFolderHold(lock));
            Ok(Some(ctx))
        }
        Err(db::upgrade::OpenError::Refused(r)) => {
            show_data_refusal(app, r)?;
            Ok(None)
        }
        Err(db::upgrade::OpenError::Open(e)) => Err(e.into()),
    }
}

/// The stores and brokers the app opens with: the installed app's.
fn open_options() -> state::OpenOptions {
    with_test_brokers(state::OpenOptions::production())
}

/// `options` as they are, except in CI's native E2E test build, which puts
/// its scripted broker in the Dhan (Sandbox) slot (src/e2e.rs).
fn with_test_brokers(options: state::OpenOptions) -> state::OpenOptions {
    #[cfg(feature = "e2e-broker")]
    let options = e2e::with_scripted_broker(options);
    options
}

fn init_logging() {
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(log_filter(cfg!(
            debug_assertions
        ))))
        .with(tracing_subscriber::fmt::layer())
        .with(services::error_log::CaptureLayer)
        .try_init();
}

/// Initialize and run the Tauri application
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_logging();
    tracing::info!("Starting OpenAlgo Desktop {}", env!("CARGO_PKG_VERSION"));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        // Rust-side native dialogs only (account reset); no JS permission.
        .plugin(tauri_plugin_dialog::init())
        // In-app updates, driven from Rust only (`updater`): no page is
        // granted the plugin's own commands.
        .plugin(updater::plugin())
        .setup(setup)
        .invoke_handler(commands::handler())
        .build(tauri::generate_context!());

    let app = match app {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("OpenAlgo Desktop could not start: {}", e);
            return;
        }
    };

    app.run(on_run_event);
}

/// Open the data folder, start the server and the background services,
/// then open the main window on them.
fn setup(app: &mut tauri::App) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let Some(ctx) = open_data_folder(app)? else {
        return Ok(());
    };
    let (server, feed) = tauri::async_runtime::block_on(start_services(&ctx));
    app.manage(feed);
    if let Some(h) = &server {
        commands::trust_app_origin(app.handle(), h.addr.port());
    }

    updater::start(app, &ctx);
    #[cfg(target_os = "macos")]
    if let Err(e) = commands::quit::route_menu_quit(app) {
        tracing::warn!("Quit in the app menu will not ask first: {}", e);
    }

    let url = main_window_url(&ctx, server.is_some());
    open_main_window(app, &ctx, url)?;

    // OpenScript live runs: one hidden window per run, loading the runner
    // page from this app's own server.
    #[cfg(feature = "runner-window")]
    set_runner_host(app, &ctx);

    app.manage(ShellState::new(ctx, server));
    Ok(())
}

/// The local server, the strategy, OpenScript and Chartink services, and
/// the market data feed for SDK clients. `None` for the server when it
/// could not start (the start-up page then says why).
async fn start_services(
    ctx: &Arc<AppState>,
) -> (Option<server::ServerHandle>, Arc<feed::FeedService>) {
    session::spawn_expiry_task(ctx.clone());
    services::system_info::mark_start();
    services::monitor::start(ctx);
    services::health_service::start(ctx);
    // Sleep and wake: feeds reconnect and orders are checked (owned task).
    services::lifecycle::wake::start(ctx);
    let server = server::start(ctx.clone()).await.ok();
    // Recovery, prices, checkpoints and the IST scheduler (owned
    // tasks, stopped by `AppState::shutdown`).
    let started = ctx.strategy.start().await;
    tracing::debug!("Strategy module started: {}", started);
    // OpenScript schedules (an owned task, stopped at shutdown).
    ctx.trading.start(ctx.now(), &ctx.sqlite);
    // Chartink intraday square-offs (owned task).
    ctx.chartink.start();
    // The Agent's model catalogue: LiteLLM's price table refreshed once,
    // 5 s, as the web gets it at import (owned task; the bundled copy
    // stays on any failure).
    agent::catalog::refresh::start(ctx);
    // Market data feed for SDK clients; a taken port is kept in
    // its status with a trader-facing message.
    let feed = feed::FeedService::new(ctx.clone());
    feed.start().await;
    (server, feed)
}

/// What the main window opens: the server's page, or the start-up page
/// explaining why the server is not running.
fn main_window_url(ctx: &AppState, server_running: bool) -> WebviewUrl {
    if !server_running {
        // Start-up page explaining why the server is not running.
        WebviewUrl::App("index.html".into())
    } else if cfg!(debug_assertions) {
        // Development: Vite (devUrl) proxies to the Rust server.
        WebviewUrl::App("index.html".into())
    } else {
        match commands::window_url(ctx) {
            Some(u) => WebviewUrl::External(u),
            None => WebviewUrl::App("index.html".into()),
        }
    }
}

/// The main window. It loads pages on this computer only from the live
/// server port (security review S-08): the grant for a port the server has
/// left cannot be taken back, so no page on it may load here.
fn open_main_window(app: &tauri::App, ctx: &Arc<AppState>, url: WebviewUrl) -> tauri::Result<()> {
    let live = Arc::downgrade(ctx);
    let listening = Arc::downgrade(ctx);
    let builder = WebviewWindowBuilder::new(app, "main", url);
    // CI's Windows native E2E build only (src/e2e.rs).
    #[cfg(all(windows, feature = "e2e-debug-port"))]
    let builder = match e2e::debug_port() {
        Some(port) => builder.additional_browser_args(&e2e::browser_args(port)),
        None => builder,
    };
    let _window = builder
        .on_navigation(move |to| {
            let port = live.upgrade().map(|c| c.live_port());
            let ok =
                port.is_some_and(|p| commands::main_window_may_load(to, p, cfg!(debug_assertions)));
            if !ok {
                tracing::warn!(
                    "Kept the OpenAlgo window from loading a page on another local port"
                );
            }
            ok
        })
        .on_permission_request(move |_, kind| microphone_permission(&listening, kind))
        .title("OpenAlgo Desktop")
        .inner_size(1400.0, 900.0)
        .min_inner_size(1024.0, 768.0)
        .center()
        .build()?;
    #[cfg(target_os = "linux")]
    enable_voice_webview(&_window);
    Ok(())
}

/// The main window grants the microphone itself only while voice is on
/// (`agent::voice::window_may_listen`); otherwise, and for anything else,
/// the platform's own answer stands (a prompt on Windows and macOS, a
/// refusal on Linux), so a refusal is never saved into the web view.
fn microphone_permission(
    app: &std::sync::Weak<AppState>,
    kind: PermissionKind,
) -> PermissionResponse {
    match kind {
        PermissionKind::Microphone
            if app
                .upgrade()
                .is_some_and(|a| agent::voice::window_may_listen(&a)) =>
        {
            PermissionResponse::Allow
        }
        _ => PermissionResponse::Default,
    }
}

/// Linux: WebKitGTK offers a page no microphone or WebRTC unless asked.
/// Asked for the main window, which loads only the app's own pages; where
/// the system's WebKitGTK was built without WebRTC this changes nothing and
/// the voice button says why. The page loads again so it starts with the
/// settings in place.
#[cfg(target_os = "linux")]
fn enable_voice_webview(window: &tauri::WebviewWindow) {
    let set = window.with_webview(|w| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        let view = w.inner();
        if let Some(settings) = WebViewExt::settings(&view) {
            settings.set_enable_media_stream(true);
            settings.set_enable_webrtc(true);
        }
        view.reload();
    });
    if let Err(e) = set {
        tracing::warn!("Voice settings for the window could not be applied: {}", e);
    }
}

/// OpenScript live runs open one hidden window each, loading the runner
/// page from this app's own server.
#[cfg(feature = "runner-window")]
fn set_runner_host(app: &tauri::App, ctx: &Arc<AppState>) {
    let weak = Arc::downgrade(ctx);
    ctx.trading
        .runner
        .set_host(Arc::new(trading::runner::window::WindowHost::new(
            app.handle().clone(),
            move || {
                if cfg!(debug_assertions) {
                    // Development: the page comes from Vite, which
                    // proxies /openscript to this server.
                    "http://localhost:5173".to_string()
                } else {
                    let port = weak.upgrade().map(|c| c.listening_port()).unwrap_or(5000);
                    format!("http://127.0.0.1:{}", port)
                }
            },
        )));
}

/// Closing the main window first asks the trader when Live or sandbox
/// work would stop being watched (ARCH-03). Hidden runner windows would
/// keep the app alive after the trader closes the main window: pause the
/// runs (closing their windows) so the app exits as it always has. On
/// exit, stop everything.
fn on_run_event(handle: &tauri::AppHandle, event: RunEvent) {
    if let RunEvent::WindowEvent {
        label,
        event: tauri::WindowEvent::CloseRequested { api, .. },
        ..
    } = &event
    {
        if label == "main" && commands::quit::hold_close(handle) {
            api.prevent_close();
        }
    }
    if let RunEvent::WindowEvent {
        label,
        event: tauri::WindowEvent::Destroyed,
        ..
    } = &event
    {
        if label == "main" {
            if let Some(shell) = handle.try_state::<ShellState>() {
                shell
                    .ctx
                    .trading
                    .runner
                    .pause_all("Paused because OpenAlgo is closing");
            }
        }
    }
    if let RunEvent::Exit = event {
        if handle.try_state::<ShellState>().is_some() {
            tauri::async_runtime::block_on(commands::stop_app(handle));
            tracing::info!("OpenAlgo Desktop stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::brokers::scripted;
    use std::sync::Arc;

    /// Only the e2e-broker test build changes an adapter, and only the one
    /// in the Dhan (Sandbox) slot: every other build, the shipped one
    /// included, opens with the very adapters it was given (the same
    /// instances, not look-alikes).
    #[test]
    fn only_the_test_build_puts_the_scripted_broker_in_its_slot() {
        let production = crate::state::OpenOptions::production();
        let given = production.brokers.clone();
        let opened = super::with_test_brokers(production).brokers;
        assert_eq!(opened.ids(), given.ids());
        let test_build = cfg!(feature = "e2e-broker");
        for id in given.ids() {
            let (got, was) = (opened.get(&id).unwrap(), given.get(&id).unwrap());
            let replaced = test_build && id == scripted::SLOT;
            assert_eq!(Arc::ptr_eq(&got, &was), !replaced, "{}", id);
        }
        let slot = opened.get(scripted::SLOT).unwrap();
        assert_eq!(
            slot.name(),
            if test_build { "Mock" } else { "Dhan Sandbox" }
        );
        // The app opens with exactly these.
        let app = super::open_options().brokers;
        assert_eq!(app.ids(), given.ids());
    }
}
