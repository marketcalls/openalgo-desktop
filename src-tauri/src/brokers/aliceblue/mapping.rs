//! AliceBlue <-> OpenAlgo translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `api/error_codes.py`,
//! `streaming/aliceblue_order_adapter.py`).
//!
//! The web first renames V2 rows to the legacy Noren keys
//! (`normalize_order` and friends) and then transforms them; here both
//! steps are one function per book, reading the V2 field names directly.

use crate::brokers::common::mapping::{PriceType, Product};
use crate::brokers::common::mpp::py_round;
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use chrono::{FixedOffset, NaiveDateTime, NaiveTime, Offset, Utc};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Value helpers (Python `dict.get(key, default)` over loosely typed JSON)
// ---------------------------------------------------------------------------

/// `d.get(key)` as text; missing and `null` are empty.
pub fn s(v: &Value, key: &str) -> String {
    super::text(v.get(key))
}

/// `d.get(key, d.get(fallback))`: the first key wins when it is present.
fn first<'a>(v: &'a Value, key: &str, fallback: &str) -> Option<&'a Value> {
    match v.get(key) {
        Some(Value::Null) | None => v.get(fallback),
        some => some,
    }
}

/// Number or numeric string -> f64 (0 otherwise).
pub fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().replace(',', "").parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `int(float(x))`.
pub fn int(v: Option<&Value>) -> i64 {
    num(v) as i64
}

fn i32_of(v: Option<&Value>) -> i32 {
    int(v).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Python `str(float)` for a price: `100.0`, `100.5`.
pub fn py_float_str(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// `str(int(float(token)))`; unparsable tokens pass through.
pub fn normalize_token(token: &str) -> String {
    let t = token.trim();
    match t.parse::<f64>() {
        Ok(f) if f.is_finite() => format!("{}", f.trunc() as i64),
        _ => t.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Enums (web `transform_data.py`)
// ---------------------------------------------------------------------------

/// OpenAlgo product -> AliceBlue V2 product.
pub fn map_product(p: Product) -> &'static str {
    match p {
        Product::Cnc => "LONGTERM",
        Product::Nrml => "NRML",
        Product::Mis => "INTRADAY",
    }
}

/// AliceBlue product -> OpenAlgo product (unknown -> MIS, like the web).
pub fn reverse_product(p: &str) -> &'static str {
    match p {
        "LONGTERM" | "MTF" | "CNC" | "DELIVERY" => "CNC",
        "NRML" => "NRML",
        _ => "MIS",
    }
}

/// OpenAlgo price type -> AliceBlue V2 order type.
pub fn map_order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL",
        PriceType::SlM => "SLM",
    }
}

/// AliceBlue V2 order type -> OpenAlgo price type (web goes through the
/// legacy `MKT/L/SL/SL-M` codes; unknown is `UNKNOWN`).
pub fn reverse_order_type(t: &str) -> &'static str {
    match t {
        "MARKET" | "MKT" => "MARKET",
        "LIMIT" | "L" => "LIMIT",
        "SL" => "SL",
        "SLM" | "SL-M" => "SL-M",
        _ => "UNKNOWN",
    }
}

// ---------------------------------------------------------------------------
// Request payloads
// ---------------------------------------------------------------------------

/// One place-order item (web `transform_data`); the request body is a
/// one-element list of these.
pub fn place_payload(o: &ResolvedOrder) -> Value {
    json!({
        "exchange": o.exchange.as_str(),
        "instrumentId": normalize_token(o.token()),
        "transactionType": o.action.as_str(),
        "quantity": o.quantity,
        "product": map_product(o.product),
        "orderComplexity": "REGULAR",
        "orderType": map_order_type(o.pricetype),
        "validity": "DAY",
        "price": py_float_str(o.price),
        "slLegPrice": "",
        "targetLegPrice": "",
        "slTriggerPrice": py_float_str(o.trigger_price),
        "disclosedQuantity": o.disclosed_quantity.to_string(),
        "marketProtectionPercent": "",
        "deviceId": "",
        "trailingSlAmount": "",
        "apiOrderSource": "",
        "algoId": "",
        "orderTag": "openalgo",
    })
}

/// Modify body (web `transform_modify_order_data`).
pub fn modify_payload(m: &ResolvedModify) -> Value {
    json!({
        "brokerOrderId": m.order_id,
        "quantity": m.quantity,
        "orderType": map_order_type(m.pricetype),
        "slTriggerPrice": py_float_str(m.trigger_price),
        "price": py_float_str(m.price),
        "slLegPrice": "",
        "trailingSlAmount": "",
        "targetLegPrice": "",
        "validity": "DAY",
        "disclosedQuantity": m.disclosed_quantity.to_string(),
        "marketProtection": "",
        "deviceId": "",
    })
}

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// OpenAlgo symbol for a broker row: the web's `get_oa_symbol(brsymbol,
/// exchange)` on each candidate trading symbol, then the instrument id,
/// then the broker symbol unchanged.
pub fn oa_symbol(
    symbols: &SymbolResolver,
    exchange: &str,
    candidates: &[&str],
    token: &str,
) -> String {
    let g = symbols.snapshot();
    for c in candidates.iter().filter(|c| !c.is_empty()) {
        if let Some(r) = g.by_brsymbol(exchange, c) {
            return r.symbol.clone();
        }
    }
    if !token.is_empty() {
        if let Some(r) = g.by_token(exchange, &normalize_token(token)) {
            return r.symbol.clone();
        }
    }
    candidates
        .iter()
        .find(|c| !c.is_empty())
        .map(|c| c.to_string())
        .unwrap_or_default()
}

/// web `normalize_order`'s `Trsym`: `formattedInstrumentName`, falling back
/// to `tradingSymbol` only when the key is absent.
fn order_symbol_candidates(r: &Value) -> Vec<String> {
    let mut v = Vec::with_capacity(2);
    if let Some(f) = first(r, "formattedInstrumentName", "tradingSymbol") {
        v.push(super::text(Some(f)));
    }
    v.push(s(r, "tradingSymbol"));
    v
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// Order-book row (web `normalize_order` + `transform_order_data`).
pub fn order_from(r: &Value, symbols: &SymbolResolver) -> Order {
    let exchange = s(r, "exchange");
    let cands = order_symbol_candidates(r);
    let cand_refs: Vec<&str> = cands.iter().map(String::as_str).collect();
    let side = match s(r, "transactionType").as_str() {
        "BUY" | "B" => "BUY",
        "SELL" | "S" => "SELL",
        _ => "UNKNOWN",
    };
    let quantity = i32_of(r.get("quantity"));
    let filled = i32_of(r.get("filledQuantity"));
    let reason = s(r, "rejectionReason");
    let exch_id = s(r, "exchangeOrderId");
    Order {
        order_id: s(r, "brokerOrderId"),
        exchange_order_id: (!exch_id.is_empty()).then_some(exch_id),
        symbol: oa_symbol(symbols, &exchange, &cand_refs, &s(r, "instrumentId")),
        exchange,
        side: side.to_string(),
        quantity,
        filled_quantity: filled,
        pending_quantity: i32_of(r.get("pendingQuantity")),
        price: num(r.get("price")),
        trigger_price: num(r.get("slTriggerPrice")),
        average_price: num(r.get("averageTradedPrice")),
        order_type: reverse_order_type(&s(r, "orderType")).to_string(),
        product: reverse_product(&s(r, "product")).to_string(),
        status: s(r, "orderStatus").to_lowercase(),
        validity: {
            let v = s(r, "validity");
            if v.is_empty() {
                "DAY".to_string()
            } else {
                v
            }
        },
        order_timestamp: s(r, "orderTime"),
        exchange_timestamp: None,
        rejection_reason: (!reason.is_empty()).then_some(reason),
    }
}

/// India Standard Time (+05:30).
pub fn ist() -> FixedOffset {
    FixedOffset::east_opt(19_800).unwrap_or(Utc.fix())
}

/// web `transform_tradebook_data` fill time: `DD-MM-YYYY HH:MM:SS` ->
/// `YYYY-MM-DD HH:MM:SS`; `HH:MM:SS` gets today's (IST) date; anything else
/// passes through.
pub fn fill_time(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(raw, "%d-%m-%Y %H:%M:%S") {
        return dt.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    if NaiveTime::parse_from_str(raw, "%H:%M:%S").is_ok() {
        let today = Utc::now().with_timezone(&ist()).date_naive();
        return format!("{} {}", today.format("%Y-%m-%d"), raw);
    }
    raw.to_string()
}

/// Trade-book row (web `normalize_trade` + `transform_tradebook_data`).
pub fn trade_from(r: &Value, symbols: &SymbolResolver) -> Trade {
    let exchange = s(r, "exchange");
    let cands = order_symbol_candidates(r);
    let cand_refs: Vec<&str> = cands.iter().map(String::as_str).collect();
    let quantity = i32_of(r.get("filledQuantity"));
    let avg = num(r.get("tradedPrice"));
    let side = match s(r, "transactionType").as_str() {
        "BUY" | "B" => "BUY".to_string(),
        "SELL" | "S" => "SELL".to_string(),
        other => other.to_string(),
    };
    let fill = super::text(first(r, "fillTimestamp", "orderTime"));
    Trade {
        order_id: s(r, "brokerOrderId"),
        trade_id: s(r, "exchangeTradeId"),
        symbol: oa_symbol(symbols, &exchange, &cand_refs, &s(r, "instrumentId")),
        exchange,
        product: reverse_product(&s(r, "product")).to_string(),
        side,
        quantity,
        average_price: avg,
        trade_value: f64::from(quantity) * avg,
        timestamp: fill_time(&fill),
    }
}

/// Net position row (web `normalize_position` + `transform_positions_data`).
pub fn position_from(r: &Value, symbols: &SymbolResolver) -> Position {
    let exchange = s(r, "exchange");
    let tsym = super::text(first(r, "tradingSymbol", "formattedInstrumentName"));
    let net = num(r.get("netQuantity"));
    let buy_avg = num(first(r, "dayBuyPrice", "netAveragePrice"));
    let sell_avg = num(r.get("daySellPrice"));
    let avg = if net > 0.0 {
        buy_avg
    } else if net < 0.0 {
        sell_avg
    } else {
        0.0
    };
    let ltp = num(first(r, "ltp", "previousDayClose"));
    let mut pnl = 0.0;
    if net != 0.0 && avg > 0.0 && ltp > 0.0 {
        pnl = if net > 0.0 {
            (ltp - avg) * net
        } else {
            (avg - ltp) * net.abs()
        };
    }
    let unrealised = num(first(r, "unrealisedPnl", "unrealisedProfitLoss"));
    if unrealised != 0.0 {
        pnl = unrealised;
    }
    let realised = num(first(r, "realizedPnl", "realisedPnl"));
    let buy_qty = i32_of(first(r, "buyQuantity", "dayBuyQuantity"));
    let sell_qty = i32_of(first(r, "sellQuantity", "daySellQuantity"));
    Position {
        symbol: oa_symbol(symbols, &exchange, &[&tsym], &s(r, "instrumentId")),
        exchange,
        product: reverse_product(&s(r, "product")).to_string(),
        quantity: net as i32,
        overnight_quantity: 0,
        average_price: py_round(avg, 2),
        ltp,
        pnl: py_round(pnl, 2),
        realized_pnl: py_round(realised, 2),
        unrealized_pnl: py_round(unrealised, 2),
        buy_quantity: buy_qty,
        buy_value: py_round(buy_avg * f64::from(buy_qty), 2),
        sell_quantity: sell_qty,
        sell_value: py_round(sell_avg * f64::from(sell_qty), 2),
    }
}

/// Holding row (web `normalize_holding` + `transform_holdings_data`);
/// `None` for a row without a symbol (the web skips it).
pub fn holding_from(r: &Value, symbols: &SymbolResolver) -> Option<Holding> {
    let nse = s(r, "nseTradingSymbol");
    let bse = s(r, "bseTradingSymbol");
    let exchange = if !nse.is_empty() {
        "NSE"
    } else if !bse.is_empty() {
        "BSE"
    } else {
        "NSE"
    };
    let symbol = if exchange == "NSE" { nse } else { bse };
    if symbol.is_empty() {
        return None;
    }
    let ltp = num(r.get("ltp"));
    let price = num(first(r, "averageTradedPrice", "investedPrice"));
    let hold = int(first(r, "dpQuantity", "totalQuantity"));
    let t1 = int(r.get("t1Quantity"));
    let quantity = if hold > 0 { hold } else { t1 };
    let pnl = if quantity != 0 {
        py_round((ltp - price) * quantity as f64, 2)
    } else {
        0.0
    };
    let pnl_pct = if price != 0.0 {
        py_round((ltp - price) / price * 100.0, 2)
    } else {
        0.0
    };
    let token = super::text(first(r, "nseInstrumentId", "bseInstrumentId"));
    let isin = s(r, "isin");
    Some(Holding {
        symbol: oa_symbol(symbols, exchange, &[&symbol], &token),
        exchange: exchange.to_string(),
        product: "CNC".to_string(),
        isin: (!isin.is_empty()).then_some(isin),
        quantity: quantity as i32,
        t1_quantity: t1 as i32,
        average_price: py_round(price, 2),
        ltp: py_round(ltp, 2),
        close_price: 0.0,
        pnl,
        pnl_percentage: pnl_pct,
        current_value: ltp * quantity as f64,
    })
}

// ---------------------------------------------------------------------------
// Order Status Feed (web `aliceblue_order_adapter.py:45-206`)
// ---------------------------------------------------------------------------

/// Noren status text -> OpenAlgo status (case-insensitive; unknown passes
/// through lowercased, empty is `open`).
pub fn order_feed_status(raw: &str) -> String {
    let l = raw.trim().to_ascii_lowercase();
    match l.as_str() {
        "open" | "new" | "replaced" => "open".into(),
        "trigger_pending" | "trigger pending" => "trigger pending".into(),
        "complete" => "complete".into(),
        "rejected" => "rejected".into(),
        "cancelled" | "canceled" => "cancelled".into(),
        "" => "open".into(),
        _ => l,
    }
}

/// An `om` frame -> order update; `None` for other frames.
pub fn order_update_from(v: &Value, symbols: &SymbolResolver) -> Option<OrderUpdate> {
    if s(v, "t") != "om" {
        return None;
    }
    let raw_status = s(v, "status").to_ascii_lowercase();
    let qty = int(v.get("qty"));
    let filled = int(v.get("fillshares"));
    let exchange = s(v, "exch");
    let tsym = s(v, "tsym");
    let symbol = symbols
        .by_brsymbol(&exchange, &tsym)
        .map(|r| r.symbol)
        .unwrap_or(tsym);
    let trantype = s(v, "trantype");
    let prctyp = s(v, "prctyp");
    Some(OrderUpdate {
        orderid: s(v, "norenordno"),
        symbol,
        exchange,
        action: match trantype.as_str() {
            "B" => "BUY".into(),
            "S" => "SELL".into(),
            _ => trantype,
        },
        quantity: qty,
        price: num(v.get("prc")),
        trigger_price: num(v.get("trgprc")),
        pricetype: match prctyp.as_str() {
            "MKT" => "MARKET".into(),
            "L" => "LIMIT".into(),
            "SL" => "SL".into(),
            "SL-M" => "SL-M".into(),
            _ => prctyp,
        },
        product: s(v, "pcode"),
        order_status: order_feed_status(&raw_status),
        filled_quantity: filled,
        pending_quantity: (qty - filled).max(0),
        average_price: num(v.get("avgprc")),
        rejection_reason: if raw_status == "rejected" {
            s(v, "rejreason")
        } else {
            String::new()
        },
    })
}

// ---------------------------------------------------------------------------
// Error codes (web `api/error_codes.py`, from AliceBlue's 15-error-code.md)
// ---------------------------------------------------------------------------

/// Expand every `ECnnn` code in a broker message into `ECnnn: <text>`;
/// unknown codes and plain messages pass through.
pub fn describe(message: &str) -> String {
    let b = message.as_bytes();
    let mut out = String::with_capacity(message.len() + 32);
    let mut i = 0;
    while i < b.len() {
        let boundary_before = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        let is_code = boundary_before
            && i + 5 <= b.len()
            && &b[i..i + 2] == b"EC"
            && b[i + 2..i + 5].iter().all(u8::is_ascii_digit)
            && (i + 5 == b.len() || !(b[i + 5].is_ascii_alphanumeric() || b[i + 5] == b'_'));
        if is_code {
            let code = &message[i..i + 5];
            out.push_str(code);
            if let Some((_, d)) = ERROR_CODES.iter().find(|(c, _)| *c == code) {
                out.push_str(": ");
                out.push_str(d);
            }
            i += 5;
        } else {
            let ch = message[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// AliceBlue's published codes, verbatim.
pub const ERROR_CODES: &[(&str, &str)] = &[
    ("EC003", "An error occurred. Please try again later."),
    ("EC900", "'exchange' cannot be empty or null."),
    ("EC901", "'exchange' should be one of the following values: { 'NSE', 'BSE', 'MCX', 'NFO', 'BFO', 'CDS', 'BCD'}."),
    ("EC902", "'tradingSymbol' cannot be empty or null."),
    ("EC903", "'quantity' cannot be empty or null."),
    ("EC904", "'quantity' should be a positive number."),
    ("EC906", "'product' cannot be empty or null."),
    ("EC907", "'transactionType' cannot be empty or null."),
    ("EC908", "'token' cannot be empty or null."),
    ("EC909", "'disclosedQty' cannot be empty or null."),
    ("EC910", "'price' cannot be empty or null."),
    ("EC911", "'triggerPrice' cannot be empty or null."),
    ("EC912", "Failed to place the order."),
    ("EC913", "Failed to retrieve user details."),
    ("EC914", "'Request parameter' cannot be empty or null."),
    ("EC915", "Failed to retrieve the order book."),
    ("EC916", "No orders found for this user."),
    ("EC917", "Failed to retrieve order history."),
    ("EC918", "No order history found for the given order ID."),
    ("EC919", "Failed to retrieve the position book."),
    ("EC920", "No positions found for this user."),
    ("EC921", "Failed to retrieve holdings."),
    ("EC922", "No holdings found for this user."),
    ("EC923", "Failed to retrieve profile details."),
    ("EC924", "Failed to retrieve RMS limits."),
    ("EC925", "'nestOrderNo' cannot be empty or null."),
    ("EC926", "No trades found for this user."),
    ("EC927", "Failed to retrieve the trade book."),
    ("EC929", "'transactionType' should be one of the following values: {'BUY', 'SELL'}."),
    ("EC930", "'orderType' should be one of the following values: {'LIMIT', 'MARKET', 'SL', 'SLM'}."),
    ("EC932", "'validity' should be one of the following values: {'DAY', 'IOC'}."),
    ("EC933", "'priceType' cannot be empty or null."),
    ("EC934", "'orderType' cannot be empty or null."),
    ("EC935", "Failed to retrieve the single order margin."),
    ("EC936", "'product' cannot be empty or null."),
    ("EC937", "Failed to cancel all orders."),
    ("EC938", "No open orders to cancel from the order book."),
    ("EC939", "Failed to retrieve the span margin."),
    ("EC941", "'instrumentId' cannot be empty or null."),
    ("EC942", "'orderComplexity' cannot be empty or null."),
    ("EC944", "'validity' cannot be empty or null."),
    ("EC945", "'brokerOrderId' cannot be empty or null."),
    ("EC946", "Invalid 'instrumentId'. It must contain only numeric characters."),
    ("EC947", "'instrumentId' does not exist."),
    ("EC948", "'quantity' cannot exceed 50,000,000."),
    ("EC949", "'quantity' should be a positive number."),
    ("EC950", "'price' is required and cannot be empty or null."),
    ("EC951", "'slTriggerPrice' is required and cannot be empty or null."),
    ("EC953", "'targetPrice' is required and cannot be empty or null."),
    ("EC954", "'quantity' should be a multiple of the lot size."),
    ("EC957", "Invalid 'price'."),
    ("EC958", "'price' cannot be zero or negative."),
    ("EC959", "Invalid 'slTriggerPrice'."),
    ("EC960", "'slTriggerPrice' cannot be zero or negative."),
    ("EC962", "'stopLossPrice' cannot be zero or negative."),
    ("EC963", "Invalid 'targetPrice'."),
    ("EC964", "'targetPrice' cannot be zero or negative."),
    ("EC966", "'trailingSlAmount' cannot be empty or null for SL order type."),
    ("EC967", "'trailingSlAmount' should be a positive number."),
    ("EC968", "'trailingSlAmount' cannot be zero or negative."),
    ("EC969", "'Product' should be either 'NORMAL' or 'INTRADAY'."),
    ("EC970", "'disclosedQuantity' is not applicable for this segment."),
    ("EC971", "'orderTag' should not exceed 50 characters"),
    ("EC972", "'algoId' should not exceed 12 characters."),
    ("EC973", "For a buy order, 'slTriggerPrice' should be less than the 'price'."),
    ("EC974", "For a sell order, 'slTriggerPrice' should be greater than the 'price'."),
    ("EC975", "'disclosedQuantity' cannot exceed the total order 'quantity'."),
    ("EC979", "Invalid 'brokerOrderId'."),
    ("EC980", "Invalid 'instrumentId'."),
    ("EC981", "Invalid 'disclosedQty'."),
    ("EC982", "For 'AMO', 'disclosedQuantity' should be zero."),
    ("EC983", "Invalid 'algoId'."),
    ("EC984", "Invalid 'orderTag'."),
    ("EC986", "SpanMargin is not allowed for 'NSEEQ' and 'BSEEQ'."),
    ("EC988", "'marketProtection' should be a positive number."),
    ("EC990", "'quantity' should be a multiple of the lot size."),
    ("EC991", "'disclosedQuantity' should be a multiple of the lot size."),
    ("EC992", "Unable to modify the given order. 'brokerOrderId' is invalid."),
    ("EC993", "Provided 'brokerOrderId' is not in a valid state to modify the order."),
    ("EC994", "The given 'brokerOrderId' is not in your order book."),
    ("EC996", "'validity' of IOC is not allowed for AMO orders."),
    ("EC997", "The specified order is not available in the order book and cannot be canceled. Please verify the order details and try again."),
    ("EC998", "The specified order is not available in the order book, and order history cannot be retrieved. Please verify the order ID and try again."),
    ("EC999", "The specified order is not available in the order book and cannot be modified. Please verify the order details and try again."),
    ("EC801", "Orders with exchange 'BSEEQ/BSEFO/BSECURR' cannot be modified to order type 'SL' (Stop Loss)."),
    ("EC806", "'exchange' accepts only {'NSEEQ', 'BSEEQ'}."),
    ("EC807", "'product' - 'NORMAL' is not allowed in cash segment."),
    ("EC813", "'deviceId' cannot exceed 98 characters."),
    ("EC814", "'brokerOrderId' cannot be empty or null."),
    ("EC815", "Invalid 'brokerOrderId'."),
    ("EC819", "Only the trigger price field can be modified."),
    ("EC822", "SL trigger price should be lower than price."),
    ("EC823", "SL trigger price should be higher than price."),
    ("EC824", "SL trigger price should be %.2f%% below price."),
    ("EC825", "SL trigger price should be %.2f%% above price."),
    ("EC826", "Please enter a price."),
    ("EC827", "Please enter a target price."),
    ("EC828", "Please enter an SL trigger price."),
    ("EC829", "AMO is not allowed for this product."),
    ("EC830", "AMO is not allowed for this order type."),
    ("EC831", "AMO is not allowed for this validity."),
    ("EC832", "AMO is not allowed for this segment."),
    ("EC834", "Market protection cannot be modified."),
    ("EC837", "This product is not allowed for this segment."),
    ("EC838", "This order type is not allowed."),
    ("EC842", "'disclosedQuantity' should be at least %.2f%% of the total order quantity."),
    ("EC843", "Only price and quantity fields can be modified."),
    ("EC844", "Only price and order type fields can be modified."),
    ("EC846", "SL trigger price should be %.2f%% or %.2f paise below price."),
    ("EC847", "SL trigger price should be %.2f%% or %.2f paise above price."),
    ("EC848", "Price should be higher than the SL trigger price."),
    ("EC849", "Price should be lower than the SL trigger price."),
    ("EC850", "Price should be %.2f%% or %.2f paise above the SL trigger price."),
    ("EC851", "Price should be %.2f%% or %.2f paise below the SL trigger price."),
    ("EC852", "This product is not allowed."),
    ("EC855", "Modification is not allowed."),
    ("EC856", "SL trigger price should be less than main leg price."),
    ("EC857", "SL trigger price should be more than main leg price."),
    ("EC858", "Order placement not allowed for this exchange."),
    ("EC865", "'product' - 'Delivery' is not allowed in FnO segment."),
    ("EC868", "Position not found for the specified instrument."),
    ("EC869", "Insufficient buy quantity available for conversion."),
    ("EC870", "Insufficient sell quantity available for conversion."),
    ("EC871", "Conversion of overnight BUY positions in options is not allowed."),
    ("EC873", "Failed to convert positions."),
    ("EC082", "Invalid parameter: 'deviceId' cannot be empty or null."),
    ("EC086", "You are a read-only user and are not allowed to place, modify, or cancel orders."),
    ("EC087", "Session Expired"),
    ("EC088", "Single order slicing limit exceeded"),
    ("EC089", "'disclosedQuantity' cannot be same the total order 'quantity'."),
    ("EC090", "'exchange' should be one of the following values: { 'NSE', 'BSE', 'MCX', 'NFO', 'BFO'}."),
    ("EC091", "'orderComplexity' should be one of the following values: {'REGULAR', 'AMO'}."),
    ("EC092", "'product' should be one of the following values: {'INTRADAY', 'LONGTERM', 'MTF'}."),
];
