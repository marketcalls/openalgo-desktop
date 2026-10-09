//! OpenAlgo <-> mStock Type B translation (web `mapping/transform_data.py`,
//! `mapping/order_data.py`, `mapping/margin_data.py`).

use crate::brokers::common::mapping::{PriceType, Product};
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Value helpers (mStock sends numbers as numbers or strings)
// ---------------------------------------------------------------------------

/// String field (numbers rendered, null/absent empty).
pub fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

/// Float field (numeric strings parsed, anything else 0).
pub fn f(v: &Value, k: &str) -> f64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(x)) => x.trim().replace(',', "").parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Integer field (Python `int(float(x))`).
pub fn i(v: &Value, k: &str) -> i64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| f(v, k) as i64),
        _ => f(v, k) as i64,
    }
}

fn clamp(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// A number the way the web's `str()` of a form value reads: whole numbers
/// without a fraction (`"0"`, `"100"`), others shortest (`"101.5"`).
pub fn num_text(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{}", x)
    }
}

// ---------------------------------------------------------------------------
// Enum maps
// ---------------------------------------------------------------------------

/// `map_order_type` (`transform_data.py:139-155`).
pub fn map_order_type(p: PriceType) -> &'static str {
    match p {
        PriceType::Market => "MARKET",
        PriceType::Limit => "LIMIT",
        PriceType::Sl => "STOPLOSS_LIMIT",
        PriceType::SlM => "STOPLOSS_MARKET",
    }
}

/// `map_variety` (`transform_data.py:172-179`).
pub fn map_variety(p: PriceType) -> &'static str {
    match p {
        PriceType::Market | PriceType::Limit => "NORMAL",
        PriceType::Sl | PriceType::SlM => "STOPLOSS",
    }
}

/// `map_product_type` (`transform_data.py:158-169`).
pub fn map_product_type(p: Product) -> &'static str {
    match p {
        Product::Cnc => "DELIVERY",
        Product::Nrml => "CARRYFORWARD",
        Product::Mis => "INTRADAY",
    }
}

/// `reverse_map_product_type` (`transform_data.py:182-192`).
pub fn reverse_map_product_type(p: &str) -> Option<&'static str> {
    Some(match p {
        "DELIVERY" => "CNC",
        "CARRYFORWARD" => "NRML",
        "INTRADAY" | "MARGIN" => "MIS",
        _ => return None,
    })
}

/// `map_order_type_to_openalgo` (`order_data.py:32-57`): every stop-loss
/// spelling mStock uses; anything else passes through.
pub fn order_type_to_openalgo(t: &str) -> String {
    match t {
        "STOPLOSS_LIMIT" | "STOP_LOSS" | "SL" => "SL".into(),
        "STOPLOSS_MARKET" | "STOP_LOSS_MARKET" | "SL-M" => "SL-M".into(),
        other => other.to_string(),
    }
}

/// `transform_order_data` status normalisation (`order_data.py:205-220`).
pub fn order_status(raw: &str) -> String {
    if raw == "Traded" || raw.contains("TRADE CONFIRMED") {
        "complete".into()
    } else if matches!(
        raw,
        "O-Pending" | "Pending" | "pending" | "O-Modified" | "o-modified"
    ) {
        "open".into()
    } else if raw == "Rejected" || raw == "rejected" {
        "rejected".into()
    } else if matches!(
        raw,
        "Cancelled" | "cancelled" | "O-Cancelled" | "o-cancelled"
    ) {
        "cancelled".into()
    } else if raw.to_ascii_lowercase().contains("trigger pending") {
        "trigger pending".into()
    } else {
        crate::brokers::lower_status(raw)
    }
}

/// Raw statuses a cancel-all touches (`order_api.py:686-689`).
pub fn is_cancellable(raw: &str) -> bool {
    matches!(
        raw.to_ascii_lowercase().as_str(),
        "open" | "pending" | "o-pending" | "trigger pending"
    )
}

const DERIVATIVE_TYPES: &[&str] = &["OPTIDX", "OPTSTK", "FUTIDX", "FUTSTK"];
const CURRENCY_TYPES: &[&str] = &["OPTCUR", "FUTCUR", "OPTIRC", "FUTIRC"];

/// `map_broker_exchange_to_openalgo` (`order_data.py:9-29`): mStock reports
/// NSE/BSE for derivatives. Currency rows get the master's own CDS/BCD remap
/// (`master_contract_db.py:324-333`) so their symbols resolve too.
pub fn oa_exchange(broker_exchange: &str, instrumenttype: &str) -> String {
    let it = instrumenttype.trim();
    if DERIVATIVE_TYPES.contains(&it) {
        match broker_exchange {
            "NSE" => return "NFO".into(),
            "BSE" => return "BFO".into(),
            _ => {}
        }
    }
    if CURRENCY_TYPES.contains(&it) {
        match broker_exchange {
            "NSE" => return "CDS".into(),
            "BSE" => return "BCD".into(),
            _ => {}
        }
    }
    broker_exchange.to_string()
}

/// OpenAlgo symbol: by token on the OpenAlgo exchange, then by broker
/// symbol, else the broker symbol unchanged (web `get_symbol` then
/// `get_oa_symbol`).
pub fn oa_symbol(symbols: &SymbolResolver, token: &str, brsymbol: &str, exchange: &str) -> String {
    if !token.is_empty() {
        if let Some(r) = symbols.by_token(exchange, token) {
            return r.symbol;
        }
    }
    symbols.oa_symbol_or_raw(brsymbol, exchange)
}

/// Book product: the reverse map, else the raw value.
pub fn oa_product(raw: &str) -> String {
    reverse_map_product_type(raw)
        .map(str::to_string)
        .unwrap_or_else(|| raw.to_string())
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

/// Place body (`transform_data.py:62-99`).
pub fn place_order_body(o: &ResolvedOrder) -> Value {
    json!({
        "variety": map_variety(o.pricetype),
        "tradingsymbol": o.brsymbol(),
        "symboltoken": o.token(),
        "exchange": o.exchange.as_str(),
        "transactiontype": o.action.as_str(),
        "ordertype": map_order_type(o.pricetype),
        "quantity": o.quantity.to_string(),
        "producttype": map_product_type(o.product),
        "price": num_text(o.price),
        "triggerprice": num_text(o.trigger_price),
        "squareoff": "0",
        "stoploss": "0",
        "trailingStopLoss": "",
        "disclosedquantity": o.disclosed_quantity.to_string(),
        "duration": "DAY",
        "ordertag": "",
    })
}

/// Modify body (`transform_data.py:102-135`).
pub fn modify_order_body(m: &ResolvedModify) -> Value {
    json!({
        "variety": map_variety(m.pricetype),
        "tradingsymbol": m.brsymbol(),
        "symboltoken": m.token(),
        "exchange": m.exchange.as_str(),
        "transactiontype": m.action.as_str(),
        "orderid": m.order_id,
        "ordertype": map_order_type(m.pricetype),
        "quantity": m.quantity.to_string(),
        "producttype": map_product_type(m.product),
        "duration": "DAY",
        "price": num_text(m.price),
        "triggerprice": num_text(m.trigger_price),
        "disclosedquantity": m.disclosed_quantity.to_string(),
        "modqty_remng": "0",
    })
}

/// Cancel body (`order_api.py:520`).
pub fn cancel_order_body(order_id: &str) -> Value {
    json!({"variety": "NORMAL", "orderid": order_id})
}

/// web `extract_orderid`: `data.orderid`, fallback `data.uniqueorderid`.
pub fn extract_order_id(v: &Value) -> Option<String> {
    if !super::is_success(v) {
        return None;
    }
    let d = v.get("data")?;
    if !d.is_object() {
        return None;
    }
    let id = s(d, "orderid");
    let id = if id.is_empty() {
        s(d, "uniqueorderid")
    } else {
        id
    };
    (!id.is_empty()).then_some(id)
}

// ---------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------

/// The rows of a book answer (`data` list; anything else empty).
pub fn rows(v: &Value) -> Vec<Value> {
    match v.get("data") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(_)) => vec![v["data"].clone()],
        _ => Vec::new(),
    }
}

/// One order-book row (`map_order_data` + `transform_order_data`).
pub fn map_order(o: &Value, symbols: &SymbolResolver) -> Order {
    let instrumenttype = s(o, "instrumenttype");
    let exchange = oa_exchange(&s(o, "exchange"), &instrumenttype);
    let symbol = oa_symbol(
        symbols,
        &s(o, "symboltoken"),
        &s(o, "tradingsymbol"),
        &exchange,
    );
    let order_type = order_type_to_openalgo(&s(o, "ordertype"));
    let status = order_status(&s(o, "status"));
    // Completed orders show the average fill; LIMIT / SL the order price;
    // MARKET / SL-M the average (0 while pending).
    let price = if status == "complete" || !matches!(order_type.as_str(), "LIMIT" | "SL") {
        f(o, "averageprice")
    } else {
        f(o, "price")
    };
    let quantity = i(o, "quantity");
    let filled = i(o, "filledshares");
    let pending = if o.get("unfilledshares").is_some() {
        i(o, "unfilledshares")
    } else {
        (quantity - filled).max(0)
    };
    let reason = s(o, "text");
    Order {
        order_tag: None,
        order_id: s(o, "orderid"),
        exchange_order_id: Some(s(o, "exchangeorderid")).filter(|x| !x.is_empty()),
        symbol,
        exchange,
        side: s(o, "transactiontype").to_ascii_uppercase(),
        quantity: clamp(quantity),
        filled_quantity: clamp(filled),
        pending_quantity: clamp(pending),
        price,
        trigger_price: f(o, "triggerprice"),
        average_price: f(o, "averageprice"),
        order_type,
        product: oa_product(&s(o, "producttype")),
        status,
        validity: {
            let d = s(o, "duration");
            if d.is_empty() {
                "DAY".into()
            } else {
                d
            }
        },
        order_timestamp: s(o, "updatetime"),
        exchange_timestamp: Some(s(o, "exchtime")).filter(|x| !x.is_empty()),
        rejection_reason: Some(reason).filter(|x| !x.is_empty()),
    }
}

pub fn map_orders(v: &Value, symbols: &SymbolResolver) -> Vec<Order> {
    rows(v).iter().map(|o| map_order(o, symbols)).collect()
}

/// One trade-book row (`map_trade_data` + `transform_tradebook_data`); the
/// Type B trade book uses uppercase keys.
pub fn map_trade(t: &Value, symbols: &SymbolResolver) -> Trade {
    let raw_exchange = s(t, "EXCHANGE");
    let instrument = s(t, "INSTRUMENT_NAME");
    let exchange = oa_exchange(&raw_exchange, &instrument);
    let token = s(t, "SEC_ID");
    let base = s(t, "SYMBOL");
    let symbol = if raw_exchange.is_empty() {
        base.clone()
    } else {
        let by_token = (!token.is_empty())
            .then(|| symbols.by_token(&exchange, &token))
            .flatten();
        match by_token {
            Some(r) => r.symbol,
            None if base.is_empty() => String::new(),
            None => {
                let br = if matches!(raw_exchange.as_str(), "NSE" | "BSE") && instrument == "EQUITY"
                {
                    format!("{}-EQ", base)
                } else {
                    base.clone()
                };
                symbols.oa_symbol(&br, &exchange).unwrap_or(base)
            }
        }
    };
    let product = match s(t, "PRODUCT").as_str() {
        "CNC" => "CNC".to_string(),
        other => oa_product(other),
    };
    let trade_id = {
        let a = s(t, "TRADE_NUMBER");
        if a.is_empty() {
            s(t, "TRADE_ID")
        } else {
            a
        }
    };
    Trade {
        order_tag: None,
        order_id: s(t, "ORDER_NUMBER"),
        trade_id,
        symbol,
        exchange,
        product,
        side: s(t, "BUY_SELL").to_ascii_uppercase(),
        quantity: clamp(i(t, "QUANTITY")),
        average_price: f(t, "PRICE"),
        trade_value: f(t, "TRADE_VALUE"),
        timestamp: s(t, "ORDER_DATE_TIME"),
    }
}

pub fn map_trades(v: &Value, symbols: &SymbolResolver) -> Vec<Trade> {
    rows(v).iter().map(|t| map_trade(t, symbols)).collect()
}

/// One position row (`map_position_data` + `transform_positions_data`):
/// `ltp` is not in the Type B book (web "NA", here 0) and the P&L is
/// `netvalue`.
pub fn map_position(p: &Value, symbols: &SymbolResolver) -> Position {
    let exchange = oa_exchange(&s(p, "exchange"), &s(p, "instrumenttype"));
    let symbol = oa_symbol(
        symbols,
        &s(p, "symboltoken"),
        &s(p, "symbolname"),
        &exchange,
    );
    let pnl = f(p, "netvalue");
    Position {
        symbol,
        exchange,
        product: oa_product(&s(p, "producttype")),
        quantity: clamp(i(p, "netqty")),
        overnight_quantity: clamp(i(p, "cfbuyqty") - i(p, "cfsellqty")),
        average_price: f(p, "avgnetprice"),
        ltp: f(p, "ltp"),
        pnl,
        realized_pnl: f(p, "realised"),
        unrealized_pnl: f(p, "unrealised"),
        buy_quantity: clamp(i(p, "buyqty")),
        buy_value: f(p, "totalbuyvalue"),
        sell_quantity: clamp(i(p, "sellqty")),
        sell_value: f(p, "totalsellvalue"),
    }
}

pub fn map_positions(v: &Value, symbols: &SymbolResolver) -> Vec<Position> {
    rows(v).iter().map(|p| map_position(p, symbols)).collect()
}

/// One holding row (`map_portfolio_data` + `transform_holdings_data`).
pub fn map_holding(h: &Value, symbols: &SymbolResolver) -> Holding {
    let exchange = {
        let e = s(h, "exchange");
        if e.is_empty() {
            "NSE".to_string()
        } else {
            e
        }
    };
    let brsymbol = s(h, "tradingsymbol");
    let token = s(h, "symboltoken");
    let symbol = symbols
        .oa_symbol(&brsymbol, &exchange)
        .or_else(|| {
            (!token.is_empty())
                .then(|| symbols.by_token(&exchange, &token).map(|r| r.symbol))
                .flatten()
        })
        .unwrap_or(brsymbol);
    let quantity = i(h, "quantity");
    let ltp = f(h, "ltp");
    Holding {
        symbol,
        exchange,
        product: "CNC".into(),
        isin: Some(s(h, "isin")).filter(|x| !x.is_empty()),
        quantity: clamp(quantity),
        t1_quantity: clamp(i(h, "t1quantity")),
        average_price: f(h, "averageprice"),
        ltp,
        close_price: f(h, "close"),
        pnl: round2(f(h, "profitandloss")),
        pnl_percentage: round2(f(h, "pnlpercentage")),
        current_value: ltp * quantity as f64,
    }
}

pub fn map_holdings(v: &Value, symbols: &SymbolResolver) -> Vec<Holding> {
    rows(v).iter().map(|h| map_holding(h, symbols)).collect()
}

// ---------------------------------------------------------------------------
// Funds and margin
// ---------------------------------------------------------------------------

/// `get_margin_data` (`funds.py:48-65`): `data[0]` keys, missing or
/// `"None"` read as 0, two decimals.
pub fn funds_from_summary(d: &Value) -> Funds {
    let g = |k: &str| round2(f(d, k));
    let used = g("AMOUNT_UTILIZED");
    Funds {
        available_cash: g("AVAILABLE_BALANCE"),
        used_margin: used,
        collateral: g("COLLATERALS"),
        m2m_realized: g("REALISED_PROFITS"),
        m2m_unrealized: g("MTM_COMBINED"),
        utilised_debits: used,
        ..Default::default()
    }
}

/// One margin leg (`transform_margin_positions`, `margin_data.py:39-48`).
pub fn margin_leg(leg: &MarginLeg, brsymbol: &str, token: &str) -> Value {
    json!({
        "product_type": map_product_type(leg.product),
        "transaction_type": leg.action.as_str(),
        "quantity": leg.quantity.to_string(),
        "price": num_text(leg.price),
        "exchange": leg.key.exchange,
        "symbol_name": brsymbol,
        "token": token,
        "trigger_price": leg.trigger_price,
    })
}

/// web token check: digits once `.` and `-` are removed.
pub fn valid_margin_token(token: &str) -> bool {
    let t = token.trim();
    let digits: String = t.chars().filter(|c| *c != '.' && *c != '-').collect();
    !t.is_empty()
        && !t.eq_ignore_ascii_case("none")
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.is_empty()
}

/// `parse_margin_response` (`margin_data.py:103-146`).
pub fn parse_margin(v: &Value) -> Option<MarginResult> {
    if !matches!(v.get("status"), Some(Value::Bool(true)))
        && v.get("status").and_then(Value::as_str) != Some("true")
    {
        return None;
    }
    let summary = v.get("data").and_then(|d| d.get("summary"));
    let summary = summary.cloned().unwrap_or(Value::Null);
    let mut out = MarginResult {
        total_margin_required: f(&summary, "total_charges"),
        ..Default::default()
    };
    if let Some(Value::Array(items)) = summary.get("breakup") {
        for it in items {
            match s(it, "name").as_str() {
                "SPANMARGIN" => out.span_margin = f(it, "amount"),
                "EXPOMARGIN" => out.exposure_margin = f(it, "amount"),
                _ => {}
            }
        }
    }
    Some(out)
}
