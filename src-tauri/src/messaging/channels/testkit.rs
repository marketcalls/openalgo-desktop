//! What the alert channel tests share: an app context, and a stand-in for
//! Slack and Discord that records every request and answers as told (the
//! web tests' `provider` fixture).

use super::webhook::WebhookKind;
use crate::brokers::BrokerRegistry;
use crate::flow::contract::env::{HttpCall, HttpReply, OutboundHttp};
use crate::state::testing::{build, TestCtx};
use async_trait::async_trait;
use chrono::TimeZone;
use parking_lot::Mutex;
use serde_json::Value;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

/// Obviously fake, so no secret scanner mistakes them for real webhooks.
pub const SLACK_URL: &str = "https://hooks.slack.com/services/TFAKE/BFAKE/notarealslacktoken";
pub const DISCORD_URL: &str = "https://discord.com/api/webhooks/1234/not-a-real-discord-token";
pub const SECRETS: [&str; 2] = ["notarealslacktoken", "not-a-real-discord-token"];

/// Slack and Discord, played by the test.
pub struct Provider {
    pub status: Mutex<u16>,
    /// The transport failure `send` answers with instead, if any.
    pub fail: Mutex<Option<String>>,
    /// What the provider's name resolves to.
    pub addresses: Mutex<Vec<IpAddr>>,
    pub requests: Mutex<Vec<HttpCall>>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            status: Mutex::new(200),
            fail: Mutex::new(None),
            addresses: Mutex::new(vec!["104.18.0.10".parse().unwrap()]),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Provider {
    pub fn count(&self) -> usize {
        self.requests.lock().len()
    }

    /// The JSON body of request `i`.
    pub fn body(&self, i: usize) -> Value {
        let requests = self.requests.lock();
        serde_json::from_slice(requests[i].body.as_deref().unwrap()).unwrap()
    }

    /// The text request `i` posted, whichever provider it went to.
    pub fn text(&self, i: usize) -> String {
        let body = self.body(i);
        body.get("text")
            .or_else(|| body.get("content"))
            .and_then(Value::as_str)
            .unwrap()
            .to_string()
    }

    pub fn url(&self, i: usize) -> String {
        self.requests.lock()[i].url.clone()
    }
}

#[async_trait]
impl OutboundHttp for Provider {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
        Ok(self.addresses.lock().clone())
    }

    async fn send(&self, call: HttpCall) -> Result<HttpReply, String> {
        self.requests.lock().push(call);
        if let Some(reason) = self.fail.lock().clone() {
            return Err(reason);
        }
        Ok(HttpReply {
            status: *self.status.lock(),
            headers: Vec::new(),
            body: b"ok".to_vec(),
        })
    }
}

/// An app context whose Slack and Discord posts reach `Provider`.
pub fn with_provider() -> (TestCtx, Arc<Provider>) {
    let t = build(
        BrokerRegistry::new(),
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 11, 3, 45, 30)
            .unwrap(),
    );
    let provider = Arc::new(Provider::default());
    t.ctx.messaging.channels.http.set(provider.clone());
    (t, provider)
}

/// A validated URL of `kind`.
pub fn url(kind: WebhookKind, text: &str) -> super::webhook::WebhookUrl {
    kind.validate_url(Some(&Value::String(text.into())))
        .unwrap()
}

/// Wait until `kind`'s lane has run everything queued on it.
pub async fn drain(t: &TestCtx, kind: WebhookKind) {
    let lane = t.ctx.messaging.channels.lanes.get(&t.ctx, kind);
    for _ in 0..500 {
        if lane.pending() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the {:?} lane did not drain", kind);
}

/// A hook that holds the first call until the test releases it, and lets
/// every later call through: `entered` hears when the first call arrived.
pub struct Gate {
    pub hook: Arc<dyn Fn() + Send + Sync>,
    pub entered: std::sync::mpsc::Receiver<()>,
    pub release: std::sync::mpsc::Sender<()>,
}

pub fn gate() -> Gate {
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let release_rx = Mutex::new(Some(release_rx));
    let hook = Arc::new(move || {
        let Some(tx) = entered_tx.lock().take() else {
            return;
        };
        let _ = tx.send(());
        if let Some(rx) = release_rx.lock().take() {
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    });
    Gate {
        hook,
        entered,
        release,
    }
}
