//! SmartStream market-data feed and the order-status socket (web
//! `streaming/smartWebSocketV2.py`, `angel_adapter.py`, `angel_mapping.py`,
//! `angel_order_adapter.py`).
//!
//! Market data:
//! * URL `wss://smartapisocket.angelone.in/smart-stream`, headers
//!   `Authorization: <jwt>` (raw, no `Bearer`), `x-api-key`,
//!   `x-client-code`, `x-feed-token` (`smartWebSocketV2.py:383-388`).
//! * Subscribe / unsubscribe are JSON, one frame per mode:
//!   `{"correlationID","action":1|0,"params":{"mode","tokenList":[{"exchangeType","tokens"}]}}`
//!   with `exchangeType` from the master row's `brexchange`
//!   (`angel_mapping.py:8-18`); OpenAlgo modes 1/2/3 are Angel LTP / QUOTE /
//!   SNAP_QUOTE (Angel mode 4, 20-level depth, is never requested).
//! * Text `ping` every 10 s, the server answers `pong`
//!   (`smartWebSocketV2.py:22-23`).
//! * Binary ticks are little-endian (`_parse_binary_data`,
//!   `smartWebSocketV2.py:560-636`):
//!   `[0]` mode u8, `[1]` exchange type u8, `[2..27]` token (NUL-terminated),
//!   `[27..35]` sequence i64, `[35..43]` exchange timestamp i64 (ms),
//!   `[43..51]` LTP i64; QUOTE/SNAP add `[51]` last qty, `[59]` average
//!   price, `[67]` volume (i64), `[75]` total buy qty, `[83]` total sell qty
//!   (f64), `[91]` open, `[99]` high, `[107]` low, `[115]` close (i64);
//!   SNAP adds `[123]` last trade time, `[131]` OI, `[139]` OI change %,
//!   `[147..347]` ten 20-byte best-five packets (`flag u16, qty i64,
//!   price i64, orders u16`), `[347]` upper and `[355]` lower circuit,
//!   `[363]` / `[371]` 52-week high / low.
//! * Best-five packets are routed by flag: the web splits flag 0 / other and
//!   then swaps the lists (`smartWebSocketV2.py:629-630, 685-713`), so a
//!   non-zero flag is a BUY level and flag 0 a SELL level.
//! * Prices are paise (/100); currency derivatives (exchange type 13) are
//!   /10^7 per the SmartAPI WebSocket 2.0 documentation.
//! * The tick's symbol and exchange come from the subscription map keyed by
//!   `(exchange type, token)`, so `NSE_INDEX:NIFTY` stays `NSE_INDEX`.
//!
//! Order status: `wss://tns.angelone.in/smart-order-update` with
//! `Authorization: Bearer <jwt>`; JSON frames with an `order-status` code
//! and `orderData` (`angel_order_adapter.py`).

use super::mapping::oa_symbol;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedSubscription, Message, NormalizedDepth, NormalizedTick,
    OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{AuthToken, DepthLevel};
use crate::error::{AppError, Result};
use crate::security::Secret;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const WS_URL: &str = "wss://smartapisocket.angelone.in/smart-stream";
pub const ORDER_WS_URL: &str = "wss://tns.angelone.in/smart-order-update";
/// web `HEART_BEAT_INTERVAL`.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// web `ws_ping_interval` of the order adapter.
pub const ORDER_HEARTBEAT: Duration = Duration::from_secs(9);
const CORRELATION_ID: &str = "openalgo";

/// Packet sizes by Angel mode.
pub const LTP_LEN: usize = 51;
pub const QUOTE_LEN: usize = 123;
pub const SNAP_LEN: usize = 379;

/// web `AngelExchangeMapper.EXCHANGE_TYPES` (default NSE).
pub fn exchange_type(brexchange: &str) -> u8 {
    match brexchange {
        "NSE" | "NSE_INDEX" => 1,
        "NFO" => 2,
        "BSE" | "BSE_INDEX" => 3,
        "BFO" => 4,
        "MCX" | "MCX_INDEX" => 5,
        "NCX" | "NCDEX" | "NCO" => 7,
        "CDS" => 13,
        _ => 1,
    }
}

/// Price divisor for an exchange type.
pub fn price_divisor(exchange_type: u8) -> f64 {
    if exchange_type == 13 {
        10_000_000.0
    } else {
        100.0
    }
}

#[derive(Debug, Clone)]
struct SubInfo {
    symbol: String,
    exchange: String,
}

pub struct AngelFeed {
    url: String,
    jwt: Secret,
    api_key: Secret,
    client_code: String,
    feed_token: Secret,
    /// `(exchange type, token)` -> subscription; one entry per subscribed
    /// instrument, removed on unsubscribe.
    subs: HashMap<(u8, String), SubInfo>,
}

impl AngelFeed {
    pub fn new(
        url: &str,
        jwt: &str,
        api_key: &str,
        client_code: &str,
        feed_token: &str,
        _symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: url.to_string(),
            jwt: Secret::new(jwt),
            api_key: Secret::new(api_key),
            client_code: client_code.to_string(),
            feed_token: Secret::new(feed_token),
            subs: HashMap::new(),
        }
    }

    /// From the stored session: `api_key:jwt`, the feed token and the client
    /// code issued at login.
    pub fn from_auth(url: &str, auth: &AuthToken, symbols: SymbolResolver) -> Result<Self> {
        let (api_key, jwt) = auth.pair().ok_or_else(super::session_expired)?;
        let feed = auth.feed().filter(|f| !f.is_empty()).ok_or_else(|| {
            AppError::Auth(
                "Angel One did not issue a live market data token for this session. Log in to Angel One again."
                    .into(),
            )
        })?;
        let client = auth.user_id().filter(|c| !c.is_empty()).ok_or_else(|| {
            AppError::Auth(
                "The Angel One client ID for this session is missing. Log in to Angel One again."
                    .into(),
            )
        })?;
        Ok(Self::new(url, jwt, api_key, client, feed, symbols))
    }

    /// Number of instruments currently registered (for tests and hygiene).
    pub fn registered(&self) -> usize {
        self.subs.len()
    }

    fn frames(&mut self, subs: &[FeedSubscription], subscribe: bool) -> Vec<Message> {
        let mut by_mode: BTreeMap<u8, BTreeMap<u8, Vec<String>>> = BTreeMap::new();
        for s in subs {
            let et = exchange_type(&s.brexchange);
            let key = (et, s.token.clone());
            if subscribe {
                self.subs.insert(
                    key,
                    SubInfo {
                        symbol: s.symbol.clone(),
                        exchange: s.exchange.clone(),
                    },
                );
            } else {
                self.subs.remove(&key);
            }
            by_mode
                .entry(s.mode.code())
                .or_default()
                .entry(et)
                .or_default()
                .push(s.token.clone());
        }
        by_mode
            .into_iter()
            .map(|(mode, groups)| {
                let token_list: Vec<Value> = groups
                    .into_iter()
                    .map(|(et, tokens)| json!({"exchangeType": et, "tokens": tokens}))
                    .collect();
                Message::Text(
                    json!({
                        "correlationID": CORRELATION_ID,
                        "action": if subscribe { 1 } else { 0 },
                        "params": {"mode": mode, "tokenList": token_list},
                    })
                    .to_string(),
                )
            })
            .collect()
    }

    /// Decode one binary tick packet.
    pub fn parse_binary(&self, p: &[u8]) -> Vec<FeedEvent> {
        if p.len() < LTP_LEN {
            return Vec::new();
        }
        let mode = p[0];
        let et = p[1];
        let token: String = p[2..27]
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as char)
            .collect();
        let Some(sub) = self.subs.get(&(et, token)) else {
            return Vec::new();
        };
        let div = price_divisor(et);
        let price = |o: usize| le_i64(p, o) as f64 / div;
        let now = now_ms();
        let mut t = NormalizedTick {
            symbol: sub.symbol.clone(),
            exchange: sub.exchange.clone(),
            mode: mode.min(3),
            ltp: price(43),
            last_trade_time_ms: le_i64(p, 35),
            timestamp_ms: now,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut depth = None;
        if (mode == 2 || mode == 3) && p.len() >= QUOTE_LEN {
            t.last_quantity = le_i64(p, 51);
            t.average_price = price(59);
            t.volume = le_i64(p, 67);
            t.total_buy_quantity = le_f64(p, 75) as i64;
            t.total_sell_quantity = le_f64(p, 83) as i64;
            t.open = price(91);
            t.high = price(99);
            t.low = price(107);
            t.close = price(115);
            t.derive_change();
        }
        if mode == 3 && p.len() >= SNAP_LEN {
            t.oi = le_i64(p, 131);
            let mut buy = Vec::with_capacity(5);
            let mut sell = Vec::with_capacity(5);
            for i in 0..10 {
                let o = 147 + i * 20;
                let level = DepthLevel {
                    quantity: le_i64(p, o + 2),
                    price: le_i64(p, o + 10) as f64 / div,
                    orders: i64::from(le_u16(p, o + 18)),
                };
                if le_u16(p, o) == 0 {
                    sell.push(level);
                } else {
                    buy.push(level);
                }
            }
            buy.resize(5, DepthLevel::default());
            sell.resize(5, DepthLevel::default());
            depth = Some(NormalizedDepth {
                symbol: t.symbol.clone(),
                exchange: t.exchange.clone(),
                ltp: t.ltp,
                buy,
                sell,
                total_buy_quantity: t.total_buy_quantity,
                total_sell_quantity: t.total_sell_quantity,
                timestamp_ms: now,
                ..Default::default()
            });
        }
        out.push(FeedEvent::Tick(t));
        if let Some(d) = depth {
            out.push(FeedEvent::Depth(d));
        }
        out
    }

    fn parse_text(&self, text: &str) -> Vec<FeedEvent> {
        if text.trim() == "pong" {
            return vec![FeedEvent::Heartbeat];
        }
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            let code = v
                .get("errorCode")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let msg = v
                .get("errorMessage")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !code.is_empty() || !msg.is_empty() {
                tracing::warn!("Angel One market data feed error {}: {}", code, msg);
            }
        }
        Vec::new()
    }
}

fn le_i64(b: &[u8], o: usize) -> i64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    i64::from_le_bytes(a)
}

fn le_f64(b: &[u8], o: usize) -> f64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    f64::from_le_bytes(a)
}

fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn header(v: &str) -> Result<HeaderValue> {
    HeaderValue::from_str(v).map_err(|_| {
        AppError::Auth("The Angel One session token is not valid. Log in again.".into())
    })
}

impl BrokerFeed for AngelFeed {
    fn broker(&self) -> &'static str {
        "angel"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let mut req = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Angel One feed address is invalid".into()))?;
        let h = req.headers_mut();
        h.insert("Authorization", header(self.jwt.expose())?);
        h.insert("x-api-key", header(self.api_key.expose())?);
        h.insert("x-client-code", header(&self.client_code)?);
        h.insert("x-feed-token", header(self.feed_token.expose())?);
        Ok(req)
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, true)
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        self.frames(subs, false)
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Binary(b) => self.parse_binary(b),
            Message::Text(t) => self.parse_text(t),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((HEARTBEAT, Message::Text("ping".into())))
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}

// ---------------------------------------------------------------------------
// Order status socket
// ---------------------------------------------------------------------------

/// web `_STATUS_CODE_MAP`.
pub fn order_status_from_code(code: &str) -> Option<&'static str> {
    Some(match code {
        "AB01" | "AB04" | "AB06" | "AB08" | "AB09" | "AB11" => "open",
        "AB02" | "AB07" => "cancelled",
        "AB03" => "rejected",
        "AB05" => "complete",
        "AB10" => "trigger pending",
        _ => return None,
    })
}

/// web `_STATUS_TEXT_MAP` (fallback when the code is missing).
pub fn order_status_from_text(text: &str) -> String {
    let t = text.trim().to_ascii_lowercase();
    match t.as_str() {
        "open" | "pending" | "open pending" | "modified" | "modify pending" => "open".into(),
        "trigger pending" => "trigger pending".into(),
        "executed" | "complete" => "complete".into(),
        "rejected" => "rejected".into(),
        "cancelled" => "cancelled".into(),
        "" => "open".into(),
        _ => t,
    }
}

pub struct AngelOrderFeed {
    url: String,
    jwt: Secret,
    symbols: SymbolResolver,
}

impl AngelOrderFeed {
    pub fn new(url: &str, jwt: &str, symbols: SymbolResolver) -> Self {
        Self {
            url: url.to_string(),
            jwt: Secret::new(jwt),
            symbols,
        }
    }

    /// web `AngelOrderUpdateAdapter.normalize`.
    pub fn normalize(&self, text: &str) -> Option<OrderUpdate> {
        let v: Value = serde_json::from_str(text).ok()?;
        let code = v
            .get("order-status")
            .map(|c| match c {
                Value::String(s) => s.to_ascii_uppercase(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let d = v.get("orderData")?;
        let s = |k: &str| match d.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        let f = |k: &str| match d.get(k) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(s)) => s.parse().unwrap_or(0.0),
            _ => 0.0,
        };
        let orderid = s("orderid");
        if code == "AB00" || orderid.is_empty() {
            return None;
        }
        let order_status = match order_status_from_code(&code) {
            Some(st) => st.to_string(),
            None => {
                let raw = if d.get("orderstatus").is_some() {
                    s("orderstatus")
                } else {
                    s("status")
                };
                order_status_from_text(&raw)
            }
        };
        let filled = f("filledshares") as i64;
        let unfilled = f("unfilledshares") as i64;
        let quantity = match f("quantity") as i64 {
            0 => filled + unfilled,
            q => q,
        };
        let exchange = s("exchange");
        let ordertype = s("ordertype");
        let producttype = s("producttype");
        Some(OrderUpdate {
            orderid,
            symbol: oa_symbol(
                &self.symbols,
                &s("symboltoken"),
                &s("tradingsymbol"),
                &exchange,
            ),
            exchange,
            action: s("transactiontype").to_ascii_uppercase(),
            quantity,
            price: f("price"),
            trigger_price: f("triggerprice"),
            pricetype: match ordertype.as_str() {
                "STOPLOSS_LIMIT" => "SL".into(),
                "STOPLOSS_MARKET" => "SL-M".into(),
                _ => ordertype,
            },
            product: super::mapping::reverse_map_product_type(&producttype)
                .map(str::to_string)
                .unwrap_or(producttype),
            rejection_reason: if order_status == "rejected" {
                s("text")
            } else {
                String::new()
            },
            order_status,
            filled_quantity: filled,
            pending_quantity: unfilled,
            average_price: f("averageprice"),
        })
    }
}

impl BrokerFeed for AngelOrderFeed {
    fn broker(&self) -> &'static str {
        "angel"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let mut req =
            self.url.as_str().into_client_request().map_err(|_| {
                AppError::Internal("Angel One order feed address is invalid".into())
            })?;
        req.headers_mut().insert(
            "Authorization",
            header(&format!("Bearer {}", self.jwt.expose()))?,
        );
        Ok(req)
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => self
                .normalize(t)
                .map(FeedEvent::OrderUpdate)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((ORDER_HEARTBEAT, Message::Ping(Vec::new())))
    }
}
