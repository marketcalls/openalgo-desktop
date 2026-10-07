//! OpenAlgo <-> Nubra V3 translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `mapping/margin_data.py`).
//!
//! * Order items: `refId` (the master token), `qty`, `side`, `deliveryType`
//!   (`CNC` / `IDAY`; NRML collapses to CNC), `priceType`, `validityType`
//!   (`IOC` for MARKET families, else `DAY`), `isMultiLeg: false`,
//!   `executionMode: "ENTRY"`, `stratTags: [tag]`, `entryPrice` (LIMIT
//!   only) and for SL / SL-M an LTP entry trigger (`atOrAbove` for BUY,
//!   `atOrBelow` for SELL). Prices are integer paise.
//! * Nubra has only NSE / BSE / MCX: an F&O row reports `NSE` with
//!   `derivativeType` OPT/FUT and is folded to NFO (BSE to BFO). Every row
//!   is resolved against the master by `refId` first, then the broker
//!   symbol, so books carry OpenAlgo symbols.
//! * Order status comes from the bucket (`open`, `executed`, `cancelled`,
//!   `rejected`, `expired`, `gtt`); a working stop order is `trigger
//!   pending`.

use crate::brokers::common::mapping::{Action, PriceType, Product};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------------

/// Rupees -> integer paise (web `_paise`).
pub fn paise(rupees: f64) -> i64 {
    if !rupees.is_finite() {
        return 0;
    }
    (rupees * 100.0).round() as i64
}

/// web `sanitize_strat_tag`: one lowercase tag, runs of anything outside
/// `[A-Za-z0-9]` become `-`.
pub fn sanitize_strat_tag(tag: Option<&str>) -> String {
    let src = tag.unwrap_or("openalgo");
    let mut out = String::new();
    let mut dash = false;
    for c in src.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    if out.is_empty() {
        "openalgo".to_string()
    } else {
        out
    }
}

pub fn side(action: Action) -> &'static str {
    match action {
        Action::Buy => "BUY",
        Action::Sell => "SELL",
    }
}

/// OpenAlgo product -> `deliveryType`.
pub fn delivery_type(product: Product) -> &'static str {
    match product {
        Product::Cnc | Product::Nrml => "CNC",
        Product::Mis => "IDAY",
    }
}

/// `deliveryType` -> OpenAlgo product (web `reverse_map_product_type`).
pub fn product_from(delivery: &str) -> &'static str {
    match delivery.trim().to_ascii_uppercase().as_str() {
        "CNC" => "CNC",
        _ => "MIS",
    }
}

/// OpenAlgo price type -> `priceType`.
pub fn price_type(pt: PriceType) -> &'static str {
    match pt {
        PriceType::Market | PriceType::SlM => "MARKET",
        PriceType::Limit | PriceType::Sl => "LIMIT",
    }
}

/// `IOC` for the MARKET family, `DAY` otherwise.
pub fn validity_type(pt: PriceType) -> &'static str {
    if price_type(pt) == "MARKET" {
        "IOC"
    } else {
        "DAY"
    }
}

/// LTP entry trigger for SL / SL-M (web `build_entry_trigger`).
pub fn entry_trigger(pt: PriceType, trigger: f64, action: Action) -> Option<Value> {
    if !matches!(pt, PriceType::Sl | PriceType::SlM) {
        return None;
    }
    let p = paise(trigger);
    if p == 0 {
        return None;
    }
    let bound = match action {
        Action::Buy => "atOrAbove",
        Action::Sell => "atOrBelow",
    };
    Some(json!({"triggers": {"ltp": {bound: {"value": p}}}}))
}

/// Numeric ref id from a master token.
pub fn ref_id(token: &str) -> Option<i64> {
    let t = token.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// One place-order item (web `transform_data`).
#[allow(clippy::too_many_arguments)]
pub fn order_item(
    ref_id: i64,
    quantity: i64,
    action: Action,
    product: Product,
    pt: PriceType,
    price: f64,
    trigger: f64,
    tag: &str,
) -> Value {
    let mut m = Map::new();
    m.insert("refId".into(), json!(ref_id));
    m.insert("qty".into(), json!(quantity));
    m.insert("side".into(), json!(side(action)));
    m.insert("deliveryType".into(), json!(delivery_type(product)));
    m.insert("priceType".into(), json!(price_type(pt)));
    m.insert("validityType".into(), json!(validity_type(pt)));
    m.insert("isMultiLeg".into(), json!(false));
    m.insert("executionMode".into(), json!("ENTRY"));
    m.insert("stratTags".into(), json!([tag]));
    if price_type(pt) == "LIMIT" {
        m.insert("entryPrice".into(), json!(paise(price)));
    }
    if let Some(cfg) = entry_trigger(pt, trigger, action) {
        m.insert("entryConfig".into(), cfg);
    }
    Value::Object(m)
}

/// Place item for a resolved order.
pub fn place_item(o: &ResolvedOrder, ref_id: i64) -> Value {
    order_item(
        ref_id,
        o.quantity,
        o.action,
        o.product,
        o.pricetype,
        o.price,
        o.trigger_price,
        &sanitize_strat_tag(None),
    )
}

/// One modify item (web `transform_modify_order_data`).
pub fn modify_item(m: &ResolvedModify, order_id: i64) -> Value {
    let mut o = Map::new();
    o.insert("orderId".into(), json!(order_id));
    o.insert("qty".into(), json!(m.quantity));
    o.insert("deliveryType".into(), json!(delivery_type(m.product)));
    o.insert("priceType".into(), json!(price_type(m.pricetype)));
    o.insert("validityType".into(), json!(validity_type(m.pricetype)));
    o.insert("executionMode".into(), json!("ENTRY"));
    if price_type(m.pricetype) == "LIMIT" {
        o.insert("entryPrice".into(), json!(paise(m.price)));
    }
    if let Some(cfg) = entry_trigger(m.pricetype, m.trigger_price, m.action) {
        o.insert("entryConfig".into(), cfg);
    }
    Value::Object(o)
}

// ---------------------------------------------------------------------------
// Exchange folding and instrument resolution
// ---------------------------------------------------------------------------

fn is_derivative_type(d: &str) -> bool {
    matches!(d.to_ascii_uppercase().as_str(), "OPT" | "FUT")
}

fn derivative_exchange(ex: &str) -> Option<&'static str> {
    match ex {
        "NSE" => Some("NFO"),
        "BSE" => Some("BFO"),
        _ => None,
    }
}

/// web `map_exchange`.
pub fn map_exchange(brexchange: &str, derivative_type: &str) -> String {
    let ex = brexchange.trim().to_ascii_uppercase();
    if is_derivative_type(derivative_type) {
        derivative_exchange(&ex).map(str::to_string).unwrap_or(ex)
    } else {
        ex
    }
}

/// web `candidate_exchanges`: OpenAlgo exchanges worth probing.
pub fn candidate_exchanges(brexchange: &str, derivative_type: &str) -> Vec<String> {
    let ex = brexchange.trim().to_ascii_uppercase();
    if ex.is_empty() {
        return Vec::new();
    }
    let deriv = derivative_exchange(&ex);
    let d = derivative_type.trim();
    if is_derivative_type(d) {
        return vec![deriv.map(str::to_string).unwrap_or(ex)];
    }
    if !d.is_empty() {
        return vec![ex];
    }
    match deriv {
        Some(x) => vec![x.to_string(), ex],
        None => vec![ex],
    }
}

/// web `resolve_instrument`: `(symbol, exchange)` confirmed against the
/// master, by ref id then broker symbol.
pub fn resolve_instrument(
    symbols: &SymbolResolver,
    brexchange: &str,
    derivative_type: &str,
    ref_id: &str,
    broker_symbol: &str,
) -> Option<(String, String)> {
    let r = ref_id.trim();
    let bs = broker_symbol.trim();
    for ex in candidate_exchanges(brexchange, derivative_type) {
        if !r.is_empty() {
            if let Some(row) = symbols.by_token(&ex, r) {
                return Some((row.symbol, ex));
            }
        }
        if !bs.is_empty() {
            if let Some(row) = symbols.by_brsymbol(&ex, bs) {
                return Some((row.symbol, ex));
            }
        }
    }
    None
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn num(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(x)) => x.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// First present (non-null) of several keys, as a number.
fn first_num(v: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .find(|k| v.get(**k).is_some_and(|x| !x.is_null()))
        .map(|k| num(v, k))
        .unwrap_or(0.0)
}

/// web `derivative_type_of`.
pub fn derivative_type_of(row: &Value) -> String {
    for c in [Some(row), row.get("refData")].into_iter().flatten() {
        let d = s(c, "derivativeType");
        if !d.is_empty() {
            return d;
        }
    }
    if let Some(leg) = row.get("legs").and_then(|l| l.get(0)) {
        if let Some(r) = leg.get("refData") {
            let d = s(r, "derivativeType");
            if !d.is_empty() {
                return d;
            }
        }
    }
    String::new()
}

/// web `brexchange_of`.
pub fn brexchange_of(row: &Value) -> String {
    let ex = s(row, "exchange");
    if !ex.is_empty() {
        return ex;
    }
    let mut sources = vec![row.get("refData")];
    sources.push(row.get("legs").and_then(|l| l.get(0)));
    for src in sources.into_iter().flatten() {
        let e = s(src, "exchange");
        if !e.is_empty() {
            return e;
        }
        if let Some(r) = src.get("refData") {
            let e = s(r, "exchange");
            if !e.is_empty() {
                return e;
            }
        }
    }
    String::new()
}

/// web `resolve_position`: unresolved rows keep Nubra's own symbol.
pub fn resolve_position(symbols: &SymbolResolver, p: &Value) -> (String, String) {
    let bs = s(p, "symbol");
    let ex = brexchange_of(p);
    let d = derivative_type_of(p);
    let r = s(p, "refId");
    match resolve_instrument(symbols, &ex, &d, &r, &bs) {
        Some(x) => x,
        None => {
            tracing::warn!(
                "Nubra position is not in the master contract (refId {}); showing the broker symbol",
                r
            );
            (bs, map_exchange(&ex, &d))
        }
    }
}

/// web `position_net_qty` (live `netQty`, documented `netQuantity`).
pub fn position_net_qty(p: &Value) -> i64 {
    first_num(p, &["netQty", "netQuantity"]) as i64
}

/// web `_resolve_symbol` for an order row: `(symbol, exchange, ref_id)`.
pub fn resolve_order_symbol(symbols: &SymbolResolver, o: &Value) -> (String, String, String) {
    let mut ref_data = o.get("refData").cloned().unwrap_or(Value::Null);
    let mut rid = s(o, "refId");
    if rid.is_empty() || rid == "0" {
        if let Some(leg) = o.get("legs").and_then(|l| l.get(0)) {
            if let Some(r) = leg.get("refData") {
                ref_data = r.clone();
            }
            rid = s(leg, "refId");
        }
    }
    let mut ex = s(&ref_data, "exchange");
    if ex.is_empty() {
        ex = s(o, "exchange");
    }
    let mut d = s(&ref_data, "derivativeType");
    if d.is_empty() {
        d = derivative_type_of(o);
    }
    let bs = s(&ref_data, "stockName");
    if let Some((sym, exch)) = resolve_instrument(symbols, &ex, &d, &rid, &bs) {
        return (sym, exch, rid);
    }
    tracing::warn!(
        "Nubra order is not in the master contract (refId {}); showing the broker symbol",
        rid
    );
    let shown = if bs.is_empty() {
        s(&ref_data, "displayName")
    } else {
        bs
    };
    (shown, map_exchange(&ex, &d), rid)
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// Bucket -> OpenAlgo status (web `_BUCKET_STATUS`).
pub fn bucket_status(bucket: &str) -> Option<&'static str> {
    Some(match bucket {
        "open" | "gtt" => "open",
        "executed" => "complete",
        "cancelled" | "expired" => "cancelled",
        "rejected" => "rejected",
        _ => return None,
    })
}

/// web `flatten_order_buckets`: `(bucket, order)` pairs.
pub fn flatten_buckets(resp: &Value, only: Option<&[&str]>) -> Vec<(String, Value)> {
    let Some(Value::Object(grouped)) = resp.get("orders") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (bucket, orders) in grouped {
        if only.is_some_and(|o| !o.contains(&bucket.as_str())) {
            continue;
        }
        if let Some(list) = orders.as_array() {
            for o in list.iter().filter(|o| o.is_object()) {
                out.push((bucket.clone(), o.clone()));
            }
        }
    }
    out
}

/// LTP entry trigger in paise (web `_trigger_price_paise`).
pub fn trigger_paise(o: &Value) -> i64 {
    let first = |v: Option<&Value>| -> Option<Value> {
        match v? {
            Value::Array(a) => a.first().cloned(),
            other => Some(other.clone()),
        }
    };
    let Some(cfg) = first(o.get("entryConfig")) else {
        return 0;
    };
    let Some(trig) = first(cfg.get("triggers")) else {
        return 0;
    };
    let Some(ltp) = trig.get("ltp") else {
        return 0;
    };
    for bound in ["atOrAbove", "atOrBelow"] {
        let v = ltp.get(bound).map(|n| num(n, "value")).unwrap_or(0.0);
        if v != 0.0 {
            return v as i64;
        }
    }
    0
}

/// web `_order_type`.
pub fn order_type(o: &Value, trig: i64) -> &'static str {
    let pt = s(o, "priceType").to_ascii_uppercase();
    if trig != 0 {
        return if pt == "MARKET" { "SL-M" } else { "SL" };
    }
    if pt == "LIMIT" {
        "LIMIT"
    } else {
        "MARKET"
    }
}

/// RFC3339 (9 fractional digits) -> `YYYY-MM-DD HH:MM:SS` (web
/// `_parse_timestamp`; the wall clock of the given offset).
pub fn format_timestamp(v: &Value) -> String {
    match v {
        Value::Number(n) => {
            let ns = n.as_i64().unwrap_or(0);
            chrono::DateTime::from_timestamp(ns / 1_000_000_000, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| n.to_string())
        }
        Value::String(t) if !t.trim().is_empty() => {
            match chrono::DateTime::parse_from_rfc3339(t.trim()) {
                Ok(d) => d.format("%Y-%m-%d %H:%M:%S").to_string(),
                Err(_) => t.clone(),
            }
        }
        _ => String::new(),
    }
}

fn last_timestamp(o: &Value) -> String {
    let Some(ts) = o.get("timestamps") else {
        return String::new();
    };
    for k in [
        "lastUpdatedAt",
        "filledAt",
        "sentToColoAt",
        "intentCreatedAt",
    ] {
        if let Some(v) = ts.get(k).filter(|v| match v {
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
            _ => true,
        }) {
            return format_timestamp(v);
        }
    }
    String::new()
}

/// `exchangeOrderIds` map flattened (web `_exchange_order_id`).
pub fn exchange_order_id(o: &Value) -> String {
    let Some(Value::Object(m)) = o.get("exchangeOrderIds") else {
        return String::new();
    };
    let mut ids = Vec::new();
    for v in m.values() {
        match v {
            Value::Array(a) => ids.extend(a.iter().map(|x| match x {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })),
            Value::Null => {}
            Value::String(s) => ids.push(s.clone()),
            other => ids.push(other.to_string()),
        }
    }
    ids.join(",")
}

fn i32_of(x: f64) -> i32 {
    x as i32
}

/// web `map_order_data` + `transform_order_data`.
pub fn order_book(resp: &Value, symbols: &SymbolResolver) -> Vec<Order> {
    flatten_buckets(resp, None)
        .into_iter()
        .map(|(bucket, o)| {
            let (symbol, exchange, _) = resolve_order_symbol(symbols, &o);
            let trig = trigger_paise(&o);
            let ot = order_type(&o, trig);
            let mut status = bucket_status(&bucket)
                .map(str::to_string)
                .unwrap_or_else(|| s(&o, "status").to_ascii_lowercase());
            if matches!(ot, "SL" | "SL-M") && status == "open" {
                status = "trigger pending".into();
            }
            let qty = num(&o, "orderQty");
            let filled = num(&o, "filledQty");
            let eoid = exchange_order_id(&o);
            Order {
                order_id: s(&o, "intentOrderId"),
                exchange_order_id: (!eoid.is_empty()).then_some(eoid),
                symbol,
                exchange,
                side: s(&o, "side").to_ascii_uppercase(),
                quantity: i32_of(qty),
                filled_quantity: i32_of(filled),
                pending_quantity: i32_of((qty - filled).max(0.0)),
                price: num(&o, "orderPrice") / 100.0,
                trigger_price: trig as f64 / 100.0,
                average_price: num(&o, "filledPrice") / 100.0,
                order_type: ot.to_string(),
                product: product_from(&s(&o, "deliveryType")).to_string(),
                status,
                validity: "DAY".to_string(),
                order_timestamp: last_timestamp(&o),
                exchange_timestamp: None,
                rejection_reason: None,
            }
        })
        .collect()
}

/// web `map_trade_data`: orders with a fill (full or partial).
pub fn trade_book(resp: &Value, symbols: &SymbolResolver) -> Vec<Trade> {
    flatten_buckets(resp, None)
        .into_iter()
        .filter(|(_, o)| num(o, "filledQty") > 0.0)
        .map(|(_, o)| {
            let (symbol, exchange, _) = resolve_order_symbol(symbols, &o);
            let qty = num(&o, "filledQty");
            let price = num(&o, "filledPrice") / 100.0;
            Trade {
                order_id: s(&o, "intentOrderId"),
                trade_id: String::new(),
                symbol,
                exchange,
                product: product_from(&s(&o, "deliveryType")).to_string(),
                side: s(&o, "side").to_ascii_uppercase(),
                quantity: i32_of(qty),
                average_price: round2(price),
                trade_value: round2(price * qty),
                timestamp: last_timestamp(&o),
            }
        })
        .collect()
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// `portfolio.positions` (web `extract_positions`).
pub fn positions_list(resp: &Value) -> Vec<Value> {
    if let Some(a) = resp.as_array() {
        return a.clone();
    }
    resp.get("portfolio")
        .and_then(|p| p.get("positions"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// web `map_position_data` + `transform_positions_data`.
pub fn positions(resp: &Value, symbols: &SymbolResolver) -> Vec<Position> {
    positions_list(resp)
        .iter()
        .map(|p| {
            let (symbol, exchange) = resolve_position(symbols, p);
            let qty = position_net_qty(p);
            Position {
                symbol,
                exchange,
                product: product_from(&s(p, "deliveryType")).to_string(),
                quantity: qty as i32,
                overnight_quantity: 0,
                average_price: num(p, "avgPrice") / 100.0,
                ltp: first_num(p, &["ltp", "lastTradedPrice"]) / 100.0,
                pnl: num(p, "pnl") / 100.0,
                realized_pnl: 0.0,
                unrealized_pnl: 0.0,
                buy_quantity: first_num(p, &["buyQty", "buyQuantity"]) as i32,
                buy_value: 0.0,
                sell_quantity: first_num(p, &["sellQty", "sellQuantity"]) as i32,
                sell_value: 0.0,
            }
        })
        .collect()
}

/// web `map_portfolio_data` + `transform_holdings_data` (paise -> rupees).
pub fn holdings(resp: &Value, symbols: &SymbolResolver) -> Vec<Holding> {
    let list = resp
        .get("portfolio")
        .and_then(|p| p.get("holdings"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    list.iter()
        .map(|h| {
            let mut exchange = s(h, "exchange");
            if exchange.is_empty() {
                exchange = "NSE".into();
            }
            let bs = s(h, "symbol");
            let ltp = round2(num(h, "lastTradedPrice") / 100.0);
            Holding {
                symbol: symbols.oa_symbol_or_raw(&bs, &exchange),
                exchange,
                product: "CNC".to_string(),
                isin: None,
                quantity: num(h, "quantity") as i32,
                t1_quantity: 0,
                average_price: round2(num(h, "avgPrice") / 100.0),
                ltp,
                close_price: round2(num(h, "prevClose") / 100.0),
                pnl: round2(num(h, "netPnl") / 100.0),
                pnl_percentage: round2(num(h, "netPnlChg")),
                current_value: round2(num(h, "currentValue") / 100.0),
            }
        })
        .collect()
}

/// web `funds.get_margin_data` (paise -> rupees).
pub fn funds(resp: &Value) -> Option<Funds> {
    let d = resp.get("portFundsAndMargin").filter(|d| d.is_object())?;
    let r = |k: &str| num(d, k) / 100.0;
    let utilised = r("totalMarginBlocked");
    Some(Funds {
        available_cash: round2(r("netMarginAvailable")),
        used_margin: round2(utilised),
        collateral: round2(r("totalCollateral")),
        m2m_realized: round2(r("netDerivativePrem")),
        m2m_unrealized: round2(r("mtmEqIdayCnc") + r("mtmEqDelivery") + r("mtmDeriv")),
        utilised_debits: round2(utilised),
        ..Default::default()
    })
}

/// web `parse_margin_response`. `Err(reason)` when Nubra refused.
pub fn margin(resp: &Value) -> Result<MarginResult, String> {
    let Some(info) = resp.get("marginInfo").filter(|m| m.is_object()) else {
        return Err(super::error_text(resp).unwrap_or_else(|| "Unknown error from Nubra".into()));
    };
    let total_margin = num(info, "totalMargin");
    let funds_required = num(resp, "totalFundsRequired");
    Ok(MarginResult {
        total_margin_required: if funds_required != 0.0 {
            funds_required
        } else {
            total_margin
        },
        span_margin: total_margin,
        exposure_margin: 0.0,
    })
}
