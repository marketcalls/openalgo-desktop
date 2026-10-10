//! Shared harness for the feed server tests: start a server on an ephemeral
//! loopback port (never 5000 or 8765) against a `FakeSource`, and a small
//! JSON WebSocket client.

#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use openalgo_desktop_lib::events::OrderUpdate;
use openalgo_desktop_lib::feed::auth::StaticAuth;
use openalgo_desktop_lib::feed::orders::OrderRelay;
use openalgo_desktop_lib::feed::{
    start, FakeSource, FeedConfig, FeedDeps, FeedHandle, InstrumentKey,
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub const API_KEY: &str = "feed-test-key-0123456789abcdef";
pub const USER_ID: &str = "<USER_ID>";
pub const BROKER: &str = "zerodha";

pub struct Harness {
    pub handle: FeedHandle,
    pub source: Arc<FakeSource>,
    pub relay: Arc<OrderRelay>,
    pub url: String,
}

pub fn known() -> Vec<InstrumentKey> {
    vec![
        InstrumentKey::new("RELIANCE", "NSE"),
        InstrumentKey::new("SBIN", "NSE"),
        InstrumentKey::new("INFY", "NSE"),
        InstrumentKey::new("NIFTY", "NSE_INDEX"),
    ]
}

pub fn brokers() -> Vec<String> {
    openalgo_desktop_lib::brokers::catalog::ALL_BROKERS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub async fn start_with(source: Arc<FakeSource>, tweak: impl FnOnce(&mut FeedConfig)) -> Harness {
    start_full(source, OrderRelay::new(), Some(BROKER), tweak).await
}

pub async fn start_full(
    source: Arc<FakeSource>,
    relay: Arc<OrderRelay>,
    broker: Option<&str>,
    tweak: impl FnOnce(&mut FeedConfig),
) -> Harness {
    let mut cfg = FeedConfig {
        port: 0,
        ..FeedConfig::default()
    };
    tweak(&mut cfg);
    let handle = start(
        cfg,
        FeedDeps {
            source: source.clone(),
            auth: Arc::new(StaticAuth {
                api_key: API_KEY.into(),
                user_id: USER_ID.into(),
                broker: broker.map(String::from),
            }),
            orders: Some(relay.receiver()),
            supported_brokers: brokers(),
        },
    )
    .await
    .expect("feed server starts on an ephemeral port");
    let url = format!("ws://{}", handle.local_addr());
    Harness {
        handle,
        source,
        relay,
        url,
    }
}

pub async fn start_default() -> Harness {
    start_with(FakeSource::new(known()), |_| {}).await
}

pub fn order_update(orderid: &str) -> OrderUpdate {
    OrderUpdate {
        mode: "analyze".into(),
        broker: "sandbox".into(),
        orderid: orderid.into(),
        symbol: "NIFTY25AUG26FUT".into(),
        exchange: "NFO".into(),
        action: "BUY".into(),
        quantity: 65,
        price: 24077.8,
        trigger_price: 0.0,
        pricetype: "MARKET".into(),
        product: "NRML".into(),
        order_status: "complete".into(),
        filled_quantity: 65,
        pending_quantity: 0,
        average_price: 24077.8,
        rejection_reason: String::new(),
        session_generation: 0,
    }
}

pub struct Client {
    pub ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

pub const RECV_TIMEOUT: Duration = Duration::from_secs(5);

impl Client {
    pub async fn connect(url: &str) -> Client {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("client connects");
        Client { ws }
    }

    pub async fn send(&mut self, v: &Value) {
        self.send_raw(&v.to_string()).await;
    }

    pub async fn send_raw(&mut self, text: &str) {
        self.ws
            .send(Message::Text(text.to_string()))
            .await
            .expect("client send");
    }

    /// Next JSON text frame, or `None` on close / timeout.
    pub async fn recv_within(&mut self, d: Duration) -> Option<Value> {
        loop {
            let msg = tokio::time::timeout(d, self.ws.next()).await.ok()??.ok()?;
            match msg {
                Message::Text(t) => return serde_json::from_str(&t).ok(),
                Message::Close(_) => return None,
                _ => continue,
            }
        }
    }

    pub async fn recv(&mut self) -> Value {
        self.recv_within(RECV_TIMEOUT)
            .await
            .expect("expected a frame from the feed server")
    }

    /// Wait for the server to close; returns (code, reason).
    pub async fn expect_close(&mut self, d: Duration) -> Option<(u16, String)> {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.ws.next()).await {
                Err(_) => return None,
                Ok(None) => return Some((1006, String::new())),
                Ok(Some(Err(_))) => return Some((1006, String::new())),
                Ok(Some(Ok(Message::Close(Some(f))))) => {
                    return Some((u16::from(f.code), f.reason.to_string()))
                }
                Ok(Some(Ok(Message::Close(None)))) => return Some((1005, String::new())),
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    pub async fn auth(&mut self) -> Value {
        self.send(&serde_json::json!({"action": "authenticate", "api_key": API_KEY}))
            .await;
        let v = self.recv().await;
        assert_eq!(v["type"], "auth", "{}", v);
        v
    }

    pub async fn request(&mut self, v: Value) -> Value {
        self.send(&v).await;
        self.recv().await
    }
}

/// Poll `f` until it is true or `d` passes.
pub async fn eventually(d: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        if f() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Open descriptors of this process.
pub fn fd_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc/self/fd")
            .map(|d| d.count())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
    }
}

/// Resident set size of this process in KiB.
pub fn rss_kib() -> u64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmRSS:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|n| n.parse().ok())
            })
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }
}
