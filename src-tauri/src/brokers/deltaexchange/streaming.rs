//! Delta public market-data socket (web `streaming/delta_websocket.py`,
//! `delta_adapter.py`, `delta_mapping.py`).
//!
//! * `wss://public-socket.india.delta.exchange`, no authentication (the
//!   private socket's `key-auth` frame is refused there).
//! * Frames are JSON: `{"type":"subscribe"|"unsubscribe","payload":
//!   {"channels":[{"name":"ticker","symbols":[..]}]}}`.
//! * Modes: LTP and QUOTE subscribe `ticker`; DEPTH subscribes `ob_l2`
//!   and `ticker` (the book carries no prices or OI). `ticker` takes any
//!   number of symbols per frame; `ob_l2` exactly one.
//! * `ticker`: `{"type":"ticker","sy","sp","d":[{"s","m","ohlc":[o,h,l,c],
//!   "oi":[oi,chg],"q":[ask,ask_size,bid,bid_size,mid]}]}`; LTP is the mark
//!   price `m` (else the frame's spot `sp`). A field Delta omits keeps its
//!   last value; a null inside a present array publishes 0.
//! * `ob_l2`: `{"type":"ob_l2","sy","a":[[price,size],..],"b":[..]}`, best
//!   first, top five used.
//! * Fields from both channels are merged per symbol, so a depth
//!   subscriber still carries LTP and OI. The cache holds one entry per
//!   subscribed symbol and is cleared on unsubscribe.

use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub const CHANNEL_TICKER: &str = "ticker";
pub const CHANNEL_OB_L2: &str = "ob_l2";
/// web `HEARTBEAT_INTERVAL` (protocol pings).
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct Sub {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct DeltaFeed {
    url: String,
    /// Broker symbol -> subscription.
    subs: HashMap<String, Sub>,
    /// Broker symbol -> last merged values from both channels.
    cache: HashMap<String, NormalizedTick>,
}

impl DeltaFeed {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            subs: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Number of instruments with cached values (bounded by subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn frame(kind: &str, channel: &str, symbols: &[String]) -> Message {
        Message::Text(
            json!({
                "type": kind,
                "payload": {"channels": [{"name": channel, "symbols": symbols}]}
            })
            .to_string(),
        )
    }

    /// One `ticker` frame for all symbols, one `ob_l2` frame per depth
    /// symbol (Delta refuses more than one there).
    fn frames(kind: &str, ticker: &[String], depth: &[String]) -> Vec<Message> {
        let mut out = Vec::new();
        if !ticker.is_empty() {
            out.push(Self::frame(kind, CHANNEL_TICKER, ticker));
        }
        for s in depth {
            out.push(Self::frame(kind, CHANNEL_OB_L2, std::slice::from_ref(s)));
        }
        out
    }

    fn ticker(&mut self, v: &Value) -> Vec<FeedEvent> {
        let spot = v.get("sp");
        let Some(entries) = v.get("d").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let now = now_ms();
        for e in entries {
            let br = e
                .get("s")
                .and_then(Value::as_str)
                .or_else(|| v.get("sy").and_then(Value::as_str))
                .unwrap_or("");
            let Some(sub) = self.subs.get(br) else {
                continue;
            };
            let t = self
                .cache
                .entry(br.to_string())
                .or_insert_with(|| NormalizedTick {
                    symbol: sub.symbol.clone(),
                    exchange: sub.exchange.clone(),
                    ..Default::default()
                });
            if let Some(ltp) = e.get("m").filter(|m| !m.is_null()).or(spot).and_then(num) {
                t.ltp = ltp;
            }
            if let Some(ohlc) = e.get("ohlc").and_then(Value::as_array) {
                t.open = at(ohlc, 0);
                t.high = at(ohlc, 1);
                t.low = at(ohlc, 2);
                t.close = at(ohlc, 3);
            }
            if let Some(oi) = e.get("oi").and_then(Value::as_array) {
                t.oi = at(oi, 0) as i64;
            }
            t.mode = sub.mode.code();
            t.timestamp_ms = now;
            out.push(FeedEvent::Tick(t.clone()));
        }
        out
    }

    fn book(&mut self, v: &Value) -> Vec<FeedEvent> {
        let br = v.get("sy").and_then(Value::as_str).unwrap_or("");
        let Some(sub) = self.subs.get(br) else {
            return Vec::new();
        };
        let side = |k: &str| -> Vec<DepthLevel> {
            let mut l: Vec<DepthLevel> = v
                .get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_array)
                        .filter(|lvl| lvl.len() >= 2)
                        .take(5)
                        .map(|lvl| DepthLevel {
                            price: at(lvl, 0),
                            quantity: at(lvl, 1) as i64,
                            orders: 0,
                        })
                        .collect()
                })
                .unwrap_or_default();
            l.resize(5, DepthLevel::default());
            l
        };
        let buy = side("b");
        let sell = side("a");
        let ltp = self.cache.get(br).map(|t| t.ltp).unwrap_or(0.0);
        vec![FeedEvent::Depth(NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp,
            total_buy_quantity: buy.iter().map(|l| l.quantity).sum(),
            total_sell_quantity: sell.iter().map(|l| l.quantity).sum(),
            buy,
            sell,
            timestamp_ms: now_ms(),
        })]
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, text: &str) -> Vec<FeedEvent> {
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return Vec::new();
        };
        match v.get("type").and_then(Value::as_str) {
            Some(CHANNEL_TICKER) => self.ticker(&v),
            Some(CHANNEL_OB_L2) => self.book(&v),
            Some("heartbeat") | Some("subscriptions") => vec![FeedEvent::Heartbeat],
            Some("error") => {
                let detail = v
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| v.get("error").and_then(Value::as_str))
                    .unwrap_or("");
                tracing::warn!("Delta Exchange feed error: {}", detail);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Element `i` of a present array; missing or null publishes 0.
fn at(a: &[Value], i: usize) -> f64 {
    a.get(i).and_then(num).unwrap_or(0.0)
}

impl BrokerFeed for DeltaFeed {
    fn broker(&self) -> &'static str {
        "deltaexchange"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Delta Exchange feed address is invalid".into()))
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut ticker = Vec::new();
        let mut depth = Vec::new();
        for s in subs {
            let br = s.brsymbol.clone();
            self.subs.insert(
                br.clone(),
                Sub {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            if s.mode == FeedMode::Depth {
                depth.push(br.clone());
            }
            ticker.push(br);
        }
        Self::frames("subscribe", &ticker, &depth)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut ticker = Vec::new();
        let mut depth = Vec::new();
        for s in subs {
            self.subs.remove(&s.brsymbol);
            self.cache.remove(&s.brsymbol);
            if s.mode == FeedMode::Depth {
                depth.push(s.brsymbol.clone());
            }
            ticker.push(s.brsymbol.clone());
        }
        Self::frames("unsubscribe", &ticker, &depth)
    }

    fn mode_change_frames(
        &mut self,
        old: &FeedSubscription,
        new: &FeedSubscription,
    ) -> Vec<Message> {
        if let Some(s) = self.subs.get_mut(&new.brsymbol) {
            s.mode = new.mode;
        }
        let br = std::slice::from_ref(&new.brsymbol);
        // The ticker stays subscribed in every mode; only the book moves.
        match (old.mode == FeedMode::Depth, new.mode == FeedMode::Depth) {
            (false, true) => vec![Self::frame("subscribe", CHANNEL_OB_L2, br)],
            (true, false) => vec![Self::frame("unsubscribe", CHANNEL_OB_L2, br)],
            _ => Vec::new(),
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((PING_INTERVAL, Message::Ping(Vec::new())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
