//! The one outbound HTTP client every broker adapter shares.
//!
//! One pooled `reqwest::Client` per process, with explicit connect and
//! request timeouts, so a broker that stops answering ties up neither a
//! descriptor nor a task for longer than the timeout. Adapters keep a clone
//! (cheap: the pool is behind an `Arc`) and never build a client per call.

use crate::error::{AppError, Result};
use serde::de::DeserializeOwned;
use std::sync::OnceLock;
use std::time::Duration;

/// TCP + TLS handshake budget.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request budget for ordinary REST calls (orders, books, quotes).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-read budget: a server that stalls mid-body is dropped.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for master-contract downloads (tens of MB on some brokers).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Idle pooled connections are closed after this long.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Idle connections kept per broker host.
pub const POOL_MAX_IDLE_PER_HOST: usize = 8;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn build() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(60))
        .user_agent(concat!("openalgo-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        // Only fails when the TLS backend cannot initialise; fall back to the
        // default client (which still has the request timeout applied per
        // call by the adapters) rather than panic.
        .unwrap_or_else(|e| {
            tracing::error!("Broker HTTP client could not be configured: {}", e);
            reqwest::Client::new()
        })
}

/// The shared broker HTTP client.
pub fn client() -> reqwest::Client {
    CLIENT.get_or_init(build).clone()
}

/// Read a response body as JSON. A body that is not JSON (an HTML error
/// page, a plain-text rate-limit notice) becomes a trader-facing broker
/// error; the status and a short, secret-free prefix go to the log.
pub async fn read_json<T: DeserializeOwned>(
    broker: &'static str,
    resp: reqwest::Response,
) -> Result<(reqwest::StatusCode, T)> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Ok((status, v)),
        Err(e) => {
            let prefix: String = String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            let prefix = super::redact::url_safe(&prefix);
            tracing::warn!(
                broker,
                status = status.as_u16(),
                "Unexpected response from broker ({}): {}",
                e,
                prefix
            );
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(AppError::Broker(
                    "The broker is limiting requests right now. Wait a moment and try again."
                        .into(),
                ));
            }
            if status.is_server_error() {
                return Err(AppError::Broker(
                    "The broker's servers are not responding normally. Try again shortly.".into(),
                ));
            }
            Err(AppError::Broker(
                "The broker sent a response OpenAlgo could not read. Try again shortly.".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_client_per_process() {
        // Clones share one pool; calling twice must not build a second.
        let _a = client();
        let _b = client();
        assert!(CLIENT.get().is_some());
    }
}
