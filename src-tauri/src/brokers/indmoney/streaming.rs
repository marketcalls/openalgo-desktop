//! INDstocks WebSockets (web `streaming/indWebSocket.py`,
//! `indmoney_adapter.py`, `indmoney_mapping.py`,
//! `indmoney_order_adapter.py`).
//!
//! Prices: `wss://ws-prices.indstocks.com/api/v1/ws/prices` with the raw
//! token in `Authorization`; no auth frame. Subscribe / unsubscribe are JSON
//! text: `{"action":"subscribe","mode":"ltp"|"quote","instruments":
//! ["NSE:2885",..]}`, one frame per (mode, segment) because a mixed frame
//! only delivers its first segment, at most 1000 instruments per frame.
//! Ticks are JSON text (sometimes double-encoded):
//! `{"mode":"ltp","instrument":"2885","timestamp":<ms>,"data":{"ltp":1426}}`;
//! `instrument` may be a bare token, resolved against the subscriptions and
//! dropped when two segments share it. Zero fields keep the last non-zero
//! value. Mode 3 (depth) is served from the quote stream's best bid/ask: the
//! broker streams level 1 only. Heartbeat: a `ping` every 30 s.
//!
//! Orders: `wss://ws-order-updates.indstocks.com/api/v1/ws/trades`, then
//! `{"action":"subscribe","mode":"order_update"}`. Frames carry
//! `data.{order_id, order_status (R/P/S/...), order_type (the side),
//! req_quantity, executed_price, error_message}`.

use super::mapping::{map_status, num_value, ws_segment};
use super::OrderIdMap;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, OrderUpdate, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use crate::security::Secret;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

/// Instruments per subscribe frame.
pub const MAX_PER_FRAME: usize = 1000;
pub const HEARTBEAT: Duration = Duration::from_secs(30);

fn request(url: &str, token: &str, what: &str) -> Result<WsRequest> {
    let mut req = url
        .into_client_request()
        .map_err(|_| AppError::Internal(format!("INDmoney {} feed address is invalid", what)))?;
    let v = HeaderValue::from_str(token)
        .map_err(|_| AppError::Auth("Your INDmoney session is not valid. Log in again.".into()))?;
    req.headers_mut().insert("Authorization", v);
    Ok(req)
}

/// Decode a frame to a JSON object, unwrapping JSON-in-a-string up to
/// three times.
pub fn decode(text: &str) -> Option<Value> {
    let mut v = Value::String(text.to_string());
    for _ in 0..3 {
        match v {
            Value::Object(_) => return Some(v),
            Value::String(s) => v = serde_json::from_str(&s).ok()?,
            _ => return None,
        }
    }
    v.is_object().then_some(v)
}

fn ws_mode(mode: FeedMode) -> &'static str {
    match mode {
        FeedMode::Ltp => "ltp",
        FeedMode::Quote | FeedMode::Depth => "quote",
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    token: String,
    mode: FeedMode,
}

/// Last non-zero values per instrument.
type Cache = HashMap<String, f64>;

pub struct IndmoneyFeed {
    url: String,
    token: Secret,
    /// `SEGMENT:TOKEN` -> subscription.
    subs: HashMap<String, SubInfo>,
    cache: HashMap<String, Cache>,
}

impl IndmoneyFeed {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.to_string(),
            token: Secret::new(token),
            subs: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    pub fn subscription_count(&self) -> usize {
        self.subs.len()
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    fn frames(&mut self, subs: &[FeedSubscription], action: &str) -> Vec<Message> {
        // (mode, segment) -> instruments, in a stable order.
        let mut groups: BTreeMap<(&'static str, &'static str), Vec<String>> = BTreeMap::new();
        for s in subs {
            let Some(seg) = ws_segment(&s.exchange) else {
                tracing::warn!("INDmoney streams no data for {}", s.exchange);
                continue;
            };
            let inst = format!("{}:{}", seg, s.token);
            if action == "subscribe" {
                self.subs.insert(
                    inst.clone(),
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                        token: s.token.clone(),
                        mode: s.mode,
                    },
                );
            } else if self.subs.remove(&inst).is_some() {
                self.cache.remove(&inst);
            }
            let g = groups.entry((ws_mode(s.mode), seg)).or_default();
            if !g.contains(&inst) {
                g.push(inst);
            }
        }
        let mut out = Vec::new();
        for ((mode, _), insts) in groups {
            for chunk in insts.chunks(MAX_PER_FRAME) {
                out.push(Message::Text(
                    json!({"action": action, "mode": mode, "instruments": chunk}).to_string(),
                ));
            }
        }
        out
    }

    /// The subscription a tick belongs to: exact `SEGMENT:TOKEN`, else a
    /// bare token claimed by exactly one subscription.
    fn find(&self, instrument: &str) -> Option<(String, SubInfo)> {
        if let Some(s) = self.subs.get(instrument) {
            return Some((instrument.to_string(), s.clone()));
        }
        let mut it = self.subs.iter().filter(|(_, s)| s.token == instrument);
        let first = it.next()?;
        if it.next().is_some() {
            tracing::warn!(
                "INDmoney tick for token {} matches several segments; dropped",
                instrument
            );
            return None;
        }
        Some((first.0.clone(), first.1.clone()))
    }

    fn tick(&mut self, msg: &Value) -> Vec<FeedEvent> {
        let instrument = match msg.get("instrument") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Vec::new(),
        };
        let Some(mode) = msg.get("mode").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some((key, sub)) = self.find(&instrument) else {
            return Vec::new();
        };
        let data = msg.get("data").cloned().unwrap_or(Value::Null);
        let ts = msg
            .get("timestamp")
            .map(|v| num_value(Some(v)) as i64)
            .filter(|t| *t > 0)
            .unwrap_or_else(now_ms);
        let cache = self.cache.entry(key).or_default();
        let mut get = |k: &str| {
            let v = num_value(data.get(k));
            if v != 0.0 {
                cache.insert(k.to_string(), v);
                v
            } else {
                cache.get(k).copied().unwrap_or(0.0)
            }
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: get("ltp"),
            last_trade_time_ms: ts,
            timestamp_ms: now_ms(),
            ..Default::default()
        };
        let (mut bid, mut bid_qty, mut ask, mut ask_qty) = (0.0, 0.0, 0.0, 0.0);
        if mode == "quote" {
            t.open = get("open");
            t.high = get("high");
            t.low = get("low");
            t.close = get("close");
            t.volume = get("volume") as i64;
            t.average_price = get("average_price");
            t.oi = get("oi") as i64;
            bid = get("bid_price");
            bid_qty = get("bid_qty");
            ask = get("ask_price");
            ask_qty = get("ask_qty");
            t.total_buy_quantity = bid_qty as i64;
            t.total_sell_quantity = ask_qty as i64;
        }
        t.derive_change();
        let mut out = Vec::new();
        if sub.mode == FeedMode::Depth && mode == "quote" {
            let mut buy = vec![DepthLevel::default(); 5];
            let mut sell = vec![DepthLevel::default(); 5];
            buy[0] = DepthLevel {
                price: bid,
                quantity: bid_qty as i64,
                orders: 0,
            };
            sell[0] = DepthLevel {
                price: ask,
                quantity: ask_qty as i64,
                orders: 0,
            };
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol.clone(),
                exchange: sub.exchange.clone(),
                ltp: t.ltp,
                buy,
                sell,
                total_buy_quantity: bid_qty as i64,
                total_sell_quantity: ask_qty as i64,
                timestamp_ms: t.timestamp_ms,
            }));
        }
        out.insert(0, FeedEvent::Tick(t));
        out
    }
}

impl BrokerFeed for IndmoneyFeed {
    fn broker(&self) -> &'static str {
        "indmoney"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url, self.token.expose(), "price")
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, "subscribe")
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, "unsubscribe")
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let text = match msg {
            Message::Text(t) => t.as_str(),
            Message::Binary(b) => match std::str::from_utf8(b) {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            },
            Message::Pong(_) => return vec![FeedEvent::Heartbeat],
            _ => return Vec::new(),
        };
        if text.trim() == "pong" {
            return vec![FeedEvent::Heartbeat];
        }
        match decode(text) {
            Some(v) => self.tick(&v),
            None => {
                tracing::debug!("INDmoney feed frame was not JSON");
                Vec::new()
            }
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Ping(b"ping".to_vec())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

// ---------------------------------------------------------------------------
// Order updates
// ---------------------------------------------------------------------------

/// Stream status code -> OpenAlgo status (R/P/S confirmed live by the web;
/// C/X/F/E/J inferred), else the REST vocabulary.
pub fn map_stream_status(raw: &str) -> String {
    let code = raw.trim().to_ascii_uppercase();
    match code.as_str() {
        "R" | "P" => "open".into(),
        "S" => "complete".into(),
        "C" | "X" => "cancelled".into(),
        "F" | "E" | "J" => "rejected".into(),
        _ => map_status(&code.replace('_', " ")),
    }
}

fn field<'a>(d: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| match d.get(*k) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(v) => Some(v),
    })
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

/// An order-update frame as an OpenAlgo update (web `normalize`).
pub fn normalize_order(frame: &Value, ids: &OrderIdMap) -> Option<OrderUpdate> {
    let data = match frame.get("data") {
        Some(d) if d.is_object() => d,
        _ => frame,
    };
    let orderid = field(
        data,
        &[
            "order_id", "orderId", "orderid", "id", "order_no", "orderNo",
        ],
    )?;
    let raw_status = field(data, &["order_status", "orderStatus", "status"])?;
    let status = map_stream_status(&as_text(raw_status));
    let mut quantity = num_value(field(
        data,
        &["req_quantity", "quantity", "qty", "requested_qty"],
    )) as i64;
    let filled = field(
        data,
        &[
            "filled_quantity",
            "filledQuantity",
            "traded_qty",
            "tradedQty",
            "filled_qty",
        ],
    );
    let remaining = field(
        data,
        &[
            "remaining_quantity",
            "remainingQuantity",
            "pending_qty",
            "pendingQty",
            "remaining_qty",
        ],
    );
    let (filled, pending) = if filled.is_none() && remaining.is_none() {
        if status == "complete" {
            (quantity, 0)
        } else {
            (0, quantity)
        }
    } else {
        let f = num_value(filled) as i64;
        let r = num_value(remaining) as i64;
        if quantity == 0 {
            quantity = f + r;
        }
        (f, r)
    };
    let average_price = num_value(field(
        data,
        &[
            "executed_price",
            "average_price",
            "averagePrice",
            "avg_price",
            "avgPrice",
            "traded_price",
        ],
    ));
    let action = field(data, &["order_type", "txn_type", "transaction_type"])
        .map(as_text)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let action = if action == "BUY" || action == "SELL" {
        action
    } else {
        String::new()
    };
    let reason = field(data, &["error_message", "reason"])
        .map(as_text)
        .unwrap_or_default();
    Some(OrderUpdate {
        orderid: ids.canonical(&as_text(orderid)),
        action,
        quantity,
        order_status: status.clone(),
        filled_quantity: filled,
        pending_quantity: pending,
        average_price,
        rejection_reason: if status == "rejected" {
            reason
        } else {
            String::new()
        },
        ..Default::default()
    })
}

pub struct IndmoneyOrderFeed {
    url: String,
    token: Secret,
    ids: Arc<Mutex<OrderIdMap>>,
}

impl IndmoneyOrderFeed {
    pub fn new(url: &str, token: &str, ids: Arc<Mutex<OrderIdMap>>) -> Self {
        Self {
            url: url.to_string(),
            token: Secret::new(token),
            ids,
        }
    }
}

impl BrokerFeed for IndmoneyOrderFeed {
    fn broker(&self) -> &'static str {
        "indmoney"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url, self.token.expose(), "order")
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![Message::Text(
            json!({"action": "subscribe", "mode": "order_update"}).to_string(),
        )]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let text = match msg {
            Message::Text(t) => t.as_str(),
            Message::Binary(b) => match std::str::from_utf8(b) {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            },
            Message::Pong(_) => return vec![FeedEvent::Heartbeat],
            _ => return Vec::new(),
        };
        if text.trim() == "pong" {
            return vec![FeedEvent::Heartbeat];
        }
        let Some(frame) = decode(text) else {
            tracing::warn!("INDmoney order-update frame could not be decoded");
            return Vec::new();
        };
        match normalize_order(&frame, &self.ids.lock()) {
            Some(u) => vec![FeedEvent::OrderUpdate(u)],
            None => {
                tracing::debug!("INDmoney order-update frame carried no order id or status");
                Vec::new()
            }
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Ping(b"ping".to_vec())))
    }
}
