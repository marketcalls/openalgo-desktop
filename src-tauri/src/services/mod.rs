//! The service layer: every `/api/v1` endpoint's business logic, shared by
//! the HTTP handlers (and later MCP tools and Tauri commands).
//!
//! Handlers validate with [`schema`] (the web's Marshmallow rules), call one
//! function here and send its [`core::Reply`] as is. Services decide the
//! destination once (sandbox engine in analyzer mode, otherwise the broker)
//! and publish the web's event on the bus; side effects are subscribers.
//!
//! | Module | Web services |
//! |---|---|
//! | [`order_service`] | place, smart, modify, cancel, cancel all, close position |
//! | [`batch_order_service`] | basket, split |
//! | [`options_order_service`] | optionsorder, optionsmultiorder |
//! | [`options_service`] | optionsymbol, optionchain, syntheticfuture, optiongreeks, multioptiongreeks |
//! | [`account_service`] | orderbook, tradebook, positionbook, holdings, funds, orderstatus, openposition, pnl/symbols |
//! | [`market_data_service`] | quotes, multiquotes, depth, history, intervals, ticker, margin |
//! | [`chain_tools_service`] | OI tracker, max pain, GEX, IV smile, gamma density, OI profile |
//! | [`strike_profile_service`] | Strike Profile: GEX and DEX by strike and expiry |
//! | [`history_tools_service`] | IV chart, straddle chart and simulation, vol surface, Strategy Builder charts |
//! | [`tools_service`] | shared tool plumbing: references, nearest future, bounded history fan-out |
//! | [`arbitrage_service`] | futures calendar-spread universe |
//! | [`symbol_service`] | symbol, search, expiry, instruments, freeze quantities |
//! | [`gtt_service`] | place/modify/cancel GTT, GTT book |
//! | [`analyzer_service`] | analyzer status and toggle (engine lifecycle) |
//! | [`sandbox_feed`] | the sandbox engine's quote, tick and symbol adapters |

pub mod account_service;
pub mod action_center_service;
pub mod alert_log_service;
pub mod analyzer_log_service;
pub mod analyzer_service;
pub mod apikey_service;
pub mod arbitrage_service;
pub mod auth_service;
pub mod batch_order_service;
pub mod broker_auth_service;
pub mod broker_config_service;
pub mod broker_runtime;
pub mod broker_settings_service;
pub mod broker_switch;
pub mod chain_tools_service;
pub mod chart_prefs_service;
pub mod chart_test_service;
pub mod core;
pub mod error_log;
pub mod feed_connections;
pub mod feed_losses;
pub mod freeze_qty_service;
pub mod gtt_service;
pub mod health_service;
pub mod history_tools_service;
pub mod latency_probe;
pub mod latency_service;
pub mod lifecycle;
pub mod live_owner;
pub mod market_calendar_service;
pub mod market_data_service;
pub mod master_contract_service;
pub mod monitor;
pub mod notify_service;
pub mod obligations;
pub mod options_order_service;
pub mod options_service;
pub mod order_log_service;
pub mod order_reconciler;
pub mod order_router;
pub mod order_service;
pub mod pnl_tracker_service;
pub mod record_loss;
pub mod report_flight;
pub mod request_budget_service;
pub mod sandbox_export_service;
pub mod sandbox_feed;
pub mod schema;
pub mod schemas;
pub mod search_ui_service;
pub mod security_service;
pub mod session_account;
pub mod settings_service;
pub mod store_sizes;
pub mod strategy_portfolio_service;
pub mod strategy_ticks;
pub mod strike_profile_service;
pub mod symbol_service;
pub mod system_info;
pub mod telegram_admin_service;
pub mod tools_service;
pub mod traffic_service;
pub mod ui_order_service;
pub mod update;
pub mod watchlist_service;

pub use analyzer_service::{AnalyzerService, AnalyzerStatus};
pub use core::Reply;
pub use order_service::Route;
