//! OpenAlgo Desktop - Algorithmic Trading Platform
//!
//! A desktop application for algorithmic trading with support for
//! multiple Indian brokers (Angel One, Zerodha, Fyers).

pub mod commands;
pub mod db;
pub mod brokers;
pub mod security;
pub mod websocket;
pub mod scheduler;
pub mod error;
pub mod state;

use scheduler::AutoLogoutScheduler;
use state::AppState;
use tauri::Manager;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize and run the Tauri application
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize tracing/logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "openalgo_desktop=debug,tauri=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting OpenAlgo Desktop...");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Initialize application state
            let app_state = AppState::new(app.handle())?;
            app.manage(app_state);

            // Start auto-logout scheduler (3:00 AM IST for broker compliance)
            let scheduler = AutoLogoutScheduler::new(app.handle().clone());
            scheduler.start();

            tracing::info!("Application state initialized");
            tracing::info!("Auto-logout scheduler started (3:00 AM IST)");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Auth commands
            commands::auth::login,
            commands::auth::logout,
            commands::auth::check_session,
            commands::auth::get_current_user,
            // Broker commands
            commands::broker::broker_login,
            commands::broker::broker_logout,
            commands::broker::get_broker_status,
            commands::broker::set_active_broker,
            commands::broker::get_available_brokers,
            // Order commands
            commands::orders::place_order,
            commands::orders::modify_order,
            commands::orders::cancel_order,
            commands::orders::get_order_book,
            commands::orders::get_trade_book,
            // Position commands
            commands::positions::get_positions,
            commands::positions::close_position,
            commands::positions::close_all_positions,
            // Holdings commands
            commands::holdings::get_holdings,
            // Funds commands
            commands::funds::get_funds,
            // Quote commands
            commands::quotes::get_quote,
            commands::quotes::get_market_depth,
            // Symbol commands
            commands::symbols::search_symbols,
            commands::symbols::get_symbol_info,
            commands::symbols::refresh_symbol_master,
            // Strategy commands
            commands::strategy::get_strategies,
            commands::strategy::create_strategy,
            commands::strategy::update_strategy,
            commands::strategy::delete_strategy,
            commands::strategy::toggle_strategy,
            // Settings commands
            commands::settings::get_settings,
            commands::settings::update_settings,
            commands::settings::save_broker_credentials,
            commands::settings::delete_broker_credentials,
            // Sandbox commands
            commands::sandbox::get_sandbox_positions,
            commands::sandbox::get_sandbox_orders,
            commands::sandbox::place_sandbox_order,
            commands::sandbox::reset_sandbox,
            // Historify commands
            commands::historify::get_market_data,
            commands::historify::download_historical_data,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
