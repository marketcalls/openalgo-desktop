//! Wire format of the OpenAlgo web WebSocket feed (`websocket_proxy/server.py`
//! and `mode_utils.py`), reproduced message for message.
//!
//! The web server is Python, and several error texts are Python artefacts
//! that clients already match on (`Invalid action: None`, `'list' object has
//! no attribute 'get'`, `Invalid mode '2'; ...`). The helpers here render
//! JSON values the way Python's `str()` / `repr()` would so those texts are
//! byte-identical.

use super::dialect::{aliases, Aliases, Canonical};
use super::source::{DepthLevel, InstrumentKey, MarketUpdate, Mode};
use serde::Serialize;
use serde_json::Value;

/// Error codes the web server sends.
pub mod code {
    pub const NOT_AUTHENTICATED: &str = "NOT_AUTHENTICATED";
    pub const AUTHENTICATION_ERROR: &str = "AUTHENTICATION_ERROR";
    pub const BROKER_ERROR: &str = "BROKER_ERROR";
    pub const INVALID_MODE: &str = "INVALID_MODE";
    pub const INVALID_PARAMETERS: &str = "INVALID_PARAMETERS";
    pub const INVALID_ACTION: &str = "INVALID_ACTION";
    pub const INVALID_JSON: &str = "INVALID_JSON";
    pub const SERVER_ERROR: &str = "SERVER_ERROR";
}

pub const NOT_AUTHENTICATED_MSG: &str = "You must authenticate first";

/// Python type name of a decoded JSON value.
pub fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python truthiness (`x or y` picks `y` when `x` is falsy).
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn py_float(f: f64) -> String {
    if f.is_nan() {
        "nan".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf" } else { "-inf" }.into()
    } else if f.fract() == 0.0 && f.abs() < 1e16 {
        format!("{:.1}", f)
    } else {
        format!("{}", f)
    }
}

/// Python `repr()` of a string.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr()` of a decoded JSON value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// Python `str()` of a decoded JSON value.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// `mode_utils.normalize_mode`: int 1/2/3 or a case-insensitive label.
/// The error text is what the web puts in the `INVALID_MODE` frame.
pub fn normalize_mode(v: &Value) -> Result<Mode, String> {
    match v {
        Value::Bool(b) => Err(format!(
            "Mode must be int or str, got bool ({})",
            if *b { "True" } else { "False" }
        )),
        Value::Number(n) if !n.is_f64() => {
            let m = n.as_i64().and_then(|i| u8::try_from(i).ok());
            match m.and_then(Mode::from_u8) {
                Some(mode) => Ok(mode),
                None => Err(format!(
                    "Invalid mode {}; expected 1 (LTP), 2 (Quote), or 3 (Depth)",
                    py_str(v)
                )),
            }
        }
        Value::String(s) => match s.trim().to_uppercase().as_str() {
            "LTP" => Ok(Mode::Ltp),
            "QUOTE" => Ok(Mode::Quote),
            "DEPTH" => Ok(Mode::Depth),
            _ => Err(format!(
                "Invalid mode {}; expected 'LTP', 'Quote', or 'Depth' (case-insensitive)",
                py_repr_str(s)
            )),
        },
        other => Err(format!(
            "Mode must be int or str, got {}",
            py_type_name(other)
        )),
    }
}

/// `{"status":"error","code","message","request_id"?}`, no `type`.
#[derive(Serialize)]
pub struct ErrorFrame<'a> {
    pub status: &'static str,
    pub code: &'a str,
    pub message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<&'a Value>,
}

/// A request id is echoed when the client sent one that is not `null`.
pub fn request_id(data: &Value) -> Option<&Value> {
    data.get("request_id").filter(|v| !v.is_null())
}

pub fn error_frame(code: &str, message: &str, request_id: Option<&Value>) -> String {
    to_json(&ErrorFrame {
        status: "error",
        code,
        message,
        request_id,
    })
}

pub fn to_json<T: Serialize>(v: &T) -> String {
    // Serializing these plain structs cannot fail; fall back to an empty
    // object rather than panicking on a data path.
    serde_json::to_string(v).unwrap_or_else(|_| "{}".into())
}

#[derive(Serialize)]
pub struct SupportedFeatures {
    pub ltp: bool,
    pub quote: bool,
    pub depth: bool,
}

#[derive(Serialize)]
pub struct AuthAck<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub message: &'static str,
    pub broker: &'a str,
    pub user_id: &'a str,
    pub supported_features: SupportedFeatures,
}

pub fn auth_ack(broker: &str, user_id: &str) -> String {
    to_json(&AuthAck {
        kind: "auth",
        status: "success",
        message: "Authentication successful",
        broker,
        user_id,
        supported_features: SupportedFeatures {
            ltp: true,
            quote: true,
            depth: true,
        },
    })
}

#[derive(Serialize)]
pub struct Pong<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub server_timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_timestamp: Option<&'a Value>,
    #[serde(rename = "_pingId", skip_serializing_if = "Option::is_none")]
    pub ping_id: Option<&'a Value>,
}

pub fn pong(data: &Value, now_ms: i64) -> String {
    to_json(&Pong {
        kind: "pong",
        status: "success",
        server_timestamp: now_ms,
        client_timestamp: data.get("timestamp").filter(|v| !v.is_null()),
        ping_id: data.get("_pingId").filter(|v| !v.is_null()),
    })
}

#[derive(Serialize)]
pub struct BrokerInfo<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub broker: &'a str,
    pub adapter_status: &'a str,
    pub user_id: &'a str,
}

#[derive(Serialize)]
pub struct SupportedBrokers<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub brokers: &'a [String],
    pub count: usize,
}

/// One entry of the `subscriptions` array in a subscribe ack.
#[derive(Serialize)]
#[serde(untagged)]
pub enum SubscribeItem {
    Ok {
        symbol: Value,
        exchange: Value,
        status: &'static str,
        mode: &'static str,
        depth: i64,
        broker: String,
    },
    Err {
        symbol: Value,
        exchange: Value,
        status: &'static str,
        message: String,
        broker: String,
    },
}

#[derive(Serialize)]
pub struct SubscribeAck<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub subscriptions: Vec<SubscribeItem>,
    pub message: &'static str,
    pub broker: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<&'a Value>,
}

/// One entry of `successful` / `failed` in an unsubscribe ack.
#[derive(Serialize)]
pub struct UnsubscribeItem {
    pub symbol: Value,
    pub exchange: Value,
    /// Canonical label, or `null` when the requested mode was invalid.
    pub mode: Option<&'static str>,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub broker: String,
}

#[derive(Serialize)]
pub struct UnsubscribeAck<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub message: &'static str,
    pub successful: Vec<UnsubscribeItem>,
    pub failed: Vec<UnsubscribeItem>,
    pub broker: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<&'a Value>,
}

#[derive(Serialize)]
pub struct SimpleAck<'a> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: &'static str,
    pub message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<&'a Value>,
}

#[derive(Serialize)]
struct LevelOut {
    price: f64,
    quantity: i64,
    orders: i64,
}

impl From<&DepthLevel> for LevelOut {
    fn from(l: &DepthLevel) -> Self {
        Self {
            price: l.price,
            quantity: l.quantity,
            orders: l.orders,
        }
    }
}

#[derive(Serialize)]
struct BookOut {
    buy: Vec<LevelOut>,
    sell: Vec<LevelOut>,
}

/// `market_data.data`, in the web's key order.
#[derive(Serialize)]
struct DataOut<'a> {
    symbol: &'a str,
    exchange: &'a str,
    mode: &'static str,
    ltp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ltt: Option<i64>,
    timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_buy_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_sell_quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    high: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    low: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    price_change_percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oi: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_interest: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<BookOut>,
    /// The broker's web names for the same values (WS-05).
    #[serde(flatten)]
    aliases: Aliases,
}

#[derive(Serialize)]
struct MarketDataOut<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    symbol: &'a str,
    exchange: &'a str,
    mode: u8,
    data: DataOut<'a>,
    broker: &'a str,
}

/// Render `update` for a subscriber at `mode` (which must not exceed
/// `update.mode`), with each depth side truncated to `depth` levels.
pub fn market_data(update: &MarketUpdate, mode: Mode, depth: u8, broker: &str) -> String {
    let key: &InstrumentKey = &update.key;
    let index = key.is_index();
    let mut d = DataOut {
        symbol: &key.symbol,
        exchange: &key.exchange,
        mode: mode.data_label(),
        ltp: update.ltp,
        ltt: update.ltt,
        timestamp: update.timestamp,
        volume: None,
        last_quantity: None,
        average_price: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        open: None,
        high: None,
        low: None,
        close: None,
        price_change: None,
        price_change_percent: None,
        oi: None,
        open_interest: None,
        depth: None,
        aliases: Aliases::default(),
    };
    if mode >= Mode::Quote {
        let q = update.quote.clone().unwrap_or_default();
        d.volume = Some(q.volume);
        if index {
            d.open = q.open;
            d.high = q.high;
            d.low = q.low;
            d.close = q.close;
            d.price_change = Some(q.price_change.unwrap_or(0.0));
            d.price_change_percent = Some(q.price_change_percent.unwrap_or(0.0));
        } else {
            d.last_quantity = Some(q.last_quantity);
            d.average_price = Some(q.average_price);
            d.total_buy_quantity = Some(q.total_buy_quantity);
            d.total_sell_quantity = Some(q.total_sell_quantity);
            d.open = Some(q.open.unwrap_or(0.0));
            d.high = Some(q.high.unwrap_or(0.0));
            d.low = Some(q.low.unwrap_or(0.0));
            d.close = Some(q.close.unwrap_or(0.0));
            d.price_change = q.price_change;
            d.price_change_percent = q.price_change_percent;
        }
        if let Some(oi) = q.oi {
            d.oi = Some(oi);
            d.open_interest = Some(oi);
        } else if mode == Mode::Depth && !index {
            d.oi = Some(0);
            d.open_interest = Some(0);
        }
    }
    d.aliases = aliases(
        broker,
        Canonical {
            last_quantity: d.last_quantity,
            average_price: d.average_price,
            price_change: d.price_change,
            price_change_percent: d.price_change_percent,
        },
    );
    if mode == Mode::Depth && !index {
        let n = depth as usize;
        let book = update.depth.as_ref();
        d.depth = Some(BookOut {
            buy: book
                .map(|b| b.buy.iter().take(n).map(LevelOut::from).collect())
                .unwrap_or_default(),
            sell: book
                .map(|b| b.sell.iter().take(n).map(LevelOut::from).collect())
                .unwrap_or_default(),
        });
    }
    to_json(&MarketDataOut {
        kind: "market_data",
        symbol: &key.symbol,
        exchange: &key.exchange,
        mode: mode.as_u8(),
        data: d,
        broker,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_renderings_match_the_web_texts() {
        assert_eq!(py_str(&Value::Null), "None");
        assert_eq!(py_str(&json!("bogus_action")), "bogus_action");
        assert_eq!(py_str(&json!(5)), "5");
        assert_eq!(py_str(&json!(2.0)), "2.0");
        assert_eq!(py_repr(&json!([1, "a"])), "[1, 'a']");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_type_name(&json!([1, 2, 3])), "list");
    }

    #[test]
    fn mode_normalisation_matches_mode_utils() {
        // Table from the web's test_mode_normalization.py.
        for (v, m) in [
            (json!(1), Mode::Ltp),
            (json!(2), Mode::Quote),
            (json!(3), Mode::Depth),
            (json!("LTP"), Mode::Ltp),
            (json!("ltp"), Mode::Ltp),
            (json!("Quote"), Mode::Quote),
            (json!("QUOTE"), Mode::Quote),
            (json!("DePtH"), Mode::Depth),
            (json!("  depth "), Mode::Depth),
        ] {
            assert_eq!(normalize_mode(&v), Ok(m), "{}", v);
        }
        assert_eq!(
            normalize_mode(&json!("2")).unwrap_err(),
            "Invalid mode '2'; expected 'LTP', 'Quote', or 'Depth' (case-insensitive)"
        );
        assert_eq!(
            normalize_mode(&json!(2.0)).unwrap_err(),
            "Mode must be int or str, got float"
        );
        assert_eq!(
            normalize_mode(&json!(9)).unwrap_err(),
            "Invalid mode 9; expected 1 (LTP), 2 (Quote), or 3 (Depth)"
        );
        assert_eq!(
            normalize_mode(&json!(true)).unwrap_err(),
            "Mode must be int or str, got bool (True)"
        );
        assert_eq!(
            normalize_mode(&Value::Null).unwrap_err(),
            "Mode must be int or str, got NoneType"
        );
        assert!(normalize_mode(&json!("")).is_err());
        assert!(normalize_mode(&json!(0)).is_err());
        assert!(normalize_mode(&json!(-1)).is_err());
    }

    #[test]
    fn error_frame_has_no_type_and_echoes_request_id() {
        let rid = json!("r1");
        let v: Value = serde_json::from_str(&error_frame("X", "m", Some(&rid))).unwrap();
        assert_eq!(
            v,
            json!({"status":"error","code":"X","message":"m","request_id":"r1"})
        );
        let v: Value = serde_json::from_str(&error_frame("X", "m", None)).unwrap();
        assert!(v.get("request_id").is_none() && v.get("type").is_none());
    }
}
