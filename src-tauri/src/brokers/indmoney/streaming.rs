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
//! dropped when two segments share it. A field sent as 0 or not sent says
//! nothing new: the web keeps its last value (`get_value`), and so does the
//! bridge's snapshot, for one connection. Each packet is published at its
//! own mode (an `ltp` packet as LTP, a `quote` packet as Quote, with OI,
//! the best bid and ask and the OI change) and stamped with its own
//! millisecond time, as `ltt` and `timestamp` (web `_on_data`). The stream
//! has no last traded quantity and no book totals. Mode 3 (depth) is served
//! from the quote stream's best bid/ask: the broker streams level 1 only, so
//! a Quote <-> Depth change sends nothing, and an LTP <-> Quote change
//! subscribes the new stream before it drops the old one. Heartbeat: a
//! `ping` every 30 s.
//!
//! Orders: `wss://ws-order-updates.indstocks.com/api/v1/ws/trades`, then
//! `{"action":"subscribe","mode":"order_update"}`. Frames carry
//! `data.{order_id, order_status (R/P/S/...), order_type (the side),
//! req_quantity, executed_price, error_message}`.

use super::mapping::{map_status, num_value, ws_segment};
use super::OrderIdMap;
use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, OrderUpdate, QuoteField, WsRequest,
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

impl SubInfo {
    fn of(s: &FeedSubscription) -> Self {
        Self {
            symbol: s.symbol.clone(),
            exchange: s.exchange.clone(),
            token: s.token.clone(),
            mode: s.mode,
        }
    }
}

/// Last non-zero values per instrument, for this connection: the price a
/// packet without one is published at, and the best bid and ask of the
/// one-level book.
type Cache = HashMap<String, f64>;

/// A field of a packet's `data` when the broker sent a value: INDstocks
/// sends 0 for a field with nothing new, which the web reads as "keep the
/// last value" (`get_value`, indmoney_adapter.py:651-665).
fn sent(data: &Value, key: &str) -> Option<f64> {
    let v = num_value(data.get(key));
    (v != 0.0).then_some(v)
}

/// The packet's own millisecond time, which the web sends as both `ltt`
/// and `timestamp` (indmoney_adapter.py:610-614). A packet without one has
/// no trade time and is stamped on receipt.
fn stamp(msg: &Value, t: &mut NormalizedTick) {
    let packet = msg
        .get("timestamp")
        .map(|v| num_value(Some(v)) as i64)
        .filter(|ms| *ms > 0);
    match packet {
        Some(ms) => {
            t.last_trade_time_ms = ms;
            t.timestamp_ms = ms;
        }
        None => {
            t.timestamp_ms = now_ms();
            t.mark_absent(QuoteField::LastTradeTime);
        }
    }
}

/// What a quote packet carries (web `_normalize_market_data`, quote mode):
/// the day's prices, volume and average price, open interest beyond the
/// Quote mode, and the best bid and ask and the OI change under the web's
/// names. The stream has no last traded quantity and no book totals.
fn quote_fields(data: &Value, t: &mut NormalizedTick) {
    use QuoteField as F;
    t.mark_absent(F::LastQuantity);
    t.mark_absent(F::TotalBuyQuantity);
    t.mark_absent(F::TotalSellQuantity);
    for (key, field) in [
        ("open", F::Open),
        ("high", F::High),
        ("low", F::Low),
        ("close", F::Close),
        ("average_price", F::AveragePrice),
        ("volume", F::Volume),
    ] {
        let Some(v) = sent(data, key) else {
            t.mark_absent(field);
            continue;
        };
        match field {
            F::Open => t.open = v,
            F::High => t.high = v,
            F::Low => t.low = v,
            F::Close => t.close = v,
            F::AveragePrice => t.average_price = v,
            _ => t.volume = v as i64,
        }
    }
    if let Some(oi) = sent(data, "oi") {
        t.oi = oi as i64;
        t.mark_carried(F::OpenInterest);
    }
    let quantity = |key: &str| sent(data, key).map(|q| q as i64);
    t.extras.bid_price = sent(data, "bid_price");
    t.extras.bid_quantity = quantity("bid_qty");
    t.extras.ask_price = sent(data, "ask_price");
    t.extras.ask_quantity = quantity("ask_qty");
    t.extras.oi_change = quantity("oi_change");
    // INDstocks sends no change: worked out from this packet's previous
    // close, else by the bridge from the last one.
    t.derive_change();
}

pub struct IndmoneyFeed {
    url: String,
    token: Secret,
    /// `SEGMENT:TOKEN` -> subscription.
    subs: HashMap<String, SubInfo>,
    /// Bare token -> the `SEGMENT:TOKEN` keys subscribed under it, for the
    /// ticks that name only the token (every tick looks it up).
    by_token: HashMap<String, Vec<String>>,
    cache: HashMap<String, Cache>,
}

fn frame(action: &str, mode: &str, instruments: &[String]) -> Message {
    Message::Text(json!({"action": action, "mode": mode, "instruments": instruments}).to_string())
}

impl IndmoneyFeed {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.to_string(),
            token: Secret::new(token),
            subs: HashMap::new(),
            by_token: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    fn add(&mut self, inst: String, sub: SubInfo) {
        let keys = self.by_token.entry(sub.token.clone()).or_default();
        if !keys.contains(&inst) {
            keys.push(inst.clone());
        }
        self.subs.insert(inst, sub);
    }

    fn remove(&mut self, inst: &str) {
        let Some(sub) = self.subs.remove(inst) else {
            return;
        };
        if let Some(keys) = self.by_token.get_mut(&sub.token) {
            keys.retain(|k| k != inst);
            if keys.is_empty() {
                self.by_token.remove(&sub.token);
            }
        }
        self.cache.remove(inst);
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
                self.add(inst.clone(), SubInfo::of(s));
            } else {
                self.remove(&inst);
            }
            let g = groups.entry((ws_mode(s.mode), seg)).or_default();
            if !g.contains(&inst) {
                g.push(inst);
            }
        }
        let mut out = Vec::new();
        for ((mode, _), insts) in groups {
            for chunk in insts.chunks(MAX_PER_FRAME) {
                out.push(frame(action, mode, chunk));
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
        match self.by_token.get(instrument)?.as_slice() {
            [only] => self.subs.get(only).map(|s| (only.clone(), s.clone())),
            [] => None,
            _ => {
                tracing::warn!(
                    "INDmoney tick for token {} matches several segments; dropped",
                    instrument
                );
                None
            }
        }
    }

    fn tick(&mut self, msg: &Value) -> Vec<FeedEvent> {
        let instrument = match msg.get("instrument") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Vec::new(),
        };
        let packet_mode = match msg.get("mode").and_then(Value::as_str) {
            Some("ltp") => FeedMode::Ltp,
            Some("quote") => FeedMode::Quote,
            _ => return Vec::new(),
        };
        let Some((key, sub)) = self.find(&instrument) else {
            return Vec::new();
        };
        let data = msg.get("data").cloned().unwrap_or(Value::Null);
        let cache = self.cache.entry(key).or_default();
        let mut held = |k: &str| match sent(&data, k) {
            Some(v) => {
                cache.insert(k.to_string(), v);
                Some(v)
            }
            None => cache.get(k).copied(),
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: packet_mode.code(),
            ..Default::default()
        };
        // The last price is published again with a packet that has none
        // (the web's held `ltp`); before any price, nothing is.
        match held("ltp") {
            Some(ltp) => t.ltp = ltp,
            None => t.mark_absent(QuoteField::Ltp),
        }
        stamp(msg, &mut t);
        let mut out = Vec::new();
        if packet_mode == FeedMode::Quote {
            quote_fields(&data, &mut t);
            // Kept for every holder, so a Quote -> Depth change has its book.
            let best = [
                held("bid_price"),
                held("bid_qty"),
                held("ask_price"),
                held("ask_qty"),
            ]
            .map(|v| v.unwrap_or(0.0));
            if sub.mode == FeedMode::Depth {
                out.push(FeedEvent::Depth(one_level_book(&sub, &t, best)));
            }
        }
        out.insert(0, FeedEvent::Tick(t));
        out
    }
}

/// The quote's best bid and ask (`[bid, bid qty, ask, ask qty]`, last
/// non-zero values) as level 1 of a five-level book, the other four empty
/// (audit RB-13: the web refuses Depth on INDmoney). The stream has no book
/// totals.
fn one_level_book(sub: &SubInfo, t: &NormalizedTick, best: [f64; 4]) -> NormalizedDepth {
    let mut buy = vec![DepthLevel::default(); 5];
    let mut sell = vec![DepthLevel::default(); 5];
    buy[0] = DepthLevel {
        price: best[0],
        quantity: best[1] as i64,
        orders: 0,
    };
    sell[0] = DepthLevel {
        price: best[2],
        quantity: best[3] as i64,
        orders: 0,
    };
    NormalizedDepth {
        symbol: sub.symbol.clone(),
        exchange: sub.exchange.clone(),
        ltp: t.ltp,
        buy,
        sell,
        timestamp_ms: t.timestamp_ms,
        absent: AbsentFields::of(&[QuoteField::TotalBuyQuantity, QuoteField::TotalSellQuantity]),
        ..Default::default()
    }
}

impl BrokerFeed for IndmoneyFeed {
    fn broker(&self) -> &'static str {
        "indmoney"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url, self.token.expose(), "price")
    }

    /// A new connection starts every instrument afresh, as the bridge's
    /// snapshot does: no price from before an outage is published as
    /// current.
    fn on_connected(&mut self) -> Vec<Message> {
        self.cache.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, "subscribe")
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, "unsubscribe")
    }

    /// Without a gap and keeping the last values: Quote and Depth share the
    /// `quote` stream, so moving between them sends nothing (it sent an
    /// unsubscribe and a subscribe of the same stream, and the prices
    /// stopped until the broker sent the next one); LTP <-> Quote subscribes
    /// the new stream first. The web subscribes each mode on its own and
    /// drops one only when its last holder leaves.
    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let Some(seg) = ws_segment(&new.exchange) else {
            return Vec::new();
        };
        let inst = format!("{}:{}", seg, new.token);
        match self.subs.get_mut(&inst) {
            Some(sub) => sub.mode = new.mode,
            None => self.add(inst.clone(), SubInfo::of(new)),
        }
        let (from, to) = (ws_mode(old.mode), ws_mode(new.mode));
        if from == to {
            return Vec::new();
        }
        let inst = [inst];
        vec![
            frame("subscribe", to, &inst),
            frame("unsubscribe", from, &inst),
        ]
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
