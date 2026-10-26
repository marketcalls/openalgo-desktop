//! Kotak order updates (web `streaming/kotak_order_adapter.py`): the
//! realtime socket at the session's `baseUrl` host.

use super::KotakSession;
use crate::brokers::common::streaming::{
    ping, BrokerFeed, FeedEvent, FeedSubscription, Message, OrderUpdate, WsRequest,
    WEB_PING_INTERVAL, WEB_PING_TIMEOUT,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// `wss://<baseUrl host>/realtime` (web `_realtime_ws_url`).
pub fn realtime_url(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    let b = if let Some(r) = b.strip_prefix("https://") {
        format!("wss://{}", r)
    } else if let Some(r) = b.strip_prefix("http://") {
        format!("ws://{}", r)
    } else if b.starts_with("wss://") || b.starts_with("ws://") {
        b.to_string()
    } else {
        format!("wss://{}", b)
    };
    format!("{}/realtime", b)
}

/// The Kotak order-update feed.
pub struct KotakOrderFeed {
    url: String,
    token: String,
    sid: String,
    symbols: SymbolResolver,
    use_json: bool,
    attempted: bool,
    acked: bool,
}

impl KotakOrderFeed {
    pub fn new(s: &KotakSession, symbols: SymbolResolver) -> Self {
        Self {
            url: realtime_url(&s.base_url),
            token: s.token.clone(),
            sid: s.sid.clone(),
            symbols,
            use_json: true,
            attempted: false,
            acked: false,
        }
    }

    /// The connect frame: JSON first; the raw `{type:cn,...}` string when a
    /// previous attempt opened but was never acknowledged (web
    /// `on_open_extra`).
    pub fn connect_frame(&mut self) -> Message {
        if self.attempted && !self.acked {
            self.use_json = !self.use_json;
        }
        self.attempted = true;
        self.acked = false;
        if self.use_json {
            Message::Text(
                json!({"type": "cn", "Authorization": self.token, "Sid": self.sid, "src": "WEB"})
                    .to_string(),
            )
        } else {
            Message::Text(format!(
                "{{type:cn,Authorization:{},Sid:{},src:WEB}}",
                self.token, self.sid
            ))
        }
    }

    /// One text frame -> events (web `normalize`).
    pub fn parse_text(&mut self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "order" || ty == "position" {
            self.acked = true;
        }
        if ty != "order" {
            if ty == "cn" || v.get("ak").is_some() {
                let ok = v
                    .get("ak")
                    .and_then(Value::as_str)
                    .is_some_and(|a| a.eq_ignore_ascii_case("ok"));
                self.acked = ok;
                if ok {
                    return vec![FeedEvent::AuthOk];
                }
            }
            return Vec::new();
        }
        let d = v.get("data").cloned().unwrap_or(Value::Null);
        let s = |k: &str| super::mapping::s(&d, k);
        let n = |k: &str| super::mapping::n(&d, k);
        let order_id = s("nOrdNo");
        if order_id.is_empty() {
            return Vec::new();
        }
        let raw_status = s("ordSt").to_ascii_lowercase();
        // The order feed keeps `trigger pending` (web `_STATUS_MAP`); only
        // the REST book collapses it to `open`.
        let status = if raw_status == "trigger pending" {
            raw_status.clone()
        } else {
            super::mapping::map_status(&raw_status)
        };
        let seg = s("exSeg");
        let exchange = super::mapping::row_exchange(&seg);
        let symbol = super::mapping::openalgo_symbol(&self.symbols, &d, &exchange);
        let quantity = n("qty") as i64;
        let filled = n("fldQty") as i64;
        let pending = match d.get("unFldSz") {
            Some(x) if !x.is_null() && !s("unFldSz").is_empty() => n("unFldSz") as i64,
            _ => (quantity - filled).max(0),
        };
        vec![FeedEvent::OrderUpdate(OrderUpdate {
            orderid: order_id,
            symbol,
            exchange,
            action: super::mapping::map_action(&s("trnsTp")),
            quantity,
            price: n("prc"),
            trigger_price: n("trgPrc"),
            pricetype: super::mapping::reverse_order_type(&s("prcTp")),
            product: s("prod"),
            rejection_reason: if status == "rejected" {
                s("rejRsn")
            } else {
                String::new()
            },
            order_status: status,
            filled_quantity: filled,
            pending_quantity: pending,
            average_price: n("avgPrc"),
        })]
    }
}

impl BrokerFeed for KotakOrderFeed {
    fn broker(&self) -> &'static str {
        "kotak"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Kotak order feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![self.connect_frame()]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    /// The web pings every 30 s and drops the socket when a ping goes
    /// unanswered for 10 s (`kotak_order_adapter.py` `ws_ping_interval`,
    /// `websocket_proxy/order_adapter.py`).
    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((WEB_PING_INTERVAL, ping()))
    }

    fn heartbeat_reply_timeout(&self) -> Option<Duration> {
        Some(WEB_PING_TIMEOUT)
    }
}
