//! OpenAlgo Desktop: a single-user desktop port of OpenAlgo web.
//!
//! The Rust process owns everything: one HTTP server (UI, session routes,
//! broker callbacks, `/api/v1`, Socket.IO), the services, the event bus and
//! the databases. The Tauri window is a browser pointed at that server.

pub mod analytics;
pub mod brokers;
pub mod clock;
pub mod commands;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod feed;
pub mod historify;
pub mod messaging;
pub mod sandbox;
pub mod security;
pub mod server;
pub mod services;
pub mod session;
pub mod state;
pub mod webhook;
pub mod websocket;

use commands::ShellState;
use state::AppState;
use std::sync::Arc;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn init_logging() {
    // Fixed filter (no environment read): info in release, debug in development.
    let filter = if cfg!(debug_assertions) {
        "openalgo_desktop_lib=debug,openalgo_desktop=debug,tauri=info,warn"
    } else {
        "openalgo_desktop_lib=info,openalgo_desktop=info,tauri=warn,warn"
    };
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(filter))
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
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let (ctx, server, feed) = tauri::async_runtime::block_on(async {
                let ctx = AppState::open_default(&data_dir)?;
                session::spawn_expiry_task(ctx.clone());
                services::system_info::mark_start();
                services::monitor::start(&ctx);
                services::health_service::start(&ctx);
                let server = server::start(ctx.clone()).await.ok();
                // Market data feed for SDK clients; a taken port is kept in
                // its status with a trader-facing message.
                let feed = feed::FeedService::new(ctx.clone());
                feed.start().await;
                messaging::autostart(&ctx);
                Ok::<_, error::AppError>((ctx, server, feed))
            })?;
            app.manage(feed);

            let url = if server.is_none() {
                // Start-up page explaining why the server is not running.
                WebviewUrl::App("index.html".into())
            } else if cfg!(debug_assertions) {
                // Development: Vite (devUrl) proxies to the Rust server.
                WebviewUrl::App("index.html".into())
            } else {
                match commands::window_url(&ctx) {
                    Some(u) => WebviewUrl::External(u),
                    None => WebviewUrl::App("index.html".into()),
                }
            };
            WebviewWindowBuilder::new(app, "main", url)
                .title("OpenAlgo Desktop")
                .inner_size(1400.0, 900.0)
                .min_inner_size(1024.0, 768.0)
                .center()
                .build()?;

            app.manage(ShellState {
                ctx,
                server: tokio::sync::Mutex::new(server),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::startup_status,
            commands::retry_server,
            commands::restart_server,
            commands::open_external,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("OpenAlgo Desktop could not start: {}", e);
            return;
        }
    };

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(shell) = handle.try_state::<ShellState>() {
                let ctx: Arc<AppState> = shell.ctx.clone();
                let feed = handle.try_state::<Arc<feed::FeedService>>();
                tauri::async_runtime::block_on(async {
                    if let Some(f) = feed {
                        f.stop().await;
                    }
                    if let Some(h) = shell.server.lock().await.take() {
                        h.stop().await;
                    }
                    ctx.shutdown().await;
                });
                tracing::info!("OpenAlgo Desktop stopped");
            }
        }
    });
}
