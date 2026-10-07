//! The session route table. Every browser-session route is declared here
//! with its access level, and the router is built from this table, so a
//! route cannot be added without saying who may call it. The guard test
//! walks the same table.

pub mod account;
pub mod admin;
pub mod analyzer;
pub mod app_config;
pub mod auth;
pub mod broker;
pub mod charts;
pub mod health;
pub mod latency;
pub mod leverage;
pub mod log;
pub mod market_calendar;
pub mod options_tools;
pub mod orders;
pub mod playground;
pub mod sandbox;
pub mod search;
pub mod security;
pub mod settings;
pub mod strategy_portfolio;
pub mod telegram;
pub mod traffic;
pub mod watchlist;
pub mod websocket_example;
pub mod webui;
pub mod whatsapp;

use crate::server::middleware::{require_user, require_user_for_json};
use crate::state::AppState;
use axum::{
    http::Method,
    middleware,
    routing::{delete, get, patch, post, put, MethodRouter},
    Router,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No session needed (setup, sign-in, CSRF token, session status).
    Public,
    /// Signed-in OpenAlgo user required.
    User,
    /// A browser navigation gets the SPA; a JSON request needs the user.
    UserJson,
    /// Broker redirect target: public, verified by the server-issued state.
    BrokerCallback,
}

pub struct RouteSpec {
    pub path: &'static str,
    pub method: Method,
    pub access: Access,
    pub make: fn() -> MethodRouter<Arc<AppState>>,
}

macro_rules! r {
    ($m:ident, $path:expr, $acc:ident, $f:path) => {
        RouteSpec {
            path: $path,
            method: Method::$m,
            access: Access::$acc,
            make: || r!(@route $m, $f),
        }
    };
    (@route GET, $f:path) => { get($f) };
    (@route POST, $f:path) => { post($f) };
    (@route PUT, $f:path) => { put($f) };
    (@route DELETE, $f:path) => { delete($f) };
    (@route PATCH, $f:path) => { patch($f) };
}

pub fn table() -> Vec<RouteSpec> {
    vec![
        // Public
        r!(GET, "/auth/csrf-token", Public, auth::csrf_token),
        r!(GET, "/auth/check-setup", Public, auth::check_setup),
        r!(GET, "/auth/app-info", Public, auth::app_info),
        r!(GET, "/auth/session-status", Public, auth::session_status),
        r!(GET, "/auth/broker-config", Public, broker::broker_config),
        r!(POST, "/setup", Public, auth::setup),
        r!(GET, "/auth/login", Public, auth::login_page),
        r!(POST, "/auth/login", Public, auth::login),
        r!(POST, "/auth/login/totp", Public, auth::login_totp),
        r!(GET, "/auth/logout", Public, auth::logout),
        r!(POST, "/auth/logout", Public, auth::logout),
        r!(POST, "/auth/reset-password", Public, auth::reset_password),
        r!(POST, "/auth/reset-account", Public, auth::reset_account),
        // Broker redirect target (state-verified)
        r!(
            GET,
            "/{broker}/callback",
            BrokerCallback,
            broker::oauth_callback
        ),
        // Signed-in user
        r!(POST, "/{broker}/callback", User, broker::form_login),
        r!(
            GET,
            "/{broker}/initiate-oauth",
            User,
            broker::initiate_oauth
        ),
        r!(
            POST,
            "/auth/broker/oauth/manual",
            User,
            broker::oauth_manual
        ),
        r!(GET, "/auth/analyzer-mode", User, auth::analyzer_mode),
        r!(POST, "/auth/analyzer-toggle", User, auth::analyzer_toggle),
        r!(GET, "/auth/dashboard-data", User, auth::dashboard_data),
        r!(GET, "/auth/profile-data", User, auth::profile_data),
        r!(
            POST,
            "/auth/change-password",
            User,
            auth::change_password_api
        ),
        r!(POST, "/auth/change", User, auth::change_password_legacy),
        r!(GET, "/auth/2fa/status", User, auth::two_factor_status),
        r!(
            POST,
            "/auth/2fa/configure",
            User,
            auth::two_factor_configure
        ),
        r!(GET, "/auth/active-sessions", User, auth::active_sessions),
        r!(GET, "/apikey", UserJson, account::get_apikey),
        r!(POST, "/apikey", User, account::regenerate),
        r!(POST, "/apikey/mode", User, account::set_mode),
        r!(
            GET,
            "/api/broker/credentials",
            User,
            broker::get_credentials
        ),
        r!(
            POST,
            "/api/broker/credentials",
            User,
            broker::update_credentials
        ),
        r!(GET, "/api/broker/capabilities", User, broker::capabilities),
        r!(GET, "/api/broker/configured", User, broker::configured),
        // Desktop server settings (the older path is an alias).
        r!(GET, "/settings/api/server", User, settings::get_server),
        r!(POST, "/settings/api/server", User, settings::save_server),
        r!(GET, "/api/desktop/settings", User, settings::get_server),
        r!(POST, "/api/desktop/settings", User, settings::save_server),
        r!(GET, "/settings/analyze-mode", User, settings::analyze_mode),
        r!(
            GET,
            "/api/websocket/config",
            User,
            websocket_example::config
        ),
        r!(
            GET,
            "/api/websocket/apikey",
            User,
            websocket_example::apikey
        ),
        r!(GET, "/api/config/host", User, app_config::host),
        // Admin (web blueprints/admin.py)
        r!(GET, "/admin/api/stats", User, admin::stats),
        r!(GET, "/admin/api/freeze", User, admin::freeze_list),
        r!(POST, "/admin/api/freeze", User, admin::freeze_add),
        r!(PUT, "/admin/api/freeze/{id}", User, admin::freeze_edit),
        r!(DELETE, "/admin/api/freeze/{id}", User, admin::freeze_delete),
        r!(POST, "/admin/api/freeze/upload", User, admin::freeze_upload),
        r!(GET, "/admin/api/holidays", User, admin::holidays),
        r!(POST, "/admin/api/holidays", User, admin::holiday_add),
        r!(
            DELETE,
            "/admin/api/holidays/{id}",
            User,
            admin::holiday_delete
        ),
        r!(GET, "/admin/api/timings", User, admin::timings),
        r!(
            PUT,
            "/admin/api/timings/{exchange}",
            User,
            admin::timing_edit
        ),
        r!(POST, "/admin/api/timings/check", User, admin::timing_check),
        r!(GET, "/admin/api/errors", User, admin::errors),
        r!(POST, "/admin/api/errors/client", User, admin::errors_client),
        r!(GET, "/admin/api/errors/stats", User, admin::errors_stats),
        r!(GET, "/admin/api/errors/groups", User, admin::errors_groups),
        r!(GET, "/admin/api/system", User, admin::system),
        r!(
            POST,
            "/admin/api/system/diagnostics",
            User,
            admin::diagnostics
        ),
        r!(GET, "/admin/api/system/report", User, admin::report),
        // Logs and monitoring
        r!(GET, "/logs", UserJson, log::view),
        r!(GET, "/logs/export", User, log::export),
        r!(GET, "/traffic/api/logs", User, traffic::logs),
        r!(GET, "/traffic/api/stats", User, traffic::stats),
        r!(GET, "/traffic/export", User, traffic::export),
        r!(GET, "/latency/api/logs", User, latency::logs),
        r!(GET, "/latency/api/stats", User, latency::stats),
        r!(
            GET,
            "/latency/api/broker/{broker}/stats",
            User,
            latency::broker_stats
        ),
        r!(GET, "/latency/export", User, latency::export),
        r!(POST, "/security/ban", User, security::ban),
        r!(POST, "/security/unban", User, security::unban),
        r!(POST, "/security/ban-host", User, security::ban_host),
        r!(POST, "/security/clear-404", User, security::clear_404),
        r!(GET, "/security/api/data", User, security::data),
        r!(GET, "/security/stats", User, security::stats),
        r!(POST, "/security/settings", User, security::settings),
        r!(
            GET,
            "/security/api/login-activity",
            User,
            security::login_activity
        ),
        r!(
            POST,
            "/security/api/login-activity/clear",
            User,
            security::clear_login_activity
        ),
        r!(
            GET,
            "/security/api/active-sessions",
            User,
            security::active_sessions
        ),
        r!(GET, "/health", UserJson, health::status),
        r!(GET, "/health/status", User, health::status),
        r!(GET, "/health/check", User, health::check),
        r!(GET, "/health/api/current", User, health::current),
        r!(GET, "/health/api/history", User, health::history),
        r!(GET, "/health/api/stats", User, health::stats),
        r!(GET, "/health/api/alerts", User, health::alerts),
        r!(
            POST,
            "/health/api/alerts/{alert_id}/acknowledge",
            User,
            health::acknowledge
        ),
        r!(
            POST,
            "/health/api/alerts/{alert_id}/resolve",
            User,
            health::resolve
        ),
        r!(GET, "/health/export", User, health::export),
        r!(GET, "/playground/api-key", User, playground::api_key),
        r!(GET, "/playground/endpoints", User, playground::endpoints),
        r!(GET, "/leverage/api/current", User, leverage::current),
        r!(POST, "/leverage/api/update", User, leverage::update),
        // Search (web blueprints/search.py)
        r!(GET, "/search/api/search", User, search::api_search),
        r!(GET, "/search/api/expiries", User, search::api_expiries),
        r!(
            GET,
            "/search/api/underlyings",
            User,
            search::api_underlyings
        ),
        // Order actions from the pages (web blueprints/orders.py)
        r!(POST, "/close_position", User, orders::close_position),
        r!(
            POST,
            "/close_all_positions",
            User,
            orders::close_all_positions
        ),
        r!(POST, "/cancel_all_orders", User, orders::cancel_all_orders),
        r!(POST, "/cancel_order", User, orders::cancel_order),
        r!(POST, "/modify_order", User, orders::modify_order),
        r!(POST, "/modify_gtt_order", User, orders::modify_gtt_order),
        r!(POST, "/cancel_gtt_order", User, orders::cancel_gtt_order),
        // Action Center
        r!(POST, "/action-center/approve/{id}", User, orders::approve),
        r!(POST, "/action-center/reject/{id}", User, orders::reject),
        r!(DELETE, "/action-center/delete/{id}", User, orders::delete),
        r!(GET, "/action-center/count", User, orders::count),
        r!(
            POST,
            "/action-center/approve-all",
            User,
            orders::approve_all
        ),
        r!(GET, "/action-center/api/data", User, orders::data),
        // Sandbox (web blueprints/sandbox.py)
        r!(GET, "/sandbox/api/configs", User, sandbox::configs),
        r!(POST, "/sandbox/update", User, sandbox::update),
        r!(POST, "/sandbox/reset", User, sandbox::reset),
        r!(
            POST,
            "/sandbox/reload-squareoff",
            User,
            sandbox::reload_squareoff
        ),
        r!(
            GET,
            "/sandbox/squareoff-status",
            User,
            sandbox::squareoff_status
        ),
        r!(GET, "/sandbox/mypnl/api/data", User, sandbox::mypnl),
        r!(
            GET,
            "/sandbox/mypnl/export/{kind}",
            User,
            sandbox::mypnl_export
        ),
        // Analyzer log (web blueprints/analyzer.py)
        r!(GET, "/analyzer/api/data", User, analyzer::data),
        r!(GET, "/analyzer/export", User, analyzer::export),
        // Charts (web blueprints/pnltracker.py, chart_test.py)
        r!(POST, "/pnltracker/api/pnl", User, charts::pnl),
        r!(GET, "/chart/test/api/history", User, charts::chart_history),
        // Watchlists and the alert log (web blueprints/watchlist.py, alerts.py)
        r!(GET, "/watchlist/api/lists", User, watchlist::lists),
        r!(POST, "/watchlist/api/lists", User, watchlist::create),
        r!(PATCH, "/watchlist/api/lists/{id}", User, watchlist::rename),
        r!(DELETE, "/watchlist/api/lists/{id}", User, watchlist::delete),
        r!(
            POST,
            "/watchlist/api/lists/{id}/clear",
            User,
            watchlist::clear
        ),
        r!(
            POST,
            "/watchlist/api/lists/{id}/items",
            User,
            watchlist::add_item
        ),
        r!(
            DELETE,
            "/watchlist/api/lists/{id}/items/{item_id}",
            User,
            watchlist::remove_item
        ),
        r!(
            PUT,
            "/watchlist/api/lists/{id}/items/order",
            User,
            watchlist::reorder
        ),
        r!(POST, "/alerts/fired", User, watchlist::alert_fired),
        r!(GET, "/alerts/log", User, watchlist::alert_log_list),
        r!(DELETE, "/alerts/log", User, watchlist::alert_log_clear),
        // Options tools (web blueprints oiprofile, oitracker, ivchart,
        // gamma_density, straddle_chart, custom_straddle, vol_surface, gex,
        // ivsmile, arbitrage, strategy_chart)
        r!(
            POST,
            "/oiprofile/api/profile-data",
            User,
            options_tools::profile_data
        ),
        r!(
            GET,
            "/oiprofile/api/intervals",
            User,
            options_tools::oiprofile_intervals
        ),
        r!(POST, "/oitracker/api/oi-data", User, options_tools::oi_data),
        r!(
            POST,
            "/oitracker/api/maxpain",
            User,
            options_tools::max_pain
        ),
        r!(POST, "/ivchart/api/iv-data", User, options_tools::iv_data),
        r!(
            POST,
            "/ivchart/api/default-symbols",
            User,
            options_tools::default_symbols
        ),
        r!(
            GET,
            "/ivchart/api/intervals",
            User,
            options_tools::ivchart_intervals
        ),
        r!(
            POST,
            "/gammadensity/api/gamma-data",
            User,
            options_tools::gamma_data
        ),
        r!(
            POST,
            "/straddle/api/straddle-data",
            User,
            options_tools::straddle_data
        ),
        r!(
            GET,
            "/straddle/api/intervals",
            User,
            options_tools::all_intervals
        ),
        r!(
            POST,
            "/straddlepnl/api/simulate",
            User,
            options_tools::simulate
        ),
        r!(
            GET,
            "/straddlepnl/api/lotsize",
            User,
            options_tools::lotsize
        ),
        r!(
            GET,
            "/straddlepnl/api/intervals",
            User,
            options_tools::all_intervals
        ),
        r!(
            POST,
            "/volsurface/api/surface-data",
            User,
            options_tools::surface_data
        ),
        r!(POST, "/gex/api/gex-data", User, options_tools::gex_data),
        r!(
            POST,
            "/ivsmile/api/iv-smile-data",
            User,
            options_tools::iv_smile_data
        ),
        r!(
            GET,
            "/arbitrage/api/universe",
            User,
            options_tools::arbitrage_universe
        ),
        r!(
            POST,
            "/strategybuilder/api/strategy-chart",
            User,
            options_tools::strategy_chart
        ),
        r!(
            POST,
            "/strategybuilder/api/multi-strike-oi",
            User,
            options_tools::multi_strike_oi
        ),
        r!(
            GET,
            "/strategybuilder/api/intervals",
            User,
            options_tools::all_intervals
        ),
        // Strategy Builder portfolio (web blueprints/strategy_portfolio.py)
        r!(
            GET,
            "/api/strategy-portfolio",
            User,
            strategy_portfolio::list
        ),
        r!(
            POST,
            "/api/strategy-portfolio",
            User,
            strategy_portfolio::create
        ),
        r!(
            GET,
            "/api/strategy-portfolio/{id}",
            User,
            strategy_portfolio::get
        ),
        r!(
            PUT,
            "/api/strategy-portfolio/{id}",
            User,
            strategy_portfolio::update
        ),
        r!(
            DELETE,
            "/api/strategy-portfolio/{id}",
            User,
            strategy_portfolio::delete
        ),
        // Telegram (web blueprints/telegram.py). GET /telegram/config is
        // the page for a browser and the settings for a JSON request.
        r!(POST, "/telegram/config", User, telegram::configuration),
        r!(GET, "/telegram/config", UserJson, telegram::api_config),
        r!(POST, "/telegram/bot/start", User, telegram::bot_start),
        r!(POST, "/telegram/bot/stop", User, telegram::bot_stop),
        r!(GET, "/telegram/bot/status", User, telegram::bot_status),
        r!(POST, "/telegram/broadcast", User, telegram::broadcast),
        r!(
            POST,
            "/telegram/user/{telegram_id}/unlink",
            User,
            telegram::unlink_user
        ),
        r!(POST, "/telegram/test-message", User, telegram::test_message),
        r!(POST, "/telegram/send-message", User, telegram::send_message),
        r!(GET, "/telegram/api/index", User, telegram::api_index),
        r!(GET, "/telegram/api/config", User, telegram::api_config),
        r!(GET, "/telegram/api/users", User, telegram::api_users),
        r!(
            GET,
            "/telegram/api/analytics",
            User,
            telegram::api_analytics
        ),
        // WhatsApp (web blueprints/whatsapp.py)
        r!(GET, "/whatsapp/config", User, whatsapp::get_config),
        r!(POST, "/whatsapp/config", User, whatsapp::update_config),
        r!(POST, "/whatsapp/pair", User, whatsapp::pair),
        r!(GET, "/whatsapp/pair/status", User, whatsapp::pair_status),
        r!(POST, "/whatsapp/unlink", User, whatsapp::unlink),
        r!(POST, "/whatsapp/bot/start", User, whatsapp::bot_start),
        r!(POST, "/whatsapp/bot/stop", User, whatsapp::bot_stop),
        r!(GET, "/whatsapp/bot/status", User, whatsapp::bot_status),
        r!(GET, "/whatsapp/users", User, whatsapp::users),
        r!(
            POST,
            "/whatsapp/user/{jid}/unlink",
            User,
            whatsapp::unlink_user
        ),
        r!(POST, "/whatsapp/broadcast", User, whatsapp::broadcast),
        r!(POST, "/whatsapp/test-message", User, whatsapp::test_message),
        r!(POST, "/whatsapp/send", User, whatsapp::send),
        r!(GET, "/whatsapp/stats", User, whatsapp::stats),
    ]
}

/// Build the session router from the table, applying the guard per access.
pub fn router() -> Router<Arc<AppState>> {
    let mut by_path: Vec<(&'static str, MethodRouter<Arc<AppState>>)> = Vec::new();
    for spec in table() {
        let mr = (spec.make)();
        let mr = match spec.access {
            Access::Public | Access::BrokerCallback => mr,
            Access::User => mr.route_layer(middleware::from_fn(require_user)),
            Access::UserJson => mr.route_layer(middleware::from_fn(require_user_for_json)),
        };
        match by_path.iter_mut().find(|(p, _)| *p == spec.path) {
            Some((_, existing)) => {
                let prev = std::mem::take(existing);
                *existing = prev.merge(mr);
            }
            None => by_path.push((spec.path, mr)),
        }
    }
    by_path
        .into_iter()
        .fold(Router::new(), |r, (p, mr)| r.route(p, mr))
}
