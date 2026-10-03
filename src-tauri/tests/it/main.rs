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

mod api_v1_support;
mod feed_support;
mod sandbox_support;
mod webui_support;

mod api_v1_contract;
mod api_v1_events;
mod broker_angel_http;
mod broker_contract;
mod broker_dhan_adapter;
mod broker_dhan_feed;
mod broker_fyers_adapter;
mod broker_fyers_streaming;
mod broker_groww_feed;
mod broker_groww_hygiene;
mod broker_kotak_adapter;
mod broker_kotak_feed;
mod broker_upstox_feed;
mod broker_upstox_feed_hygiene;
mod broker_upstox_rest;
mod brokers_noren;
mod brokers_xts;
mod feed_app;
mod feed_behaviour;
mod feed_bridge;
mod feed_conformance;
mod feed_hygiene;
mod feed_soak;
mod sandbox_concurrency;
mod sandbox_contract;
mod sandbox_engine;
mod sandbox_fills;
mod sandbox_gtt;
mod sandbox_props;
mod sandbox_schedule;
mod sandbox_web_scenarios;
mod webui2;
mod webui_admin;
mod webui_market;
mod webui_monitoring;
mod webui_settings;
