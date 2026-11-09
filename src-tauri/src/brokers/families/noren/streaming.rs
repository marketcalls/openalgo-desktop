//! Noren JSON WebSocket (web `streaming/*_websocket.py`, `*_adapter.py`,
//! `*_order_adapter.py`).
//!
//! * connect: `{"t":"a","uid","actid","source":"API","accesstoken"}`,
//!   ack `{"t":"ak","s":"OK"}` (field and type as the live servers want
//!   them, not the doc's `c`/`susertoken`).
//! * touchline `{"t":"t","k":"NSE|22#NSE|2885"}` / `{"t":"u"}`, depth
//!   `{"t":"d"}` / `{"t":"ud"}`, batches of at most 100 scrips.
//! * heartbeat `{"t":"h"}` every 30 s; a session that keeps answering it
//!   while no market data comes is reconnected (`liveness`, FB-03).
//! * frames `tk`/`tf` (touchline snapshot/update), `dk`/`df` (depth),
//!   `om` (order update after `{"t":"o","actid"}`), routed by `e|tk`, or
//!   by the token alone when a frame has no `e` and one subscribed scrip
//!   has that token (FB-08).
//! * updates carry only changed fields: each scrip keeps a snapshot, new
//!   values overlay it, and a zero/blank o/h/l/c/ap never overwrites a
//!   non-zero cached value. What the snapshot holds becomes a tick and a
//!   book as each member's web adapter reads it (`quote`, FB-07).

use super::mapping::{self, noren_exchange};
use super::NorenConfig;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::error::{AppError, Result};
use liveness::Liveness;
use quote::Instrument;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

mod liveness;
mod quote;
#[cfg(test)]
mod tests;

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
    liveness: Liveness,
    /// Epoch milliseconds now (a fake clock in tests).
    clock: fn() -> i64,
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

/// A frame field as text (Noren sends numbers as strings).
fn field(m: &Map<String, Value>, key: &str) -> String {
    match m.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
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
            liveness: Liveness::default(),
            clock: now_ms,
        }
    }

    /// Cached scrips (bounded by the subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: fn() -> i64) {
        self.clock = clock;
    }

    fn frames(t: &str, keys: &[String]) -> Vec<Message> {
        keys.chunks(BATCH)
            .map(|c| Message::Text(json!({"t": t, "k": c.join("#")}).to_string()))
            .collect()
    }

    /// The subscribed scrip a market data frame is for: its `e|tk`, or,
    /// when it carries no exchange, the one subscribed scrip with its token
    /// (web `_resolve_scrip`). A token two subscribed exchanges share is
    /// dropped rather than routed to the wrong instrument (web #1732).
    fn route(&self, m: &Map<String, Value>) -> Option<String> {
        let token = field(m, "tk");
        if token.is_empty() {
            return None;
        }
        let exchange = field(m, "e");
        if !exchange.is_empty() {
            let key = format!("{}|{}", exchange, token);
            return self.subs.contains_key(&key).then_some(key);
        }
        let suffix = format!("|{}", token);
        let mut found = self.subs.keys().filter(|k| k.ends_with(&suffix));
        match (found.next(), found.next()) {
            (Some(key), None) => Some(key.clone()),
            _ => None,
        }
    }

    /// A touchline or depth frame: merged into its scrip's snapshot and
    /// published as a tick, and as a book to a depth subscription.
    fn market(&mut self, t: &str, m: &Map<String, Value>) -> Vec<FeedEvent> {
        let now = (self.clock)();
        self.liveness.note_data(now);
        let Some(key) = self.route(m) else {
            return Vec::new();
        };
        let Some(sub) = self.subs.get(&key).cloned() else {
            return Vec::new();
        };
        let entry = self.cache.entry(key).or_default();
        merge(entry, m);
        let snapshot = entry.clone();
        let depth_packet = t == "dk" || t == "df";
        let at = || Instrument {
            symbol: &sub.symbol,
            exchange: &sub.exchange,
        };
        let mut out = vec![FeedEvent::Tick(quote::tick(
            self.cfg,
            at(),
            &snapshot,
            depth_packet,
            now,
        ))];
        if sub.mode == FeedMode::Depth && depth_packet {
            out.push(FeedEvent::Depth(quote::depth(at(), &snapshot, now)));
        }
        out
    }

    /// Decode one text frame.
    pub fn parse_text(&mut self, text_frame: &str) -> Vec<FeedEvent> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(text_frame) else {
            return Vec::new();
        };
        let t = m.get("t").and_then(Value::as_str).unwrap_or("").to_string();
        match t.as_str() {
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
            "tk" | "tf" | "dk" | "df" => self.market(&t, &m),
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
        self.liveness.reset();
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

    /// Web `_check_connection_health`'s data silence (FB-03): armed by
    /// market data over time, then 180 s without any.
    fn data_stalled(&mut self) -> bool {
        self.liveness.stalled_at((self.clock)())
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(r#"{"t":"h"}"#.to_string())))
    }
}
