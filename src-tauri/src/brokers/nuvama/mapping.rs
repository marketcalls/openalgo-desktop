//! Exchanges, index names, products, order types, sides and statuses
//! between OpenAlgo and Nuvama (web `mapping/exchange.py`,
//! `mapping/transform_data.py`, `mapping/order_data.py`).
//!
//! Nuvama keys every instrument by the contract file's `exchangetoken`:
//! `<scripcode>_<EXCHANGE>` for a tradable instrument (`2885_NSE`) and a
//! bare negative number for an index (`-29` is Nifty 50). Orders send it as
//! `sym`, the quote and chart routes take it as `{symbol}`, the stream
//! subscribes to it, and the master stores it as `token` unchanged.

use crate::brokers::common::mapping::{PriceType, Product};
use serde_json::Value;

/// OpenAlgo exchange -> the `exc` Nuvama's order routes take.
pub fn nuvama_exchange(exchange: &str) -> &str {
    match exchange {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        other => other,
    }
}

/// A book row's `exc` -> OpenAlgo exchange (the same codes for tradables).
pub fn oa_exchange(exc: &str) -> String {
    exc.trim().to_ascii_uppercase()
}

pub fn is_index_exchange(exchange: &str) -> bool {
    matches!(exchange, "NSE_INDEX" | "BSE_INDEX")
}

/// Nuvama's NSE index names whose OpenAlgo symbol is not the name in
/// capitals without spaces (web `_NSE_INDEX_NAMES`).
const NSE_INDEX_NAMES: &[(&str, &str)] = &[
    ("Nifty 50", "NIFTY"),
    ("Nifty Bank", "BANKNIFTY"),
    ("Nifty Fin Service", "FINNIFTY"),
    ("NIFTY MID SELECT", "MIDCPNIFTY"),
    ("Nifty Next 50", "NIFTYNXT50"),
];

/// The BSE short codes (web `_BSE_INDEX_NAMES`).
const BSE_INDEX_NAMES: &[(&str, &str)] = &[
    ("SNSX50", "SENSEX50"),
    ("AUTO", "BSEAUTO"),
    ("BSE CG", "BSECAPITALGOODS"),
    ("BSE CD", "BSECONSUMERDURABLES"),
    ("BSEFMC", "BSEFASTMOVINGCONSUMERGOODS"),
    ("BSE IT", "BSEINFORMATIONTECHNOLOGY"),
    ("METAL", "BSEMETAL"),
    ("MIDCAP", "BSEMIDCAP"),
    ("SMLCAP", "BSESMALLCAP"),
    ("OILGAS", "BSEOIL&GAS"),
    ("POWER", "BSEPOWER"),
    ("REALTY", "BSEREALTY"),
    ("TELCOM", "BSETELECOM"),
    ("INDSTR", "BSEINDUSTRIALS"),
];

/// OpenAlgo symbol of a Nuvama index row (web `oa_index_symbol`).
pub fn oa_index_symbol(name: &str, nuvama_exchange: &str) -> String {
    let name = name.trim();
    let table = if nuvama_exchange == "NSE" {
        NSE_INDEX_NAMES
    } else {
        BSE_INDEX_NAMES
    };
    if let Some((_, to)) = table.iter().find(|(from, _)| *from == name) {
        return (*to).to_string();
    }
    name.split_whitespace()
        .collect::<String>()
        .to_ascii_uppercase()
}

/// web `oa_index_exchange`.
pub fn oa_index_exchange(nuvama_exchange: &str) -> &'static str {
    if nuvama_exchange == "BSE" {
        "BSE_INDEX"
    } else {
        "NSE_INDEX"
    }
}

/// The chart route's `aTyp` (web `chart_asset_type`).
pub fn chart_asset_type(
    exchange: &str,
    instrument_type: &str,
    index_underlying: bool,
) -> &'static str {
    if is_index_exchange(exchange) {
        return "INDEX";
    }
    if matches!(exchange, "NSE" | "BSE") {
        return "EQUITY";
    }
    let option = matches!(instrument_type, "CE" | "PE");
    match (exchange, option, index_underlying) {
        ("CDS", true, _) => "OPTCUR",
        ("CDS", false, _) => "FUTCUR",
        ("MCX" | "NCDEX", true, _) => "OPTFUT",
        ("MCX" | "NCDEX", false, _) => "FUTCOM",
        (_, true, true) => "OPTIDX",
        (_, true, false) => "OPTSTK",
        (_, false, true) => "FUTIDX",
        (_, false, false) => "FUTSTK",
    }
}

/// The chart route's `{exc}`: indices chart under `INDEX`.
pub fn chart_exchange(exchange: &str) -> &str {
    if is_index_exchange(exchange) {
        "INDEX"
    } else {
        exchange
    }
}

/// OpenAlgo price type -> `ordTyp` (web `_ORDER_TYPE`).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "STOP_LIMIT",
        PriceType::SlM => "STOP_MARKET",
    }
}

/// `ordTyp` / `oTyp` -> OpenAlgo price type, or the broker's text (web
/// `reverse_map_order_type`).
pub fn price_type_from_nuvama(t: &str) -> String {
    match t.trim().to_ascii_uppercase().as_str() {
        "MARKET" | "MKT" => "MARKET".into(),
        "LIMIT" | "LMT" => "LIMIT".into(),
        "STOP_LIMIT" | "SL" | "SL-L" => "SL".into(),
        "STOP_MARKET" | "SL-M" | "SL_M" => "SL-M".into(),
        _ => t.to_string(),
    }
}

/// OpenAlgo product -> `prdCode` (web `_PRODUCT`).
pub fn product(p: Product) -> &'static str {
    match p {
        Product::Cnc => "CNC",
        Product::Mis => "MIS",
        Product::Nrml => "NRML",
    }
}

/// `prdCode` / `pCode` -> OpenAlgo product, or the broker's value when it
/// has no OpenAlgo twin (MTF, CO, BO) (web `reverse_map_product_type`,
/// with the SDK's single-letter codes).
pub fn product_from_nuvama(p: &str) -> String {
    match p.trim().to_ascii_uppercase().as_str() {
        "CNC" | "C" => "CNC".into(),
        "MIS" | "I" => "MIS".into(),
        "NRML" | "M" => "NRML".into(),
        _ => p.to_string(),
    }
}

/// `trsTyp` / `tTyp` -> `BUY` / `SELL` (web `reverse_map_side`).
pub fn side(s: &str) -> String {
    match s.trim().to_ascii_uppercase().as_str() {
        "BUY" | "B" => "BUY".into(),
        "SELL" | "S" => "SELL".into(),
        other => other.to_string(),
    }
}

/// Nuvama's status text -> OpenAlgo's lowercase status (web
/// `normalize_status`). The in-flight states Nuvama streams ("validation
/// pending", "open pending", "cancel pending") are still open: a cancel
/// that is pending has cancelled nothing yet.
pub fn normalize_status(status: &str) -> String {
    let text = status.trim().to_ascii_lowercase();
    if text.is_empty() {
        return String::new();
    }
    let s = if text.ends_with("pending") && !text.contains("trigger") {
        "open"
    } else if text.contains("reject") {
        "rejected"
    } else if text.contains("cancel") {
        "cancelled"
    } else if text.contains("trigger") {
        "trigger pending"
    } else if text.contains("partial") {
        "open"
    } else if text.contains("complete")
        || text.contains("executed")
        || matches!(text.as_str(), "traded" | "filled")
    {
        "complete"
    } else {
        "open"
    };
    s.to_string()
}

/// A JSON value as text (`""` for null and missing).
pub fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// A number as Nuvama sends one, text with thousands separators included
/// (`"1,503.45"`); `None` for anything else (web `_f` reads those as 0).
pub fn number(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) => {
            let t = s.trim().replace(',', "");
            if t.is_empty() || t == "--" {
                return None;
            }
            t.parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

/// [`number`], 0 when absent (the web's `_f`).
pub fn num(v: Option<&Value>) -> f64 {
    number(v).unwrap_or(0.0)
}

/// A whole number (the web's `int(_f(value))`), 0 when absent.
pub fn int(v: Option<&Value>) -> i64 {
    number(v).map(|f| f.trunc() as i64).unwrap_or(0)
}
