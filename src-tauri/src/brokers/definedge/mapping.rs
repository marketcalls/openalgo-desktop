//! OpenAlgo <-> Definedge translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `streaming/definedge_order_adapter.py`).

use super::{int, num, text};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Map, Value};

/// SEBI generic algo id for unregistered retail algos (web
/// `GENERIC_ALGO_ID_NSE` / `_BSE`; the web's `DEFINEDGE_ALGO_ID` override
/// is an environment variable, which the desktop does not read).
pub const GENERIC_ALGO_ID_NSE: &str = "99999";
pub const GENERIC_ALGO_ID_BSE: &str = "9999999999999999";

/// web `get_algo_id`.
pub fn algo_id(exchange: &str) -> &'static str {
    if matches!(exchange, "BSE" | "BFO" | "BCD") {
        GENERIC_ALGO_ID_BSE
    } else {
        GENERIC_ALGO_ID_NSE
    }
}

/// web `map_product_type` (CNC equity only, INTRADAY both, NORMAL derivatives).
pub fn product_code(p: Product) -> &'static str {
    match p {
        Product::Mis => "INTRADAY",
        Product::Cnc => "CNC",
        Product::Nrml => "NORMAL",
    }
}

/// web `map_price_type`.
pub fn price_type_code(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "SL-LIMIT",
        PriceType::SlM => "SL-MARKET",
    }
}

/// Broker product -> OpenAlgo (web `map_order_data` / `map_trade_data`):
/// INTRADAY -> MIS; NORMAL -> CNC on NSE/BSE, NRML on derivatives; CNC
/// stays; anything else passes through.
pub fn book_product(exchange: &str, product: &str) -> String {
    match product {
        "INTRADAY" => "MIS".into(),
        "NORMAL" if matches!(exchange, "NSE" | "BSE") => "CNC".into(),
        "NORMAL" => "NRML".into(),
        other => other.to_string(),
    }
}

/// Broker price type -> OpenAlgo (web `transform_order_data`).
pub fn book_price_type(p: &str) -> String {
    match p {
        "SL-LIMIT" => "SL".into(),
        "SL-MARKET" => "SL-M".into(),
        other => other.to_string(),
    }
}

/// web `transform_order_data` status rules: COMPLETE/EXECUTED -> complete;
/// REJECTED -> rejected; OPEN/NEW/REPLACED/PENDING/TRIGGER PENDING ->
/// open (a resting SL order is still modifiable); CANCELED/CANCELLED ->
/// cancelled; anything else lowercased.
pub fn order_status(raw: &str) -> String {
    let s = raw.trim().to_ascii_uppercase();
    match s.as_str() {
        "COMPLETE" | "EXECUTED" => "complete".into(),
        "REJECTED" => "rejected".into(),
        "OPEN" | "NEW" | "REPLACED" | "PENDING" | "TRIGGER PENDING" | "TRIGGER_PENDING" => {
            "open".into()
        }
        "CANCELED" | "CANCELLED" => "cancelled".into(),
        _ => s.to_ascii_lowercase(),
    }
}

/// Statuses `cancel_all_orders_api` treats as cancellable (lowercased).
pub const CANCELLABLE: &[&str] = &[
    "open",
    "new",
    "replaced",
    "trigger pending",
    "pending",
    "open pending",
    "trigger_pending",
];

pub fn is_cancellable(o: &Value) -> bool {
    let s = text(o, "status").to_ascii_lowercase();
    let os = text(o, "order_status").to_ascii_lowercase();
    CANCELLABLE.contains(&s.as_str()) || CANCELLABLE.contains(&os.as_str())
}

/// Order id under any of the web's spellings.
pub fn order_id(o: &Value) -> String {
    ["order_id", "norenordno", "orderid"]
        .into_iter()
        .map(|k| text(o, k))
        .find(|s| !s.is_empty())
        .unwrap_or_default()
}

/// Python `str(float)`-like price text: `100.0`, `100.5`.
pub fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// web `transform_data` place-order body.
pub fn place_body(o: &ResolvedOrder) -> Value {
    let pt = price_type_code(o.pricetype);
    let mut m = Map::new();
    m.insert("tradingsymbol".into(), json!(o.brsymbol()));
    m.insert("exchange".into(), json!(o.exchange.as_str()));
    m.insert("quantity".into(), json!(o.quantity));
    if matches!(pt, "MARKET" | "SL-MARKET") {
        m.insert("price".into(), json!("0"));
    } else {
        m.insert("price".into(), json!(o.price));
    }
    m.insert("price_type".into(), json!(pt));
    m.insert("product_type".into(), json!(product_code(o.product)));
    m.insert("order_type".into(), json!(o.action.as_str()));
    m.insert("algo_id".into(), json!(algo_id(o.exchange.as_str())));
    if o.trigger_price != 0.0 && matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        m.insert("trigger_price".into(), json!(o.trigger_price));
    }
    if o.disclosed_quantity != 0 {
        m.insert("disclosed_quantity".into(), json!(o.disclosed_quantity));
    }
    Value::Object(m)
}

/// web `transform_modify_order_data` (every value a string, validity DAY,
/// trigger only for SL/SL-M with a non-zero value).
pub fn modify_body(m: &ResolvedModify) -> Value {
    let mut b = Map::new();
    b.insert("order_id".into(), json!(m.order_id));
    b.insert("tradingsymbol".into(), json!(m.brsymbol()));
    b.insert("exchange".into(), json!(m.exchange.as_str()));
    b.insert("quantity".into(), json!(m.quantity.to_string()));
    b.insert("price".into(), json!(py_float(m.price)));
    b.insert("price_type".into(), json!(price_type_code(m.pricetype)));
    b.insert("product_type".into(), json!(product_code(m.product)));
    b.insert("order_type".into(), json!(m.action.as_str()));
    if m.trigger_price != 0.0 && matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        b.insert("trigger_price".into(), json!(py_float(m.trigger_price)));
    }
    if m.disclosed_quantity != 0 {
        b.insert(
            "disclosed_quantity".into(),
            json!(m.disclosed_quantity.to_string()),
        );
    }
    b.insert("validity".into(), json!("DAY"));
    Value::Object(b)
}

/// The rows of a book envelope (`orders` / `trades` / `positions` /
/// `data`), or the body itself when it is a list.
pub fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    if let Some(a) = v.as_array() {
        return a;
    }
    v.get(key)
        .or_else(|| v.get("data"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn clamp_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn oa_symbol(symbols: &SymbolResolver, brsymbol: &str, exchange: &str) -> String {
    symbols.oa_symbol_or_raw(brsymbol, exchange)
}

/// One order-book row (web `map_order_data` + `transform_order_data`).
pub fn map_order(o: &Value, symbols: &SymbolResolver) -> Order {
    let exchange = text(o, "exchange");
    let ts = Some(text(o, "exchange_time"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(o, "order_entry_time"));
    let reason = text(o, "message");
    Order {
        order_id: text(o, "order_id"),
        exchange_order_id: Some(text(o, "exchange_orderid")).filter(|s| !s.is_empty()),
        symbol: oa_symbol(symbols, &text(o, "tradingsymbol"), &exchange),
        exchange: exchange.clone(),
        side: text(o, "order_type").to_ascii_uppercase(),
        quantity: clamp_i32(int(o, "quantity")),
        filled_quantity: clamp_i32(int(o, "filled_qty")),
        pending_quantity: clamp_i32(int(o, "pending_qty")),
        price: num(o, "price"),
        trigger_price: num(o, "trigger_price"),
        average_price: num(o, "average_traded_price"),
        order_type: book_price_type(&text(o, "price_type")),
        product: book_product(&exchange, &text(o, "product_type")),
        status: order_status(&text(o, "order_status")),
        validity: Some(text(o, "validity"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "DAY".into()),
        order_timestamp: ts,
        exchange_timestamp: Some(text(o, "exchange_time")).filter(|s| !s.is_empty()),
        rejection_reason: Some(reason).filter(|s| !s.is_empty()),
    }
}

/// One trade-book row (web `map_trade_data` + `transform_tradebook_data`).
pub fn map_trade(t: &Value, symbols: &SymbolResolver) -> Trade {
    let exchange = text(t, "exchange");
    let qty = if t.get("filled_qty").is_some() {
        int(t, "filled_qty")
    } else {
        int(t, "quantity")
    };
    let mut price = num(t, "fill_price");
    if price == 0.0 {
        price = if t.get("average_traded_price").is_some() {
            num(t, "average_traded_price")
        } else {
            num(t, "price")
        };
    }
    let ts = Some(text(t, "fill_time"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(t, "exchange_time"));
    Trade {
        order_id: text(t, "order_id"),
        trade_id: Some(text(t, "fill_id"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text(t, "exchange_trade_id")),
        symbol: oa_symbol(symbols, &text(t, "tradingsymbol"), &exchange),
        exchange: exchange.clone(),
        product: book_product(&exchange, &text(t, "product_type")),
        side: text(t, "order_type").to_ascii_uppercase(),
        quantity: clamp_i32(qty),
        average_price: round2(price),
        trade_value: round2(qty as f64 * price),
        timestamp: ts,
    }
}

fn round2(v: f64) -> f64 {
    crate::brokers::common::mpp::py_round(v, 2)
}

/// One position row (web `map_position_data` + `transform_positions_data`):
/// pnl is the realized P&L of a closed position, the unrealized otherwise.
pub fn map_position(p: &Value, symbols: &SymbolResolver) -> Position {
    let exchange = text(p, "exchange");
    let qty = int(p, "net_quantity");
    let realized = num(p, "realized_pnl");
    let unrealized = num(p, "unrealized_pnl");
    Position {
        symbol: oa_symbol(symbols, &text(p, "tradingsymbol"), &exchange),
        exchange: exchange.clone(),
        product: book_product(&exchange, &text(p, "product_type")),
        quantity: clamp_i32(qty),
        overnight_quantity: 0,
        average_price: num(p, "net_averageprice"),
        ltp: num(p, "lastPrice"),
        pnl: if qty == 0 { realized } else { unrealized },
        realized_pnl: realized,
        unrealized_pnl: unrealized,
        buy_quantity: clamp_i32(int(p, "day_buy_quantity")),
        buy_value: num(p, "day_buy_value"),
        sell_quantity: clamp_i32(int(p, "day_sell_quantity")),
        sell_value: num(p, "day_sell_value"),
    }
}

/// Holdings (web `map_portfolio_data` + `transform_holdings_data`). Each
/// holding lists one trading symbol per exchange; the first is used. The
/// holdings API carries no last price, so like the web the current value
/// is the invested value and P&L is 0 (`ltp` = average price keeps the
/// portfolio totals equal to the web's).
pub fn map_holdings(rows: &[Value], symbols: &SymbolResolver) -> Vec<Holding> {
    let mut out = Vec::new();
    for h in rows {
        let dp = num(h, "dp_qty");
        let t1 = num(h, "t1_qty");
        let total = dp + t1;
        if total == 0.0 {
            continue;
        }
        let first = h
            .get("tradingsymbol")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Null);
        let exchange = text(&first, "exchange");
        let brsymbol = text(&first, "tradingsymbol");
        let avg = num(h, "avg_buy_price");
        let qty = clamp_i32(total as i64);
        out.push(Holding {
            symbol: if brsymbol.is_empty() {
                String::new()
            } else {
                oa_symbol(symbols, &brsymbol, &exchange)
            },
            exchange,
            product: "CNC".into(),
            isin: Some(text(&first, "isin")).filter(|s| !s.is_empty()),
            quantity: qty,
            t1_quantity: clamp_i32(t1 as i64),
            average_price: avg,
            ltp: avg,
            close_price: 0.0,
            pnl: 0.0,
            pnl_percentage: 0.0,
            current_value: total * avg,
        });
    }
    out
}

/// Order-stream status (web `definedge_order_adapter._STATUS_MAP`).
pub fn stream_status(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "complete" | "executed" => "complete".into(),
        "open" | "new" | "replaced" | "pending" => "open".into(),
        "trigger_pending" | "trigger pending" => "trigger pending".into(),
        "rejected" => "rejected".into(),
        "canceled" | "cancelled" => "cancelled".into(),
        "" => "open".into(),
        other => other.to_string(),
    }
}

/// One `t == "om"` frame (Noren order fields) -> OpenAlgo order update
/// (web `definedge_order_adapter.normalize`).
pub fn order_update(d: &Value, symbols: &SymbolResolver) -> OrderUpdate {
    let exchange = text(d, "exch");
    let status = stream_status(&text(d, "status"));
    let qty = int(d, "qty");
    let fill = int(d, "fillshares");
    let tr = text(d, "trantype");
    let pt = text(d, "prctyp");
    let prd = text(d, "prd");
    let oa_exchange = exchange.parse::<Exchange>().ok();
    OrderUpdate {
        orderid: text(d, "norenordno"),
        symbol: match oa_exchange {
            Some(_) => symbols.oa_symbol_or_raw(&text(d, "tsym"), &exchange),
            None => text(d, "tsym"),
        },
        exchange,
        action: match tr.as_str() {
            "B" => "BUY".into(),
            "S" => "SELL".into(),
            _ => tr,
        },
        quantity: qty,
        price: num(d, "prc"),
        trigger_price: num(d, "trgprc"),
        pricetype: match pt.as_str() {
            "LMT" => "LIMIT".into(),
            "MKT" => "MARKET".into(),
            "SL-LMT" => "SL".into(),
            "SL-MKT" => "SL-M".into(),
            _ => pt,
        },
        product: match prd.as_str() {
            "C" => "CNC".into(),
            "M" => "NRML".into(),
            "I" => "MIS".into(),
            _ => prd,
        },
        filled_quantity: fill,
        pending_quantity: (qty - fill).max(0),
        average_price: num(d, "avgprc"),
        rejection_reason: if status == "rejected" {
            text(d, "rejreason")
        } else {
            String::new()
        },
        order_status: status,
    }
}
