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

/// Run `fut` with a 10 ms timer beside it on this test's runtime and
/// return how late the timer ran at worst: how long `fut` held the runtime
/// thread (`#[tokio::test]` runs on one thread), so blocking work it should
/// have moved off the async workers shows (DB-01, DB-03).
pub async fn runtime_stall<F: std::future::Future>(fut: F) -> (F::Output, std::time::Duration) {
    use std::sync::atomic::{AtomicU64, Ordering};
    let worst_us = std::sync::Arc::new(AtomicU64::new(0));
    let w = worst_us.clone();
    let ticker = tokio::spawn(async move {
        let tick = std::time::Duration::from_millis(10);
        loop {
            let t = std::time::Instant::now();
            tokio::time::sleep(tick).await;
            let late = t.elapsed().saturating_sub(tick).as_micros() as u64;
            w.fetch_max(late, Ordering::Relaxed);
        }
    });
    // Let the timer start before `fut` runs.
    tokio::task::yield_now().await;
    let out = fut.await;
    // Give the timer a turn: a `fut` that blocked and then returned at
    // once has not let it see how late it is yet.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    ticker.abort();
    let worst = std::time::Duration::from_micros(worst_us.load(Ordering::Relaxed));
    (out, worst)
}

/// Run `fut` and return how long its polls took in all: the time it kept
/// this test's runtime thread busy, however finely it split the work. Work
/// done a little per poll on the async worker (a page of an export
/// formatted per poll) never shows as one long hold in [`runtime_stall`],
/// whose bound must leave room for a loaded runner, but adds up here.
pub async fn runtime_busy<F: std::future::Future>(fut: F) -> (F::Output, std::time::Duration) {
    let mut fut = std::pin::pin!(fut);
    let mut busy = std::time::Duration::ZERO;
    let out = std::future::poll_fn(|cx| {
        let t = std::time::Instant::now();
        let r = fut.as_mut().poll(cx);
        busy += t.elapsed();
        r
    })
    .await;
    (out, busy)
}

/// Run `test` on a current-thread runtime, as `#[tokio::test]` does, that
/// notes each time its thread goes idle and wakes, for
/// [`IdleLog::longest_hold`].
pub fn on_watched_runtime<F, Fut>(test: F) -> Fut::Output
where
    F: FnOnce(IdleLog) -> Fut,
    Fut: std::future::Future,
{
    let log = IdleLog::default();
    let (idle, woke) = (log.clone(), log.clone());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_park(move || idle.note(true))
        .on_thread_unpark(move || woke.note(false))
        .build()
        .unwrap()
        .block_on(test(log))
}

/// When the thread of an [`on_watched_runtime`] runtime went idle (`true`)
/// and woke (`false`).
#[derive(Clone, Default)]
pub struct IdleLog(std::sync::Arc<parking_lot::Mutex<Vec<(std::time::Instant, bool)>>>);

impl IdleLog {
    fn note(&self, idle: bool) {
        self.0.lock().push((std::time::Instant::now(), idle));
    }

    /// Wait until the runtime thread goes idle: every task that was ready
    /// (the background tasks an `AppState` starts, on their first pass) has
    /// run to its next wait.
    pub async fn until_idle(&self) {
        let since = std::time::Instant::now();
        // A timer, not `yield_now`: the thread parks, and notes it, only
        // while nothing is ready.
        while !self.0.lock().iter().any(|&(at, idle)| idle && at >= since) {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    /// Run `fut` and return the longest the runtime thread ran meanwhile
    /// without going idle: how long `fut`, or a task it started, held the
    /// runtime at once. Unlike [`runtime_stall`], it leaves out the time an
    /// idle thread waits for the operating system to run it again, which a
    /// loaded runner stretches: a CI runner once left the 10 ms timer 374 ms
    /// late while the requests it watched took 11.8 ms of polls in all.
    pub async fn longest_hold<F: std::future::Future>(
        &self,
        fut: F,
    ) -> (F::Output, std::time::Duration) {
        let start = std::time::Instant::now();
        let out = fut.await;
        let end = std::time::Instant::now();
        let mut longest = std::time::Duration::ZERO;
        // Running from `start`: this future is being polled.
        let mut running_since = Some(start);
        for &(at, idle) in self
            .0
            .lock()
            .iter()
            .filter(|(at, _)| (start..end).contains(at))
        {
            match (idle, running_since) {
                (true, Some(since)) => {
                    longest = longest.max(at - since);
                    running_since = None;
                }
                (false, None) => running_since = Some(at),
                _ => {}
            }
        }
        if let Some(since) = running_since {
            longest = longest.max(end - since);
        }
        (out, longest)
    }
}

mod alloc_support;
mod api_v1_support;
mod broker_support;
mod feed_support;
mod mcp_support;
mod sandbox_support;
mod webui_support;

mod analyzer_mode;
mod api_v1_chart;
mod api_v1_contract;
mod api_v1_events;
mod api_v1_inventory;
mod api_v1_latency;
mod api_v1_quantity;
mod api_v1_telegram;
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
mod broker_malformed_replies;
mod broker_order_outcomes;
mod broker_rupeezy;
mod broker_session_e2e;
mod broker_unread_routes;
mod broker_upstox_feed;
mod broker_upstox_feed_hygiene;
mod broker_upstox_rest;
mod brokers_direct_batch_a;
mod brokers_direct_batch_b;
mod brokers_noren;
mod brokers_oauth_batch;
mod brokers_xts;
mod chartink;
mod data_upgrade;
mod e2e_scripted_broker;
mod feed_app;
mod feed_behaviour;
mod feed_bridge;
mod feed_budget;
mod feed_conformance;
mod feed_hygiene;
mod feed_soak;
mod freeze_limits;
mod historify;
mod lifecycle;
mod mcp_caps;
mod mcp_contract;
mod mcp_http;
mod mcp_outputs;
mod mcp_research;
mod mcp_security;
mod mcp_subcommand;
mod mcp_transport;
mod messaging;
mod options_destination;
mod options_uncertainty;
mod order_identity;
mod order_quantity_safety;
mod order_split_bounds;
mod perf;
mod polled_reads;
mod sandbox_concurrency;
mod sandbox_contract;
mod sandbox_engine;
mod sandbox_fills;
mod sandbox_gtt;
mod sandbox_place_atomic;
mod sandbox_props;
mod sandbox_restart;
mod sandbox_schedule;
mod sandbox_web_scenarios;
mod scalping;
mod security_memory;
mod soak;
mod strategy;
mod tools;
mod tools_discovery;
mod trading_files;
mod trading_runner;
mod webui2;
mod webui_admin;
mod webui_market;
mod webui_monitoring;
mod webui_record_loss;
mod webui_reports;
mod webui_settings;
mod webui_update;
