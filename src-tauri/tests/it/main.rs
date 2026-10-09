//! The one integration test crate (`cargo test --test it`).
//!
//! Every integration test lives here so the ~190 MB library links into one
//! test binary instead of one per file. Layout:
//!
//! - One module per area, `tests/it/<area>_<topic>.rs` (for example
//!   `feed_bridge`, `broker_upstox_feed`). Test names are
//!   `<module>::<test>`, so `cargo test --test it feed_bridge` runs one
//!   module and `cargo test --test it feed_` an area.
//! - Shared helpers are `*_support/mod.rs` modules declared once below and
//!   imported with `use crate::<name>_support;`.
//! - Fixtures stay in `tests/fixtures/` (`include_str!("../fixtures/...")`
//!   from a module here) or the repository's `tests/fixtures/web/`.
//! - Tests that measure the whole process (descriptors, RSS) start with
//!   `crate::isolated!(test_name);` and re-run alone in a child process
//!   (`isolate.rs`), since other modules run in parallel threads here.
//!
//! A new test file is a new `mod` line below, not a new file in `tests/`.

mod isolate;

/// Name the app's own loopback address in `Host`, as every real client
/// does, unless the test set one. Harnesses that hand a request a
/// connection (`ConnectInfo`) need it: a request over a connection without
/// `Host` is refused (security review S-13), and only a request naming the
/// app's own address counts as this computer (S-03).
pub fn with_host(
    req: &mut axum::http::Request<axum::body::Body>,
    ctx: &openalgo_desktop_lib::state::AppState,
) {
    if !req.headers().contains_key(axum::http::header::HOST) {
        let host = format!("127.0.0.1:{}", ctx.server_config().http_port);
        if let Ok(v) = axum::http::HeaderValue::from_str(&host) {
            req.headers_mut().insert(axum::http::header::HOST, v);
        }
    }
}

mod api_v1_support;
mod feed_support;
mod mcp_support;
mod sandbox_support;
mod webui_support;

mod api_v1_contract;
mod api_v1_events;
mod broker_angel_http;
mod broker_contract;
mod broker_deltaexchange;
mod broker_dhan_adapter;
mod broker_dhan_feed;
mod broker_fyers_adapter;
mod broker_fyers_streaming;
mod broker_groww_feed;
mod broker_groww_hygiene;
mod broker_kotak_adapter;
mod broker_kotak_feed;
mod broker_session_e2e;
mod broker_upstox_feed;
mod broker_upstox_feed_hygiene;
mod broker_upstox_rest;
mod brokers_direct_batch_a;
mod brokers_direct_batch_b;
mod brokers_noren;
mod brokers_oauth_batch;
mod brokers_xts;
mod chartink;
mod feed_app;
mod feed_behaviour;
mod feed_bridge;
mod feed_conformance;
mod feed_hygiene;
mod feed_soak;
mod historify;
mod mcp_caps;
mod mcp_contract;
mod mcp_http;
mod mcp_research;
mod mcp_security;
mod mcp_subcommand;
mod mcp_transport;
mod messaging;
mod sandbox_concurrency;
mod sandbox_contract;
mod sandbox_engine;
mod sandbox_fills;
mod sandbox_gtt;
mod sandbox_props;
mod sandbox_schedule;
mod sandbox_web_scenarios;
mod scalping;
mod soak;
mod strategy;
mod tools;
mod trading_files;
mod trading_runner;
mod webui2;
mod webui_admin;
mod webui_market;
mod webui_monitoring;
mod webui_settings;
