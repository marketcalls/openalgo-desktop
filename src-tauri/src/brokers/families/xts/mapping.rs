//! OpenAlgo <-> XTS translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `api/funds.py`). Pure functions; the symbol
//! master is passed in.

use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Value};

/// OpenAlgo exchange -> XTS segment string for orders (`map_exchange`,
/// `transform_data.py:53-62`). Indices are not tradable.
pub fn segment(exchange: Exchange) -> Option<&'static str> {
    Some(match exchange {
        Exchange::Nse => "NSECM",
        Exchange::Bse => "BSECM",
        Exchange::Nfo => "NSEFO",
        Exchange::Bfo => "BSEFO",
        Exchange::Cds => "NSECD",
        Exchange::Mcx => "MCXFO",
        _ => return None,
    })
}

/// Segment string for `/instruments/ohlc`, indices included
/// (`data.py:531-540`).
pub fn history_segment(exchange: Exchange) -> Option<&'static str> {
    match exchange {
        Exchange::NseIndex => Some("NSECM"),
        Exchange::BseIndex => Some("BSECM"),
        e => segment(e),
    }
}

/// Numeric XTS segment (`data.py:129-138`): NSE 1, NFO 2, CDS 3, BSE 11,
/// BFO 12, MCX 51; indices share their cash segment.
pub fn segment_code(exchange: Exchange) -> Option<i64> {
    Some(match exchange {
        Exchange::Nse | Exchange::NseIndex => 1,
        Exchange::Nfo => 2,
        Exchange::Cds => 3,
        Exchange::Bse | Exchange::BseIndex => 11,
        Exchange::Bfo => 12,
        Exchange::Mcx => 51,
        _ => return None,
    })
}

/// XTS segment string -> OpenAlgo exchange (`order_data.py:19-26`);
/// unknown values pass through.
pub fn oa_exchange(segment: &str) -> String {
    match segment {
        "NSECM" => "NSE",
        "BSECM" => "BSE",
        "NSEFO" => "NFO",
        "BSEFO" => "BFO",
        "MCXFO" => "MCX",
        "NSECD" => "CDS",
        other => other,
    }
    .to_string()
}

/// Numeric segment -> OpenAlgo cash/derivative exchange (adapter
/// `segment_to_exchange`).
pub fn exchange_for_code(code: i64) -> Option<&'static str> {
    Some(match code {
        1 => "NSE",
        2 => "NFO",
        3 => "CDS",
        11 => "BSE",
        12 => "BFO",
        51 => "MCX",
        _ => return None,
    })
}

/// `map_order_type` (`transform_data.py:69-75`).
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "STOPLIMIT",
        PriceType::SlM => "STOPMARKET",
    }
}

/// Order types as XTS reports them (`order_data.py:116-121`, Title-case);
/// the request spellings are accepted too.
pub fn oa_order_type(s: &str) -> String {
    match s {
        "Limit" | "LIMIT" => "LIMIT",
        "Market" | "MARKET" => "MARKET",
        "StopLimit" | "STOPLIMIT" => "SL",
        "StopMarket" | "STOPMARKET" => "SL-M",
        other => other,
    }
    .to_string()
}

/// Product is the identity map (`transform_data.py:82-101`).
pub fn product(p: Product) -> &'static str {
    p.as_str()
}

/// Status map (`order_data.py:123-128`) plus the audit's additions:
/// `PartiallyFilled` and `Open` are open, `Trigger Pending` is
/// `trigger pending`. Anything else is lowercased.
pub fn oa_status(s: &str) -> String {
    match s.trim() {
        "Filled" => "complete".into(),
        "Rejected" => "rejected".into(),
        "Cancelled" => "cancelled".into(),
        "New" | "Open" | "PartiallyFilled" => "open".into(),
        "Trigger Pending" | "TriggerPending" => "trigger pending".into(),
        other => crate::brokers::lower_status(other),
    }
}

/// `description` (or `message`) of an XTS error envelope.
pub fn error_text(v: &Value) -> String {
    v.get("description")
        .or_else(|| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// XTS answers an expired session with HTTP 200 and "Invalid Token".
pub fn is_token_error(description: &str) -> bool {
    let d = description.to_ascii_lowercase();
    d.contains("invalid token") || (d.contains("token") && d.contains("expired"))
}

pub(crate) fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub(crate) fn f(v: &Value, k: &str) -> f64 {
    num(v.get(k))
}

pub(crate) fn i(v: &Value, k: &str) -> i64 {
    num(v.get(k)) as i64
}

pub(crate) fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Instrument id as JSON: an integer when numeric (XTS API types), else
/// the string.
pub fn instrument_id(token: &str) -> Value {
    token
        .trim()
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::from(token))
}

/// Order id `float -> int -> str` (`order_data.py:158`).
pub fn order_id(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => match s.trim().parse::<f64>() {
            Ok(x) => format!("{}", x as i64),
            Err(_) => s.trim().to_string(),
        },
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|x| x.to_string())
            .unwrap_or_else(|| format!("{}", n.as_f64().unwrap_or(0.0) as i64)),
        _ => String::new(),
    }
}

/// Place payload (`transform_data.py:18-30`). Numbers are typed, as in the
/// rmoney variant and the XTS API docs (audit D-xts C recommendation).
pub fn place_payload(o: &ResolvedOrder) -> Option<Value> {
    Some(json!({
        "exchangeSegment": segment(o.exchange)?,
        "exchangeInstrumentID": instrument_id(o.token()),
        "productType": product(o.product),
        "orderType": order_type(o.pricetype),
        "orderSide": o.action.as_str(),
        "timeInForce": "DAY",
        "disclosedQuantity": o.disclosed_quantity,
        "orderQuantity": o.quantity,
        "limitPrice": o.price,
        "stopPrice": o.trigger_price,
        "orderUniqueIdentifier": "openalgo",
    }))
}

/// Modify payload (`transform_data.py:36-46`).
pub fn modify_payload(m: &ResolvedModify) -> Value {
    json!({
        "appOrderID": instrument_id(&m.order_id),
        "modifiedProductType": product(m.product),
        "modifiedOrderType": order_type(m.pricetype),
        "modifiedOrderQuantity": m.quantity,
        "modifiedDisclosedQuantity": m.disclosed_quantity,
        "modifiedLimitPrice": m.price,
        "modifiedStopPrice": m.trigger_price,
        "modifiedTimeInForce": "DAY",
        "orderUniqueIdentifier": "openalgo",
    })
}

/// Square-off payload built from a position row (`order_api.py:306-325`).
pub fn exit_payload(segment: &str, instrument: &Value, product: &str, net_qty: i64) -> Value {
    json!({
        "exchangeSegment": segment,
        "exchangeInstrumentID": instrument,
        "productType": product,
        "orderType": "MARKET",
        "orderSide": if net_qty > 0 { "SELL" } else { "BUY" },
        "timeInForce": "DAY",
        "disclosedQuantity": 0,
        "orderQuantity": net_qty.abs(),
        "limitPrice": 0,
        "stopPrice": 0,
        "orderUniqueIdentifier": "openalgo",
    })
}

/// OpenAlgo symbol for a book row: the master's symbol for the token,
/// else the broker's `TradingSymbol` (`order_data.py:40-49`).
fn oa_symbol(symbols: &SymbolResolver, exchange: &str, token: &str, fallback: String) -> String {
    if token.is_empty() {
        return fallback;
    }
    symbols
        .by_token(exchange, token)
        .map(|r| r.symbol)
        .unwrap_or(fallback)
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// `transform_order_data` over `GET /orders` `result`.
pub fn orders(result: &Value, symbols: &SymbolResolver) -> Vec<Order> {
    list(result)
        .iter()
        .filter(|o| o.is_object())
        .map(|o| {
            let exchange = oa_exchange(&s(o, "ExchangeSegment"));
            let token = s(o, "ExchangeInstrumentID");
            let quantity = i(o, "OrderQuantity") as i32;
            let filled = i(o, "CumulativeQuantity") as i32;
            let leaves = o
                .get("LeavesQuantity")
                .map(|_| i(o, "LeavesQuantity") as i32)
                .unwrap_or((quantity - filled).max(0));
            let exchange_order_id = s(o, "ExchangeOrderID");
            let reason = s(o, "CancelRejectReason");
            Order {
                order_tag: None,
                order_id: order_id(o.get("AppOrderID")),
                exchange_order_id: (!exchange_order_id.is_empty()).then_some(exchange_order_id),
                symbol: oa_symbol(symbols, &exchange, &token, s(o, "TradingSymbol")),
                exchange,
                side: s(o, "OrderSide"),
                quantity,
                filled_quantity: filled,
                pending_quantity: leaves,
                price: f(o, "OrderPrice"),
                trigger_price: f(o, "OrderStopPrice"),
                average_price: f(o, "OrderAverageTradedPrice"),
                order_type: oa_order_type(&s(o, "OrderType")),
                product: s(o, "ProductType"),
                status: oa_status(&s(o, "OrderStatus")),
                validity: {
                    let t = s(o, "TimeInForce");
                    if t.is_empty() {
                        "DAY".into()
                    } else {
                        t
                    }
                },
                order_timestamp: s(o, "LastUpdateDateTime"),
                exchange_timestamp: Some(s(o, "ExchangeTransactTime")).filter(|x| !x.is_empty()),
                rejection_reason: (!reason.is_empty()).then_some(reason),
            }
        })
        .collect()
}

/// `transform_tradebook_data` over `GET /orders/trades` `result`
/// (`order_data.py:231-245`; tolerant numbers like wisdom's guards).
pub fn trades(result: &Value, symbols: &SymbolResolver) -> Vec<Trade> {
    list(result)
        .iter()
        .filter(|t| t.is_object())
        .map(|t| {
            let exchange = oa_exchange(&s(t, "ExchangeSegment"));
            let token = s(t, "ExchangeInstrumentID");
            let quantity = i(t, "OrderQuantity") as i32;
            let average_price = f(t, "OrderAverageTradedPrice");
            Trade {
                order_tag: None,
                order_id: order_id(t.get("AppOrderID")),
                trade_id: s(t, "ExecutionID"),
                symbol: oa_symbol(symbols, &exchange, &token, s(t, "TradingSymbol")),
                exchange,
                product: s(t, "ProductType"),
                side: s(t, "OrderSide"),
                quantity,
                average_price,
                trade_value: f64::from(quantity) * average_price,
                timestamp: s(t, "OrderGeneratedDateTime"),
            }
        })
        .collect()
}

/// The position list: `result.positionList`, or `result` itself when it is
/// a flat list (rmoney `order_api.py`).
pub fn position_list(result: &Value) -> &[Value] {
    match result {
        Value::Array(a) => a,
        Value::Object(_) => list(result.get("positionList").unwrap_or(&Value::Null)),
        _ => &[],
    }
}

/// Instrument id of a position row: `ExchangeInstrumentId` (lower-case d),
/// `ExchangeInstrumentID` or `Id` (rmoney).
pub fn position_instrument(p: &Value) -> Option<&Value> {
    ["ExchangeInstrumentId", "ExchangeInstrumentID", "Id"]
        .iter()
        .find_map(|k| p.get(*k).filter(|v| !v.is_null()))
}

fn value_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|x| x.to_string())
            .unwrap_or_else(|| n.to_string()),
        _ => String::new(),
    }
}

/// `transform_positions_data` (`order_data.py:280-325`).
pub fn positions(result: &Value, symbols: &SymbolResolver) -> Vec<Position> {
    position_list(result)
        .iter()
        .filter(|p| p.is_object())
        .map(|p| {
            let exchange = oa_exchange(&s(p, "ExchangeSegment"));
            let token = value_text(position_instrument(p));
            let qty = f(p, "Quantity");
            let average_price = if qty > 0.0 {
                f(p, "BuyAveragePrice")
            } else if qty < 0.0 {
                f(p, "SellAveragePrice")
            } else {
                0.0
            };
            let pnl = p
                .get("pnl")
                .map(|_| f(p, "pnl"))
                .unwrap_or_else(|| f(p, "MTM"));
            Position {
                symbol: oa_symbol(symbols, &exchange, &token, s(p, "TradingSymbol")),
                exchange,
                product: s(p, "ProductType"),
                quantity: qty as i32,
                overnight_quantity: 0,
                average_price: (average_price * 100.0).round() / 100.0,
                ltp: f(p, "ltp"),
                pnl,
                realized_pnl: f(p, "RealizedMTM"),
                unrealized_pnl: f(p, "UnrealizedMTM"),
                buy_quantity: i(p, "OpenBuyQuantity") as i32,
                buy_value: f(p, "BuyAmount"),
                sell_quantity: i(p, "OpenSellQuantity") as i32,
                sell_value: f(p, "SellAmount"),
            }
        })
        .collect()
}

/// `map_portfolio_data` (`order_data.py:368-438`): holdings keyed by ISIN,
/// symbol from the NSE instrument id, product CNC, P&L placeholders 0.
pub fn holdings(result: &Value, symbols: &SymbolResolver) -> Vec<Holding> {
    let Some(map) = result
        .get("RMSHoldings")
        .and_then(|r| r.get("Holdings"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    map.iter()
        .map(|(isin, h)| {
            let token = value_text(h.get("ExchangeNSEInstrumentId"));
            let quantity = i(h, "HoldingQuantity") as i32;
            let average_price = f(h, "BuyAvgPrice");
            let symbol = if token.is_empty() {
                isin.clone()
            } else {
                symbols
                    .by_token("NSE", &token)
                    .map(|r| r.symbol)
                    .unwrap_or_else(|| isin.clone())
            };
            Holding {
                symbol,
                exchange: "NSE".into(),
                product: "CNC".into(),
                isin: Some(isin.clone()),
                quantity,
                t1_quantity: 0,
                average_price,
                ltp: 0.0,
                close_price: 0.0,
                pnl: 0.0,
                pnl_percentage: 0.0,
                current_value: f64::from(quantity) * average_price,
            }
        })
        .collect()
}

fn money(v: &Value, k: &str) -> f64 {
    // `f"{float(value):.2f}"`, "nan" -> 0.00.
    let x = f(v, k);
    if x.is_finite() {
        (x * 100.0).round() / 100.0
    } else {
        0.0
    }
}

/// `get_margin_data` (`funds.py:31-63`): `BalanceList` entry (the one whose
/// `limitHeader` matches `header`, else the first), `RMSSubLimits`.
pub fn funds(result: &Value, header: Option<&str>) -> Option<Funds> {
    let entries = result.get("BalanceList")?.as_array()?;
    let first = entries.first()?;
    let entry = header
        .and_then(|h| {
            entries
                .iter()
                .find(|e| e.get("limitHeader").and_then(Value::as_str) == Some(h))
        })
        .unwrap_or(first);
    let rms = entry.get("limitObject")?.get("RMSSubLimits")?;
    let utilised = money(rms, "marginUtilized");
    Some(Funds {
        available_cash: money(rms, "netMarginAvailable"),
        used_margin: utilised,
        collateral: money(rms, "collateral"),
        m2m_unrealized: money(rms, "UnrealizedMTM"),
        m2m_realized: money(rms, "RealizedMTM"),
        utilised_debits: utilised,
        ..Default::default()
    })
}

/// rmoney `transform_margin_positions` (`margin_data.py:58-66`): one
/// portfolio entry, or `None` when the instrument has no numeric segment.
pub fn margin_leg(leg: &MarginLeg, exchange: Exchange, token: &str) -> Option<Value> {
    Some(json!({
        "exchange": segment_code(exchange)?,
        "exchangeInstrumentId": token.trim().parse::<i64>().ok()?,
        "productType": product(leg.product),
        "orderType": order_type(leg.pricetype),
        "orderSide": leg.action.as_str(),
        "quantity": leg.quantity,
        "price": leg.price,
        "stopPrice": leg.trigger_price,
        "orderSessionType": 1,
    }))
}

/// rmoney `parse_margin_response` (`margin_data.py:95-157`).
pub fn margin_result(result: &Value) -> Option<MarginResult> {
    let d = result.get("brokerageDeatils")?;
    if !d.is_object() {
        return None;
    }
    Some(MarginResult {
        total_margin_required: f(d, "MarginRequired"),
        span_margin: 0.0,
        exposure_margin: 0.0,
    })
}
