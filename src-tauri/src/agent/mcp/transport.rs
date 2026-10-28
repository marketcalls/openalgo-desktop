//! Where an external data request goes out (the web's `_shared_post`), and
//! the clock the breaker and the caches read.
//!
//! [`HttpTransport`] is the Agent's own HTTP client rules
//! (`run::client::client_rules`: rustls on ring, no redirect followed, names
//! resolved through the guarded resolver that never hands out a link-local
//! address) with the server's connect and read timeouts; each request is
//! bounded by what is left of its call's deadline. One per registered
//! server, built when the server is first used. No OpenAlgo key or broker
//! credential is ever sent.

use super::wire::Reply;
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The most characters of a reply this client reads (the web's
/// `MAX_RESPONSE_CHARS`): an answer larger than this is not something the
/// toolkit could hand the model anyway.
pub const MAX_RESPONSE_CHARS: usize = 2_000_000;

/// A monotonic clock (a test moves its own by hand).
pub trait Clock: Send + Sync {
    /// Time since an arbitrary start.
    fn now(&self) -> Duration;
}

/// The process's monotonic clock.
pub struct SystemClock(Instant);

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock(Instant::now())
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// One attempt's limits: connecting, each read, and the whole request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
    pub total: Duration,
}

/// Why no reply came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Connected, or connecting, past a limit.
    TimedOut(String),
    /// The connection failed.
    Unreachable(String),
}

/// One POST of one JSON-RPC message.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: Vec<u8>,
        timeouts: Timeouts,
    ) -> Result<Reply, Failure>;
}

/// The real transport, on the Agent's HTTP client rules.
pub struct HttpTransport {
    client: reqwest_agent::Client,
}

impl HttpTransport {
    /// A client for one server, with its connect and read timeouts.
    pub fn new(connect: Duration, read: Duration) -> Result<HttpTransport, String> {
        let client = crate::agent::run::client::client_rules(
            crate::agent::providers::egress::GuardedResolver::system(),
        )?
        .connect_timeout(connect)
        .read_timeout(read)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| format!("The external data client could not be built: {}", e))?;
        Ok(HttpTransport { client })
    }
}

fn failure(e: &reqwest_agent::Error) -> Failure {
    let detail = if e.is_connect() {
        "connect error"
    } else {
        "transport error"
    };
    if e.is_timeout() {
        Failure::TimedOut("timeout".into())
    } else {
        Failure::Unreachable(detail.into())
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: Vec<u8>,
        timeouts: Timeouts,
    ) -> Result<Reply, Failure> {
        let mut request = self.client.post(url).timeout(timeouts.total).body(body);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let mut response = request.send().await.map_err(|e| failure(&e))?;
        let status = response.status().as_u16();
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_lowercase(), v.to_str().ok()?.to_string())))
            .collect();
        // Read at most what the longest usable answer could take in UTF-8,
        // so a server cannot make the app hold an unbounded body.
        let cap = MAX_RESPONSE_CHARS * 4;
        let mut bytes: Vec<u8> = Vec::new();
        let mut oversized = false;
        while let Some(chunk) = response.chunk().await.map_err(|e| failure(&e))? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > cap {
                oversized = true;
                break;
            }
        }
        Ok(Reply {
            status,
            headers,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            oversized,
        })
    }
}
