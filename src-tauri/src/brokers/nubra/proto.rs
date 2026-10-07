//! Nubra market-feed protobuf messages (web `protos/nubrafrontend_pb2.py`,
//! package `protos.zanskarsecurities.nubrafrontend`).
//!
//! Hand-written prost structs; field numbers and types are taken from the
//! descriptors in the web's generated module (int64 = `int64`, float =
//! `float`, `inst_id` on the order book is `uint32`). Prices are int64 paise.
//!
//! Frames are an outer `google.protobuf.Any` whose `value` is an inner `Any`;
//! the inner `type_url` suffix names the batch message
//! (`api/nubrawebsocket.py:_decode_protobuf`).
//!
//! The order-update stream's `NubraToClientIntentUpdate` is not in the web's
//! protos; `decode_fields` is the minimal wire walker the web uses for it
//! (`streaming/nubra_order_adapter.py:_decode_fields`).

#![allow(clippy::derive_partial_eq_without_eq)]

use std::collections::HashMap;

/// `google.protobuf.Any`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Any {
    #[prost(string, tag = "1")]
    pub type_url: String,
    #[prost(bytes = "vec", tag = "2")]
    pub value: Vec<u8>,
}

/// `WebSocketMsgIndex` (index channel; also instruments by name).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WebSocketMsgIndex {
    #[prost(string, tag = "1")]
    pub indexname: String,
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
    #[prost(int64, tag = "3")]
    pub index_value: i64,
    #[prost(int64, tag = "4")]
    pub high_index_value: i64,
    #[prost(int64, tag = "5")]
    pub low_index_value: i64,
    #[prost(int64, tag = "6")]
    pub volume: i64,
    #[prost(float, tag = "7")]
    pub changepercent: f32,
    #[prost(int64, tag = "8")]
    pub tick_volume: i64,
    #[prost(int64, tag = "9")]
    pub prev_close: i64,
    #[prost(string, tag = "10")]
    pub exchange: String,
    #[prost(int64, tag = "11")]
    pub volume_oi: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BatchWebSocketIndexMessage {
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    #[prost(message, repeated, tag = "2")]
    pub indexes: Vec<WebSocketMsgIndex>,
    #[prost(message, repeated, tag = "3")]
    pub instruments: Vec<WebSocketMsgIndex>,
}

#[derive(Clone, Copy, PartialEq, ::prost::Message)]
pub struct OrderBookLevel {
    #[prost(int64, tag = "1")]
    pub price: i64,
    #[prost(int64, tag = "2")]
    pub quantity: i64,
    #[prost(int64, tag = "3")]
    pub orders: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WebSocketMsgOrderBook {
    #[prost(uint32, tag = "1")]
    pub inst_id: u32,
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
    #[prost(message, repeated, tag = "3")]
    pub bids: Vec<OrderBookLevel>,
    #[prost(message, repeated, tag = "4")]
    pub asks: Vec<OrderBookLevel>,
    #[prost(int64, tag = "5")]
    pub ltp: i64,
    #[prost(int64, tag = "6")]
    pub ltq: i64,
    #[prost(int64, tag = "7")]
    pub volume: i64,
    #[prost(int64, tag = "8")]
    pub ref_id: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BatchWebSocketOrderbookMessage {
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    #[prost(message, repeated, tag = "2")]
    pub instruments: Vec<WebSocketMsgOrderBook>,
}

/// `WebSocketMsgOptionChainItem` (greeks channel; carries OI).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WebSocketMsgOptionChainItem {
    #[prost(int64, tag = "1")]
    pub inst_id: i64,
    #[prost(int64, tag = "2")]
    pub ts: i64,
    #[prost(int64, tag = "3")]
    pub sp: i64,
    #[prost(int32, tag = "4")]
    pub ls: i32,
    #[prost(int64, tag = "5")]
    pub ltp: i64,
    #[prost(float, tag = "6")]
    pub ltpchg: f32,
    #[prost(float, tag = "7")]
    pub iv: f32,
    #[prost(float, tag = "8")]
    pub delta: f32,
    #[prost(float, tag = "9")]
    pub gamma: f32,
    #[prost(float, tag = "10")]
    pub theta: f32,
    #[prost(float, tag = "11")]
    pub vega: f32,
    #[prost(int64, tag = "12")]
    pub oi: i64,
    #[prost(int64, tag = "13")]
    pub volume: i64,
    #[prost(int64, tag = "14")]
    pub ref_id: i64,
    #[prost(int64, tag = "15")]
    pub prev_oi: i64,
    #[prost(int64, tag = "16")]
    pub price_pcp: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BatchWebSocketGreeksMessage {
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    #[prost(message, repeated, tag = "2")]
    pub instruments: Vec<WebSocketMsgOptionChainItem>,
}

/// `WebSocketMsgIndexBucket` (index_bucket channel: OHLCV candles).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WebSocketMsgIndexBucket {
    #[prost(string, tag = "1")]
    pub indexname: String,
    #[prost(string, tag = "2")]
    pub exchange: String,
    #[prost(int32, tag = "3")]
    pub interval: i32,
    #[prost(int64, tag = "4")]
    pub timestamp: i64,
    #[prost(int64, tag = "5")]
    pub open: i64,
    #[prost(int64, tag = "6")]
    pub high: i64,
    #[prost(int64, tag = "7")]
    pub low: i64,
    #[prost(int64, tag = "8")]
    pub close: i64,
    #[prost(int64, tag = "9")]
    pub bucket_volume: i64,
    #[prost(int64, tag = "10")]
    pub tick_volume: i64,
    #[prost(int64, tag = "11")]
    pub cumulative_volume: i64,
    #[prost(int64, tag = "12")]
    pub bucket_timestamp: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BatchWebSocketIndexBucketMessage {
    #[prost(int64, tag = "1")]
    pub timestamp: i64,
    #[prost(message, repeated, tag = "2")]
    pub indexes: Vec<WebSocketMsgIndexBucket>,
    #[prost(message, repeated, tag = "3")]
    pub instruments: Vec<WebSocketMsgIndexBucket>,
}

/// One decoded market frame.
#[derive(Debug, Clone, PartialEq)]
pub enum MarketFrame {
    Index(BatchWebSocketIndexMessage),
    Orderbook(BatchWebSocketOrderbookMessage),
    Greeks(BatchWebSocketGreeksMessage),
    Bucket(BatchWebSocketIndexBucketMessage),
}

/// Unwrap Any-in-Any; `(inner type_url, inner value)`.
pub fn unwrap_any(raw: &[u8]) -> Option<(String, Vec<u8>)> {
    use prost::Message as _;
    let outer = Any::decode(raw).ok()?;
    let inner = Any::decode(outer.value.as_slice()).ok()?;
    Some((inner.type_url, inner.value))
}

/// Decode a market frame by its inner type URL suffix.
pub fn decode_market(raw: &[u8]) -> Option<MarketFrame> {
    use prost::Message as _;
    let (url, value) = unwrap_any(raw)?;
    let v = value.as_slice();
    if url.ends_with("BatchWebSocketIndexMessage") {
        BatchWebSocketIndexMessage::decode(v)
            .ok()
            .map(MarketFrame::Index)
    } else if url.ends_with("BatchWebSocketOrderbookMessage") {
        BatchWebSocketOrderbookMessage::decode(v)
            .ok()
            .map(MarketFrame::Orderbook)
    } else if url.ends_with("BatchWebSocketIndexBucketMessage") {
        BatchWebSocketIndexBucketMessage::decode(v)
            .ok()
            .map(MarketFrame::Bucket)
    } else if url.ends_with("BatchWebSocketGreeksMessage") {
        BatchWebSocketGreeksMessage::decode(v)
            .ok()
            .map(MarketFrame::Greeks)
    } else {
        None
    }
}

/// Wrap a message the way the server does (tests and fakes).
pub fn wrap(type_name: &str, value: Vec<u8>) -> Vec<u8> {
    use prost::Message as _;
    let inner = Any {
        type_url: format!(
            "type.googleapis.com/protos.zanskarsecurities.nubrafrontend.{}",
            type_name
        ),
        value,
    };
    let outer = Any {
        type_url: "type.googleapis.com/google.protobuf.Any".into(),
        value: inner.encode_to_vec(),
    };
    outer.encode_to_vec()
}

/// A raw protobuf field value.
#[derive(Debug, Clone, PartialEq)]
pub enum Wire {
    Varint(u64),
    Bytes(Vec<u8>),
    Fixed(u64),
}

/// Minimal wire walker: field number -> raw values, in order. `None` on a
/// truncated or unsupported encoding (the frame is skipped).
pub fn decode_fields(buf: &[u8]) -> Option<HashMap<u32, Vec<Wire>>> {
    fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = *buf.get(*pos)?;
            *pos += 1;
            if shift < 64 {
                result |= u64::from(b & 0x7f) << shift;
            }
            if b & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift > 70 {
                return None;
            }
        }
    }
    let mut out: HashMap<u32, Vec<Wire>> = HashMap::new();
    let mut i = 0usize;
    while i < buf.len() {
        let tag = varint(buf, &mut i)?;
        let field = u32::try_from(tag >> 3).ok()?;
        let value = match tag & 7 {
            0 => Wire::Varint(varint(buf, &mut i)?),
            1 => {
                let b: [u8; 8] = buf.get(i..i + 8)?.try_into().ok()?;
                i += 8;
                Wire::Fixed(u64::from_le_bytes(b))
            }
            2 => {
                let len = usize::try_from(varint(buf, &mut i)?).ok()?;
                let end = i.checked_add(len)?;
                let b = buf.get(i..end)?.to_vec();
                i = end;
                Wire::Bytes(b)
            }
            5 => {
                let b: [u8; 4] = buf.get(i..i + 4)?.try_into().ok()?;
                i += 4;
                Wire::Fixed(u64::from(u32::from_le_bytes(b)))
            }
            _ => return None,
        };
        out.entry(field).or_default().push(value);
    }
    Some(out)
}

/// First integer value of a field (0 when absent).
pub fn first_int(f: &HashMap<u32, Vec<Wire>>, n: u32) -> i64 {
    match f.get(&n).and_then(|v| v.first()) {
        Some(Wire::Varint(x)) | Some(Wire::Fixed(x)) => *x as i64,
        _ => 0,
    }
}

/// First length-delimited value of a field.
pub fn first_bytes(f: &HashMap<u32, Vec<Wire>>, n: u32) -> Option<&[u8]> {
    match f.get(&n).and_then(|v| v.first()) {
        Some(Wire::Bytes(b)) => Some(b.as_slice()),
        _ => None,
    }
}

/// First string value of a field (empty when absent).
pub fn first_str(f: &HashMap<u32, Vec<Wire>>, n: u32) -> String {
    first_bytes(f, n)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default()
}
