//! 5paisa market feed (web `streaming/fivepaisa_websocket.py`,
//! `fivepaisa_adapter.py`, `fivepaisa_mapping.py`).
//!
//! * Host from the access token's `RedirectServer` claim: A
//!   `wss://aopenfeed.5paisa.com/feeds/api/chat`, B `wss://bopenfeed…`,
//!   C `wss://openfeed…`, otherwise `wss://openfeed.5paisa.com/Feeds/api/chat`.
//!   Connect with `?Value1=<access_token>|<client_code>`; no login frame.
//! * Requests are JSON text:
//!   `{"Method":"MarketFeedV3"|"MarketDepthService","Operation":"Subscribe"|
//!   "Unsubscribe","ClientCode":..,"MarketFeedData":[{"Exch","ExchType",
//!   "ScripCode"}]}`, up to 50 scrips per frame. LTP and Quote ride
//!   `MarketFeedV3`; Depth subscribes `MarketDepthService` and also
//!   `MarketFeedV3`, because depth frames carry no traded price.
//! * Frames are a JSON object or an array of objects keyed by `Token`.
//!   Quote frames: `LastRate, OpenRate, High, Low, PClose, TotalQty, LastQty,
//!   AvgRate, TBidQ, TOffQ, BidRate, OffRate, TickDt` (`/Date(ms)/`). Depth
//!   frames: `TBidQ, TOffQ, Details[{BbBuySellFlag 66|83, Price, Quantity,
//!   NumberOfOrders}]`.
//! * A zero `LastRate/OpenRate/High/Low/PClose/BidRate/OffRate/AvgRate`
//!   keeps the last non-zero value seen for the instrument (snapshot merge).
//! * WebSocket ping every 10 s.
//!
//! The order-confirmation stream is not used: 5paisa allows one feed
//! connection per token and drops the other, so the web polls REST instead.

use super::mapping::{ms_date_epoch, num, text};
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use base64::Engine;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const MARKET_FEED: &str = "MarketFeedV3";
pub const MARKET_DEPTH: &str = "MarketDepthService";
/// web: up to 50 scrips per frame.
pub const FRAME_BATCH: usize = 50;
const SNAPSHOT_FIELDS: &[&str] = &[
    "LastRate", "OpenRate", "High", "Low", "PClose", "BidRate", "OffRate", "AvgRate",
];

/// `RedirectServer` claim of the access-token JWT, `default` when absent.
pub fn redirect_server(token: &str) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return "default".into();
    }
    let payload = parts[1].trim_end_matches('=');
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v.get("RedirectServer")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "default".into())
}

/// Feed host for a `RedirectServer` value.
pub fn feed_url(server: &str) -> &'static str {
    match server {
        "A" => "wss://aopenfeed.5paisa.com/feeds/api/chat",
        "B" => "wss://bopenfeed.5paisa.com/feeds/api/chat",
        "C" => "wss://openfeed.5paisa.com/feeds/api/chat",
        _ => "wss://openfeed.5paisa.com/Feeds/api/chat",
    }
}

/// Feed `Exch` / `ExchType` from the broker exchange (web
/// `FivePaisaExchangeMapper`, defaults N / C).
pub fn feed_codes(brexchange: &str) -> (&'static str, &'static str) {
    let e = match brexchange {
        "BSE" | "BFO" | "BSE_INDEX" | "BCD" => "B",
        "MCX" => "M",
        _ => "N",
    };
    let t = match brexchange {
        "NFO" | "BFO" | "MCX" => "D",
        "CDS" | "BCD" => "U",
        _ => "C",
    };
    (e, t)
}

/// Methods a mode needs.
pub fn methods(mode: FeedMode) -> &'static [&'static str] {
    match mode {
        FeedMode::Depth => &[MARKET_DEPTH, MARKET_FEED],
        _ => &[MARKET_FEED],
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
    scrip: Value,
}

pub struct FivepaisaFeed {
    url: String,
    client_code: String,
    subs: HashMap<String, SubInfo>,
    snapshots: HashMap<String, Map<String, Value>>,
}

fn scrip(s: &FeedSubscription) -> Value {
    let (e, t) = feed_codes(&s.brexchange);
    let code = s
        .token
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| json!(s.token));
    json!({"Exch": e, "ExchType": t, "ScripCode": code})
}

impl FivepaisaFeed {
    pub fn new(host: &str, access_token: &str, client_code: &str) -> Self {
        Self {
            url: format!("{}?Value1={}|{}", host, access_token, client_code),
            client_code: client_code.to_string(),
            subs: HashMap::new(),
            snapshots: HashMap::new(),
        }
    }

    pub fn cached(&self) -> usize {
        self.snapshots.len()
    }

    fn frames(&self, op: &str, by_method: BTreeMap<&'static str, Vec<Value>>) -> Vec<Message> {
        let mut out = Vec::new();
        for (method, scrips) in by_method {
            for chunk in scrips.chunks(FRAME_BATCH) {
                out.push(Message::Text(
                    json!({
                        "Method": method,
                        "Operation": op,
                        "ClientCode": self.client_code,
                        "MarketFeedData": chunk,
                    })
                    .to_string(),
                ));
            }
        }
        out
    }

    /// Merge zero values from the snapshot; remember non-zero ones.
    fn merge(&mut self, token: &str, frame: &Map<String, Value>) -> Map<String, Value> {
        let snap = self.snapshots.entry(token.to_string()).or_default();
        let mut merged = frame.clone();
        for f in SNAPSHOT_FIELDS {
            let v = frame.get(*f).map(num_of).unwrap_or(0.0);
            if v == 0.0 {
                if let Some(prev) = snap.get(*f) {
                    merged.insert((*f).to_string(), prev.clone());
                }
            } else {
                snap.insert((*f).to_string(), json!(v));
            }
        }
        merged
    }

    fn item(&mut self, item: &Map<String, Value>) -> Vec<FeedEvent> {
        let obj = Value::Object(item.clone());
        let token = text(&obj, "Token");
        let Some(sub) = self.subs.get(&token).cloned() else {
            return Vec::new();
        };
        let m = Value::Object(self.merge(&token, item));
        let now = now_ms();
        if let Some(details) = m.get("Details").and_then(Value::as_array) {
            if sub.mode != FeedMode::Depth {
                return Vec::new();
            }
            let level = |d: &Value| DepthLevel {
                price: num(d, "Price"),
                quantity: num(d, "Quantity") as i64,
                orders: num(d, "NumberOfOrders") as i64,
            };
            let side = |flag: i64| {
                let mut v: Vec<DepthLevel> = details
                    .iter()
                    .filter(|d| num(d, "BbBuySellFlag") as i64 == flag)
                    .map(level)
                    .take(5)
                    .collect();
                v.resize(5, DepthLevel::default());
                v
            };
            return vec![FeedEvent::Depth(NormalizedDepth {
                symbol: sub.symbol,
                exchange: sub.exchange,
                ltp: num(&m, "LastRate"),
                buy: side(66),
                sell: side(83),
                total_buy_quantity: num(&m, "TBidQ") as i64,
                total_sell_quantity: num(&m, "TOffQ") as i64,
                timestamp_ms: now,
            })];
        }
        if m.get("LastRate").is_none() {
            return Vec::new();
        }
        let mut t = NormalizedTick {
            symbol: sub.symbol,
            exchange: sub.exchange,
            mode: sub.mode.code(),
            ltp: num(&m, "LastRate"),
            last_trade_time_ms: ms_date_epoch(&text(&m, "TickDt")),
            timestamp_ms: now,
            ..Default::default()
        };
        if sub.mode != FeedMode::Ltp {
            t.open = num(&m, "OpenRate");
            t.high = num(&m, "High");
            t.low = num(&m, "Low");
            t.close = num(&m, "PClose");
            t.volume = num(&m, "TotalQty") as i64;
            t.last_quantity = num(&m, "LastQty") as i64;
            t.average_price = num(&m, "AvgRate");
            t.total_buy_quantity = num(&m, "TBidQ") as i64;
            t.total_sell_quantity = num(&m, "TOffQ") as i64;
            t.derive_change();
        }
        vec![FeedEvent::Tick(t)]
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, frame: &str) -> Vec<FeedEvent> {
        match serde_json::from_str::<Value>(frame) {
            Ok(Value::Object(m)) => self.item(&m),
            Ok(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_object)
                .flat_map(|m| self.item(m))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn register(&mut self, s: &FeedSubscription) -> Value {
        let sc = scrip(s);
        self.subs.insert(
            s.token.clone(),
            SubInfo {
                symbol: s.symbol.clone(),
                exchange: s.exchange.clone(),
                mode: s.mode,
                scrip: sc.clone(),
            },
        );
        sc
    }
}

fn num_of(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

impl BrokerFeed for FivepaisaFeed {
    fn broker(&self) -> &'static str {
        "fivepaisa"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("5paisa feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.snapshots.clear();
        Vec::new()
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut by: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for s in subs {
            let sc = self.register(s);
            for m in methods(s.mode) {
                by.entry(m).or_default().push(sc.clone());
            }
        }
        self.frames("Subscribe", by)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut by: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for s in subs {
            let sc = self
                .subs
                .remove(&s.token)
                .map(|i| i.scrip)
                .unwrap_or_else(|| scrip(s));
            self.snapshots.remove(&s.token);
            for m in methods(s.mode) {
                by.entry(m).or_default().push(sc.clone());
            }
        }
        self.frames("Unsubscribe", by)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        let sc = self.register(new);
        let (was, now) = (methods(old.mode), methods(new.mode));
        let mut drop: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        let mut add: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
        for m in was.iter().filter(|m| !now.contains(m)) {
            drop.entry(m).or_default().push(sc.clone());
        }
        for m in now.iter().filter(|m| !was.contains(m)) {
            add.entry(m).or_default().push(sc.clone());
        }
        let mut out = self.frames("Unsubscribe", drop);
        out.extend(self.frames("Subscribe", add));
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(10), Message::Ping(Vec::new())))
    }
}
