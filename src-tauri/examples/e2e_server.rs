//! The real app server for the Flow end-to-end journeys (`e2e/flow/`,
//! `playwright.flow.config.ts`, CI's `e2e-flow` job): the app's HTTP server
//! and every service its routes call, Flow's scheduler, monitors and
//! webhooks included (`AppState::open` starts them), on a port the system
//! picks, over a data folder of its own, with the scripted broker of CI's
//! native test build (`brokers::scripted`, a small NIFTY market) in the
//! Dhan (Sandbox) slot. No window, no market data server for SDK clients,
//! no update check and no other network: what a page or a webhook sender
//! reaches is this process alone.
//!
//!     cargo build --example e2e_server --features e2e-broker
//!     e2e_server --data <empty folder>
//!
//! It prints `OPENALGO_E2E_SERVER http://127.0.0.1:<port>` once the server
//! listens, then runs until its standard input closes (the test runner
//! that started it is gone, however it ended), SIGTERM or Ctrl-C, and stops
//! the server and the app's tasks before it exits.
//!
//! Never shipped: it needs the `e2e-broker` feature (Cargo.toml
//! `required-features`), which a release build refuses (build.rs and the
//! guard in lib.rs), and no bundle or release workflow builds examples.

use openalgo_desktop_lib::brokers::{scripted, BrokerRegistry};
use openalgo_desktop_lib::clock::SystemClock;
use openalgo_desktop_lib::security::keystore::MemoryKeyStore;
use openalgo_desktop_lib::state::{AppState, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::io::AsyncReadExt;

/// The line the test runner waits for, followed by the base URL.
const READY: &str = "OPENALGO_E2E_SERVER";

fn data_folder() -> Result<PathBuf, String> {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--data"), Some(dir)) => Ok(PathBuf::from(dir)),
        _ => Err("Usage: e2e_server --data <empty folder>".into()),
    }
}

/// Resolves when the parent closes our standard input (or it fails).
async fn stdin_closed() {
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 256];
    while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn terminated() {
    std::future::pending::<()>().await
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "openalgo_desktop_lib=info,warn",
        ))
        .init();
    let dir = match data_folder() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}", e);
            return ExitCode::from(2);
        }
    };
    let today = chrono::Utc::now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .date_naive();
    let (brokers, _) = scripted::registry(&BrokerRegistry::new(), today);
    let ctx = match AppState::open(
        &dir,
        OpenOptions {
            keystore: Arc::new(MemoryKeyStore::new()),
            clock: Arc::new(SystemClock),
            brokers: Arc::new(brokers),
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not open the data folder {}: {}", dir.display(), e);
            return ExitCode::FAILURE;
        }
    };
    // An ephemeral port: never 5000/8765 or the development ports.
    ctx.pin_listener_ports(0, 0);
    openalgo_desktop_lib::session::spawn_expiry_task(ctx.clone());
    let handle = match openalgo_desktop_lib::server::start(ctx.clone()).await {
        Ok(h) => h,
        Err(status) => {
            eprintln!("The server did not start: {:?}", status);
            ctx.shutdown().await;
            return ExitCode::FAILURE;
        }
    };
    let mut out = std::io::stdout();
    let announced = writeln!(out, "{} http://{}", READY, handle.addr).and_then(|_| out.flush());
    if announced.is_ok() {
        tokio::select! {
            _ = stdin_closed() => {}
            _ = terminated() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    handle.stop().await;
    ctx.shutdown().await;
    ExitCode::SUCCESS
}
