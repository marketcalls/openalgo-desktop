//! The one outbound HTTP client every broker adapter shares.
//!
//! One pooled `reqwest::Client` per process, with explicit connect and
//! request timeouts, so a broker that stops answering ties up neither a
//! descriptor nor a task for longer than the timeout. Adapters keep a clone
//! (cheap: the pool is behind an `Arc`) and never build a client per call.
//!
//! The client offers HTTP/2 and HTTP/1.1 by ALPN, like the web's httpx
//! client in its default mode, and each broker host picks. Zerodha, Dhan,
//! Fyers, Upstox's main host and most others pick HTTP/2; Upstox's order
//! host (`api-hft`), Angel, Motilal, Pocketful and the XTS hosts answer
//! HTTP/1.1 only (measured 2026-10-16). The web turns HTTP/2 off in its
//! Docker mode because of failures in Python's HTTP stack (h11 parse
//! errors, a Windows socket error inside httpcore's HTTP/2 code; web issues
//! #277 and #2067). No broker's server misbehaved over HTTP/2, so every
//! broker shares this one client.
//!
//! What a failed HTTP/2 order request means for the order (RFC 9113
//! section 8.7) is decided in `outcome::after_send`.

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
/// A pooled connection unused for this long is not used again, and is
/// closed. Half the shortest idle close measured on a broker order host
/// (Upstox's `api-hft`, 60 s; the XTS hosts, Motilal, Pocketful and
/// Shoonya about 65 s), and the web's `keepalive_expiry`. An order sent on
/// a connection the server is closing at that moment loses its answer, and
/// its outcome is unknown.
pub const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle connections kept per broker host. An HTTP/2 host needs one; on an
/// HTTP/1.1 host, a basket batch (ten legs at once) and the books and quotes
/// read beside it all go out on warm connections.
pub const POOL_MAX_IDLE_PER_HOST: usize = 16;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// The shared client's settings (tests build from them too).
pub(super) fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .tcp_keepalive(Duration::from_secs(60))
        .user_agent(concat!("openalgo-desktop/", env!("CARGO_PKG_VERSION")))
}

fn build() -> reqwest::Client {
    builder()
        .build()
        // Fails only when the TLS backend cannot initialise, and
        // `Client::new()` panics on that same failure, so this fallback is
        // not reached in practice. If it were, the client would have none
        // of the timeouts above: most adapters set no timeout per call and
        // rely on the ones configured here (BR-04).
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
///
/// Inside an order placement (`outcome::placing`) the answers are the same
/// and keep the wire unchanged; their meaning follows the placement rules:
/// a server error is noted (no later refusal in the placement reads as "not
/// placed"), an unreadable body on anything but a 4xx stays an ordinary
/// broker error (uncertain), and only a 4xx or a rate-limit notice is a
/// refusal.
pub async fn read_json<T: DeserializeOwned>(
    broker: &'static str,
    resp: reqwest::Response,
) -> Result<(reqwest::StatusCode, T)> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if status.is_server_error() {
        super::outcome::note_server_error();
    }
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
                return Err(super::outcome::refusal(
                    "The broker is limiting requests right now. Wait a moment and try again.",
                ));
            }
            if status.is_client_error() {
                return Err(super::outcome::refusal(
                    "The broker sent a response OpenAlgo could not read. Try again shortly.",
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
    use crate::services::batch_order_service::{BASKET_BATCH, BASKET_BATCH_PAUSE};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// The shortest idle close measured on a broker order host (2026-10-16):
    /// Upstox's `api-hft.upstox.com` answers `Keep-Alive: timeout=60` and
    /// closes then.
    const SHORTEST_BROKER_IDLE_CLOSE: Duration = Duration::from_secs(60);

    #[test]
    fn one_client_per_process() {
        // Clones share one pool; calling twice must not build a second.
        let _a = client();
        let _b = client();
        assert!(CLIENT.get().is_some());
    }

    #[test]
    fn a_pooled_connection_is_retired_well_before_a_broker_closes_it() {
        const {
            assert!(POOL_IDLE_TIMEOUT.as_secs() * 2 <= SHORTEST_BROKER_IDLE_CLOSE.as_secs());
        }
    }

    /// The ALPN protocols named in a TLS ClientHello record.
    fn alpn_offer(hello: &[u8]) -> Vec<String> {
        let u16_at = |i: usize| u16::from_be_bytes([hello[i], hello[i + 1]]) as usize;
        // Record header, handshake header, version, random.
        let mut i = 5 + 4 + 2 + 32;
        i += 1 + hello[i] as usize; // session id
        i += 2 + u16_at(i); // cipher suites
        i += 1 + hello[i] as usize; // compression methods
        let end = i + 2 + u16_at(i);
        i += 2;
        while i + 4 <= end {
            let (kind, len) = (u16_at(i), u16_at(i + 2));
            if kind == 16 {
                let list = &hello[i + 6..i + 4 + len];
                let mut names = Vec::new();
                let mut j = 0;
                while j < list.len() {
                    let n = list[j] as usize;
                    names.push(String::from_utf8_lossy(&list[j + 1..j + 1 + n]).into_owned());
                    j += 1 + n;
                }
                return names;
            }
            i += 4 + len;
        }
        Vec::new()
    }

    #[tokio::test]
    async fn the_shared_client_offers_http2_and_falls_back_to_http1() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}/", l.local_addr().unwrap());
        let hello = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut record = vec![0u8; 5];
            s.read_exact(&mut record).await.unwrap();
            let len = u16::from_be_bytes([record[3], record[4]]) as usize;
            record.resize(5 + len, 0);
            s.read_exact(&mut record[5..]).await.unwrap();
            record
        });
        // The handshake goes no further: the server only reads the hello.
        let _ = builder().no_proxy().build().unwrap().get(url).send().await;
        assert_eq!(alpn_offer(&hello.await.unwrap()), ["h2", "http/1.1"]);
    }

    /// A keep-alive HTTP/1.1 server that answers only once `burst` requests
    /// are waiting, so one burst holds `burst` connections at once. Counts
    /// the connections it accepts.
    async fn burst_server(burst: usize) -> (String, Arc<AtomicUsize>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/orders", l.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let all_waiting = Arc::new(tokio::sync::Barrier::new(burst));
        let count = accepted.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let all_waiting = all_waiting.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    // A GET arrives in one read.
                    while matches!(s.read(&mut buf).await, Ok(n) if n > 0) {
                        all_waiting.wait().await;
                        let ok = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}";
                        if s.write_all(ok).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (url, accepted)
    }

    #[tokio::test]
    async fn a_basket_batch_goes_out_on_the_previous_batch_connections() {
        // An HTTP/1.1 broker host carries one request per connection at a
        // time: a batch of ten legs holds ten connections.
        let (url, accepted) = burst_server(BASKET_BATCH).await;
        let c = builder().no_proxy().build().unwrap();
        for batch in 0..2 {
            if batch > 0 {
                tokio::time::sleep(BASKET_BATCH_PAUSE).await;
            }
            let legs = (0..BASKET_BATCH).map(|_| async { c.get(&url).send().await?.bytes().await });
            for leg in futures_util::future::join_all(legs).await {
                leg.unwrap();
            }
        }
        assert_eq!(accepted.load(Ordering::SeqCst), BASKET_BATCH);
    }
}
