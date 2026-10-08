//! What the `/scalping/api` routes do (web `blueprints/scalping.py`): the
//! symbol, expiry and strike resolution, chart history, entry orders, the
//! freeze-safe risk-reducing exits, the scalping list and the stop-loss
//! states. Every reply is the web's body and status.
//!
//! Orders go through the `/api/v1` services with `Route::INTERNAL`: the
//! trader is acting in the app, so Semi-Auto never queues them, and the
//! analyzer toggle routes them to the sandbox exactly as it does every other
//! page action.

use super::monitor::{exit_chunk, LEG_EXCHANGES, MAX_LOTS, SCALPING_STRATEGY};
use super::store::{SlUpsert, MODE_ANALYZE, MODE_LIVE};
use crate::brokers::common::symbols::SymToken;
use crate::services::core::{broker_handle, is_analyze, Reply};
use crate::services::order_service::Route;
use crate::state::AppState;
use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const VALID_ACTIONS: &[&str] = &["BUY", "SELL"];
pub const EQUITY_EXCHANGES: &[&str] = &["NSE", "BSE"];
pub const DERIVATIVE_PRODUCTS: &[&str] = &["MIS", "NRML"];
pub const EQUITY_PRODUCTS: &[&str] = &["MIS", "CNC"];
pub const VALID_PRODUCTS: &[&str] = &["MIS", "NRML", "CNC"];
pub const MAX_ORDER_QUANTITY: i64 = 100_000;
pub const IST_OFFSET_SECONDS: i64 = 19800;

/// The supported index underlyings (web `SUPPORTED_UNDERLYINGS`), in order.
pub const SUPPORTED_UNDERLYINGS: &[(&str, &str, &str)] = &[
    ("NIFTY", "NSE_INDEX", "NFO"),
    ("BANKNIFTY", "NSE_INDEX", "NFO"),
    ("FINNIFTY", "NSE_INDEX", "NFO"),
    ("MIDCPNIFTY", "NSE_INDEX", "NFO"),
    ("NIFTYNXT50", "NSE_INDEX", "NFO"),
    ("SENSEX", "BSE_INDEX", "BFO"),
    ("BANKEX", "BSE_INDEX", "BFO"),
];

const NSE_INDEX_UNDERLYINGS: &[&str] = &[
    "NIFTY",
    "BANKNIFTY",
    "FINNIFTY",
    "MIDCPNIFTY",
    "NIFTYNXT50",
    "INDIAVIX",
];
const BSE_INDEX_UNDERLYINGS: &[&str] = &["SENSEX", "BANKEX", "SENSEX50"];

fn is_order_exchange(e: &str) -> bool {
    LEG_EXCHANGES.contains(&e) || EQUITY_EXCHANGES.contains(&e)
}

fn is_derivative(e: &str) -> bool {
    LEG_EXCHANGES.contains(&e)
}

fn allowed_products(e: &str) -> &'static [&'static str] {
    if is_derivative(e) {
        DERIVATIVE_PRODUCTS
    } else {
        EQUITY_PRODUCTS
    }
}

fn err(status: u16, msg: impl Into<String>) -> Reply {
    Reply::error(status, msg)
}

/// `analyze` or `live`: segregates the list and the stops by mode.
pub fn current_mode(ctx: &AppState) -> &'static str {
    if is_analyze(ctx) {
        MODE_ANALYZE
    } else {
        MODE_LIVE
    }
}

/// `(data.get(k) or "").strip()`.
pub fn text(body: &Map<String, Value>, k: &str) -> String {
    match body.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(true)) => "True".into(),
        _ => String::new(),
    }
}

/// A value Python's `int()` or `float()` would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotANumber;

/// Python `int()` of a JSON value; `Err` when it would raise.
pub fn py_int(v: Option<&Value>) -> Result<i64, NotANumber> {
    match v {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.trunc() as i64)
            })
            .ok_or(NotANumber),
        Some(Value::String(s)) => s.trim().parse::<i64>().map_err(|_| NotANumber),
        Some(Value::Bool(b)) => Ok(i64::from(*b)),
        _ => Err(NotANumber),
    }
}

/// Python `float()` of a JSON value.
pub fn py_float(v: &Value) -> Result<f64, NotANumber> {
    match v {
        Value::Number(n) => n.as_f64().ok_or(NotANumber),
        Value::String(s) => {
            let t = s.trim().to_ascii_lowercase();
            match t.as_str() {
                "nan" => Ok(f64::NAN),
                "inf" | "infinity" | "+inf" => Ok(f64::INFINITY),
                "-inf" | "-infinity" => Ok(f64::NEG_INFINITY),
                _ => t.parse::<f64>().map_err(|_| NotANumber),
            }
        }
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(NotANumber),
    }
}

/// Python truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `10-JUL-25` to `10JUL25`.
pub fn normalize_expiry(e: &str) -> String {
    e.replace(['-', ' '], "").to_uppercase()
}

/// An expiry in any of the web's three spellings; unparseable sorts last.
pub fn parse_expiry(s: &str) -> NaiveDate {
    let u = s.trim().to_uppercase();
    for fmt in ["%d-%b-%y", "%d-%b-%Y", "%d%b%y"] {
        if let Ok(d) = NaiveDate::parse_from_str(&u, fmt) {
            return d;
        }
    }
    NaiveDate::MAX
}

/// The symbol and exchange to stream for an underlying's spot price.
pub fn underlying_quote(underlying: &str, fo_exchange: &str) -> (Value, Value) {
    match fo_exchange {
        "NFO" => (
            json!(underlying),
            json!(if NSE_INDEX_UNDERLYINGS.contains(&underlying) {
                "NSE_INDEX"
            } else {
                "NSE"
            }),
        ),
        "BFO" => (
            json!(underlying),
            json!(if BSE_INDEX_UNDERLYINGS.contains(&underlying) {
                "BSE_INDEX"
            } else {
                "BSE"
            }),
        ),
        _ => (Value::Null, Value::Null),
    }
}

// ------------------------------------------------------------------ lookups

pub fn underlyings() -> Reply {
    let data: Vec<Value> = SUPPORTED_UNDERLYINGS
        .iter()
        .map(|(u, ix, fo)| json!({"underlying": u, "index_exchange": ix, "fo_exchange": fo}))
        .collect();
    Reply::ok(json!({"status": "success", "data": data}))
}

pub fn chart_lookback(interval: &str) -> Option<i64> {
    match interval {
        "1m" => Some(1),
        "5m" => Some(3),
        "15m" => Some(9),
        _ => None,
    }
}

/// Candles for the latest N trading days (or one `date`), bar times shifted
/// to IST and floored to the minute.
pub async fn history(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    interval: &str,
    date: &str,
) -> Reply {
    let symbol: String = symbol.trim().to_uppercase().chars().take(50).collect();
    let exchange: String = exchange.trim().to_uppercase().chars().take(20).collect();
    if symbol.is_empty() || exchange.is_empty() {
        return err(400, "symbol and exchange are required");
    }
    let interval = if chart_lookback(interval.trim()).is_some() {
        interval.trim().to_string()
    } else {
        "1m".to_string()
    };
    let date = date.trim();
    let one_day = date.len() == 10 && NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok();
    let (start, end, keep) = if one_day {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap_or_default();
        (d, d, 1)
    } else {
        let keep = chart_lookback(&interval).unwrap_or(1);
        let today = ctx.now().with_timezone(&Kolkata).date_naive();
        (today - chrono::Duration::days(keep * 2 + 5), today, keep)
    };
    let reply = crate::services::market_data_service::history(
        ctx, &symbol, &exchange, &interval, start, end, "api",
    )
    .await;
    if !reply.is_success() {
        let message = reply.message();
        return Reply::new(
            reply.status,
            json!({"status": "error", "message": if message.is_empty() { "History fetch failed".to_string() } else { message }}),
        );
    }
    let rows = reply
        .body
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut by_date: BTreeMap<String, Vec<(i64, Value)>> = BTreeMap::new();
    for r in rows {
        let Some(ts) = r.get("timestamp").and_then(|t| match t {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }) else {
            continue;
        };
        let ts = ts as i64;
        let Some(dt) = Utc.timestamp_opt(ts, 0).single() else {
            continue;
        };
        let day = dt.with_timezone(&Kolkata).format("%Y-%m-%d").to_string();
        by_date.entry(day).or_default().push((ts, r));
    }
    if by_date.is_empty() {
        return Reply::ok(json!({
            "status": "success", "symbol": symbol, "exchange": exchange,
            "interval": interval, "date": null, "candles": [],
        }));
    }
    let dates: Vec<String> = by_date.keys().cloned().collect();
    let selected = &dates[dates.len().saturating_sub(keep as usize)..];
    let latest = selected.last().cloned();
    let mut rows: Vec<(i64, Value)> = Vec::new();
    for d in selected {
        if let Some(v) = by_date.remove(d) {
            rows.extend(v);
        }
    }
    rows.sort_by_key(|(t, _)| *t);
    let num = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let candles: Vec<Value> = rows
        .iter()
        .map(|(ts, r)| {
            json!({
                "time": ((ts + IST_OFFSET_SECONDS).div_euclid(60)) * 60,
                "open": num(r, "open"),
                "high": num(r, "high"),
                "low": num(r, "low"),
                "close": num(r, "close"),
                "volume": num(r, "volume"),
            })
        })
        .collect();
    Reply::ok(json!({
        "status": "success", "symbol": symbol, "exchange": exchange,
        "interval": interval, "date": latest, "candles": candles,
    }))
}

/// Every F&O underlying on an exchange, indices first.
pub fn all_underlyings(ctx: &AppState, exchange: &str, instrumenttype: &str) -> Reply {
    let exchange = if exchange.trim().is_empty() {
        "NFO".to_string()
    } else {
        exchange.trim().to_uppercase()
    };
    let it = if instrumenttype.trim().is_empty() {
        "options".to_string()
    } else {
        instrumenttype.trim().to_lowercase()
    };
    let include_futures = it == "futures" || exchange == "MCX" || exchange == "CDS";
    let today = crate::services::options_service::today_ist(ctx);
    let snap = ctx.symbols.snapshot();
    let names = crate::services::search_ui_service::underlyings(
        snap.rows(),
        Some(&exchange),
        include_futures,
        today,
    );
    let is_index =
        |u: &str| NSE_INDEX_UNDERLYINGS.contains(&u) || BSE_INDEX_UNDERLYINGS.contains(&u);
    let mut indices: Vec<String> = names.iter().filter(|u| is_index(u)).cloned().collect();
    let mut rest: Vec<String> = names.iter().filter(|u| !is_index(u)).cloned().collect();
    indices.sort();
    rest.sort();
    indices.extend(rest);
    Reply::ok(json!({"status": "success", "data": indices}))
}

pub fn expiry(ctx: &AppState, underlying: &str, exchange: &str, instrumenttype: &str) -> Reply {
    let underlying = underlying.trim().to_uppercase();
    let exchange = exchange.trim().to_uppercase();
    let it = if instrumenttype.trim().is_empty() {
        "options".to_string()
    } else {
        instrumenttype.trim().to_lowercase()
    };
    if underlying.is_empty() {
        return err(400, "underlying is required");
    }
    if !LEG_EXCHANGES.contains(&exchange.as_str()) {
        return err(400, format!("Invalid exchange: {}", exchange));
    }
    let r = crate::services::symbol_service::expiry(ctx, &underlying, &exchange, &it);
    if !r.is_success() {
        return r;
    }
    let data: Vec<String> = r
        .body
        .get("data")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(normalize_expiry)
                .collect()
        })
        .unwrap_or_default();
    Reply::ok(json!({"status": "success", "data": data}))
}

/// The option ladder around ATM for an underlying and expiry.
pub async fn strikes(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    strike_count: Option<&str>,
) -> Reply {
    let underlying = underlying.trim().to_uppercase();
    let exchange = exchange.trim().to_uppercase();
    if underlying.is_empty() {
        return err(400, "underlying is required");
    }
    if !LEG_EXCHANGES.contains(&exchange.as_str()) {
        return err(400, format!("Invalid exchange: {}", exchange));
    }
    let expiry = normalize_expiry(expiry.trim());
    if expiry.is_empty() {
        return err(400, "expiry parameter is required");
    }
    let count = strike_count
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(10)
        .clamp(1, 50);
    if exchange == "NFO" || exchange == "BFO" {
        let mut r = crate::services::options_service::option_chain(
            ctx,
            &underlying,
            &exchange,
            &expiry,
            Some(count),
            false,
            None,
        )
        .await;
        if let Some(m) = r.body.as_object_mut() {
            let (sym, ex) = underlying_quote(&underlying, &exchange);
            m.insert("fo_exchange".into(), json!(exchange));
            m.insert("underlying_symbol".into(), sym);
            m.insert("underlying_exchange".into(), ex);
        }
        return r;
    }
    mcx_cds_chain(ctx, &underlying, &exchange, &expiry, count).await
}

fn ladder_leg(r: Option<&SymToken>, label: &str) -> Value {
    json!({
        "symbol": r.map(|r| r.symbol.clone()),
        "label": label,
        "lotsize": r.map(|r| r.lot_size),
        "tick_size": r.map(|r| r.tick_size),
    })
}

/// MCX and CDS: ATM measured against the current-month future.
async fn mcx_cds_chain(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    count: i64,
) -> Reply {
    let snap = ctx.symbols.snapshot();
    let mut futs: Vec<&SymToken> = snap
        .rows()
        .iter()
        .filter(|r| r.exchange == exchange && r.name == underlying && r.instrument_type == "FUT")
        .collect();
    futs.sort_by_key(|r| parse_expiry(&r.expiry));
    let Some(fut) = futs.first() else {
        return err(
            400,
            format!("No futures for {} on {}", underlying, exchange),
        );
    };
    let fut_symbol = fut.symbol.clone();
    let ltp = match broker_handle(ctx) {
        Ok(h) => {
            match crate::services::market_data_service::fetch_quote(ctx, &h, &fut_symbol, exchange)
                .await
            {
                Ok(q) => q.ltp,
                Err(_) => 0.0,
            }
        }
        Err(_) => 0.0,
    };
    if ltp.is_nan() || ltp <= 0.0 {
        return err(400, format!("No LTP for {}", fut_symbol));
    }
    let mut by_strike: BTreeMap<i64, (f64, Option<&SymToken>, Option<&SymToken>)> = BTreeMap::new();
    for r in snap.rows().iter().filter(|r| {
        r.exchange == exchange
            && r.name == underlying
            && (r.instrument_type == "CE" || r.instrument_type == "PE")
            && normalize_expiry(&r.expiry) == expiry
    }) {
        // Strikes keyed exactly (to the paisa) so float noise cannot split one.
        let k = (r.strike * 100.0).round() as i64;
        let e = by_strike.entry(k).or_insert((r.strike, None, None));
        if r.instrument_type == "CE" {
            e.1 = Some(r);
        } else {
            e.2 = Some(r);
        }
    }
    if by_strike.is_empty() {
        return err(400, "No option strikes for that expiry");
    }
    let strikes: Vec<(f64, Option<&SymToken>, Option<&SymToken>)> =
        by_strike.into_values().collect();
    let mut atm_idx = 0;
    for (i, s) in strikes.iter().enumerate() {
        if (s.0 - ltp).abs() < (strikes[atm_idx].0 - ltp).abs() {
            atm_idx = i;
        }
    }
    let c = count as usize;
    let lo = atm_idx.saturating_sub(c);
    let hi = (atm_idx + c + 1).min(strikes.len());
    let mut chain = Vec::with_capacity(hi - lo);
    for (i, (k, ce, pe)) in strikes.iter().enumerate().take(hi).skip(lo) {
        let n = i as i64 - atm_idx as i64;
        let (ce_label, pe_label) = if n == 0 {
            ("ATM".to_string(), "ATM".to_string())
        } else if n < 0 {
            (format!("ITM{}", -n), format!("OTM{}", -n))
        } else {
            (format!("OTM{}", n), format!("ITM{}", n))
        };
        chain.push(json!({
            "strike": k,
            "ce": ladder_leg(*ce, &ce_label),
            "pe": ladder_leg(*pe, &pe_label),
        }));
    }
    Reply::ok(json!({
        "status": "success",
        "underlying": underlying,
        "underlying_ltp": ltp,
        "underlying_symbol": fut_symbol,
        "underlying_exchange": exchange,
        "expiry_date": expiry,
        "atm_strike": strikes[atm_idx].0,
        "chain": chain,
        "fo_exchange": exchange,
    }))
}

pub fn search(ctx: &AppState, exchange: &str, query: &str) -> Reply {
    let exchange = exchange.trim().to_uppercase();
    let query = query.trim();
    if !is_order_exchange(&exchange) {
        return err(400, format!("Invalid exchange: {}", exchange));
    }
    if query.chars().count() < 2 {
        return Reply::ok(json!({"status": "success", "data": []}));
    }
    crate::services::symbol_service::search(ctx, query, Some(&exchange))
}

pub fn futures(ctx: &AppState, underlying: &str, exchange: &str) -> Reply {
    let underlying = underlying.trim().to_uppercase();
    let exchange = exchange.trim().to_uppercase();
    if underlying.is_empty() {
        return err(400, "underlying is required");
    }
    if !LEG_EXCHANGES.contains(&exchange.as_str()) {
        return err(400, format!("Invalid exchange: {}", exchange));
    }
    let snap = ctx.symbols.snapshot();
    let mut rows: Vec<&SymToken> = snap
        .rows()
        .iter()
        .filter(|r| r.exchange == exchange && r.name == underlying && r.instrument_type == "FUT")
        .collect();
    rows.sort_by_key(|r| parse_expiry(&r.expiry));
    let data: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "symbol": r.symbol,
                "expiry": r.expiry,
                "lotsize": r.lot_size,
                "tick_size": r.tick_size,
            })
        })
        .collect();
    Reply::ok(json!({"status": "success", "data": data}))
}

// ------------------------------------------------------------------ orders

/// The lot rules on an entry: whole lots, at most `MAX_LOTS`, under the
/// exchange freeze.
pub fn validate_quantity(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    quantity: i64,
) -> Option<String> {
    let lot = ctx
        .symbols
        .by_symbol(exchange, symbol)
        .map(|r| i64::from(r.lot_size))
        .filter(|l| *l > 0);
    let Some(lot) = lot else {
        return Some(format!(
            "Unknown symbol or lot size unavailable: {}",
            symbol
        ));
    };
    if quantity % lot != 0 {
        return Some(format!(
            "quantity must be a whole number of lots (lot size {})",
            lot
        ));
    }
    if quantity > MAX_LOTS * lot {
        return Some(format!("quantity exceeds the {}-lot cap", MAX_LOTS));
    }
    let freeze = crate::services::symbol_service::freeze_qty_for_option(symbol, exchange);
    if freeze > 0 && quantity > freeze {
        return Some(format!(
            "quantity exceeds the exchange freeze limit ({})",
            freeze
        ));
    }
    None
}

struct OrderFields {
    symbol: String,
    exchange: String,
    action: String,
    product: String,
    quantity: i64,
}

fn order_fields(body: &Map<String, Value>, default_product: &str) -> Result<OrderFields, Reply> {
    let symbol = text(body, "symbol");
    let exchange = text(body, "exchange").to_uppercase();
    let action = text(body, "action").to_uppercase();
    let product = {
        let p = text(body, "product").to_uppercase();
        if p.is_empty() {
            default_product.to_string()
        } else {
            p
        }
    };
    let quantity = py_int(body.get("quantity")).unwrap_or(0);
    if symbol.is_empty() {
        return Err(err(400, "symbol is required"));
    }
    if !is_order_exchange(&exchange) {
        return Err(err(400, format!("Invalid exchange: {}", exchange)));
    }
    if !VALID_ACTIONS.contains(&action.as_str()) {
        return Err(err(400, format!("Invalid action: {}", action)));
    }
    if !allowed_products(&exchange).contains(&product.as_str()) {
        return Err(err(
            400,
            format!("Invalid product for {}: {}", exchange, product),
        ));
    }
    if quantity <= 0 {
        return Err(err(400, "quantity must be positive"));
    }
    if quantity > MAX_ORDER_QUANTITY {
        return Err(err(400, "quantity exceeds the safety limit"));
    }
    Ok(OrderFields {
        symbol,
        exchange,
        action,
        product,
        quantity,
    })
}

fn accepted(r: &Reply) -> bool {
    r.is_success() && r.body.get("status").and_then(Value::as_str) != Some("error")
}

/// `POST /scalping/api/order`: one MARKET entry.
pub async fn place(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let f = match order_fields(body, "MIS") {
        Ok(f) => f,
        Err(r) => return r,
    };
    if is_derivative(&f.exchange) {
        if let Some(lots) = body.get("lots").filter(|v| !v.is_null()) {
            let Ok(lots) = py_int(Some(lots)) else {
                return err(400, "lots must be an integer");
            };
            if !(1..=MAX_LOTS).contains(&lots) {
                return err(400, format!("lots must be between 1 and {}", MAX_LOTS));
            }
        }
        if let Some(e) = validate_quantity(ctx, &f.symbol, &f.exchange, f.quantity) {
            return err(400, e);
        }
    }
    let req = json!({
        "strategy": SCALPING_STRATEGY,
        "symbol": f.symbol,
        "exchange": f.exchange,
        "action": f.action,
        "pricetype": "MARKET",
        "product": f.product,
        "quantity": f.quantity,
        "price": 0,
        "trigger_price": 0,
        "disclosed_quantity": 0,
    });
    let r = crate::services::order_service::place_order(ctx, &req, Route::INTERNAL).await;
    if accepted(&r) {
        let mode = current_mode(ctx);
        if let Err(e) = ctx
            .scalping
            .store
            .track(&f.symbol, &f.exchange, &f.product, mode)
        {
            tracing::error!("Scalping list could not record {}: {}", f.symbol, e);
        }
    }
    r
}

/// The freeze-safe, whole-lot risk-reducing exit (web `_reducing_exit`).
pub async fn reducing_exit(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    product: &str,
    action: &str,
    quantity: i64,
) -> Reply {
    let chunk = match exit_chunk(&ctx.symbols, symbol, exchange, quantity) {
        Ok(c) => c,
        Err(e) => return err(400, e),
    };
    if let Some(c) = chunk.filter(|c| quantity > *c) {
        let req = json!({
            "strategy": SCALPING_STRATEGY,
            "symbol": symbol,
            "exchange": exchange,
            "action": action,
            "quantity": quantity,
            "splitsize": c,
            "pricetype": "MARKET",
            "product": product,
        });
        return crate::services::batch_order_service::split_order(ctx, &req, Route::INTERNAL).await;
    }
    let req = json!({
        "strategy": SCALPING_STRATEGY,
        "symbol": symbol,
        "exchange": exchange,
        "action": action,
        "pricetype": "MARKET",
        "product": product,
        "quantity": quantity,
        "price": 0,
        "trigger_price": 0,
        "disclosed_quantity": 0,
    });
    crate::services::order_service::place_order(ctx, &req, Route::INTERNAL).await
}

/// `POST /scalping/api/close_leg`.
pub async fn close_leg(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let f = match order_fields(body, "NRML") {
        Ok(f) => f,
        Err(r) => return r,
    };
    reducing_exit(
        ctx,
        &f.symbol,
        &f.exchange,
        &f.product,
        &f.action,
        f.quantity,
    )
    .await
}

/// `POST /scalping/api/close_all`: only the scalping list's open positions.
pub async fn close_all(ctx: &AppState) -> Reply {
    let mode = current_mode(ctx);
    let tracked = ctx.scalping.store.tracked(Some(mode)).unwrap_or_else(|e| {
        tracing::error!("Scalping list could not be read: {}", e);
        Vec::new()
    });
    if tracked.is_empty() {
        return Reply::ok(json!({
            "status": "success",
            "message": "No scalping positions to close",
            "results": [],
        }));
    }
    let book = crate::services::account_service::positionbook(ctx).await;
    if !accepted(&book) {
        let msg = book.message();
        let msg = if msg.is_empty() {
            "positionbook unavailable".to_string()
        } else {
            msg
        };
        return err(502, format!("Could not fetch positions to close: {}", msg));
    }
    let mut results = Vec::new();
    let mut closed = 0;
    for t in tracked {
        let net = super::monitor::net_quantity(&book.body, &t.symbol, &t.exchange, &t.product);
        if net == 0 {
            continue;
        }
        let action = if net > 0 { "SELL" } else { "BUY" };
        let r = reducing_exit(ctx, &t.symbol, &t.exchange, &t.product, action, net.abs()).await;
        let ok = accepted(&r);
        results.push(json!({
            "symbol": t.symbol,
            "status": if ok { "success" } else { "error" },
            "message": r.body.get("message").cloned().unwrap_or(Value::Null),
        }));
        if ok {
            closed += 1;
        }
    }
    Reply::ok(json!({
        "status": "success",
        "message": format!("Closed {} scalping position(s)", closed),
        "results": results,
    }))
}

/// `POST /scalping/api/cancel_all`.
pub async fn cancel_all(ctx: &AppState) -> Reply {
    crate::services::order_service::cancel_all_orders(ctx, &json!({}), Route::INTERNAL).await
}

// ------------------------------------------------------------------ list and stops

pub fn tracked(ctx: &AppState) -> Reply {
    match ctx.scalping.store.tracked(Some(current_mode(ctx))) {
        Ok(rows) => Reply::ok(json!({
            "status": "success",
            "data": rows.iter().map(|t| t.to_dict()).collect::<Vec<_>>(),
        })),
        Err(e) => {
            tracing::error!("Scalping list could not be read: {}", e);
            Reply::ok(json!({"status": "success", "data": []}))
        }
    }
}

pub fn reset_tracked(ctx: &AppState) -> Reply {
    let cleared = match ctx.scalping.store.clear_tracked(Some(current_mode(ctx))) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!("Scalping list could not be cleared: {}", e);
            false
        }
    };
    Reply::ok(json!({"status": "success", "cleared": cleared}))
}

pub fn get_sl(ctx: &AppState) -> Reply {
    match ctx.scalping.store.active_sl(Some(current_mode(ctx))) {
        Ok(rows) => Reply::ok(json!({
            "status": "success",
            "data": rows.iter().map(|s| s.to_dict()).collect::<Vec<_>>(),
        })),
        Err(e) => {
            tracing::error!("Scalping stops could not be read: {}", e);
            Reply::ok(json!({"status": "success", "data": []}))
        }
    }
}

const SL_PRICE_FIELDS: &[&str] = &[
    "entry_price",
    "initial_sl",
    "trailing_step",
    "highest_price",
    "lowest_price",
    "current_sl",
    "target",
];

/// `POST /scalping/api/sl`: create or update one leg's stop.
pub fn upsert_sl(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let symbol = text(body, "symbol");
    let exchange = text(body, "exchange").to_uppercase();
    let product = text(body, "product").to_uppercase();
    if symbol.is_empty()
        || !LEG_EXCHANGES.contains(&exchange.as_str())
        || !allowed_products(&exchange).contains(&product.as_str())
    {
        return err(400, "Invalid symbol/exchange/product");
    }
    let side = {
        let s = text(body, "side").to_uppercase();
        if s.is_empty() {
            "BUY".to_string()
        } else {
            s
        }
    };
    if !VALID_ACTIONS.contains(&side.as_str()) {
        return err(400, format!("Invalid side: {}", side));
    }
    let quantity = match py_int(body.get("quantity").filter(|v| truthy(v))) {
        Ok(q) => q,
        Err(NotANumber) => return err(400, "quantity must be an integer"),
    };
    if !(0..=MAX_ORDER_QUANTITY).contains(&quantity) {
        return err(400, "quantity out of range");
    }
    let mut u = SlUpsert {
        symbol,
        exchange,
        product,
        mode: current_mode(ctx).to_string(),
        side: Some(side),
        quantity: Some(quantity),
        ..Default::default()
    };
    for field in SL_PRICE_FIELDS {
        let Some(v) = body.get(*field).filter(|v| !v.is_null()) else {
            continue;
        };
        let Ok(val) = py_float(v) else {
            return err(400, format!("{} must be a number", field));
        };
        if !val.is_finite() || val < 0.0 {
            return err(400, format!("{} out of range", field));
        }
        let slot = match *field {
            "entry_price" => &mut u.entry_price,
            "initial_sl" => &mut u.initial_sl,
            "trailing_step" => &mut u.trailing_step,
            "highest_price" => &mut u.highest_price,
            "lowest_price" => &mut u.lowest_price,
            "current_sl" => &mut u.current_sl,
            _ => &mut u.target,
        };
        *slot = Some(val);
    }
    if let Some(v) = body.get("trailing_enabled") {
        u.trailing_enabled = Some(truthy(v));
    }
    if let Some(v) = body.get("is_active") {
        u.is_active = Some(truthy(v));
    }
    match ctx.scalping.store.upsert_sl(&u) {
        Ok(row) => {
            ctx.scalping.monitor.request_sync();
            Reply::ok(json!({"status": "success", "data": row.to_dict()}))
        }
        Err(e) => {
            tracing::error!("Scalping stop for {} could not be saved: {}", u.symbol, e);
            err(500, "Failed to save SL state")
        }
    }
}

/// `DELETE /scalping/api/sl`.
pub fn delete_sl(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let symbol = text(body, "symbol");
    let exchange = text(body, "exchange").to_uppercase();
    let product = text(body, "product").to_uppercase();
    if symbol.is_empty()
        || !LEG_EXCHANGES.contains(&exchange.as_str())
        || !VALID_PRODUCTS.contains(&product.as_str())
    {
        return err(400, "Invalid symbol/exchange/product");
    }
    let deleted =
        match ctx
            .scalping
            .store
            .delete_sl(&symbol, &exchange, &product, Some(current_mode(ctx)))
        {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("Scalping stop for {} could not be removed: {}", symbol, e);
                false
            }
        };
    ctx.scalping.monitor.request_sync();
    Reply::ok(json!({"status": "success", "deleted": deleted}))
}
