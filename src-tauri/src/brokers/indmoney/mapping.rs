//! OpenAlgo <-> INDstocks vocabulary and book normalisers (web
//! `mapping/transform_data.py`, `mapping/order_data.py`).
//!
//! Books are read as JSON values because INDstocks leaves inapplicable
//! price fields as `""`, sends numbers as strings with thousands commas, and
//! has changed field names between documented and live payloads.

use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{Holding, Order, Position, Trade};
use serde_json::Value;

// ---------------------------------------------------------------------------
// OpenAlgo -> INDstocks
// ---------------------------------------------------------------------------

/// `exchange` field of an order: F&O folds to its NSE/BSE parent.
pub fn api_exchange(exchange: &str) -> &'static str {
    match exchange {
        "BSE" | "BFO" | "BCD" => "BSE",
        "MCX" => "MCX",
        _ => "NSE",
    }
}

/// `segment` field of an order / margin leg.
pub fn segment(exchange: &str) -> &'static str {
    match exchange {
        "NFO" | "BFO" | "CDS" | "BCD" | "MCX" => "DERIVATIVE",
        _ => "EQUITY",
    }
}

/// Segment of an order id (`DRV-` derivative, else equity).
pub fn segment_from_order_id(order_id: &str) -> &'static str {
    if order_id.starts_with("DRV-") {
        "DERIVATIVE"
    } else {
        "EQUITY"
    }
}

pub fn product(p: Product) -> &'static str {
    match p {
        Product::Cnc => "CNC",
        Product::Nrml => "MARGIN",
        Product::Mis => "INTRADAY",
    }
}

/// `MARKET`/`LIMIT` go to `/order`; SL and SL-M are `TRIGGER` orders on
/// `/smart/order`.
pub fn order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl | PriceType::SlM => "TRIGGER",
    }
}

/// `algo_id`: `99999` on NSE, `9999999999999999` on BSE.
pub fn algo_id(api_exchange: &str) -> &'static str {
    if api_exchange == "BSE" {
        "9999999999999999"
    } else {
        "99999"
    }
}

/// Scrip-code segment for the quote and history APIs.
pub fn scrip_segment(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" => "NSE",
        "BSE" => "BSE",
        "NFO" => "NFO",
        "BFO" => "BFO",
        "MCX" => "MCX",
        "CDS" => "CDS",
        "BCD" => "BCD",
        "NSE_INDEX" => "NIDX",
        "BSE_INDEX" => "BIDX",
        _ => return None,
    })
}

/// Segment prefix on the price WebSocket (`SEGMENT:TOKEN`).
pub fn ws_segment(exchange: &str) -> Option<&'static str> {
    Some(match exchange {
        "NSE" => "NSE",
        "NFO" => "NFO",
        "BSE" => "BSE",
        "BFO" => "BFO",
        "NSE_INDEX" => "NIDX",
        "BSE_INDEX" => "BIDX",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// INDstocks -> OpenAlgo
// ---------------------------------------------------------------------------

const COMPLETED: &[&str] = &["SUCCESS", "TRADED", "COMPLETE", "EXECUTED"];
pub const OPEN: &[&str] = &[
    "QUEUED",
    "O-PENDING",
    "PENDING",
    "PROCESSING",
    "INITIATED",
    "MODIFIED",
    "PARTIALLY FILLED",
    "PARTIALLY EXECUTED",
];
pub const TRIGGER_PENDING: &[&str] = &["SL-PENDING"];
const REJECTED: &[&str] = &["REJECTED", "FAILED", "ABORTED"];
const CANCELLED: &[&str] = &[
    "CANCELLED",
    "EXPIRED",
    "PARTIALLY FILLED - CANCELLED",
    "PARTIALLY FILLED - EXPIRED",
];

/// Order status -> OpenAlgo (unknown values lowercased).
pub fn map_status(status: &str) -> String {
    let s = status.trim().to_ascii_uppercase();
    let s = s.as_str();
    if COMPLETED.contains(&s) {
        "complete".into()
    } else if TRIGGER_PENDING.contains(&s) {
        "trigger pending".into()
    } else if OPEN.contains(&s) {
        "open".into()
    } else if REJECTED.contains(&s) {
        "rejected".into()
    } else if CANCELLED.contains(&s) {
        "cancelled".into()
    } else {
        s.to_ascii_lowercase()
    }
}

/// Raw statuses cancel-all acts on.
pub fn is_cancellable(status: &str) -> bool {
    let s = status.trim().to_ascii_uppercase();
    OPEN.contains(&s.as_str()) || TRIGGER_PENDING.contains(&s.as_str())
}

/// Broker order types -> OpenAlgo price types (the book reports a TRIGGER
/// order back as `GTT_LIMIT`).
pub fn map_order_type(t: &str) -> String {
    let u = t.trim().to_ascii_uppercase();
    match u.as_str() {
        "MARKET" => "MARKET",
        "LIMIT" => "LIMIT",
        "STOP_LOSS" | "TRIGGER" | "GTT_LIMIT" => "SL",
        "STOP_LOSS_MARKET" | "GTT_MARKET" => "SL-M",
        "OCO" => "OCO",
        _ => return u,
    }
    .to_string()
}

/// Order types that live on the `/smart/order` endpoints.
pub fn is_smart_type(t: &str) -> bool {
    let u = t.trim().to_ascii_uppercase();
    u == "TRIGGER" || u == "OCO" || u.starts_with("GTT_")
}

/// web `map_product_to_openalgo`.
pub fn map_product(product: &str, exchange: &str) -> String {
    match product.trim().to_ascii_uppercase().as_str() {
        "INTRADAY" => "MIS",
        "CNC" | "DELIVERY" => "CNC",
        "MARGIN" => "NRML",
        _ if matches!(exchange, "NFO" | "BFO" | "CDS" | "BCD" | "MCX") => "NRML",
        _ => "MIS",
    }
    .to_string()
}

/// web `resolve_exchange`: the `(exchange, segment)` pair names the venue;
/// when it does not, the token is probed against the master.
pub fn resolve_exchange(
    symbols: &SymbolResolver,
    token: &str,
    exchange: &str,
    segment: &str,
) -> String {
    let exch = exchange.trim().to_ascii_uppercase();
    let seg = segment.trim().to_ascii_uppercase();
    let legacy = match exch.as_str() {
        "NSE_EQ" => Some("NSE"),
        "BSE_EQ" => Some("BSE"),
        "NSE_FNO" | "NSE_FO" => Some("NFO"),
        "BSE_FNO" | "BSE_FO" => Some("BFO"),
        _ => None,
    };
    if let Some(l) = legacy {
        return l.into();
    }
    match (exch.as_str(), seg.as_str()) {
        ("NSE", "EQUITY") => return "NSE".into(),
        ("BSE", "EQUITY") => return "BSE".into(),
        ("NSE", "DERIVATIVE") => return "NFO".into(),
        ("BSE", "DERIVATIVE") => return "BFO".into(),
        _ => {}
    }
    let token = token.trim();
    if !token.is_empty() {
        let mut candidates: Vec<&str> = match seg.as_str() {
            "DERIVATIVE" => vec!["NFO", "BFO"],
            "EQUITY" => vec!["NSE", "BSE"],
            _ => vec!["NFO", "BFO", "NSE", "BSE"],
        };
        if exch == "NSE" {
            candidates.retain(|c| matches!(*c, "NSE" | "NFO"));
        } else if exch == "BSE" {
            candidates.retain(|c| matches!(*c, "BSE" | "BFO"));
        }
        for c in candidates {
            if symbols.by_token(c, token).is_some() {
                return c.into();
            }
        }
    }
    tracing::debug!(
        "INDmoney row venue unresolved (exchange {:?}, segment {:?}); using NSE",
        exchange,
        segment
    );
    "NSE".into()
}

// ---------------------------------------------------------------------------
// Lenient JSON access
// ---------------------------------------------------------------------------

/// Text of a field (numbers rendered), empty when absent or null.
pub fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// Number from a number or a string with thousands commas (web
/// `_clean_number` / `_as_float`); 0 otherwise.
pub fn num_value(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.replace(',', "").trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

pub fn num(v: &Value, key: &str) -> f64 {
    num_value(v.get(key))
}

/// First key present (and not null / empty) among `keys`.
pub fn first<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| match v.get(*k) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.trim().is_empty() => None,
        Some(x) => Some(x),
    })
}

/// web `_first_price`: first value that parses as a non-zero price.
pub fn first_price(v: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .map(|k| num_value(v.get(*k)))
        .find(|p| *p != 0.0)
        .unwrap_or(0.0)
}

fn int(v: f64) -> i32 {
    v as i32
}

fn symbol_for(symbols: &SymbolResolver, token: &str, exchange: &str, fallback: &str) -> String {
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(exchange, token) {
            return r.symbol;
        }
        tracing::debug!(
            "No OpenAlgo symbol for INDmoney token {} on {}",
            token,
            exchange
        );
    }
    fallback.to_string()
}

/// Rows of a list answer (also tolerates `{net_positions, day_positions}`).
pub fn rows(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.iter().filter(|x| x.is_object()).cloned().collect(),
        Value::Object(o) if o.contains_key("net_positions") || o.contains_key("day_positions") => {
            let mut out = Vec::new();
            for k in ["net_positions", "day_positions"] {
                if let Some(Value::Array(a)) = o.get(k) {
                    out.extend(a.iter().filter(|x| x.is_object()).cloned());
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// One order-book row (web `map_order_data` + `transform_order_data`).
pub fn map_order(symbols: &SymbolResolver, o: &Value) -> Order {
    let token = text(o, "security_id");
    let exchange = resolve_exchange(symbols, &token, &text(o, "exchange"), &text(o, "segment"));
    let quantity = int(num(o, "requested_qty"));
    let status = map_status(&text(o, "status"));
    let filled = {
        let f = int(first_price(
            o,
            &["traded_qty", "filled_qty", "filled_quantity"],
        ));
        if f == 0 && status == "complete" {
            quantity
        } else {
            f
        }
    };
    let reason = text(o, "error_message");
    Order {
        order_tag: None,
        order_id: text(o, "id"),
        exchange_order_id: Some(text(o, "exch_order_id")).filter(|s| !s.is_empty()),
        symbol: symbol_for(symbols, &token, &exchange, &text(o, "name")),
        product: map_product(&text(o, "product"), &exchange),
        exchange,
        side: text(o, "txn_type").to_ascii_uppercase(),
        quantity,
        filled_quantity: filled,
        pending_quantity: (quantity - filled).max(0),
        price: first_price(o, &["requested_price", "tgt_limit_price", "sl_limit_price"]),
        trigger_price: first_price(o, &["sl_trigger_price", "tgt_trigger_price"]),
        average_price: first_price(
            o,
            &[
                "avg_traded_price",
                "traded_price",
                "average_price",
                "avg_price",
            ],
        ),
        order_type: map_order_type(&text(o, "order_type")),
        status,
        validity: Some(text(o, "validity"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "DAY".into())
            .to_ascii_uppercase(),
        order_timestamp: text(o, "created_at"),
        exchange_timestamp: None,
        rejection_reason: Some(reason).filter(|s| !s.is_empty()),
    }
}

/// Facts a trade borrows from its order (the trade book has no side,
/// product or exchange).
#[derive(Debug, Clone, Default)]
pub struct OrderFacts {
    pub txn_type: String,
    pub product: String,
    pub exchange: String,
}

/// Index the raw order book by exchange order id and internal id.
pub fn order_facts(book: &[Value]) -> std::collections::HashMap<String, OrderFacts> {
    let mut m = std::collections::HashMap::new();
    for o in book {
        let f = OrderFacts {
            txn_type: text(o, "txn_type"),
            product: text(o, "product"),
            exchange: text(o, "exchange"),
        };
        for k in ["exch_order_id", "id"] {
            let id = text(o, k);
            if !id.is_empty() {
                m.insert(id, f.clone());
            }
        }
    }
    m
}

/// One trade-book row (web `map_trade_data` + `transform_tradebook_data`).
/// `segment` is the query the row came from.
pub fn map_trade(
    symbols: &SymbolResolver,
    t: &Value,
    segment: &str,
    facts: &std::collections::HashMap<String, OrderFacts>,
) -> Trade {
    let token = text(t, "scrip_code");
    let exch_order_id = text(t, "exch_order_id");
    let f = facts.get(&exch_order_id).cloned().unwrap_or_default();
    let exchange = resolve_exchange(symbols, &token, &f.exchange, segment);
    let qty = num(t, "quantity");
    let price = num(t, "price");
    Trade {
        order_tag: None,
        order_id: exch_order_id,
        trade_id: text(t, "fill_id"),
        symbol: symbol_for(symbols, &token, &exchange, &token),
        product: map_product(&f.product, &exchange),
        side: f.txn_type.to_ascii_uppercase(),
        exchange,
        quantity: int(qty),
        average_price: price,
        trade_value: qty * price,
        timestamp: text(t, "trade_date"),
    }
}

/// Exchange of a position row (`segment`, else the query segment).
pub fn position_exchange(symbols: &SymbolResolver, p: &Value) -> String {
    let token = text(p, "security_id");
    let exch = Some(text(p, "exchange"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(p, "exchange_segment"));
    let seg = Some(text(p, "segment"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(p, "query_segment"));
    resolve_exchange(symbols, &token, &exch, &seg)
}

pub fn position_qty(p: &Value) -> i64 {
    num_value(first(p, &["net_qty", "net_quantity"])) as i64
}

/// One position row (web `map_position_data` + `transform_positions_data`).
pub fn map_position(symbols: &SymbolResolver, p: &Value) -> Position {
    let token = text(p, "security_id");
    let exchange = position_exchange(symbols, p);
    let net = num_value(first(p, &["net_quantity", "net_qty"]));
    let avg = num_value(first(p, &["average_price", "avg_price"]));
    let ltp = first_price(
        p,
        &["last_traded_price", "ltp", "current_price", "market_price"],
    );
    let multiplier = match p.get("multiplier") {
        Some(m) if num_value(Some(m)) != 0.0 => num_value(Some(m)),
        _ => 1.0,
    };
    let realized = num(p, "realized_profit");
    let explicit =
        first(p, &["pnl_absolute", "pnl", "unrealized_profit"]).map(|v| num_value(Some(v)));
    let unrealized = if net != 0.0 && ltp != 0.0 {
        (ltp - avg) * net * multiplier
    } else {
        0.0
    };
    let pnl = explicit.unwrap_or(realized + unrealized);
    let product = match text(p, "query_product").as_str() {
        "intraday" => "MIS".to_string(),
        "cnc" => "CNC".to_string(),
        "margin" => "NRML".to_string(),
        _ => match text(p, "product").as_str() {
            "INTRADAY" => "MIS".to_string(),
            "DELIVERY" | "CNC" => "CNC".to_string(),
            "MARGIN" => "NRML".to_string(),
            _ if matches!(exchange.as_str(), "NFO" | "MCX" | "BFO" | "CDS") => "NRML".to_string(),
            _ => "MIS".to_string(),
        },
    };
    let fallback = Some(text(p, "trading_symbol"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(p, "symbol"));
    Position {
        symbol: symbol_for(symbols, &token, &exchange, &fallback),
        exchange,
        product,
        quantity: int(net),
        overnight_quantity: 0,
        average_price: avg,
        ltp,
        pnl,
        realized_pnl: realized,
        unrealized_pnl: if explicit.is_some() {
            pnl - realized
        } else {
            unrealized
        },
        buy_quantity: int(num(p, "buy_qty")),
        buy_value: num(p, "buy_value"),
        sell_quantity: int(num(p, "sell_qty")),
        sell_value: num(p, "sell_value"),
    }
}

/// One holding row (web `map_portfolio_data` + `transform_holdings_data`):
/// exchange NSE, product CNC; INDstocks sends no live price, so the web
/// marks at the average price and reports zero P&L.
pub fn map_holding(symbols: &SymbolResolver, h: &Value) -> Holding {
    let token = text(h, "security_id");
    let qty = num(h, "total_qty");
    let avg = num(h, "avg_price");
    Holding {
        symbol: symbol_for(symbols, &token, "NSE", &text(h, "symbol")),
        exchange: "NSE".into(),
        product: "CNC".into(),
        isin: Some(text(h, "isin")).filter(|s| !s.is_empty()),
        quantity: int(qty),
        t1_quantity: int(num(h, "t1_qty")),
        average_price: avg,
        ltp: avg,
        close_price: 0.0,
        pnl: 0.0,
        pnl_percentage: 0.0,
        current_value: qty * avg,
    }
}

/// Exchanges the order API can place on (web `transform_data` guard).
pub fn exchange_is_placeable(exchange: Exchange) -> bool {
    matches!(
        exchange,
        Exchange::Nse
            | Exchange::Bse
            | Exchange::Nfo
            | Exchange::Bfo
            | Exchange::Cds
            | Exchange::Bcd
    )
}
