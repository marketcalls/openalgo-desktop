//! Noren JSON WebSocket (web `streaming/*_websocket.py`, `*_adapter.py`,
//! `*_order_adapter.py`).
//!
//! * connect: `{"t":"a","uid","actid","source":"API","accesstoken"}`,
//!   ack `{"t":"ak","s":"OK"}` (field and type as the live servers want
//!   them, not the doc's `c`/`susertoken`).
//! * touchline `{"t":"t","k":"NSE|22#NSE|2885"}` / `{"t":"u"}`, depth
//!   `{"t":"d"}` / `{"t":"ud"}`, batches of at most 100 scrips.
//! * heartbeat `{"t":"h"}` every 30 s.
//! * frames `tk`/`tf` (touchline snapshot/update), `dk`/`df` (depth),
//!   `om` (order update after `{"t":"o","actid"}`).
//! * updates carry only changed fields: each scrip keeps a snapshot, new
//!   values overlay it, and a zero/blank o/h/l/c/ap never overwrites a
//!   non-zero cached value.

use super::mapping::{self, f, i, noren_exchange, text};
use super::NorenConfig;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::DepthLevel;
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Scrips per subscribe frame.
pub const BATCH: usize = 100;
pub const HEARTBEAT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

pub struct NorenFeed {
    cfg: &'static NorenConfig,
    url: String,
    uid: String,
    token: String,
    symbols: SymbolResolver,
    subs: HashMap<String, SubInfo>,
    cache: HashMap<String, Map<String, Value>>,
}

/// `EXCH|token` scrip key.
pub fn scrip(s: &FeedSubscription) -> String {
    format!("{}|{}", noren_exchange(&s.exchange), s.token)
}

fn is_zero(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty() || s.parse::<f64>().map(|x| x == 0.0).unwrap_or(false),
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// Overlay `new` on `cached`, keeping non-zero o/h/l/c/ap.
pub fn merge(cached: &mut Map<String, Value>, new: &Map<String, Value>) {
    for (k, v) in new {
        if matches!(k.as_str(), "o" | "h" | "l" | "c" | "ap")
            && is_zero(v)
            && cached.get(k).is_some_and(|c| !is_zero(c))
        {
            continue;
        }
        cached.insert(k.clone(), v.clone());
    }
}

impl NorenFeed {
    pub fn new(
        cfg: &'static NorenConfig,
        url: &str,
        uid: &str,
        token: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            cfg,
            url: url.to_string(),
            uid: uid.to_string(),
            token: token.to_string(),
            symbols,
            subs: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Cached scrips (bounded by the subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn frames(t: &str, keys: &[String]) -> Vec<Message> {
        keys.chunks(BATCH)
            .map(|c| Message::Text(json!({"t": t, "k": c.join("#")}).to_string()))
            .collect()
    }

    fn tick(&self, sub: &SubInfo, d: &Map<String, Value>) -> NormalizedTick {
        let v = Value::Object(d.clone());
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: sub.mode.code(),
            ltp: f(&v, "lp"),
            open: f(&v, "o"),
            high: f(&v, "h"),
            low: f(&v, "l"),
            close: f(&v, "c"),
            volume: i(&v, "v"),
            average_price: f(&v, "ap"),
            last_quantity: i(&v, "ltq"),
            total_buy_quantity: i(&v, "tbq"),
            total_sell_quantity: i(&v, "tsq"),
            oi: if d.contains_key("toi") {
                i(&v, "toi")
            } else {
                i(&v, "oi")
            },
            change: 0.0,
            change_percent: f(&v, "pc"),
            last_trade_time_ms: i(&v, "ft") * 1000,
            timestamp_ms: now_ms(),
        };
        let pc = t.change_percent;
        t.derive_change();
        if t.change == 0.0 && pc != 0.0 {
            t.change_percent = pc;
        }
        t
    }

    fn depth(&self, sub: &SubInfo, d: &Map<String, Value>) -> NormalizedDepth {
        let v = Value::Object(d.clone());
        let lvl = |p: &str, q: &str, o: &str, n: usize| DepthLevel {
            price: f(&v, &format!("{}{}", p, n)),
            quantity: i(&v, &format!("{}{}", q, n)),
            orders: i(&v, &format!("{}{}", o, n)),
        };
        NormalizedDepth {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            ltp: f(&v, "lp"),
            buy: (1..=5).map(|n| lvl("bp", "bq", "bo", n)).collect(),
            sell: (1..=5).map(|n| lvl("sp", "sq", "so", n)).collect(),
            total_buy_quantity: i(&v, "tbq"),
            total_sell_quantity: i(&v, "tsq"),
            timestamp_ms: now_ms(),
        }
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, text_frame: &str) -> Vec<FeedEvent> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(text_frame) else {
            return Vec::new();
        };
        let t = m.get("t").and_then(Value::as_str).unwrap_or("");
        match t {
            "ak" => {
                let s = m.get("s").and_then(Value::as_str).unwrap_or("");
                if s.eq_ignore_ascii_case("OK") {
                    vec![FeedEvent::AuthOk]
                } else {
                    let why = m
                        .get("emsg")
                        .and_then(Value::as_str)
                        .unwrap_or("the session was refused");
                    tracing::warn!(broker = self.cfg.id, "Feed login refused: {}", why);
                    vec![FeedEvent::AuthFailed(format!(
                        "{} refused the market data connection. Log in to {} again.",
                        self.cfg.name, self.cfg.name
                    ))]
                }
            }
            "h" | "hk" => vec![FeedEvent::Heartbeat],
            "om" => vec![FeedEvent::OrderUpdate(mapping::order_update(
                &Value::Object(m),
                &self.symbols,
            ))],
            "tk" | "tf" | "dk" | "df" => {
                let key = format!(
                    "{}|{}",
                    m.get("e").and_then(Value::as_str).unwrap_or(""),
                    text(&Value::Object(m.clone()), "tk")
                );
                let Some(sub) = self.subs.get(&key).cloned() else {
                    return Vec::new();
                };
                let entry = self.cache.entry(key).or_default();
                merge(entry, &m);
                let snapshot = entry.clone();
                let mut out = vec![FeedEvent::Tick(self.tick(&sub, &snapshot))];
                if sub.mode == FeedMode::Depth && (t == "dk" || t == "df") {
                    out.push(FeedEvent::Depth(self.depth(&sub, &snapshot)));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for NorenFeed {
    fn broker(&self) -> &'static str {
        self.cfg.id
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Market data feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.cache.clear();
        vec![Message::Text(
            json!({
                "t": "a",
                "uid": self.uid,
                "actid": self.uid,
                "source": "API",
                "accesstoken": self.token,
            })
            .to_string(),
        )]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    /// Order updates ride the market socket (one session per login on
    /// single-session brokers): subscribed once per connection, after the
    /// login ack, with or without market subscriptions.
    fn on_authenticated(&mut self) -> Vec<Message> {
        if !self.cfg.order_feed_subscribe {
            return Vec::new();
        }
        vec![Message::Text(
            json!({"t": "o", "actid": self.uid}).to_string(),
        )]
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut out = Vec::new();
        let (mut touch, mut depth) = (Vec::new(), Vec::new());
        for s in subs {
            let k = scrip(s);
            self.subs.insert(
                k.clone(),
                SubInfo {
                    symbol: s.symbol.clone(),
                    exchange: s.exchange.clone(),
                    mode: s.mode,
                },
            );
            if s.mode == FeedMode::Depth {
                depth.push(k);
            } else {
                touch.push(k);
            }
        }
        out.extend(Self::frames("t", &touch));
        out.extend(Self::frames("d", &depth));
        out
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let (mut touch, mut depth) = (Vec::new(), Vec::new());
        for s in subs {
            let k = scrip(s);
            self.subs.remove(&k);
            self.cache.remove(&k);
            if s.mode == FeedMode::Depth {
                depth.push(k);
            } else {
                touch.push(k);
            }
        }
        let mut out = Self::frames("u", &touch);
        out.extend(Self::frames("ud", &depth));
        out
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(r#"{"t":"h"}"#.to_string())))
    }
}
