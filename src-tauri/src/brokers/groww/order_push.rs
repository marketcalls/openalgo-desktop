//! Groww's order push: the protobuf `OrderDetailsBroadCastDto` its order
//! subjects carry (growwapi 1.5.0 `proto/stock_orders_socket_response_pb2.py`),
//! read as the web reads it (`groww_order_adapter.py` `decode_order_update`,
//! openalgo#2474): field 2 of the outer message is an
//! `OrderDetailUpdateDto`, whose fields are taken by number and wire type.
//!
//! Proto3 leaves zero values off the wire, so an absent enum is its first
//! name (exchange BSE, segment CASH, status NEW, product CNC, order type
//! MARKET, side BUY), an absent number 0 and an absent text empty. An enum
//! value past the names Groww publishes is kept as its number, as the web
//! keeps `str(value)`. A field of an unexpected wire type is skipped; a
//! message the reader cannot walk (an unknown wire type, a cut-off number)
//! is no update at all, as the web drops a message its decoder raises on.

/// `OrderStatus` names, by wire value (StockOrdersSocketResponse.proto).
const ORDER_STATUS: &[&str] = &[
    "NEW",
    "ACKED",
    "TRIGGER_PENDING",
    "APPROVED",
    "REJECTED",
    "FAILED",
    "EXECUTED",
    "DELIVERY_AWAITED",
    "CANCELLED",
    "CANCELLATION_REQUESTED",
    "MODIFICATION_REQUESTED",
    "COMPLETED",
];
const EXCHANGE: &[&str] = &["BSE", "NSE", "MCX", "MCXSX", "NCDEX", "US"];
const SEGMENT: &[&str] = &["CASH", "FNO", "CURRENCY", "COMMODITY"];
const PRODUCT: &[&str] = &["CNC", "MIS", "CO", "BO", "NRML", "ARB", "MTF"];
/// The SDK renames MKT and L to MARKET and LIMIT (`proto_parser.get_data_dict`);
/// SL and SL_M keep their names, as Groww's REST `order_type` does.
const ORDER_TYPE: &[&str] = &["MARKET", "LIMIT", "SL", "SL_M"];
const SIDE: &[&str] = &["BUY", "SELL"];

/// One order update as Groww pushed it, the fields an update uses. Its
/// prices (fields 2, 3 and 6) are integers in a unit neither Groww's docs
/// nor its SDK state, so they are not read: the web leaves them out rather
/// than report them as rupees and risk being 100 times off
/// (openalgo#2474). Nor is the exchange's order id (field 8), which an
/// order update has no field for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderPush {
    pub qty: i64,
    pub filled_qty: i64,
    pub remaining_qty: i64,
    pub groww_order_id: String,
    pub order_status: String,
    pub exchange: String,
    pub segment: String,
    pub product: String,
    pub order_type: String,
    pub transaction_type: String,
    pub remark: String,
    /// The instrument: an ISIN for equity, Groww's contract name for F&O.
    pub contract_id: String,
}

impl Default for OrderPush {
    /// Every field at its proto3 zero value.
    fn default() -> Self {
        Self {
            qty: 0,
            filled_qty: 0,
            remaining_qty: 0,
            groww_order_id: String::new(),
            order_status: ORDER_STATUS[0].into(),
            exchange: EXCHANGE[0].into(),
            segment: SEGMENT[0].into(),
            product: PRODUCT[0].into(),
            order_type: ORDER_TYPE[0].into(),
            transaction_type: SIDE[0].into(),
            remark: String::new(),
            contract_id: String::new(),
        }
    }
}

/// One field's value by wire type: a varint, or a length-delimited run of
/// bytes. Fixed-width values are walked over and not read.
enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// A base-128 varint at `pos`; at most ten bytes (the web's `_read_varint`).
fn varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// `count` bytes from `pos`, fewer when the message ends first (the web
/// slices past the end without complaint), and the position after them.
fn take(data: &[u8], pos: usize, count: u64) -> (&[u8], usize) {
    let end = usize::try_from(count)
        .ok()
        .and_then(|n| pos.checked_add(n))
        .unwrap_or(usize::MAX);
    let slice = data
        .get(pos.min(data.len())..end.min(data.len()))
        .unwrap_or(&[]);
    (slice, end)
}

/// Every `(field number, value)` of one message, in order (the web's
/// `_fields`); `None` when the message cannot be walked.
fn fields(data: &[u8]) -> Option<Vec<(u64, Value<'_>)>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let key = varint(data, &mut pos)?;
        let value = match key & 7 {
            0 => Value::Varint(varint(data, &mut pos)?),
            2 => {
                let len = varint(data, &mut pos)?;
                let (bytes, next) = take(data, pos, len);
                pos = next;
                Value::Bytes(bytes)
            }
            1 => {
                pos = take(data, pos, 8).1;
                Value::Fixed
            }
            5 => {
                pos = take(data, pos, 4).1;
                Value::Fixed
            }
            _ => return None,
        };
        out.push((key >> 3, value));
    }
    Some(out)
}

/// An enum's name, or its number when Groww sent one past the names.
fn name(names: &[&str], value: u64) -> String {
    usize::try_from(value)
        .ok()
        .and_then(|i| names.get(i))
        .map_or_else(|| value.to_string(), |n| (*n).to_string())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `OrderDetailsBroadCastDto` bytes to the order update it carries, or
/// `None` when it carries none (web `decode_order_update`).
pub fn decode(payload: &[u8]) -> Option<OrderPush> {
    let mut inner = None;
    for (number, value) in fields(payload)? {
        if let (2, Value::Bytes(bytes)) = (number, value) {
            inner = Some(bytes);
        }
    }
    let mut push = OrderPush::default();
    for (number, value) in fields(inner?)? {
        match value {
            Value::Varint(v) => read_number(&mut push, number, v),
            Value::Bytes(b) => read_text(&mut push, number, b),
            Value::Fixed => {}
        }
    }
    Some(push)
}

/// A varint field: a number (int64, two's complement) or an enum.
fn read_number(push: &mut OrderPush, number: u64, v: u64) {
    // An int64 on the wire is its two's complement (the web subtracts 2^64).
    let int = v as i64;
    match number {
        1 => push.qty = int,
        4 => push.filled_qty = int,
        5 => push.remaining_qty = int,
        9 => push.order_status = name(ORDER_STATUS, v),
        11 => push.exchange = name(EXCHANGE, v),
        12 => push.segment = name(SEGMENT, v),
        13 => push.product = name(PRODUCT, v),
        14 => push.order_type = name(ORDER_TYPE, v),
        15 => push.transaction_type = name(SIDE, v),
        _ => {}
    }
}

/// A length-delimited field: Groww's text fields.
fn read_text(push: &mut OrderPush, number: u64, bytes: &[u8]) {
    match number {
        7 => push.groww_order_id = text(bytes),
        16 => push.remark = text(bytes),
        22 => push.contract_id = text(bytes),
        _ => {}
    }
}
