//! OpenAlgo Desktop - Algorithmic Trading Platform
//!
//! A desktop application for algorithmic trading with support for
//! multiple Indian brokers (Angel One, Zerodha, Fyers).

pub mod commands;
pub mod db;
pub mod brokers;
pub mod security;
pub mod websocket;
pub mod webhook;
pub mod scheduler;
pub mod error;
pub mod state;

use scheduler::AutoLogoutScheduler;
use state::AppState;
use webhook::WebhookServer;
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

            // Get webhook config before managing state
            let webhook_config = app_state.sqlite.get_webhook_config().ok();

            app.manage(app_state);

            // Start auto-logout scheduler (configurable, default 3:00 AM IST)
            let scheduler = AutoLogoutScheduler::new(app.handle().clone());
            scheduler.start();

            // Start webhook server if enabled
            if let Some(config) = webhook_config {
                if config.enabled {
                    let app_handle = app.handle().clone();
                    tokio::spawn(async move {
                        let mut server = WebhookServer::new(app_handle.clone());
                        if let Err(e) = server.start(config).await {
                            tracing::error!("Failed to start webhook server: {}", e);
                        }
                        // Keep server running
                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                        }
                    });
                    tracing::info!("Webhook server starting...");
                }
            }

            tracing::info!("Application state initialized");
            tracing::info!("Auto-logout scheduler started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Auth commands
            commands::auth::check_setup,
            commands::auth::setup,
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
            commands::symbols::get_symbol_by_token,
            commands::symbols::get_symbol_count,
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
            commands::settings::get_auto_logout_config,
            commands::settings::update_auto_logout_config,
            commands::settings::get_webhook_config,
            commands::settings::update_webhook_config,
            commands::settings::get_broker_config,
            commands::settings::get_broker_credentials,
            // API key commands
            commands::api_keys::create_api_key,
            commands::api_keys::list_api_keys,
            commands::api_keys::delete_api_key,
            commands::api_keys::delete_api_key_by_id,
            // Sandbox commands
            commands::sandbox::get_sandbox_positions,
            commands::sandbox::get_sandbox_orders,
            commands::sandbox::place_sandbox_order,
            commands::sandbox::reset_sandbox,
            commands::sandbox::get_sandbox_holdings,
            commands::sandbox::get_sandbox_funds,
            commands::sandbox::update_sandbox_ltp,
            commands::sandbox::cancel_sandbox_order,
            // Order logs commands
            commands::order_logs::get_order_logs,
            commands::order_logs::get_order_logs_by_order_id,
            commands::order_logs::get_recent_order_logs,
            commands::order_logs::get_order_log_stats,
            commands::order_logs::clear_old_order_logs,
            // Market commands
            commands::market::create_market_holiday,
            commands::market::get_market_holidays_by_year,
            commands::market::get_market_holidays_by_exchange,
            commands::market::is_market_holiday,
            commands::market::delete_market_holiday,
            commands::market::get_all_market_timings,
            commands::market::get_market_timing,
            commands::market::update_market_timing,
            commands::market::is_market_open,
            // Historify commands
            commands::historify::get_market_data,
            commands::historify::download_historical_data,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
