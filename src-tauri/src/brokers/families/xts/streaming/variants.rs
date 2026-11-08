//! RMoney's payload variants (audit FB-04): the shapes its web adapter
//! accepts beyond the XTS template's, put into that shape before the
//! message is routed and normalised (web `rmoney_adapter.py`
//! `_process_json_data`, `_normalize_market_data`). Only RMoney's feed
//! reads them (`XtsHooks::payload_variants`); the other members' web
//! adapters read the template's keys alone.
//!
//! * the instrument under `exchangeSegment` / `exchangeInstrumentID` /
//!   `exchangeInstrumentId`, the segment as a number, a numeric string or
//!   its name (`NSECM`);
//! * the code under `messageCode` or `xtsMessageCode`, a touchline when
//!   there is none;
//! * a book side in the root `Bids` / `Asks`, else `Touchline.Bids` /
//!   `Asks`, else `BidInfo` / `AskInfo` at the root or in `Touchline` (a
//!   single level as an object, or a list), and a book from either side
//!   alone, the other then empty.

use serde_json::{Map, Value};

/// XTS segment names (web `segment_name_to_code`).
const SEGMENTS: &[(&str, i64)] = &[
    ("NSECM", 1),
    ("NSEFO", 2),
    ("NSECD", 3),
    ("BSECM", 11),
    ("BSEFO", 12),
    ("MCXFO", 51),
];

/// `v` in the template's shape.
pub(crate) fn canonical(v: &Value) -> Value {
    let Value::Object(m) = v else {
        return v.clone();
    };
    let mut out = m.clone();
    if let Some(seg) = first(m, &["ExchangeSegment", "exchangeSegment"]).and_then(as_segment) {
        out.insert("ExchangeSegment".into(), Value::from(seg));
    }
    if let Some(id) = first(
        m,
        &[
            "ExchangeInstrumentID",
            "exchangeInstrumentID",
            "exchangeInstrumentId",
        ],
    ) {
        out.insert("ExchangeInstrumentID".into(), id.clone());
    }
    let code = first(m, &["MessageCode", "messageCode", "xtsMessageCode"])
        .and_then(as_code)
        .unwrap_or(1501);
    out.insert("MessageCode".into(), Value::from(code));
    let touchline = m.get("Touchline").and_then(Value::as_object);
    let bids = side(m, touchline, "Bids", "BidInfo");
    let asks = side(m, touchline, "Asks", "AskInfo");
    if bids.is_some() || asks.is_some() {
        out.insert(
            "Bids".into(),
            bids.unwrap_or_else(|| Value::Array(Vec::new())),
        );
        out.insert(
            "Asks".into(),
            asks.unwrap_or_else(|| Value::Array(Vec::new())),
        );
    }
    Value::Object(out)
}

fn first<'a>(m: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| m.get(*k).filter(|v| !v.is_null()))
}

/// A segment as its code.
fn as_segment(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => {
            let s = s.trim().to_ascii_uppercase();
            s.parse().ok().or_else(|| {
                SEGMENTS
                    .iter()
                    .find(|(name, _)| *name == s)
                    .map(|(_, code)| *code)
            })
        }
        _ => None,
    }
}

/// A message code, a number or a numeric string.
fn as_code(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One book side: the root's list when the message has one (an empty list
/// is a cleared side), else the first non-empty alternative, as a list.
fn side(
    m: &Map<String, Value>,
    touchline: Option<&Map<String, Value>>,
    list: &str,
    info: &str,
) -> Option<Value> {
    if let Some(root) = m.get(list).filter(|v| v.is_array()) {
        return Some(root.clone());
    }
    let alternatives = [
        touchline.and_then(|t| t.get(list)),
        m.get(info),
        touchline.and_then(|t| t.get(info)),
    ];
    alternatives.into_iter().flatten().find_map(|v| match v {
        Value::Array(a) if !a.is_empty() => Some(v.clone()),
        Value::Object(_) => Some(Value::Array(vec![v.clone()])),
        _ => None,
    })
}
