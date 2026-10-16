//! Alice Blue order status feed (web `aliceblue_order_adapter.py`): `GET
//! /open-api/order-notify/ws/createWsToken` -> `result[0].orderToken`,
//! connect `wss://a3.aliceblueonline.com/open-api/order-notify/websocket`,
//! send `{"orderToken","userId"}`, heartbeat `{"heartbeat":"h","userId"}`
//! every 55 s, `t == "om"` frames carry Noren order fields. Like the market
//! socket it runs through the loopback relay.

use super::mapping::{self, s};
use super::Endpoints;
use crate::brokers::common::relay::{self, Open, RelayHandle, Session, Step, Upstream};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedSubscription, Message, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Order-feed heartbeat (under AliceBlue's 60 s timeout).
pub const ORDER_HEARTBEAT: Duration = Duration::from_secs(55);

/// What `createWsToken` produced.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderToken {
    Token(crate::security::Secret),
    /// The account cannot use the feed, or the session is gone.
    Refused(String),
    Unavailable,
}

fn order_feed_disabled() -> String {
    "AliceBlue order updates are not enabled for this account. Ask AliceBlue to enable the Order Status Feed; orders still work."
        .into()
}

/// `GET /open-api/order-notify/ws/createWsToken` -> `result[0].orderToken`
/// (an empty or non-JSON body means the feed is not enabled).
pub async fn fetch_order_token(http: &reqwest::Client, ep: &Endpoints, jwt: &str) -> OrderToken {
    let resp = match http
        .get(format!(
            "{}/open-api/order-notify/ws/createWsToken",
            ep.base
        ))
        .header("Authorization", format!("Bearer {}", jwt))
        .timeout(Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("AliceBlue createWsToken failed: {}", e.without_url());
            return OrderToken::Unavailable;
        }
    };
    let status = resp.status();
    if status == 401 || status == 403 {
        return OrderToken::Refused(
            "AliceBlue refused the order update session. Log in to AliceBlue again.".into(),
        );
    }
    if !status.is_success() {
        return OrderToken::Unavailable;
    }
    let body = resp.text().await.unwrap_or_default();
    if body.trim().is_empty() {
        tracing::warn!("AliceBlue createWsToken answered with an empty body");
        return OrderToken::Refused(order_feed_disabled());
    }
    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        tracing::warn!("AliceBlue createWsToken answered with a non-JSON body");
        return OrderToken::Refused(order_feed_disabled());
    };
    let token = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|r| s(r, "orderToken"))
        .unwrap_or_default();
    if token.is_empty() {
        tracing::warn!(
            "AliceBlue createWsToken returned no orderToken (status {})",
            s(&v, "status")
        );
        return OrderToken::Refused(order_feed_disabled());
    }
    OrderToken::Token(crate::security::Secret::new(token))
}

struct OrderUpstream {
    http: reqwest::Client,
    ep: Endpoints,
    jwt: crate::security::Secret,
    ucc: String,
    token: parking_lot::Mutex<Option<crate::security::Secret>>,
}

#[async_trait]
impl Upstream for OrderUpstream {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    async fn open(&self) -> Open {
        let token = match fetch_order_token(&self.http, &self.ep, self.jwt.expose()).await {
            OrderToken::Token(t) => t,
            OrderToken::Refused(m) => return Open::AuthFailed(m),
            OrderToken::Unavailable => return Open::Unavailable,
        };
        *self.token.lock() = Some(token);
        match tokio::time::timeout(
            relay::OPEN_TIMEOUT,
            tokio_tungstenite::connect_async(self.ep.order_ws.as_str()),
        )
        .await
        {
            Ok(Ok((ws, _))) => Open::Ready(Box::new(ws)),
            _ => Open::Unavailable,
        }
    }

    fn session(&self) -> Box<dyn Session> {
        let token = self
            .token
            .lock()
            .as_ref()
            .map(|t| t.expose().to_string())
            .unwrap_or_default();
        Box::new(OrderSession {
            subscribe: order_subscribe_frame(&token, &self.ucc),
            heartbeat: order_heartbeat_frame(&self.ucc),
        })
    }
}

pub fn order_subscribe_frame(order_token: &str, ucc: &str) -> String {
    json!({"orderToken": order_token, "userId": ucc}).to_string()
}

pub fn order_heartbeat_frame(ucc: &str) -> String {
    json!({"heartbeat": "h", "userId": ucc}).to_string()
}

/// Relay-side protocol of the order socket.
pub struct OrderSession {
    pub subscribe: String,
    pub heartbeat: String,
}

impl Session for OrderSession {
    fn on_open(&mut self) -> Vec<Message> {
        vec![Message::Text(self.subscribe.clone())]
    }

    fn on_upstream(&mut self, msg: Message) -> Step {
        match msg {
            m @ Message::Text(_) => Step {
                down: vec![m],
                ..Default::default()
            },
            _ => Step::default(),
        }
    }

    fn on_downstream(&mut self, _msg: Message) -> Vec<Message> {
        Vec::new()
    }

    fn keepalive(&self) -> Option<(Duration, Message)> {
        Some((ORDER_HEARTBEAT, Message::Text(self.heartbeat.clone())))
    }
}

/// The order-update feed (`FeedEvent::OrderUpdate` per `om` frame).
pub struct AliceBlueOrderFeed {
    upstream: Arc<OrderUpstream>,
    relay: parking_lot::Mutex<Option<RelayHandle>>,
    symbols: SymbolResolver,
}

impl AliceBlueOrderFeed {
    pub fn new(
        http: reqwest::Client,
        ep: Endpoints,
        jwt: &str,
        ucc: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            upstream: Arc::new(OrderUpstream {
                http,
                ep,
                jwt: crate::security::Secret::new(jwt),
                ucc: ucc.to_string(),
                token: parking_lot::Mutex::new(None),
            }),
            relay: parking_lot::Mutex::new(None),
            symbols,
        }
    }
}

impl BrokerFeed for AliceBlueOrderFeed {
    fn broker(&self) -> &'static str {
        "aliceblue"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = relay::ensure_started(&self.relay, move || up as Arc<dyn Upstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("AliceBlue order feed relay address is invalid".into()))
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    /// The relay opens the broker side before its ready frame comes.
    fn auth_ack_deadline(&self) -> Option<Duration> {
        Some(relay::READY_DEADLINE)
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => {
                if let Some(c) = relay::control(t) {
                    return match c {
                        Ok(()) => vec![FeedEvent::AuthOk],
                        Err(m) => vec![FeedEvent::AuthFailed(m)],
                    };
                }
                serde_json::from_str::<Value>(t)
                    .ok()
                    .and_then(|v| mapping::order_update_from(&v, &self.symbols))
                    .map(|u| vec![FeedEvent::OrderUpdate(u)])
                    .unwrap_or_default()
            }
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }
}
