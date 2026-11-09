//! Firstock feed (web `streaming/firstock_websocket.py`,
//! `firstock_adapter.py`).
//!
//! * URL `wss://socket.firstock.in/V2/ws?userId=..&jKey=..&source=developer-api`;
//!   no login frame: Firstock answers the address's credentials with
//!   `{"status":"success"}`, and subscriptions wait for it, as the web's
//!   client queues them until then (`firstock_websocket.py:498`, audit
//!   FB-01); a refusal arrives as `{"status":"failed"}` or
//!   `{"message":"unauthenticated"}`.
//! * `{"action":"subscribe","tokens":"NSE:26000|NFO:65872"}` (one feed for
//!   every mode; the mode only shapes the output). Every instrument of a
//!   subscribe run goes in one frame, without repeats (web #2176 batching;
//!   the manager hands a burst of subscriptions over as one run).
//! * ticks: V1 flat `{c_symbol, c_exch_seg, i_*}` or V2
//!   `{"EX:TOKEN": {...}}`; prices in paise; `9223372036854775808` means
//!   "no value"; zero prices never overwrite the snapshot, and a depth side
//!   keeps only its valid levels, or the previous side when none is valid
//!   (web `_filter_depth_data`, audit FB-02).
//! * a tick carries only what Firstock sent for the instrument this
//!   session (C03), its circuit limits, and OI in Quote too (C05, C06).
//! * WebSocket ping every 30 s.

use crate::brokers::common::streaming::{
    now_ms, AbsentFields, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message,
    NormalizedDepth, NormalizedTick, QuoteField, WsRequest,
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

/// A book level with a value: none of its price, quantity and orders is the
/// "no value" sentinel, and it has a price or a quantity (web
/// `firstock_adapter.py::_filter_depth_data`, which reads a missing field
/// as 0).
fn valid_level(level: &Value) -> bool {
    let Some(m) = level.as_object() else {
        return false;
    };
    let field = |k: &str| m.get(k).map_or(Some(0.0), number);
    match (field("price"), field("quantity"), field("orders")) {
        (Some(price), Some(quantity), Some(_)) => price > 0.0 || quantity > 0.0,
        _ => false,
    }
}

/// Merge one tick into a snapshot (sentinels skipped, zero prices kept out,
/// a depth side replaced only by the valid levels it brings). A tick is a
/// partial update: Firstock says that past the exchange and token "other
/// fields may / may not be present", so a field it leaves out, the trade
/// time included, has not changed and stays, as on the web. The snapshot
/// lasts one connection. See "Partial-update feeds" in
/// docs/port/market-data-frames.md.
pub fn merge(snapshot: &mut Map<String, Value>, data: &Map<String, Value>) {
    for (k, v) in data {
        if k == "best_buy" || k == "best_sell" {
            let valid: Vec<Value> = v
                .as_array()
                .map(|a| a.iter().filter(|l| valid_level(l)).cloned().collect())
                .unwrap_or_default();
            if !valid.is_empty() {
                snapshot.insert(k.clone(), Value::Array(valid));
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

    /// The valid levels Firstock sent for a side, at most five. The web pads
    /// a side to five with zero levels (`_extract_depth_data`); that padding
    /// is not Firstock's and is not sent (lead rule 2026-11-09: levels are
    /// what the broker's packet carried).
    fn levels(m: &Map<String, Value>, side: &str) -> Vec<DepthLevel> {
        m.get(side)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_object)
                    .take(5)
                    .map(|l| DepthLevel {
                        price: get(l, "price") / 100.0,
                        quantity: get(l, "quantity") as i64,
                        orders: get(l, "orders") as i64,
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        let t = Self::quote(&sub, &s);
        let mut out = vec![FeedEvent::Tick(t.clone())];
        if sub.mode == FeedMode::Depth {
            out.push(FeedEvent::Depth(Self::book(&t, &s)));
        }
        out
    }

    /// The tick of an instrument's snapshot. Every Firstock tick carries
    /// the whole quote, so its mode is Quote, or Depth for a Depth
    /// subscription, whose OI is the total OI (web
    /// `_normalize_market_data`: `i_open_interest` in Quote,
    /// `i_total_open_interest` in Depth); a field the snapshot has never
    /// held a value for is absent, never 0 (audit C03).
    fn quote(sub: &SubInfo, s: &Map<String, Value>) -> NormalizedTick {
        use QuoteField as F;
        let p = |k: &str| get(s, k) / 100.0;
        let q = |k: &str| get(s, k) as i64;
        let depth = sub.mode == FeedMode::Depth;
        let oi_key = if depth {
            "i_total_open_interest"
        } else {
            "i_open_interest"
        };
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: if depth { 3 } else { 2 },
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
            oi: q(oi_key),
            last_trade_time_ms: q("i_last_trade_time") * 1000,
            timestamp_ms: now_ms(),
            ..Default::default()
        };
        for (key, field) in [
            ("i_last_traded_price", F::Ltp),
            ("i_open_price", F::Open),
            ("i_high_price", F::High),
            ("i_low_price", F::Low),
            ("i_closing_price", F::Close),
            ("i_volume_traded_today", F::Volume),
            ("i_average_trade_price", F::AveragePrice),
            ("i_last_trade_quantity", F::LastQuantity),
            ("i_total_buy_quantity", F::TotalBuyQuantity),
            ("i_total_sell_quantity", F::TotalSellQuantity),
        ] {
            if !s.contains_key(key) {
                t.mark_absent(field);
            }
        }
        // No trade time yet on this connection (the snapshot lasts one):
        // `ltt` is left out rather than a receipt time passed off as one. A
        // 0 is stored like any other value, so the time is tested, not the
        // key.
        if t.last_trade_time_ms <= 0 {
            t.mark_absent(F::LastTradeTime);
        }
        match (s.contains_key(oi_key), depth) {
            (true, false) => t.mark_carried(F::OpenInterest),
            (false, true) => t.mark_absent(F::OpenInterest),
            _ => {}
        }
        let price = |k: &str| s.contains_key(k).then(|| p(k));
        t.extras.upper_circuit = price("i_upper_circuit_limit");
        t.extras.lower_circuit = price("i_lower_circuit_limit");
        t.derive_change();
        t
    }

    /// The book of a Depth subscription, with the tick's totals.
    fn book(t: &NormalizedTick, s: &Map<String, Value>) -> NormalizedDepth {
        use QuoteField as F;
        let mut absent = AbsentFields::default();
        for (key, field) in [
            ("i_last_traded_price", F::Ltp),
            ("i_total_buy_quantity", F::TotalBuyQuantity),
            ("i_total_sell_quantity", F::TotalSellQuantity),
        ] {
            if !s.contains_key(key) {
                absent = absent.with(field);
            }
        }
        NormalizedDepth {
            symbol: t.symbol.clone(),
            exchange: t.exchange.clone(),
            ltp: t.ltp,
            buy: Self::levels(s, "best_buy"),
            sell: Self::levels(s, "best_sell"),
            total_buy_quantity: t.total_buy_quantity,
            total_sell_quantity: t.total_sell_quantity,
            timestamp_ms: t.timestamp_ms,
            absent,
            ..Default::default()
        }
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
            return vec![FeedEvent::AuthOk];
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

    /// Firstock ignores a subscribe sent before it has accepted the
    /// address's credentials (audit FB-01).
    fn awaits_auth_ack(&self) -> bool {
        true
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
