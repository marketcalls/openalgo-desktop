//! OpenAlgo <-> Pocketful translation (web `mapping/transform_data.py` and
//! `mapping/order_data.py`, marketcalls/openalgo#2467; the feed's own maps
//! in `streaming/pocketful_mapping.py`).
//!
//! Pocketful answers are read as loose JSON: numbers arrive as numbers or
//! strings, and a missing field reads as empty or zero, as the web reads
//! them. Every book returns OpenAlgo symbols.

use super::pricing::Priced;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Loose JSON readers
// ---------------------------------------------------------------------------

/// String field (numbers are printed; null and absent are empty).
pub fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// First non-empty string among `keys`.
pub fn text_any(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| text(v, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite())
}

/// Number field (numeric strings accepted; 0 when absent or unreadable).
pub fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(as_f64).unwrap_or(0.0)
}

/// The first of `keys` that is present and numeric.
pub fn num_any(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(*k).and_then(as_f64))
}

/// Integer field, truncated like Python `int(float(x))`.
pub fn int(v: &Value, key: &str) -> i64 {
    num(v, key) as i64
}

fn i32_of(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Pocketful product -> OpenAlgo product (web `reverse_map_product_type`):
/// the order pages' CNC, MIS and NRML, and the annexure's INTRADAY and
/// MARGIN for the same two; `None` for one OpenAlgo does not trade (CO,
/// BO).
pub fn openalgo_product(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_uppercase().as_str() {
        "CNC" => Some("CNC"),
        "NRML" | "MARGIN" => Some("NRML"),
        "MIS" | "INTRADAY" => Some("MIS"),
        _ => None,
    }
}

/// A book row's product: OpenAlgo's when it has one, else Pocketful's own.
pub fn oa_product(s: &str) -> String {
    openalgo_product(s)
        .map(str::to_string)
        .unwrap_or_else(|| s.trim().to_ascii_uppercase())
}

/// Pocketful order type -> OpenAlgo price type (web
/// `reverse_map_order_type`; an unknown one is kept as it is).
pub fn oa_pricetype(s: &str) -> String {
    match s.trim().to_ascii_uppercase().as_str() {
        "SLM" | "SL-M" => "SL-M".into(),
        "MARKET" | "MKT" => "MARKET".into(),
        "LIMIT" | "LMT" => "LIMIT".into(),
        "SL" | "SL-L" => "SL".into(),
        other => other.to_string(),
    }
}

/// Feed exchange codes (web `pocketful_mapping.py`); unknown exchanges
/// default to NSE (1) like the web.
pub fn exchange_code(exchange: &str) -> u8 {
    match exchange {
        "NSE" | "NSE_INDEX" => 1,
        "NFO" => 2,
        "CDS" => 3,
        "MCX" => 4,
        "BSE" | "BSE_INDEX" => 6,
        "BFO" => 7,
        _ => 1,
    }
}

/// An order update's status (the feed's `transform_order_data`): substring
/// tests on the upper `order_status`, in the web's order, with `mode ==
/// NEW` meaning open.
pub fn map_status(status: &str, mode: &str) -> String {
    let s = status.trim().to_ascii_uppercase();
    let mode = mode.trim().to_ascii_uppercase();
    if s.contains("CANCEL_CONFIRMED") {
        "cancelled".into()
    } else if s.contains("COMPLETE") {
        "complete".into()
    } else if s.contains("REJECTED") {
        "rejected".into()
    } else if s.contains("TRIGGER PENDING") {
        "trigger pending".into()
    } else if s.contains("OPEN")
        || s.contains("PENDING")
        || s.contains("AMO_SUBMIT")
        || s.contains("MODIFY")
        || mode == "NEW"
    {
        "open".into()
    } else if s.contains("CANCEL") {
        "cancelled".into()
    } else if s.is_empty() {
        "unknown".into()
    } else {
        crate::brokers::lower_status(&s)
    }
}

// ---------------------------------------------------------------------------
// Order bodies (place-regular-order.md, modify-regular-order.md)
// ---------------------------------------------------------------------------

/// What place and modify send about the instrument and the size.
struct OrderFields<'a> {
    exchange: &'a str,
    token: &'a str,
    quantity: i64,
    disclosed_quantity: i64,
    product: &'a str,
    side: &'a str,
}

/// The fields place and modify share (web `_order_body`). `user_order_id`
/// is the docs' example value, which every example and the web send.
fn order_body(
    f: OrderFields<'_>,
    client_id: &str,
    priced: &Priced,
) -> serde_json::Map<String, Value> {
    let v = json!({
        "exchange": f.exchange,
        "instrument_token": f.token.trim(),
        "client_id": client_id,
        "order_type": priced.order_type,
        "price": priced.price,
        "quantity": f.quantity,
        "disclosed_quantity": f.disclosed_quantity,
        "validity": "DAY",
        "product": f.product,
        "order_side": f.side,
        "device": "WEB",
        "user_order_id": 1,
        "trigger_price": priced.trigger_price,
        "execution_type": "REGULAR",
    });
    match v {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    }
}

/// Body for `POST /api/v1/orders` (web `transform_data`).
pub(crate) fn place_body(o: &ResolvedOrder, client_id: &str, priced: &Priced) -> Value {
    let fields = OrderFields {
        exchange: o.exchange.as_str(),
        token: o.token(),
        quantity: o.quantity,
        disclosed_quantity: o.disclosed_quantity,
        product: o.product.as_str(),
        side: o.action.as_str(),
    };
    let mut body = order_body(fields, client_id, priced);
    body.insert("amo".into(), json!(false));
    Value::Object(body)
}

/// Body for `PUT /api/v1/orders` (web `transform_modify_order_data`; the
/// modify page's packet has no `amo`).
pub(crate) fn modify_body(m: &ResolvedModify, client_id: &str, priced: &Priced) -> Value {
    let fields = OrderFields {
        exchange: m.exchange.as_str(),
        token: m.token(),
        quantity: m.quantity,
        disclosed_quantity: m.disclosed_quantity,
        product: m.product.as_str(),
        side: m.action.as_str(),
    };
    let mut body = order_body(fields, client_id, priced);
    body.insert("oms_order_id".into(), json!(m.order_id));
    Value::Object(body)
}

// ---------------------------------------------------------------------------
// Books (fetch-pending-orders.md, fetch-completed-orders.md, trade-book.md,
// fetch-position-netwise.md, fetch-demat-holdings.md)
// ---------------------------------------------------------------------------

/// The OpenAlgo symbol of a Pocketful row (web `order_data.oa_symbol`): by
/// its trading symbol, then by its instrument token, else the trading
/// symbol as Pocketful sent it, so a row missing from the master never
/// shows up blank.
pub fn row_symbol(symbols: &SymbolResolver, brsymbol: &str, token: &str, exchange: &str) -> String {
    if !exchange.is_empty() {
        if !brsymbol.is_empty() {
            if let Some(s) = symbols.oa_symbol(brsymbol, exchange) {
                return s;
            }
        }
        if !token.is_empty() {
            if let Some(r) = symbols.by_token(exchange, token) {
                return r.symbol;
            }
        }
    }
    brsymbol.to_string()
}

/// Which of Pocketful's two order lists a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrderList {
    Pending,
    Completed,
}

/// OpenAlgo's status for one order row (web `order_data.order_status`).
/// The docs name only some statuses, so the list decides what the words
/// cannot: a pending row is still working whatever its word ("trigger
/// pending" when it mentions a trigger), and a completed row is finished:
/// complete, rejected or cancelled from its word, complete when it filled
/// in full, else Pocketful's own word.
pub(crate) fn order_status(o: &Value, list: OrderList) -> String {
    let status = text(o, "order_status").to_ascii_uppercase();
    if list == OrderList::Pending {
        return if status.contains("TRIGGER") {
            "trigger pending"
        } else {
            "open"
        }
        .into();
    }
    if status.contains("COMPLETE") {
        "complete".into()
    } else if status.contains("REJECT") {
        "rejected".into()
    } else if status.contains("CANCEL") {
        "cancelled".into()
    } else {
        let quantity = int(o, "quantity");
        if quantity > 0 && int(o, "filled_quantity") >= quantity {
            "complete".into()
        } else if status.is_empty() {
            "unknown".into()
        } else {
            status.to_lowercase()
        }
    }
}

/// Pocketful's epoch seconds (`order_entry_time`, `exchange_time`) as IST
/// `DD-MM-YYYY HH:MM:SS` (web `_ist_text`); empty for none.
pub(crate) fn ist_text(v: &Value, key: &str) -> String {
    let seconds = num(v, key);
    if seconds <= 0.0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|t| {
            t.with_timezone(&crate::clock::ist_offset())
                .format("%d-%m-%Y %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

fn side(v: &Value) -> String {
    text(v, "order_side").to_ascii_uppercase()
}

/// One order row (web `map_order_data` + `transform_order_data`).
fn map_order(o: &Value, list: OrderList, symbols: &SymbolResolver) -> Order {
    let exchange = text(o, "exchange");
    let symbol = row_symbol(
        symbols,
        &text(o, "trading_symbol"),
        &text(o, "instrument_token"),
        &exchange,
    );
    let status = order_status(o, list);
    let average = match num(o, "average_trade_price") {
        a if a != 0.0 => a,
        _ => num(o, "average_price"),
    };
    let reason = text(o, "rejection_reason");
    let exchange_order_id = text(o, "exchange_order_id");
    let exchange_time = ist_text(o, "exchange_time");
    let mut timestamp = ist_text(o, "order_entry_time");
    if timestamp.is_empty() {
        timestamp = exchange_time.clone();
    }
    Order {
        order_id: text(o, "oms_order_id"),
        exchange_order_id: (!exchange_order_id.is_empty()).then_some(exchange_order_id),
        symbol,
        exchange,
        side: side(o),
        quantity: i32_of(int(o, "quantity")),
        filled_quantity: i32_of(int(o, "filled_quantity")),
        pending_quantity: i32_of(int(o, "remaining_quantity")),
        price: num(o, "price"),
        trigger_price: num(o, "trigger_price"),
        average_price: average,
        order_type: oa_pricetype(&text(o, "order_type")),
        product: oa_product(&text(o, "product")),
        rejection_reason: (status == "rejected" && !reason.is_empty()).then_some(reason),
        status,
        validity: match text(o, "validity") {
            v if v.is_empty() => "DAY".into(),
            v => v,
        },
        order_timestamp: timestamp,
        exchange_timestamp: (!exchange_time.is_empty()).then_some(exchange_time),
        order_tag: None,
    }
}

/// The order book from the two lists (web `get_order_book`): pending first,
/// then completed. An order that finished between the two reads shows up
/// in both; the completed row, the newer one, is kept.
pub(crate) fn map_orders(
    pending: &[Value],
    completed: &[Value],
    symbols: &SymbolResolver,
) -> Vec<Order> {
    let finished: std::collections::HashSet<String> = completed
        .iter()
        .map(|o| text(o, "oms_order_id"))
        .filter(|id| !id.is_empty())
        .collect();
    pending
        .iter()
        .filter(|o| !finished.contains(&text(o, "oms_order_id")))
        .map(|o| map_order(o, OrderList::Pending, symbols))
        .chain(
            completed
                .iter()
                .map(|o| map_order(o, OrderList::Completed, symbols)),
        )
        .collect()
}

/// The quantity a trade row filled: `filled_quantity`, else `quantity`
/// (the trade-book page's only example has a fill quantity of 0).
fn trade_quantity(t: &Value) -> i64 {
    match int(t, "filled_quantity") {
        0 => int(t, "quantity"),
        q => q,
    }
}

/// The price a trade row filled at: `trade_price`, else the order's average
/// trade price, else its average.
fn trade_price(t: &Value) -> f64 {
    ["trade_price", "average_trade_price", "average_price"]
        .iter()
        .map(|k| num(t, k))
        .find(|p| *p > 0.0)
        .unwrap_or(0.0)
}

/// Trade rows (web `map_trade_data` + `transform_tradebook_data`).
pub(crate) fn map_trades(rows: &[Value], symbols: &SymbolResolver) -> Vec<Trade> {
    rows.iter()
        .map(|t| {
            let exchange = text(t, "exchange");
            let quantity = trade_quantity(t);
            let price = trade_price(t);
            let mut timestamp = ist_text(t, "exchange_time");
            if timestamp.is_empty() {
                timestamp = ist_text(t, "order_entry_time");
            }
            Trade {
                order_id: text(t, "oms_order_id"),
                trade_id: text_any(t, &["trade_id", "trade_number"]),
                symbol: row_symbol(
                    symbols,
                    &text(t, "trading_symbol"),
                    &text(t, "instrument_token"),
                    &exchange,
                ),
                exchange,
                product: oa_product(&text(t, "product")),
                side: side(t),
                quantity: i32_of(quantity),
                average_price: price,
                trade_value: round2(quantity as f64 * price),
                timestamp,
                order_tag: None,
            }
        })
        .collect()
}

/// A position's whole P&L (web `_position_pnl`): the net amount of its buys
/// and sells plus the open quantity at the LTP. `net_amount` is "Net amount
/// considering buy and sell transactions"; without it, sells less buys.
pub fn position_pnl(p: &Value) -> f64 {
    let multiplier = match num(p, "multiplier") {
        m if m != 0.0 => m,
        _ => 1.0,
    };
    let net_amount = match p.get("net_amount") {
        Some(v) if !v.is_null() => num(p, "net_amount"),
        _ => num(p, "sell_amount") - num(p, "buy_amount"),
    };
    net_amount + int(p, "net_quantity") as f64 * num(p, "ltp") * multiplier
}

/// (realised, unrealised) for one position row; they add up to its P&L
/// (web `position_pnl_split`). Realised is the matched quantity at the
/// row's own average sell less its average buy; the rest is open. A
/// position closed in part carries both. `realized_mtm` is not used: the
/// docs' own example shows 0 on a closed, profitable position.
pub fn position_pnl_split(p: &Value) -> (f64, f64) {
    let total = position_pnl(p);
    let (bought, sold) = (num(p, "buy_quantity"), num(p, "sell_quantity"));
    let matched = bought.min(sold);
    if matched <= 0.0 {
        return (0.0, total);
    }
    let realised = matched * (num(p, "sell_amount") / sold - num(p, "buy_amount") / bought);
    (realised, total - realised)
}

/// Net position rows (web `map_position_data` + `transform_positions_data`).
pub(crate) fn map_positions(rows: &[Value], symbols: &SymbolResolver) -> Vec<Position> {
    rows.iter()
        .map(|p| {
            let exchange = text(p, "exchange");
            let net = int(p, "net_quantity");
            // The average of the open side: a long carries the buy
            // average, a short the sell average.
            let open_side = match net {
                n if n > 0 => num(p, "average_buy_price"),
                n if n < 0 => num(p, "average_sell_price"),
                _ => 0.0,
            };
            let average = if open_side != 0.0 {
                open_side
            } else {
                num(p, "average_price")
            };
            let (realised, unrealised) = position_pnl_split(p);
            Position {
                symbol: row_symbol(
                    symbols,
                    &text(p, "trading_symbol"),
                    &text(p, "instrument_token"),
                    &exchange,
                ),
                exchange,
                product: oa_product(&text(p, "product")),
                quantity: i32_of(net),
                overnight_quantity: i32_of(int(p, "cf_buy_quantity") - int(p, "cf_sell_quantity")),
                average_price: round2(average),
                ltp: round2(num(p, "ltp")),
                pnl: round2(position_pnl(p)),
                realized_pnl: round2(realised),
                unrealized_pnl: round2(unrealised),
                buy_quantity: i32_of(int(p, "buy_quantity")),
                buy_value: num(p, "buy_amount"),
                sell_quantity: i32_of(int(p, "sell_quantity")),
                sell_value: num(p, "sell_amount"),
                average_price_basis: None,
            }
        })
        .collect()
}

/// Demat holdings (web `map_portfolio_data` + `transform_holdings_data`).
/// The row's own `trading_symbol` is the bare name ("SBIN"); the broker
/// symbol the master stores ("SBIN-EQ") is in
/// `instrument_details.trading_symbol`.
pub(crate) fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    rows.iter()
        .map(|h| {
            let exchange = text(h, "exchange");
            let details = &h["instrument_details"];
            let brsymbol = match text(details, "trading_symbol") {
                s if s.is_empty() => text(h, "trading_symbol"),
                s => s,
            };
            let token = match text(h, "token") {
                t if t.is_empty() => text(details, "instrument_token"),
                t => t,
            };
            let quantity = int(h, "quantity");
            let average = num(h, "buy_avg");
            let ltp = num(h, "ltp");
            let pnl_pct = if average != 0.0 {
                (ltp - average) / average * 100.0
            } else {
                0.0
            };
            let isin = text(h, "isin");
            Holding {
                symbol: row_symbol(symbols, &brsymbol, &token, &exchange),
                exchange,
                // Demat holdings are delivery holdings.
                product: "CNC".into(),
                isin: (!isin.is_empty()).then_some(isin),
                quantity: i32_of(quantity),
                t1_quantity: i32_of(int(h, "t1_quantity")),
                average_price: round2(average),
                ltp: round2(ltp),
                close_price: num(h, "previous_close"),
                pnl: round2((ltp - average) * quantity as f64),
                pnl_percentage: round2(pnl_pct),
                current_value: round2(ltp * quantity as f64),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Funds
// ---------------------------------------------------------------------------

/// web `get_margin_data`: `data.values` is a list of `[label, value]`.
pub fn map_funds(body: &Value) -> Funds {
    let values = body["data"]["values"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let lookup = |label: &str| -> f64 {
        values
            .iter()
            .find(|pair| pair.get(0).and_then(Value::as_str) == Some(label))
            .and_then(|pair| pair.get(1).and_then(as_f64))
            .unwrap_or(0.0)
    };
    let available = lookup("Available Margin");
    let used = lookup("Margin Used");
    Funds {
        available_cash: round2(available),
        used_margin: round2(used),
        collateral: round2(lookup("Total Pledge Collateral")),
        m2m_unrealized: round2(lookup("unrealized_mtm")),
        m2m_realized: round2(lookup("realized_mtm")),
        utilised_debits: round2(used),
        ..Default::default()
    }
}
