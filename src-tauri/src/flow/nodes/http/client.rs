//! The HTTP Request node's own client (lead decision 12): one shared
//! `reqwest` client, never the broker's and never one per call, that
//!
//! - connects only to the address the destination check vetted: its
//!   resolver answers a name only with the address pinned for the request
//!   in flight, and refuses a name nothing pinned, so there is no second
//!   lookup for a rebinding name to answer differently;
//! - never follows a redirect (a 3xx is the node's result, as on the web);
//! - ignores the system proxy settings;
//! - stops reading a response at [`MAX_RESPONSE_BYTES`];
//! - gives every request the node's own timeout, as one deadline (httpx
//!   applies the same length to each phase).

use crate::flow::contract::env::{HttpCall, HttpReply, OutboundHttp};
use async_trait::async_trait;
use parking_lot::Mutex;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The largest response body read (5 MB).
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// What a longer body reads.
pub const TOO_LARGE: &str = "the response is larger than 5 MB, so it was not read";

/// How long a name lookup may take.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Idle connections are closed after this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The vetted address each request in flight may use, by host name.
#[derive(Default)]
struct Pins {
    next: AtomicU64,
    by_host: Mutex<HashMap<String, Vec<(u64, IpAddr)>>>,
}

/// One pin, released when the request is done (on every path).
struct Pin<'a> {
    pins: &'a Pins,
    host: String,
    id: u64,
}

impl Pins {
    fn pin(&self, host: &str, ip: IpAddr) -> Pin<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.by_host
            .lock()
            .entry(host.to_string())
            .or_default()
            .push((id, ip));
        Pin {
            pins: self,
            host: host.to_string(),
            id,
        }
    }

    fn addresses(&self, host: &str) -> Vec<IpAddr> {
        self.by_host
            .lock()
            .get(host)
            .map(|v| v.iter().map(|(_, ip)| *ip).collect())
            .unwrap_or_default()
    }
}

impl Drop for Pin<'_> {
    fn drop(&mut self) {
        let mut map = self.pins.by_host.lock();
        if let Some(list) = map.get_mut(&self.host) {
            list.retain(|(id, _)| *id != self.id);
            if list.is_empty() {
                map.remove(&self.host);
            }
        }
    }
}

/// A resolver that only answers with pinned, already vetted addresses.
struct PinnedResolver(Arc<Pins>);

impl Resolve for PinnedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let found = self.0.addresses(name.as_str());
        Box::pin(async move {
            if found.is_empty() {
                let refused: Box<dyn std::error::Error + Send + Sync> =
                    "this address was not checked, so it is not used".into();
                return Err(refused);
            }
            let addrs: Addrs = Box::new(found.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// The real [`OutboundHttp`] Flow runs use.
pub struct GuardedHttp {
    client: reqwest::Client,
    pins: Arc<Pins>,
}

impl GuardedHttp {
    pub fn new() -> Result<Self, String> {
        let pins = Arc::new(Pins::default());
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(PinnedResolver(pins.clone())))
            .pool_idle_timeout(IDLE_TIMEOUT)
            .pool_max_idle_per_host(2)
            .user_agent(concat!("OpenAlgo-Desktop-Flow/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, pins })
    }
}

/// A transport failure in a few words, never empty.
fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "timed out".into();
    }
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text = s.to_string();
        source = s.source();
    }
    if e.is_connect() {
        return format!("could not connect ({})", text);
    }
    if text.is_empty() {
        "the request failed".into()
    } else {
        text
    }
}

#[async_trait]
impl OutboundHttp for GuardedHttp {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let looked = tokio::time::timeout(LOOKUP_TIMEOUT, tokio::net::lookup_host((host, 0))).await;
        match looked {
            Err(_) => Err("timed out".into()),
            Ok(Err(e)) => Err(e.to_string()),
            Ok(Ok(addrs)) => {
                let mut out: Vec<IpAddr> = Vec::new();
                for a in addrs {
                    if !out.contains(&a.ip()) {
                        out.push(a.ip());
                    }
                }
                Ok(out)
            }
        }
    }

    async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
        let url = reqwest::Url::parse(&call.url).map_err(|e| e.to_string())?;
        let ip = call.connect_to.ip();
        let _pin = match url.host() {
            Some(::url::Host::Domain(d)) => Some(self.pins.pin(d, ip)),
            Some(::url::Host::Ipv4(a)) if IpAddr::V4(a) == ip => None,
            Some(::url::Host::Ipv6(a)) if IpAddr::V6(a) == ip => None,
            _ => return Err("this address was not checked, so it is not used".into()),
        };
        let method =
            reqwest::Method::from_bytes(call.method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self.client.request(method, url).timeout(call.timeout);
        for (k, v) in &call.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(body) = call.body {
            // An empty body still states its length, as httpx does
            // (Content-Length: 0): a server may refuse a POST without one.
            let stated = call
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
            if body.is_empty() && !stated {
                req = req.header("content-length", "0");
            }
            req = req.body(body);
        }
        let mut resp = req.send().await.map_err(|e| reason(&e))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| reason(&e))? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(TOO_LARGE.into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpReply {
            status,
            headers,
            body,
        })
    }
}

/// Give every later run the guarded client (called once at start-up).
pub fn install(flow: &crate::flow::FlowModule) {
    match GuardedHttp::new() {
        Ok(http) => flow.install_env(|env| env.http = Arc::new(http)),
        Err(e) => tracing::error!("Flow's HTTP client could not be built: {}", e),
    }
}

#[cfg(test)]
mod tests;
