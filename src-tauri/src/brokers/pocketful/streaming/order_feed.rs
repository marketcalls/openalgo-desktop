//! Pocketful order updates on a socket of their own (web
//! `streaming/pocketful_order_adapter.py`, from Pocketful's SDKs).
//!
//! * The same address as the market feed, `<host>/ws/v1/feeds?login_id=..
//!   &access_token=..` (the token in the query: never logged).
//! * Once open, `{"a":"subscribe","v":[<client_id>,"web"],"m":"updates"}`
//!   (the SDKs' `subscribe_order_update`); no unsubscribe exists, so
//!   stopping is closing the socket. Heartbeat `{"a":"h","v":[],"m":""}`
//!   every 8 s, as on the market socket.
//! * A frame's first byte is its mode: 50 an order update, 51 a trade
//!   update, 1, 2 and 4 market data. An update's JSON starts at byte 5 (the
//!   four bytes after the mode are undocumented and not read); it is cut
//!   from the bytes, so a header byte that is not UTF-8 does not lose the
//!   update. A text frame is read as its UTF-8 bytes, as the JavaScript SDK
//!   reads every frame.
//! * An order update is `{"error": {...}, "result": [<order>, ...]}` and
//!   every order in it is published. A trade update is not: it carries one
//!   fill with no order status or order quantity, and the order update of
//!   the same fill carries the order's running totals.

use super::{feed_url, heartbeat_frame, HEARTBEAT_SECS};
use crate::brokers::common::streaming::{
    BrokerFeed, FeedEvent, FeedSubscription, Message, OrderUpdate, WsRequest,
};
use crate::brokers::common::symbols::SymbolResolver;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Frame modes (the first byte), from the SDKs' `on_message`.
pub const MODE_ORDER: u8 = 50;
pub const MODE_TRADE: u8 = 51;
/// Where an update's JSON starts (`json.loads(packet.decode()[5:])`).
const UPDATE_JSON_OFFSET: usize = 5;

/// The order-update feed of one session.
pub struct PocketfulOrderFeed {
    url: String,
    client_id: String,
    symbols: SymbolResolver,
}

impl PocketfulOrderFeed {
    pub fn new(
        ws_base: &str,
        client_id: &str,
        access_token: &str,
        symbols: SymbolResolver,
    ) -> Self {
        Self {
            url: feed_url(ws_base, client_id, access_token),
            client_id: client_id.to_string(),
            symbols,
        }
    }

    /// web `on_open_extra`: the SDKs' `subscribe_order_update` message.
    pub(crate) fn subscribe_frame(&self) -> Message {
        Message::Text(
            json!({"a": "subscribe", "v": [self.client_id, "web"], "m": "updates"}).to_string(),
        )
    }

    /// The order updates in one frame (web `_handle_message`): one per order
    /// of an order update, none for anything else.
    pub(crate) fn updates(&self, frame: &[u8]) -> Vec<OrderUpdate> {
        let Some(&mode) = frame.first() else {
            return Vec::new();
        };
        if mode == MODE_TRADE {
            tracing::debug!("Pocketful trade update skipped; its order update carries the fill");
            return Vec::new();
        }
        if mode != MODE_ORDER {
            return Vec::new();
        }
        let body = frame.get(UPDATE_JSON_OFFSET..).unwrap_or_default();
        let payload = match std::str::from_utf8(body)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
        {
            Some(v) => v,
            None => {
                tracing::error!(
                    "Could not read a Pocketful order update frame ({} bytes)",
                    frame.len()
                );
                return Vec::new();
            }
        };
        if let Some(error) = payload.get("error").filter(|e| e.is_object()) {
            let code = int_of(error.get("code"));
            if code != 0 {
                tracing::warn!(
                    "Pocketful order update reported error {}: {}",
                    code,
                    text_of(error.get("message"))
                );
            }
        }
        order_records(&payload)
            .into_iter()
            .filter_map(|record| self.normalize(record))
            .collect()
    }

    /// One order of an order update -> the order-update event (web
    /// `normalize`); none for a record without `oms_order_id`.
    pub(crate) fn normalize(&self, record: &Value) -> Option<OrderUpdate> {
        if !record.is_object() {
            return None;
        }
        // oms_order_id is the id place order returns and cancel, modify and
        // order history take (place-regular-order.md).
        let orderid = text_of(record.get("oms_order_id"));
        if orderid.is_empty() {
            return None;
        }
        let exchange = text_of(record.get("exchange")).to_ascii_uppercase();
        let symbol = self.openalgo_symbol(
            &text_of(record.get("trading_symbol")),
            &exchange,
            &text_of(record.get("instrument_token")),
        );
        let order_status = order_status(record.get("order_status"));
        let quantity = int_of(record.get("quantity"));
        let filled_quantity = int_of(record.get("filled_quantity"));
        let pending_quantity = match record.get("remaining_quantity") {
            Some(v) if !v.is_null() && !text_of(Some(v)).is_empty() => int_of(Some(v)),
            _ => (quantity - filled_quantity).max(0),
        };
        // The docs' example shows "NONE" in rejection_reason on a live order.
        let reason = text_of(record.get("rejection_reason"));
        let rejection_reason = if order_status == "rejected" && !reason.eq_ignore_ascii_case("NONE")
        {
            reason
        } else {
            String::new()
        };
        let average = float_of(record.get("average_price"));
        Some(OrderUpdate {
            orderid,
            symbol,
            exchange,
            action: text_of(record.get("order_side")).to_ascii_uppercase(),
            quantity,
            price: float_of(record.get("price")),
            trigger_price: float_of(record.get("trigger_price")),
            pricetype: pricetype(&text_of(record.get("order_type")).to_ascii_uppercase()),
            product: product(&text_of(record.get("product")).to_ascii_uppercase()),
            order_status,
            filled_quantity,
            pending_quantity,
            average_price: if average != 0.0 {
                average
            } else {
                float_of(record.get("average_trade_price"))
            },
            rejection_reason,
        })
    }

    /// web `to_openalgo_symbol`: the broker token first, then the broker
    /// symbol, else the broker's own symbol.
    fn openalgo_symbol(&self, broker_symbol: &str, exchange: &str, token: &str) -> String {
        if exchange.is_empty() {
            return broker_symbol.to_string();
        }
        if let Some(row) = (!token.is_empty())
            .then(|| self.symbols.by_token(exchange, token))
            .flatten()
        {
            return row.symbol;
        }
        (!broker_symbol.is_empty())
            .then(|| self.symbols.oa_symbol(broker_symbol, exchange))
            .flatten()
            .unwrap_or_else(|| broker_symbol.to_string())
    }
}

/// The order dicts of an order update (web `order_records`): the documented
/// `{"error", "result": [...]}`, a `result` that is one order, or an order
/// (or a list of them) without the envelope.
fn order_records(payload: &Value) -> Vec<&Value> {
    fn objects(list: &[Value]) -> Vec<&Value> {
        list.iter().filter(|v| v.is_object()).collect()
    }
    match payload {
        Value::Array(list) => objects(list),
        Value::Object(map) => match map.get("result") {
            Some(Value::Array(list)) => objects(list),
            Some(one @ Value::Object(_)) => vec![one],
            Some(_) => Vec::new(),
            None if map.contains_key("oms_order_id") => vec![payload],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Pocketful's order status in OpenAlgo's words (web `_STATUS_MAP`); any
/// other passes through lowercased, never forced into a bucket, and none
/// is `open`.
fn order_status(raw: Option<&Value>) -> String {
    let key = text_of(raw).to_ascii_uppercase();
    let known = match key.as_str() {
        "COMPLETE" => "complete",
        "REJECTED" => "rejected",
        "CANCELLED" | "AMO_CANCEL_CONFIRMED" | "CANCEL_CONFIRMED" => "cancelled",
        "PENDING"
        | "CONFIRMED"
        | "MODCONFIRMED"
        | "MODIFY_CONFIRMED"
        | "AMO_REQ_RECEIVED"
        | "AMO_MODIFY_CONFIRMED"
        | "OPEN" => "open",
        "TRIGGER PENDING" | "TRIGGER_PENDING" => "trigger pending",
        "" => "open",
        other => return other.to_ascii_lowercase(),
    };
    known.to_string()
}

/// web `_PRICETYPE_MAP`, else the order type as sent.
fn pricetype(order_type: &str) -> String {
    match order_type {
        "SLM" | "SL-M" => "SL-M".to_string(),
        other => other.to_string(),
    }
}

/// web `_PRODUCT_MAP` (the annexure's INTRADAY and MARGIN), else as sent.
fn product(product: &str) -> String {
    match product {
        "INTRADAY" => "MIS".to_string(),
        "MARGIN" => "NRML".to_string(),
        other => other.to_string(),
    }
}

/// web `_text`: a string trimmed, a number as written, nothing for null.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string(),
    }
}

/// web `_float`: a number or numeric text, else 0.
fn float_of(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// web `_int`: `int(float(x))`, else 0.
fn int_of(v: Option<&Value>) -> i64 {
    float_of(v) as i64
}

impl BrokerFeed for PocketfulOrderFeed {
    fn broker(&self) -> &'static str {
        "pocketful"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        self.url
            .as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Pocketful order feed address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        vec![self.subscribe_frame()]
    }

    fn subscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn unsubscribe_frames(&mut self, _subs: &[FeedSubscription]) -> Vec<Message> {
        Vec::new()
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        let frame: &[u8] = match msg {
            Message::Binary(b) => b.as_slice(),
            Message::Text(t) => t.as_bytes(),
            _ => return Vec::new(),
        };
        self.updates(frame)
            .into_iter()
            .map(FeedEvent::OrderUpdate)
            .collect()
    }

    fn heartbeat(&self) -> Option<(Duration, Message)> {
        Some((Duration::from_secs(HEARTBEAT_SECS), heartbeat_frame()))
    }
}
