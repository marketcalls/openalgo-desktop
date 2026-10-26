//! Development server without the window: the app's HTTP server and the
//! services its routes call, a throwaway data directory and an in-memory
//! keystore, so it never touches the real app data or the OS keychain.
//! It starts only the HTTP server and the session expiry task: the market
//! data feed, the health sampler, the traffic and latency monitor, the
//! strategy and OpenScript schedulers and Chartink are not started (run the
//! full app for those; `docs/contributing.md`).
//!
//! `cargo run --example dev_server` then open http://127.0.0.1:5500/
//! (debug builds always use the development port 5500). Ctrl-C stops it.

use openalgo_desktop_lib::brokers::BrokerRegistry;
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::state::{AppState, OpenOptions};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "openalgo_desktop_lib=info,warn",
        ))
        .init();
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Could not create a temporary data directory: {}", e);
            return;
        }
    };
    let ctx = match AppState::open(
        dir.path(),
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(BrokerRegistry::new()),
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not open the data directory: {}", e);
            return;
        }
    };
    openalgo_desktop_lib::session::spawn_expiry_task(ctx.clone());
    let handle = match openalgo_desktop_lib::server::start(ctx.clone()).await {
        Ok(h) => h,
        Err(status) => {
            eprintln!("{:?}", status);
            return;
        }
    };
    println!(
        "OpenAlgo dev server on http://{} (pid {}), data in {}",
        handle.addr,
        std::process::id(),
        dir.path().display()
    );
    let _ = tokio::signal::ctrl_c().await;
    handle.stop().await;
    ctx.shutdown().await;
}
