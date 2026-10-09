//! Firstock feed (web `streaming/firstock_websocket.py`,
//! `firstock_adapter.py`).
//!
//! * URL `wss://socket.firstock.in/V2/ws?userId=..&jKey=..&source=developer-api`;
//!   no login frame; a refusal arrives as `{"status":"failed"}` or
//!   `{"message":"unauthenticated"}`.
//! * `{"action":"subscribe","tokens":"NSE:26000|NFO:65872"}` (one feed for
//!   every mode; the mode only shapes the output). Every instrument of a
//!   subscribe run goes in one frame, without repeats (web #2176 batching;
//!   the manager hands a burst of subscriptions over as one run).
//! * ticks: V1 flat `{c_symbol, c_exch_seg, i_*}` or V2
//!   `{"EX:TOKEN": {...}}`; prices in paise; `9223372036854775808` means
//!   "no value"; zero prices never overwrite the snapshot.
//! * WebSocket ping every 30 s.

use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::families::noren::mapping::noren_exchange;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const SENTINEL: u64 = 9_223_372_036_854_775_808;
const PRICE_FIELDS: &[&str] = &[
    "i_last_traded_price",
    "i_open_price",
    "i_high_price",
    "i_low_price",
    "i_closing_price",
    "i_average_trade_price",
    "i_upper_circuit_limit",
    "i_lower_circuit_limit",
];

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct FirstockFeed {
    url: String,
    subs: HashMap<String, SubInfo>,
    snapshots: HashMap<String, Map<String, Value>>,
}

/// `EX:TOKEN` key.
pub fn key(s: &FeedSubscription) -> String {
    format!("{}:{}", noren_exchange(&s.exchange), s.token)
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => {
            if n.as_u64() == Some(SENTINEL) {
                None
            } else {
                n.as_f64()
            }
        }
        Value::String(s) => {
            let t = s.trim();
            if t == "9223372036854775808" {
                None
            } else {
                t.parse().ok()
            }
        }
        _ => None,
    }
}

fn get(m: &Map<String, Value>, k: &str) -> f64 {
    m.get(k).and_then(number).unwrap_or(0.0)
}

/// Merge one tick into a snapshot (sentinels skipped, zero prices kept out).
pub fn merge(snapshot: &mut Map<String, Value>, data: &Map<String, Value>) {
    for (k, v) in data {
        if k == "best_buy" || k == "best_sell" {
            if v.as_array().is_some_and(|a| !a.is_empty()) {
                snapshot.insert(k.clone(), v.clone());
            }
            continue;
        }
        match number(v) {
            None if v.is_number() || v.as_str() == Some("9223372036854775808") => continue,
            Some(x) if x == 0.0 && PRICE_FIELDS.contains(&k.as_str()) => continue,
            _ => {
                snapshot.insert(k.clone(), v.clone());
            }
        }
    }
}

impl FirstockFeed {
    pub fn new(base: &str, uid: &str, jkey: &str) -> Self {
        Self {
            url: format!(
                "{}?userId={}&jKey={}&source=developer-api",
                base,
                urlencoding::encode(uid),
                urlencoding::encode(jkey)
            ),
            subs: HashMap::new(),
            snapshots: HashMap::new(),
        }
    }

    pub fn cached(&self) -> usize {
        self.snapshots.len()
    }

    fn levels(m: &Map<String, Value>, side: &str) -> Vec<DepthLevel> {
        let mut out: Vec<DepthLevel> = m
            .get(side)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_object)
                    .map(|l| DepthLevel {
                        price: get(l, "price") / 100.0,
                        quantity: get(l, "quantity") as i64,
                        orders: get(l, "orders") as i64,
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.resize(5, DepthLevel::default());
        out.truncate(5);
        out
    }

    fn tick(&mut self, data: &Map<String, Value>) -> Vec<FeedEvent> {
        let key = format!(
            "{}:{}",
            data.get("c_exch_seg").and_then(Value::as_str).unwrap_or(""),
            match data.get("c_symbol") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            }
        );
        let Some(sub) = self.subs.get(&key).cloned() else {
            return Vec::new();
        };
        let snap = self.snapshots.entry(key).or_default();
        merge(snap, data);
        let s = snap.clone();
        let p = |k: &str| get(&s, k) / 100.0;
        let q = |k: &str| get(&s, k) as i64;
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: p("i_last_traded_price"),
            open: p("i_open_price"),
            high: p("i_high_price"),
            low: p("i_low_price"),
            close: p("i_closing_price"),
            volume: q("i_volume_traded_today"),
            average_price: p("i_average_trade_price"),
            last_quantity: q("i_last_trade_quantity"),
            total_buy_quantity: q("i_total_buy_quantity"),
            total_sell_quantity: q("i_total_sell_quantity"),
            oi: if sub.mode == FeedMode::Depth {
                q("i_total_open_interest")
            } else {
                q("i_open_interest")
            },
            last_trade_time_ms: q("i_last_trade_time") * 1000,
            timestamp_ms: now_ms(),
            ..Default::default()
        };
        t.derive_change();
        let mut out = vec![FeedEvent::Tick(t.clone())];
        if sub.mode == FeedMode::Depth {
            out.push(FeedEvent::Depth(NormalizedDepth {
                symbol: t.symbol,
                exchange: t.exchange,
                ltp: t.ltp,
                buy: Self::levels(&s, "best_buy"),
                sell: Self::levels(&s, "best_sell"),
                total_buy_quantity: t.total_buy_quantity,
                total_sell_quantity: t.total_sell_quantity,
                timestamp_ms: t.timestamp_ms,
            }));
        }
        out
    }

    pub fn parse_text(&mut self, frame: &str) -> Vec<FeedEvent> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(frame) else {
            return Vec::new();
        };
        let status = m.get("status").and_then(Value::as_str).unwrap_or("");
        let message = m.get("message").and_then(Value::as_str).unwrap_or("");
        if status == "failed" || message.eq_ignore_ascii_case("unauthenticated") {
            tracing::warn!(broker = "firstock", "Feed refused: {} {}", status, message);
            return vec![FeedEvent::AuthFailed(
                "Firstock refused the market data connection. Log in to Firstock again.".into(),
            )];
        }
        if m.contains_key("c_symbol") {
            return self.tick(&m);
        }
        let mut out = Vec::new();
        for (k, v) in &m {
            if let (Some((ex, tok)), Some(obj)) = (k.split_once(':'), v.as_object()) {
                let mut tick = obj.clone();
                tick.entry("c_symbol").or_insert_with(|| json!(tok));
                tick.entry("c_exch_seg").or_insert_with(|| json!(ex));
                out.extend(self.tick(&tick));
            }
        }
        if out.is_empty() && status == "success" {
            return vec![FeedEvent::Heartbeat];
        }
        out
    }

    fn frames(action: &str, keys: &[String]) -> Vec<Message> {
        let mut unique: Vec<&str> = Vec::with_capacity(keys.len());
        for k in keys {
            if !unique.contains(&k.as_str()) {
                unique.push(k);
            }
        }
        if unique.is_empty() {
            return Vec::new();
        }
        vec![Message::Text(
            json!({"action": action, "tokens": unique.join("|")}).to_string(),
        )]
    }
}

impl BrokerFeed for FirstockFeed {
    fn broker(&self) -> &'static str {
        "firstock"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Firstock feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.snapshots.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let keys: Vec<String> = subs
            .iter()
            .map(|s| {
                let k = key(s);
                self.subs.insert(
                    k.clone(),
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                        mode: s.mode,
                    },
                );
                k
            })
            .collect();
        Self::frames("subscribe", &keys)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let keys: Vec<String> = subs
            .iter()
            .map(|s| {
                let k = key(s);
                self.subs.remove(&k);
                self.snapshots.remove(&k);
                k
            })
            .collect();
        Self::frames("unsubscribe", &keys)
    }

    fn mode_change_frames(
        &mut self,
        _old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        // One feed carries every mode: only the routing changes.
        self.subs.insert(
            key(new),
            SubInfo {
                symbol: new.symbol.clone(),
                exchange: new.exchange.clone(),
                mode: new.mode,
            },
        );
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(30), Message::Ping(Vec::new())))
    }
}
