//! Strategy module and RMS: the risk vectors, ports of the web's
//! `test/test_strategy_module_*.py` suites (each module is named after its
//! web source file, each test after its web test), every PORTED DEFECT, the
//! barrier-synchronised invariants, webhook auth and payload validation, the
//! session and `/api/v1/strategy` routes, and resource hygiene.
//!
//! Test names are `strategy::<module>::<test>`: `cargo test --test it
//! strategy::` runs all of them, `cargo test --test it
//! strategy::strategy_module_webhook` one web suite. Shared fixtures and
//! the test app are in `support.rs`.

mod support;

use openalgo_desktop_lib::strategy::engine::FillOpts;
use serde_json::{json, Value};
use support::*;

mod concurrency;
mod hygiene;
mod live_mode;
mod risk_vectors;
mod strategy_book;
mod strategy_module_api;
mod strategy_module_broadcast;
mod strategy_module_db;
mod strategy_module_engine;
mod strategy_module_order_dispatch;
mod strategy_module_order_events;
mod strategy_module_recovery;
mod strategy_module_risk;
mod strategy_module_scheduler;
mod strategy_module_signals;
mod strategy_module_state;
mod strategy_module_validation;
mod strategy_module_webhook;
mod strategy_restx_api;
mod webhook_security;
