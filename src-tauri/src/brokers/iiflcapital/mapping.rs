//! IIFL Capital <-> OpenAlgo vocabulary (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `mapping/margin_data.py`) and the JSON helpers
//! the REST modules share.

use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use crate::brokers::common::mpp;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{Holding, Order, Position, Trade};
use crate::error::{AppError, Result};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// JSON helpers (web `_to_float`, `_first_present`, `_extract_rows`, `_ok`)
// ---------------------------------------------------------------------------

/// Number, numeric string or anything else (0) -> f64. `"-"` and `""` are 0.
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().replace(',', "").parse::<f64>().unwrap_or(0.0),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

pub fn int(v: Option<&Value>) -> i64 {
    let f = num(v);
    if f.is_finite() {
        f.trunc() as i64
    } else {
        0
    }
}

/// String form of a scalar (`null` and missing are empty).
pub fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn present(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// First key whose value is present (not null, not empty string).
pub fn first<'a>(row: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| row.get(*k).filter(|v| present(v)))
}

/// Fields that mark a dict as a real book row rather than a status wrapper
/// (web `order_data._ROW_FIELDS`).
const ROW_FIELDS: &[&str] = &[
    "brokerOrderId",
    "exchangeOrderId",
    "orderId",
    "instrumentId",
    "token",
    "exchangeInstrumentID",
    "tradingSymbol",
    "symbol",
    "formattedInstrumentName",
    "nseTradingSymbol",
    "bseTradingSymbol",
    "netQuantity",
    "filledQuantity",
    "pendingQuantity",
    "cancelledQuantity",
    "dpQuantity",
    "totalQuantity",
    "transactionType",
    "tradedPrice",
    "averageTradedPrice",
];

pub fn looks_like_row(v: &Value) -> bool {
    v.is_object() && ROW_FIELDS.iter().any(|f| v.get(*f).is_some_and(present))
}

/// Book rows from any of the envelope shapes IIFL uses
/// (web `order_data._extract_rows`).
pub fn book_rows(payload: &Value) -> Vec<Value> {
    let keep = |list: &Vec<Value>| -> Vec<Value> {
        list.iter().filter(|v| looks_like_row(v)).cloned().collect()
    };
    match payload {
        Value::Array(list) => keep(list),
        Value::Object(map) => {
            match map.get("result") {
                Some(Value::Array(list)) => return keep(list),
                Some(r @ Value::Object(inner)) => {
                    for k in [
                        "orders",
                        "trades",
                        "positions",
                        "holdings",
                        "data",
                        "positionList",
                    ] {
                        if let Some(Value::Array(list)) = inner.get(k) {
                            return keep(list);
                        }
                    }
                    return if looks_like_row(r) {
                        vec![r.clone()]
                    } else {
                        Vec::new()
                    };
                }
                _ => {}
            }
            for k in ["data", "orders", "trades", "positions", "holdings"] {
                if let Some(Value::Array(list)) = map.get(k) {
                    return keep(list);
                }
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

fn status_ok(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str)
        .map(|s| matches!(s.to_ascii_lowercase().as_str(), "success" | "ok"))
        .unwrap_or(false)
}

/// web `order_api._ok`: top-level or nested `status` in `{success, ok}`.
pub fn is_ok(payload: &Value) -> bool {
    if status_ok(payload.get("status")) {
        return true;
    }
    match payload.get("result") {
        Some(r @ Value::Object(_)) => status_ok(r.get("status")),
        Some(Value::Array(list)) => list.first().is_some_and(|r| status_ok(r.get("status"))),
        _ => false,
    }
}

/// web `order_api._first_result`.
pub fn first_result(payload: &Value) -> Value {
    match payload.get("result") {
        Some(Value::Array(list)) => list
            .first()
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null),
        Some(r @ Value::Object(_)) => r.clone(),
        _ => Value::Null,
    }
}

/// web `order_api._extract_message`: `message`/`error`/`description` at
/// the top level, then in the first result.
pub fn message_of(payload: &Value) -> Option<String> {
    let pick = |v: &Value| {
        ["message", "error", "description"]
            .iter()
            .find_map(|k| v.get(*k).filter(|x| present(x)))
            .map(|x| match x {
                Value::String(s) => s.trim().to_string(),
                other => other.to_string(),
            })
    };
    pick(payload).or_else(|| match payload.get("result") {
        Some(Value::Array(list)) => list.first().and_then(pick),
        Some(r @ Value::Object(_)) => pick(r),
        _ => None,
    })
}

/// web `rate_limiter.is_rate_limited`.
pub fn is_rate_limited(status: u16, message: &str) -> bool {
    if status == 429 {
        return true;
    }
    let t = message.to_ascii_lowercase();
    ["rate limit", "too many request", "try after some time"]
        .iter()
        .any(|h| t.contains(h))
}

// ---------------------------------------------------------------------------
// Enum maps (web `transform_data.py`, `order_data.py`)
// ---------------------------------------------------------------------------

/// OpenAlgo exchange -> IIFL segment (web `map_exchange`).
pub fn to_segment(exchange: &str) -> String {
    match exchange.to_ascii_uppercase().as_str() {
        "NSE" => "NSEEQ".into(),
        "BSE" => "BSEEQ".into(),
        "NFO" => "NSEFO".into(),
        "BFO" => "BSEFO".into(),
        "CDS" => "NSECURR".into(),
        "BCD" => "BSECURR".into(),
        "MCX" => "MCXCOMM".into(),
        "NCDEX" => "NCDEXCOMM".into(),
        other => other.to_string(),
    }
}

/// Segment for market-data calls (web `data._normalize_exchange`): adds the
/// index exchanges.
pub fn data_segment(exchange: &str) -> String {
    match exchange.to_ascii_uppercase().as_str() {
        "NSE_INDEX" => "NSEEQ".into(),
        "BSE_INDEX" => "BSEEQ".into(),
        "MCX_INDEX" => "MCXCOMM".into(),
        other => to_segment(other),
    }
}

/// IIFL segment -> OpenAlgo exchange (web `order_data._map_exchange`).
pub fn from_segment(segment: &str) -> String {
    match segment.to_ascii_uppercase().as_str() {
        "NSEEQ" => "NSE".into(),
        "BSEEQ" => "BSE".into(),
        "NSEFO" => "NFO".into(),
        "BSEFO" => "BFO".into(),
        "NSECURR" => "CDS".into(),
        "BSECURR" => "BCD".into(),
        "MCXCOMM" | "NSECOMM" | "NCDEXCOMM" => "MCX".into(),
        other => other.to_string(),
    }
}

pub fn product_to_broker(p: Product) -> &'static str {
    match p {
        Product::Mis => "INTRADAY",
        Product::Cnc => "DELIVERY",
        Product::Nrml => "NORMAL",
    }
}

/// web `reverse_map_product_type` (unknown -> MIS).
pub fn product_from_broker(p: &str) -> &'static str {
    match p.to_ascii_uppercase().as_str() {
        "INTRADAY" => "MIS",
        "DELIVERY" | "BNPL" => "CNC",
        "NORMAL" => "NRML",
        _ => "MIS",
    }
}

pub fn order_type_to_broker(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SLM",
    }
}

/// web `reverse_map_order_type` (unknown -> MARKET).
pub fn order_type_from_broker(t: &str) -> &'static str {
    match t.to_ascii_uppercase().as_str() {
        "LIMIT" => "LIMIT",
        "SL" => "SL",
        "SLM" => "SL-M",
        _ => "MARKET",
    }
}

pub fn validity_to_broker(v: Validity) -> &'static str {
    match v {
        Validity::Ioc => "IOC",
        _ => "DAY",
    }
}

/// web `order_data._map_status` (unknown -> open).
pub fn map_status(status: &str) -> &'static str {
    match status.trim().to_ascii_uppercase().as_str() {
        "COMPLETE" | "COMPLETED" | "FILLED" | "SUCCESS" | "EXECUTED" => "complete",
        "REJECTED" | "FAIL" | "FAILED" => "rejected",
        "CANCELLED" | "CANCELED" => "cancelled",
        "TRIGGER_PENDING" | "TRIGGER PENDING" => "trigger pending",
        _ => "open",
    }
}

/// web `order_api._OPEN_STATUSES` (cancel-all filter).
pub fn is_open_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "OPEN"
            | "PENDING"
            | "TRIGGER_PENDING"
            | "PARTIALLY_FILLED"
            | "NEW"
            | "PUT ORDER REQ RECEIVED"
    )
}

/// `B`/`S`/`BUY`/`SELL` -> `BUY`/`SELL` (trade updates send single letters).
pub fn action(v: &str) -> String {
    match v.trim().to_ascii_uppercase().as_str() {
        "B" | "BUY" => "BUY".into(),
        "S" | "SELL" => "SELL".into(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// SL-M protection (web `transform_data._slm_protected_price`)
// ---------------------------------------------------------------------------

fn tick_decimals(tick: f64) -> i32 {
    // Decimal places implied by the tick (0.05 -> 2, 0.0025 -> 4).
    let s = format!("{}", tick);
    s.split_once('.')
        .map(|(_, f)| f.trim_end_matches('0').len() as i32)
        .unwrap_or(0)
}

fn snap(value: f64, tick: f64, up: bool) -> f64 {
    let ratio = mpp::py_round(value / tick, 6);
    let k = if up { ratio.ceil() } else { ratio.floor() };
    mpp::py_round(k * tick, tick_decimals(tick))
}

/// Protective limit for an SL-M order sent as SL: the MPP slab beyond the
/// trigger in the fill direction, at least one tick away, tick-snapped
/// outward (SELL floors, BUY ceils). Fails closed without a tick size.
pub fn slm_protected_price(
    symbol: &str,
    action: Action,
    trigger: f64,
    tick_size: f64,
) -> Result<f64> {
    if !tick_size.is_finite() || tick_size <= 0.0 {
        return Err(AppError::Validation(format!(
            "The tick size of {} is not known, so a protected SL-M price cannot be set. Download the master contract again and retry.",
            symbol
        )));
    }
    let pct = mpp::mpp_percentage(trigger, mpp::instrument_type_from_symbol(symbol)) / 100.0;
    match action {
        Action::Sell => {
            let raw = (trigger * (1.0 - pct)).min(trigger - tick_size);
            let limit = snap(raw, tick_size, false);
            if limit <= 0.0 {
                return Err(AppError::Validation(format!(
                    "The SL-M trigger {} for {} is too low to set a protected sell price.",
                    trigger, symbol
                )));
            }
            Ok(limit)
        }
        Action::Buy => {
            let raw = (trigger * (1.0 + pct)).max(trigger + tick_size);
            Ok(snap(raw, tick_size, true))
        }
    }
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

/// Inputs shared by place and modify payload builders.
pub struct OrderFields<'a> {
    pub symbol: &'a str,
    pub action: Action,
    pub pricetype: PriceType,
    pub price: f64,
    pub trigger_price: f64,
    pub tick_size: f64,
}

/// Fill `orderType`, `price` and `slTriggerPrice`; SL-M goes out as SL with
/// a protected limit (web `transform_data`).
fn put_price_fields(m: &mut Map<String, Value>, f: &OrderFields<'_>) -> Result<()> {
    let ot = order_type_to_broker(f.pricetype);
    m.insert("orderType".into(), json!(ot));
    if matches!(ot, "LIMIT" | "SL") {
        m.insert("price".into(), json!(f.price));
    }
    if matches!(ot, "SL" | "SLM") {
        m.insert("slTriggerPrice".into(), json!(f.trigger_price));
    }
    if ot == "SLM" {
        let limit = slm_protected_price(f.symbol, f.action, f.trigger_price, f.tick_size)?;
        tracing::info!(
            "IIFL SL-M sent as SL: trigger {}, protected limit {}",
            f.trigger_price,
            limit
        );
        m.insert("orderType".into(), json!("SL"));
        m.insert("price".into(), json!(limit));
    }
    Ok(())
}

/// Place-order body (one element of the `POST /orders` list).
#[allow(clippy::too_many_arguments)]
pub fn order_payload(
    token: &str,
    exchange: &str,
    fields: &OrderFields<'_>,
    quantity: i64,
    product: Product,
    validity: Validity,
    disclosed_quantity: i64,
    tag: Option<&str>,
) -> Result<Value> {
    let mut m = Map::new();
    m.insert("instrumentId".into(), json!(token));
    m.insert("exchange".into(), json!(to_segment(exchange)));
    m.insert("transactionType".into(), json!(fields.action.as_str()));
    m.insert("quantity".into(), json!(quantity.to_string()));
    m.insert("orderComplexity".into(), json!("REGULAR"));
    m.insert("product".into(), json!(product_to_broker(product)));
    m.insert("validity".into(), json!(validity_to_broker(validity)));
    m.insert("apiOrderSource".into(), json!("openalgo"));
    put_price_fields(&mut m, fields)?;
    if disclosed_quantity > 0 {
        m.insert(
            "disclosedQuantity".into(),
            json!(disclosed_quantity.to_string()),
        );
    }
    if let Some(t) = tag.filter(|t| !t.is_empty()) {
        m.insert(
            "orderTag".into(),
            json!(t.chars().take(50).collect::<String>()),
        );
    }
    Ok(Value::Object(m))
}

/// Modify body (web `transform_modify_order_data`): quantity, order type
/// and its prices, disclosed quantity when positive.
pub fn modify_payload(
    fields: &OrderFields<'_>,
    quantity: i64,
    disclosed_quantity: i64,
) -> Result<Value> {
    let mut m = Map::new();
    m.insert("quantity".into(), json!(quantity.to_string()));
    put_price_fields(&mut m, fields)?;
    if disclosed_quantity > 0 {
        m.insert(
            "disclosedQuantity".into(),
            json!(disclosed_quantity.to_string()),
        );
    }
    Ok(Value::Object(m))
}

/// An order id safe to put in a URL path (web `_safe_order_id`).
pub fn safe_order_id(id: &str) -> Result<&str> {
    let id = id.trim();
    if !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        Ok(id)
    } else {
        Err(AppError::Validation(format!("Invalid orderid: '{}'", id)))
    }
}

// ---------------------------------------------------------------------------
// Books (always OpenAlgo symbols)
// ---------------------------------------------------------------------------

/// OpenAlgo symbol of a row: the master row for its instrument id on the
/// OpenAlgo exchange, else the broker symbol mapped back, else the raw text
/// (web `_resolve_symbol`).
pub fn resolve_symbol(row: &Value, exchange: &str, symbols: &SymbolResolver) -> String {
    let raw = text(first(
        row,
        &["tradingSymbol", "symbol", "formattedInstrumentName"],
    ));
    let token = text(first(
        row,
        &["instrumentId", "token", "exchangeInstrumentID"],
    ));
    if !token.is_empty() && !exchange.is_empty() {
        if let Some(s) = symbols.by_token(exchange, &token) {
            return s.symbol;
        }
    }
    if raw.is_empty() || exchange.is_empty() {
        return raw;
    }
    symbols.oa_symbol_or_raw(&raw, exchange)
}

fn row_order_id(row: &Value) -> String {
    text(first(row, &["brokerOrderId", "exchangeOrderId", "orderId"]))
}

fn order_quantity(row: &Value) -> i64 {
    match first(row, &["quantity", "orderQuantity"]) {
        Some(q) => int(Some(q)),
        None => {
            (num(row.get("filledQuantity"))
                + num(row.get("pendingQuantity"))
                + num(row.get("cancelledQuantity"))) as i64
        }
    }
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

pub fn round2(v: f64) -> f64 {
    mpp::py_round(v, 2)
}

pub fn order_row(row: &Value, symbols: &SymbolResolver) -> Order {
    let exchange = from_segment(&text(row.get("exchange")));
    let status = map_status(&text(row.get("orderStatus"))).to_string();
    let filled = int(row.get("filledQuantity"));
    let reason = text(row.get("rejectionReason"));
    let validity = text(row.get("validity"));
    Order {
        order_id: row_order_id(row),
        exchange_order_id: Some(text(row.get("exchangeOrderId"))).filter(|s| !s.is_empty()),
        symbol: resolve_symbol(row, &exchange, symbols),
        exchange,
        side: text(row.get("transactionType")).to_ascii_uppercase(),
        quantity: clamp_i32(order_quantity(row)),
        filled_quantity: clamp_i32(filled),
        pending_quantity: clamp_i32(int(row.get("pendingQuantity"))),
        price: num(row.get("price")),
        trigger_price: num(first(row, &["slTriggerPrice", "triggerPrice"])),
        average_price: num(row.get("averageTradedPrice")),
        order_type: order_type_from_broker(&text(row.get("orderType"))).to_string(),
        product: product_from_broker(&text(row.get("product"))).to_string(),
        status,
        validity: if validity.is_empty() {
            "DAY".into()
        } else {
            validity.to_ascii_uppercase()
        },
        order_timestamp: text(first(
            row,
            &[
                "exchangeTimestamp",
                "exchangeUpdateTime",
                "brokerUpdateTime",
            ],
        )),
        exchange_timestamp: Some(text(row.get("exchangeTimestamp"))).filter(|s| !s.is_empty()),
        rejection_reason: Some(reason).filter(|s| !s.is_empty()),
    }
}

pub fn trade_row(row: &Value, symbols: &SymbolResolver) -> Trade {
    let exchange = from_segment(&text(row.get("exchange")));
    let qty = int(first(row, &["filledQuantity", "quantity", "filledQty"]));
    let avg = num(first(row, &["tradedPrice", "averageTradedPrice", "price"]));
    Trade {
        order_id: row_order_id(row),
        trade_id: text(first(row, &["exchangeTradeId", "tradeId"])),
        symbol: resolve_symbol(row, &exchange, symbols),
        exchange,
        product: product_from_broker(&text(row.get("product"))).to_string(),
        side: action(&text(row.get("transactionType"))),
        quantity: clamp_i32(qty),
        average_price: avg,
        trade_value: qty as f64 * avg,
        timestamp: text(first(
            row,
            &[
                "fillTimestamp",
                "exchangeTimestamp",
                "exchangeUpdateTime",
                "brokerUpdateTime",
            ],
        )),
    }
}

pub fn position_row(row: &Value, symbols: &SymbolResolver) -> Position {
    let exchange = from_segment(&text(row.get("exchange")));
    let quantity = int(first(row, &["netQuantity", "quantity"]));
    let avg = num(first(row, &["netAveragePrice", "averagePrice"]));
    let ltp = num(first(row, &["ltp", "lastPrice", "previousDayClose"]));
    let realized = num(row.get("realizedPnl"));
    // Unrealized only when an LTP is really there (a missing LTP would
    // fabricate a loss of the whole position).
    let unrealized = if quantity != 0 && ltp > 0.0 {
        (ltp - avg) * quantity as f64
    } else {
        0.0
    };
    Position {
        symbol: resolve_symbol(row, &exchange, symbols),
        exchange,
        product: product_from_broker(&{
            let p = text(row.get("product"));
            if p.is_empty() {
                "NORMAL".to_string()
            } else {
                p
            }
        })
        .to_string(),
        quantity: clamp_i32(quantity),
        overnight_quantity: clamp_i32(int(row.get("carryForwardQuantity"))),
        average_price: round2(avg),
        ltp: round2(ltp),
        pnl: round2(realized + unrealized),
        realized_pnl: round2(realized),
        unrealized_pnl: round2(unrealized),
        buy_quantity: clamp_i32(int(first(row, &["buyQuantity", "dayBuyQuantity"]))),
        buy_value: num(first(row, &["buyValue", "dayBuyValue"])),
        sell_quantity: clamp_i32(int(first(row, &["sellQuantity", "daySellQuantity"]))),
        sell_value: num(first(row, &["sellValue", "daySellValue"])),
    }
}

/// Settled DP quantity first, then the totals (web `_resolve_holding_quantity`).
fn holding_quantity(row: &Value) -> f64 {
    for k in ["dpQuantity", "dpQty", "availableQuantity"] {
        let q = num(row.get(k));
        if q > 0.0 {
            return q;
        }
    }
    for k in ["totalQuantity", "totalQty", "quantity", "holdingQuantity"] {
        let q = num(row.get(k));
        if q > 0.0 {
            return q;
        }
    }
    num(row.get("t1Quantity"))
}

/// Holding row, or `None` for IIFL's empty placeholder rows (quantity 0).
pub fn holding_row(row: &Value, symbols: &SymbolResolver) -> Option<Holding> {
    let quantity = holding_quantity(row) as i64;
    if quantity <= 0 {
        return None;
    }
    let nse = text(row.get("nseTradingSymbol"));
    let bse = text(row.get("bseTradingSymbol"));
    let (raw, exchange) = if !nse.is_empty() {
        (nse, "NSE")
    } else if !bse.is_empty() {
        (bse, "BSE")
    } else {
        (
            text(first(
                row,
                &["tradingSymbol", "formattedInstrumentName", "symbol"],
            )),
            "NSE",
        )
    };
    let token_key = if exchange == "NSE" {
        "nseInstrumentId"
    } else {
        "bseInstrumentId"
    };
    let token = text(row.get(token_key));
    let symbol = symbols
        .by_token(exchange, &token)
        .map(|s| s.symbol)
        .unwrap_or_else(|| symbols.oa_symbol_or_raw(&raw, exchange));
    let avg = num(first(row, &["averageTradedPrice", "averagePrice"]));
    let ltp = match first(row, &["ltp", "previousDayClose"]) {
        Some(v) => num(Some(v)),
        None => avg,
    };
    let q = quantity as f64;
    let pnl = q * (ltp - avg);
    let pct = if avg > 0.0 {
        pnl / (q * avg) * 100.0
    } else {
        0.0
    };
    Some(Holding {
        symbol,
        exchange: exchange.to_string(),
        product: product_from_broker(&{
            let p = text(row.get("product"));
            if p.is_empty() {
                "DELIVERY".to_string()
            } else {
                p
            }
        })
        .to_string(),
        isin: Some(text(row.get("isin"))).filter(|s| !s.is_empty()),
        quantity: clamp_i32(quantity),
        t1_quantity: clamp_i32(int(row.get("t1Quantity"))),
        average_price: round2(avg),
        ltp: round2(ltp),
        close_price: num(row.get("previousDayClose")),
        pnl: round2(pnl),
        pnl_percentage: round2(pct),
        current_value: round2(q * ltp),
    })
}

/// The OpenAlgo exchange of an order book / position row, parsed.
pub fn row_exchange(row: &Value) -> Option<Exchange> {
    from_segment(&text(row.get("exchange"))).parse().ok()
}
