//! Definedge NorenWSTRTP feeds (web `streaming/definedge_websocket.py`,
//! `definedge_adapter.py`, `definedge_order_adapter.py`).
//!
//! * URL `wss://trade.definedgesecurities.com/NorenWSTRTP/`
//!   (`definedge_websocket.py:63`).
//! * connect `{"t":"c","uid","actid","source":"TRTP","susertoken"}`
//!   (`definedge_websocket.py:364-375`), ack `{"t":"ck","s":"Ok"}`
//!   (`:383-391`).
//! * touchline `{"t":"t","k":"NSE|22#BSE|508123"}`, depth `{"t":"d"}`,
//!   unsubscribe `{"t":"u"}` / `{"t":"ud"}` (`:530-625`).
//! * frames `tk`/`tf` (touchline snapshot/update), `dk`/`df` (depth),
//!   `uk`/`udk` acks (`:317-340`); keys `e, tk, lp, ft, v, o, h, l, c, pc,
//!   ap, oi, poi, toi, tbq, tsq, ltq, bp1..5/bq1..5/bo1..5,
//!   sp1..5/sq1..5/so1..5` (`definedge_adapter.py:1039-1063`). Updates
//!   carry only changed fields, so each scrip keeps a snapshot (bounded by
//!   the subscriptions, dropped on unsubscribe).
//! * heartbeat `{"t":"h"}` every 30 s (`definedge_websocket.py:484-528`).
//! * order updates: same URL and connect, then `{"t":"o","actid"}`; frames
//!   `t == "om"` (`definedge_order_adapter.py:9-15,74-137`).

use super::mapping;
use super::DefinedgeSession;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::families::noren::streaming::merge;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Scrips per subscribe frame.
pub const BATCH: usize = 100;
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// `(uid, susertoken)` for the socket: the uid from the session's user id,
/// the token from the feed token or the stored composite.
pub fn feed_identity(auth: &AuthToken) -> Result<(String, String)> {
    let token = match auth.feed().filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => DefinedgeSession::parse(auth)?.susertoken,
    };
    let uid = auth.user_id().unwrap_or_default().trim().to_string();
    if uid.is_empty() || token.is_empty() {
        return Err(AppError::Auth(
            "Live market data needs a fresh Definedge login. Log in to Definedge again.".into(),
        ));
    }
    Ok((uid, token))
}

fn request(url: &str) -> Result<WsRequest> {
    url.into_client_request()
        .map_err(|_| AppError::Internal("Market data feed address is invalid".into()))
}

fn connect_frame(uid: &str, token: &str) -> Message {
    Message::Text(
        json!({
            "t": "c",
            "uid": uid,
            "actid": uid,
            "source": "TRTP",
            "susertoken": token,
        })
        .to_string(),
    )
}

/// `ck` -> AuthOk / AuthFailed.
fn connect_ack(m: &Map<String, Value>) -> FeedEvent {
    let s = m.get("s").and_then(Value::as_str).unwrap_or("");
    if s.eq_ignore_ascii_case("ok") {
        FeedEvent::AuthOk
    } else {
        tracing::warn!(broker = "definedge", "Feed login refused");
        FeedEvent::AuthFailed(
            "Definedge refused the live data connection. Log in to Definedge again.".into(),
        )
    }
}

fn f(v: &Value, k: &str) -> f64 {
    super::num(v, k)
}

fn i(v: &Value, k: &str) -> i64 {
    super::int(v, k)
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
    mode: FeedMode,
}

/// Market-data feed.
pub struct DefinedgeFeed {
    url: String,
    uid: String,
    token: crate::security::Secret,
    subs: HashMap<String, SubInfo>,
    cache: HashMap<String, Map<String, Value>>,
}

/// `EXCH|token` scrip key (index exchanges on their cash exchange).
pub fn scrip(s: &FeedSubscription) -> String {
    let exch = if s.brexchange.is_empty() {
        super::data::api_exchange(&s.exchange).to_string()
    } else {
        s.brexchange.clone()
    };
    format!("{}|{}", exch, s.token)
}

impl DefinedgeFeed {
    pub fn new(url: &str, uid: &str, token: &str) -> Self {
        Self {
            url: url.to_string(),
            uid: uid.to_string(),
            token: crate::security::Secret::new(token),
            subs: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    /// Cached scrip snapshots (bounded by the subscriptions).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn frames(t: &str, keys: &[String]) -> Vec<Message> {
        keys.chunks(BATCH)
            .map(|c| Message::Text(json!({"t": t, "k": c.join("#")}).to_string()))
            .collect()
    }

    fn tick(sub: &SubInfo, d: &Map<String, Value>) -> NormalizedTick {
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
            oi: i(&v, "oi"),
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

    fn depth(sub: &SubInfo, d: &Map<String, Value>) -> NormalizedDepth {
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
    pub fn parse_text(&mut self, frame: &str) -> Vec<FeedEvent> {
        let Ok(Value::Object(m)) = serde_json::from_str::<Value>(frame) else {
            return Vec::new();
        };
        let t = m.get("t").and_then(Value::as_str).unwrap_or("").to_string();
        match t.as_str() {
            "ck" => vec![connect_ack(&m)],
            "h" | "hk" | "uk" | "udk" => vec![FeedEvent::Heartbeat],
            "tk" | "tf" | "dk" | "df" => {
                let key = format!(
                    "{}|{}",
                    m.get("e").and_then(Value::as_str).unwrap_or(""),
                    super::text(&Value::Object(m.clone()), "tk")
                );
                let Some(sub) = self.subs.get(&key).cloned() else {
                    return Vec::new();
                };
                let entry = self.cache.entry(key).or_default();
                merge(entry, &m);
                let snapshot = entry.clone();
                let mut out = vec![FeedEvent::Tick(Self::tick(&sub, &snapshot))];
                if sub.mode == FeedMode::Depth && (t == "dk" || t == "df") {
                    out.push(FeedEvent::Depth(Self::depth(&sub, &snapshot)));
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for DefinedgeFeed {
    fn broker(&self) -> &'static str {
        "definedge"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        self.cache.clear();
        vec![connect_frame(&self.uid, self.token.expose())]
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
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
        let mut out = Self::frames("t", &touch);
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

/// Order-update feed: the connect frame and the order subscription
/// `{"t":"o","actid"}` go out together (Noren processes frames in order;
/// the shared manager sends no subscribe frames on a feed without
/// instruments), so no reply channel is needed.
pub struct DefinedgeOrderFeed {
    url: String,
    uid: String,
    token: crate::security::Secret,
    symbols: SymbolResolver,
}

impl DefinedgeOrderFeed {
    pub fn new(url: &str, uid: &str, token: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            uid: uid.to_string(),
            token: crate::security::Secret::new(token),
            symbols,
        }
    }

    pub fn parse_text(&self, frame: &str) -> Vec<FeedEvent> {
        let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(frame) else {
            return Vec::new();
        };
        match v.get("t").and_then(Value::as_str).unwrap_or("") {
            "ck" => match &v {
                Value::Object(m) => vec![connect_ack(m)],
                _ => Vec::new(),
            },
            "om" => vec![FeedEvent::OrderUpdate(mapping::order_update(
                &v,
                &self.symbols,
            ))],
            "h" | "hk" | "ok" | "uok" => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }
}

impl BrokerFeed for DefinedgeOrderFeed {
    fn broker(&self) -> &'static str {
        "definedge"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        request(&self.url)
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![
            connect_frame(&self.uid, self.token.expose()),
            Message::Text(json!({"t": "o", "actid": self.uid}).to_string()),
        ]
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

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text(r#"{"t":"h"}"#.to_string())))
    }
}
