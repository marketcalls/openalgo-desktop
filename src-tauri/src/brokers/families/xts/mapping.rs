//! OpenAlgo <-> XTS translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `api/funds.py`). Pure functions; the symbol
//! master is passed in.

use super::protection::Priced;
use super::XtsHooks;
use crate::brokers::common::book_read;
use crate::brokers::common::mapping::{Action, Exchange, OrderStatus, PriceType, Product};
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

/// The XTS v2 docs' `OrderStatus` values beyond the template's map that
/// are open: a modified order (`Replaced`) and the states still working at
/// the exchange (web fivepaisaxts `ORDER_STATUS_MAPPING`, #2504).
const DOCUMENTED_OPEN: &[&str] = &["Replaced", "PendingNew", "PendingReplace", "PendingCancel"];

/// What cancel-all cancels for a member that reads answers as documented
/// (web fivepaisaxts `_CANCELLABLE_STATUSES`): every order still working,
/// modified and part-filled ones included, but not a `PendingCancel`,
/// whose cancel is already on its way.
///
/// Deviation: a `Trigger Pending` (or `TriggerPending`) order is cancelled
/// too. Web #2504 dropped it as a status the XTS v2 docs do not list, but
/// `oa_status` still shows such an order as `trigger pending`, still
/// working, so cancel-all must not leave it live at the exchange. It is
/// what the desktop did before and what the six other members' web code
/// still does (`["New", "Trigger Pending"]`); for documented answers it
/// changes nothing.
const DOCUMENTED_CANCELLABLE: &[&str] = &[
    "New",
    "Open",
    "Replaced",
    "PartiallyFilled",
    "PendingNew",
    "PendingReplace",
    "Trigger Pending",
    "TriggerPending",
];

/// The order-book status of a member: the documented states read as the
/// XTS v2 docs list them, the rest as `oa_status`.
pub(crate) fn member_status(s: &str, hooks: &XtsHooks) -> String {
    if hooks.answers_as_documented && DOCUMENTED_OPEN.contains(&s.trim()) {
        return "open".into();
    }
    oa_status(s)
}

/// Whether cancel-all cancels this order-book row. A member that reads
/// answers as documented cancels by XTS's own status; the others what
/// their book reads as open or trigger pending, as the trait's default does.
pub(crate) fn cancellable(row: &Value, hooks: &XtsHooks) -> bool {
    let status = s(row, "OrderStatus");
    if hooks.answers_as_documented {
        return DOCUMENTED_CANCELLABLE.contains(&status.trim());
    }
    oa_status(&status)
        .parse::<OrderStatus>()
        .is_ok_and(OrderStatus::is_pending)
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

/// Instrument or order id as JSON: an integer when it fits an `i64` (XTS
/// API types), else the string. An order id past an `i64` (they run to 20
/// digits) goes out as its text, as the web sends every order id.
pub fn instrument_id(token: &str) -> Value {
    token
        .trim()
        .parse::<i64>()
        .map(Value::from)
        .unwrap_or_else(|_| Value::from(token))
}

/// An id written as a whole number (`42`, ` 0042 `, `1234.0`, `-5`) in its
/// plain digits, as the web's `str(int(Decimal(text)))`; anything else
/// as it is.
fn whole_number_text(text: &str) -> Option<String> {
    let (sign, body) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text.strip_prefix('+').unwrap_or(text)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !all_digits(int) || !frac.bytes().all(|b| b == b'0') {
        return None;
    }
    let digits = int.trim_start_matches('0');
    Some(if digits.is_empty() {
        "0".into()
    } else {
        format!("{}{}", sign, digits)
    })
}

/// An XTS order id as text, digit for digit (web `utils/xts_order_data.py`
/// `exact_order_id`, marketcalls/openalgo#2468). XTS ids run to 20 digits,
/// past what a float holds: read through one, 11001263160000030116, ...0117
/// and ...0118 became one id, so a day's orders shared it in the order and
/// trade books. A JSON integer is kept as it is; a JSON number above
/// `u64::MAX` has already lost digits in the JSON reader (no
/// `arbitrary_precision`). A missing id is empty, not the web's "0":
/// `place_raw` reads an empty id as an order accepted without one (an
/// uncertain placement), and no cancel or modify could use "0".
pub fn order_id(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => {
            let t = s.trim();
            whole_number_text(t).unwrap_or_else(|| t.to_string())
        }
        Some(Value::Number(n)) => {
            if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                let x = n.as_f64().unwrap_or(0.0);
                if x.is_finite() && x.fract() == 0.0 {
                    format!("{:.0}", x)
                } else {
                    n.to_string()
                }
            }
        }
        _ => String::new(),
    }
}

/// The average a trader paid (long) or received (short) for a position
/// (web `traded_average_price`, #2468, issue #2462). For a position
/// carried overnight XTS fills `BuyAveragePrice` / `SellAveragePrice` with
/// the previous close it marked the position to, and keeps the traded
/// average in `ActualBuyAveragePrice` / `ActualSellAveragePrice`; the docs
/// list only the first pair, so a missing or zero Actual field falls back
/// to it. A flat position has no average.
fn traded_average_price(p: &Value, net_qty: f64) -> f64 {
    let fields: [&str; 2] = if net_qty > 0.0 {
        ["ActualBuyAveragePrice", "BuyAveragePrice"]
    } else if net_qty < 0.0 {
        ["ActualSellAveragePrice", "SellAveragePrice"]
    } else {
        return 0.0;
    };
    fields
        .iter()
        .map(|k| f(p, k))
        .find(|x| x.is_finite() && *x > 0.0)
        .unwrap_or(0.0)
}

/// Place payload (`transform_data.py:18-30`), with the type and limit the
/// order goes out with (`protection::price`). Numbers are typed, as in the
/// rmoney variant and the XTS API docs (audit D-xts C recommendation).
pub(crate) fn place_payload(o: &ResolvedOrder, priced: Priced) -> Option<Value> {
    Some(json!({
        "exchangeSegment": segment(o.exchange)?,
        "exchangeInstrumentID": instrument_id(o.token()),
        "productType": product(o.product),
        "orderType": priced.order_type,
        "orderSide": o.action.as_str(),
        "timeInForce": "DAY",
        "disclosedQuantity": o.disclosed_quantity,
        "orderQuantity": o.quantity,
        "limitPrice": priced.limit,
        "stopPrice": o.trigger_price,
        "orderUniqueIdentifier": "openalgo",
    }))
}

/// Modify payload (`transform_data.py:36-46`), priced like a new order.
pub(crate) fn modify_payload(m: &ResolvedModify, priced: Priced) -> Value {
    json!({
        "appOrderID": instrument_id(&m.order_id),
        "modifiedProductType": product(m.product),
        "modifiedOrderType": priced.order_type,
        "modifiedOrderQuantity": m.quantity,
        "modifiedDisclosedQuantity": m.disclosed_quantity,
        "modifiedLimitPrice": priced.limit,
        "modifiedStopPrice": m.trigger_price,
        "modifiedTimeInForce": "DAY",
        "orderUniqueIdentifier": "openalgo",
    })
}

/// The side that squares off a net quantity.
pub(crate) fn exit_action(net_qty: i64) -> Action {
    if net_qty > 0 {
        Action::Sell
    } else {
        Action::Buy
    }
}

/// Square-off payload built from a position row (`order_api.py:306-325`):
/// a MARKET order, or the protected LIMIT a member prices it as.
pub(crate) fn exit_payload(
    segment: &str,
    instrument: &Value,
    product: &str,
    net_qty: i64,
    priced: Priced,
) -> Value {
    json!({
        "exchangeSegment": segment,
        "exchangeInstrumentID": instrument,
        "productType": product,
        "orderType": priced.order_type,
        "orderSide": exit_action(net_qty).as_str(),
        "timeInForce": "DAY",
        "disclosedQuantity": 0,
        "orderQuantity": net_qty.abs(),
        "limitPrice": priced.limit,
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

/// `transform_order_data` over `GET /orders` `result`, with the member's
/// status map (`member_status`).
pub fn orders(result: &Value, symbols: &SymbolResolver, hooks: &XtsHooks) -> Vec<Order> {
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
                status: member_status(&s(o, "OrderStatus"), hooks),
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

/// A field the row carries: not missing, null or empty text (the web's
/// `not in (None, "")`).
fn carried(v: &Value, k: &str) -> bool {
    match v.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// The quantity, price and time of one trade-book row. For a member that
/// reads answers as documented the row is one fill (web fivepaisaxts
/// #2504): `LastTradedQuantity` and `LastTradedPrice` when it carries both,
/// since `OrderQuantity` is the whole order and a part-filled order counted
/// it on every fill, and `LastExecutionTransactTime`; else the order's.
fn trade_fill(t: &Value, hooks: &XtsHooks) -> (i32, f64, String) {
    let order_time = || s(t, "OrderGeneratedDateTime");
    if !hooks.answers_as_documented {
        return (
            i(t, "OrderQuantity") as i32,
            f(t, "OrderAverageTradedPrice"),
            order_time(),
        );
    }
    let (quantity, price) = if carried(t, "LastTradedQuantity") && carried(t, "LastTradedPrice") {
        (i(t, "LastTradedQuantity"), f(t, "LastTradedPrice"))
    } else {
        (i(t, "OrderQuantity"), f(t, "OrderAverageTradedPrice"))
    };
    let executed = s(t, "LastExecutionTransactTime");
    let time = if executed.is_empty() {
        order_time()
    } else {
        executed
    };
    (quantity as i32, price, time)
}

/// `transform_tradebook_data` over `GET /orders/trades` `result`
/// (`order_data.py:231-245`; tolerant numbers like wisdom's guards), each
/// row read as the member reads it (`trade_fill`).
pub fn trades(result: &Value, symbols: &SymbolResolver, hooks: &XtsHooks) -> Vec<Trade> {
    list(result)
        .iter()
        .filter(|t| t.is_object())
        .map(|t| {
            let exchange = oa_exchange(&s(t, "ExchangeSegment"));
            let token = s(t, "ExchangeInstrumentID");
            let (quantity, average_price, timestamp) = trade_fill(t, hooks);
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
                timestamp,
            }
        })
        .collect()
}

/// The position list: `result.positionList`, or `result` itself when it is
/// a flat list (rmoney `order_api.py`). `None` when it is neither, or when
/// a row lacks its `Quantity` (BR-03: never an empty or flat book).
pub fn position_list(result: &Value) -> Option<&[Value]> {
    let rows: &[Value] = match result {
        Value::Array(a) => a,
        Value::Object(_) => result.get("positionList")?.as_array()?,
        _ => return None,
    };
    rows.iter()
        .all(|p| book_read::quantity(p, "Quantity").is_some())
        .then_some(rows)
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

/// The net quantity of one instrument in the position list (web rmoney
/// `get_open_position`, openalgo#2292). XTS names a position its own way
/// ("NIFTY 27OCT2026 CE 22400") while the master's broker symbol for the
/// same option is "NIFTY26OCT22400CE", so a position is matched by its
/// instrument id, and by its name only when it carries none: the broker
/// symbol, as the web matches it, or the OpenAlgo symbol, as the position
/// book reads such a row. 0 when flat.
pub fn open_position(
    rows: &[Value],
    symbols: &SymbolResolver,
    symbol: &str,
    exchange: &str,
    product: &str,
) -> i64 {
    let token = symbols.token(symbol, exchange).filter(|t| !t.is_empty());
    let brsymbol = symbols.br_symbol(symbol, exchange);
    rows.iter()
        .filter(|p| p.is_object())
        .find(|p| {
            let held = value_text(position_instrument(p));
            let same_instrument = match &token {
                Some(t) if !held.is_empty() => held == *t,
                _ => {
                    let name = s(p, "TradingSymbol");
                    brsymbol.as_deref() == Some(name.as_str()) || name == symbol
                }
            };
            same_instrument
                && oa_exchange(&s(p, "ExchangeSegment")) == exchange
                && s(p, "ProductType") == product
        })
        .map(|p| f(p, "Quantity") as i64)
        .unwrap_or(0)
}

/// `transform_positions_data` (`order_data.py:280-325`) over a list
/// [`position_list`] read.
pub fn positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .filter(|p| p.is_object())
        .map(|p| {
            let exchange = oa_exchange(&s(p, "ExchangeSegment"));
            let token = value_text(position_instrument(p));
            let qty = f(p, "Quantity");
            let average_price = traded_average_price(p, qty);
            let pnl = p
                .get("pnl")
                .map(|_| f(p, "pnl"))
                .unwrap_or_else(|| f(p, "MTM"));
            Position {
                symbol: oa_symbol(symbols, &exchange, &token, s(p, "TradingSymbol")),
                exchange,
                product: s(p, "ProductType"),
                quantity: qty as i32,
                average_price_basis: None,
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
pub fn holdings(result: &Value, symbols: &SymbolResolver) -> Option<Vec<Holding>> {
    // XTS sends `RMSHoldings.Holdings` as a map, empty when nothing is
    // held; without it the reply is not a holdings book (BR-03).
    let map = result
        .get("RMSHoldings")
        .and_then(|r| r.get("Holdings"))
        .and_then(Value::as_object)?;
    let rows = map
        .iter()
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
        .collect();
    Some(rows)
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
/// `limitHeader` matches the member's `funds_balance_header`, else the
/// first), `RMSSubLimits`. A member that reads answers as documented also
/// reads a `BalanceList` sent as one entry (web fivepaisaxts #2504).
pub fn funds(result: &Value, hooks: &XtsHooks) -> Option<Funds> {
    let list = result.get("BalanceList")?;
    let entries: &[Value] = match list {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) if hooks.answers_as_documented => std::slice::from_ref(list),
        _ => return None,
    };
    let first = entries.first()?;
    let entry = hooks
        .funds_balance_header
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
