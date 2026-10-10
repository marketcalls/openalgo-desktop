//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).
//!
//! * MARKET is sent as a LIMIT at LTP +0.1% (BUY) / -0.1% (SELL) when a
//!   quote is available, else as a native MARKET order.
//! * SL / SL-M have no stop type on `/order`; they go to `/smart/order` as
//!   `order_type: TRIGGER` (NSE only, validity DAY) with a trigger-limit:
//!   SL uses its limit price (or the MPP-protected trigger when none), SL-M
//!   always the MPP-protected trigger.
//! * Cancel / modify of a smart order (a `GTT-` id, or a book row whose type
//!   is TRIGGER / OCO / GTT_*) use the `/smart/order/*` endpoints.

use super::mapping::{self, text};
use super::{data, token, unwrap_account, IndmoneyBroker};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price, py_round};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// web `_protective_limit`: the MPP-protected limit off a trigger, or the
/// trigger itself when the instrument has no tick size.
pub fn protective_limit(trigger: f64, action: Action, symbol: &str, tick: f64) -> f64 {
    if tick > 0.0 {
        protected_price(
            trigger,
            action,
            instrument_type_from_symbol(symbol),
            Some(tick),
        )
    } else {
        trigger
    }
}

/// The body `place_order_api` posts (web `transform_data`). `ltp` is the
/// quote used to price a MARKET order (`None` or 0 sends native MARKET).
/// Returns the endpoint and the body.
pub fn place_body(o: &ResolvedOrder, ltp: Option<f64>) -> Result<(&'static str, Value)> {
    if !mapping::exchange_is_placeable(o.exchange) {
        return Err(AppError::Validation(format!(
            "INDmoney does not accept orders on {}.",
            o.exchange
        )));
    }
    let ex = o.exchange.as_str();
    let api_ex = mapping::api_exchange(ex);
    let mut m = Map::new();
    m.insert("txn_type".into(), json!(o.action.as_str()));
    m.insert("exchange".into(), json!(api_ex));
    m.insert("segment".into(), json!(mapping::segment(ex)));
    m.insert("product".into(), json!(mapping::product(o.product)));
    m.insert("security_id".into(), json!(o.token()));
    m.insert("qty".into(), json!(o.quantity));
    m.insert("algo_id".into(), json!(mapping::algo_id(api_ex)));

    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        let trigger = o.trigger_price;
        if trigger <= 0.0 {
            return Err(AppError::Validation(format!(
                "A trigger price is required for an {} order ({}).",
                o.pricetype, o.symbol
            )));
        }
        if api_ex != "NSE" {
            return Err(AppError::Validation(format!(
                "INDmoney supports stop orders (SL/SL-M) on NSE only; {} was requested for {}.",
                o.exchange, o.symbol
            )));
        }
        let tick = o.instrument.tick_size;
        let limit = if o.pricetype == PriceType::Sl && o.price > 0.0 {
            o.price
        } else {
            protective_limit(trigger, o.action, &o.symbol, tick)
        };
        m.insert("order_type".into(), json!("TRIGGER"));
        m.insert("validity".into(), json!("DAY"));
        m.insert("trigger_price".into(), json!(trigger));
        m.insert("trigger_limit_price".into(), json!(limit));
        return Ok(("/smart/order", Value::Object(m)));
    }

    let mut order_type = mapping::order_type(o.pricetype);
    let mut limit = o.price;
    if o.pricetype == PriceType::Market {
        match ltp.filter(|p| *p > 0.0) {
            Some(l) => {
                let factor = match o.action {
                    Action::Buy => 1.001,
                    Action::Sell => 0.999,
                };
                limit = py_round(l * factor, 2);
                order_type = "LIMIT";
            }
            None => tracing::warn!(
                "No LTP for {} to price a MARKET order; sending a native MARKET order",
                o.symbol
            ),
        }
    }
    m.insert("order_type".into(), json!(order_type));
    m.insert(
        "validity".into(),
        json!(if o.validity == Validity::Ioc {
            "IOC"
        } else {
            "DAY"
        }),
    );
    m.insert("is_amo".into(), json!(o.amo));
    if order_type == "LIMIT" {
        m.insert("limit_price".into(), json!(limit));
    }
    Ok(("/order", Value::Object(m)))
}

/// Order id of a placement answer: `/order` -> `data.order_id`,
/// `/smart/order` -> `data.order_data[n].order_id`.
pub fn extract_order_id(v: &Value) -> Option<String> {
    let data = v.get("data")?;
    let id = text(data, "order_id");
    if !id.is_empty() {
        return Some(id);
    }
    data.get("order_data")?
        .as_array()?
        .iter()
        .map(|e| text(e, "order_id"))
        .find(|s| !s.is_empty())
}

/// web `place_order_api` response handling.
pub fn place_outcome(status: u16, v: &Value) -> Result<String> {
    if status == 401 || status == 403 {
        return Err(super::session_expired());
    }
    let st = v.get("status").and_then(Value::as_str);
    if (status == 200 || status == 201) && st == Some("success") {
        return extract_order_id(v).ok_or_else(|| {
            AppError::uncertain(
                "INDmoney accepted the order but returned no order id. Check the order book.",
                None,
            )
        });
    }
    if (status == 200 || status == 201) && st == Some("failure") {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("msg"))
            .and_then(Value::as_str)
        {
            if msg
                .to_ascii_lowercase()
                .contains("no order number in rs response")
            {
                tracing::warn!("INDmoney order likely placed despite: {}", msg);
                return Ok("ORDER_PLACED".into());
            }
        }
    }
    if status == 429 {
        return Err(AppError::Broker(
            "INDmoney is limiting orders right now. The order was not resent; check the order book before trying again.".into(),
        ));
    }
    Err(AppError::Broker(
        super::error_message(v).unwrap_or_else(|| "INDmoney rejected the order.".into()),
    ))
}

pub async fn place_order(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let tok = token(auth)?;
    let ltp = if o.pricetype == PriceType::Market && mapping::exchange_is_placeable(o.exchange) {
        match data::get_quote(b, auth, &QuoteKey::new(o.exchange.as_str(), &o.symbol)).await {
            Ok(q) => Some(q.ltp),
            Err(e) => {
                tracing::warn!("Quote for MARKET order pricing failed: {}", e.code());
                None
            }
        }
    } else {
        None
    };
    let (path, body) = place_body(o, ltp)?;
    let r = b.send(Method::POST, path, &[], Some(&body), tok).await?;
    let id = place_outcome(r.status, &r.json)?;
    b.remember_order_ids([id.as_str()]);
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

/// The raw order book.
pub async fn raw_order_book(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let v = b
        .account_call(Method::GET, "/order-book", &[], None, auth)
        .await?;
    let rows = mapping::rows(&v);
    b.remember_order_ids(
        rows.iter()
            .filter_map(|o| o.get("id").and_then(Value::as_str)),
    );
    Ok(rows)
}

/// Whether `order_id` belongs to the smart-order book (web
/// `_is_smart_order`; on any doubt, the regular endpoint).
pub fn is_smart(order_id: &str, book: &[Value]) -> bool {
    if order_id.starts_with("GTT-") {
        return true;
    }
    book.iter()
        .find(|o| text(o, "id") == order_id)
        .map(|o| mapping::is_smart_type(&text(o, "order_type")))
        .unwrap_or(false)
}

async fn smart(b: &IndmoneyBroker, auth: &AuthToken, order_id: &str) -> bool {
    if order_id.starts_with("GTT-") {
        return true;
    }
    match raw_order_book(b, auth).await {
        Ok(book) => is_smart(order_id, &book),
        Err(e) => {
            tracing::warn!("Could not classify order {}: {}", order_id, e.code());
            false
        }
    }
}

/// web `transform_modify_order_data` (+ the smart-order extras).
pub fn modify_body(m: &ResolvedModify, smart: bool) -> Value {
    let mut body = Map::new();
    body.insert(
        "segment".into(),
        json!(mapping::segment_from_order_id(&m.order_id)),
    );
    body.insert("order_id".into(), json!(m.order_id));
    body.insert("qty".into(), json!(m.quantity));
    if smart {
        body.insert("algo_id".into(), json!("99999"));
        if m.trigger_price != 0.0 {
            body.insert("trigger_price".into(), json!(m.trigger_price));
            body.insert("trigger_limit_price".into(), json!(m.price));
            return Value::Object(body);
        }
    }
    body.insert("limit_price".into(), json!(m.price));
    Value::Object(body)
}

fn write_outcome(status: u16, v: &Value, order_id: &str, what: &str) -> Result<OrderResponse> {
    if status == 401 || status == 403 {
        return Err(super::session_expired());
    }
    if status == 200 && v.get("status").and_then(Value::as_str) == Some("success") {
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    Err(AppError::Broker(
        super::error_message(v).unwrap_or_else(|| format!("Failed to {} order", what)),
    ))
}

pub async fn modify_order(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let tok = token(auth)?;
    let is_smart = smart(b, auth, &m.order_id).await;
    let body = modify_body(m, is_smart);
    let path = if is_smart {
        "/smart/order/modify"
    } else {
        "/order/modify"
    };
    let r = b.send(Method::POST, path, &[], Some(&body), tok).await?;
    write_outcome(r.status, &r.json, &m.order_id, "modify")
}

pub fn cancel_body(order_id: &str) -> Value {
    json!({"segment": mapping::segment_from_order_id(order_id), "order_id": order_id})
}

async fn cancel_with(
    b: &IndmoneyBroker,
    tok: &str,
    order_id: &str,
    is_smart: bool,
) -> Result<OrderResponse> {
    let path = if is_smart {
        "/smart/order/cancel"
    } else {
        "/order/cancel"
    };
    let r = b
        .send(Method::POST, path, &[], Some(&cancel_body(order_id)), tok)
        .await?;
    write_outcome(r.status, &r.json, order_id, "cancel")
}

pub async fn cancel_order(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let tok = token(auth)?;
    let is_smart = smart(b, auth, order_id).await;
    cancel_with(b, tok, order_id, is_smart).await
}

/// Cancel every open / trigger-pending order, classifying each from the one
/// order book read rather than re-reading it per order.
pub async fn cancel_all_orders(b: &IndmoneyBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let tok = token(auth)?;
    let book = raw_order_book(b, auth).await?;
    let mut out = CancelAllResult::default();
    for o in book
        .iter()
        .filter(|o| mapping::is_cancellable(&text(o, "status")))
    {
        let id = text(o, "id");
        if id.is_empty() {
            continue;
        }
        let is_smart = is_smart(&id, &book);
        match cancel_with(b, tok, &id, is_smart).await {
            Ok(_) => out.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of INDmoney order {} failed: {}", id, e.code());
                out.failed.push(id);
            }
        }
    }
    Ok(out)
}

pub async fn get_order_book(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let book = raw_order_book(b, auth).await?;
    Ok(book
        .iter()
        .map(|o| mapping::map_order(b.resolver(), o))
        .collect())
}

pub async fn get_trade_book(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let mut raw: Vec<(Value, &'static str)> = Vec::new();
    for seg in ["EQUITY", "DERIVATIVE"] {
        match b
            .account_call(
                Method::GET,
                "/trade-book",
                &[("segment", seg.to_string())],
                None,
                auth,
            )
            .await
        {
            Ok(v) => raw.extend(mapping::rows(&v).into_iter().map(|t| (t, seg))),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => tracing::warn!("INDmoney {} trade book failed: {}", seg, e.code()),
        }
    }
    let book = match raw_order_book(b, auth).await {
        Ok(b) => b,
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(_) => Vec::new(),
    };
    let facts = mapping::order_facts(&book);
    Ok(raw
        .iter()
        .map(|(t, seg)| mapping::map_trade(b.resolver(), t, seg, &facts))
        .collect())
}

/// The four position queries (web `position_queries`).
pub const POSITION_QUERIES: &[(&str, &str)] = &[
    ("derivative", "margin"),
    ("derivative", "intraday"),
    ("equity", "cnc"),
    ("equity", "intraday"),
];

const EMPTY_BOOK_MARKERS: &[&str] = &[
    "no data",
    "nodata",
    "no_data",
    "no-data",
    "no position",
    "no open position",
    "no record",
    "have any position",
];

/// web `says_no_positions`: an error whose message means an empty book.
pub fn says_no_positions(v: &Value) -> bool {
    [
        "emsg",
        "message",
        "msg",
        "errorMessage",
        "statusMessage",
        "errMsg",
        "description",
        "s",
    ]
    .iter()
    .filter_map(|k| v.get(*k).and_then(Value::as_str))
    .chain(
        v.get("error")
            .and_then(|e| e.get("msg"))
            .and_then(Value::as_str),
    )
    .filter(|s| s.len() <= 2000)
    .any(|s| {
        let l = s.to_ascii_lowercase();
        EMPTY_BOOK_MARKERS.iter().any(|m| l.contains(m))
    })
}

/// Every position row tagged with its query, plus the query segments that
/// could not be read.
pub async fn fetch_positions(
    b: &IndmoneyBroker,
    auth: &AuthToken,
) -> Result<(Vec<Value>, HashSet<&'static str>)> {
    let tok = token(auth)?;
    let mut rows = Vec::new();
    let mut failed = HashSet::new();
    for (seg, prod) in POSITION_QUERIES {
        let r = b
            .send(
                Method::GET,
                "/portfolio/positions",
                &[("segment", seg.to_string()), ("product", prod.to_string())],
                None,
                tok,
            )
            .await;
        let v = match r {
            Ok(r) => match unwrap_account("/portfolio/positions", &r) {
                Ok(v) => v,
                Err(e @ AppError::Auth(_)) => return Err(e),
                Err(_) if says_no_positions(&r.json) => Value::Array(vec![]),
                Err(e) => {
                    tracing::warn!("INDmoney {} {} positions failed: {}", seg, prod, e.code());
                    failed.insert(*seg);
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!("INDmoney {} {} positions failed: {}", seg, prod, e.code());
                failed.insert(*seg);
                continue;
            }
        };
        for mut p in mapping::rows(&v) {
            if let Some(o) = p.as_object_mut() {
                o.insert("query_segment".into(), json!(seg));
                o.insert("query_product".into(), json!(prod));
            }
            rows.push(p);
        }
    }
    Ok((rows, failed))
}

/// Attach `last_traded_price` from one batched `/market/quotes/ltp` call
/// (positions carry no live price). Failures leave the rows unpriced.
async fn enrich_ltp(b: &IndmoneyBroker, auth: &AuthToken, rows: &mut [Value]) {
    let mut codes: Vec<Option<String>> = Vec::with_capacity(rows.len());
    for p in rows.iter() {
        let token = text(p, "security_id");
        let code = if mapping::position_qty(p) == 0 || token.is_empty() {
            None
        } else {
            let ex = mapping::position_exchange(b.resolver(), p);
            matches!(ex.as_str(), "NSE" | "BSE" | "NFO" | "BFO")
                .then(|| format!("{}_{}", ex, token))
        };
        codes.push(code);
    }
    let mut unique: Vec<String> = codes.iter().flatten().cloned().collect();
    unique.sort();
    unique.dedup();
    if unique.is_empty() {
        return;
    }
    let quotes = match data::market_call(b, auth, "/market/quotes/ltp", &unique.join(",")).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Could not price INDmoney positions: {}", e.message());
            return;
        }
    };
    for (p, code) in rows.iter_mut().zip(codes) {
        let Some(code) = code else { continue };
        if let Some(lp) = quotes.get(&code).and_then(|q| q.get("live_price")) {
            if !lp.is_null() {
                if let Some(o) = p.as_object_mut() {
                    o.insert("last_traded_price".into(), lp.clone());
                }
            }
        }
    }
}

pub async fn get_positions(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let (mut rows, failed) = fetch_positions(b, auth).await?;
    if failed.len() == 2 && rows.is_empty() {
        return Err(AppError::Broker(
            "INDmoney did not return the position book. Try again shortly.".into(),
        ));
    }
    enrich_ltp(b, auth, &mut rows).await;
    Ok(rows
        .iter()
        .map(|p| mapping::map_position(b.resolver(), p))
        .collect())
}

fn segment_of(exchange: Exchange) -> Option<&'static str> {
    match exchange {
        Exchange::Nse | Exchange::Bse => Some("equity"),
        Exchange::Nfo | Exchange::Bfo => Some("derivative"),
        _ => None,
    }
}

/// web `get_open_position`: first row whose token (or broker symbol) and
/// resolved exchange match. Like the web, the product is not compared. A
/// read that failed for the symbol's segment is an error, never "flat".
pub async fn get_open_position(
    b: &IndmoneyBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    _product: Product,
) -> Result<i64> {
    let row = b.resolver().by_symbol(exchange.as_str(), symbol);
    let target_token = row.as_ref().map(|r| r.token.clone()).unwrap_or_default();
    let brsymbol = row
        .as_ref()
        .map(|r| r.br_symbol().to_string())
        .unwrap_or_else(|| symbol.to_string());
    let (rows, failed) = fetch_positions(b, auth).await?;
    if !failed.is_empty() {
        match segment_of(exchange) {
            Some(seg) if !failed.contains(seg) => {}
            _ => {
                return Err(AppError::Broker(
                    "INDmoney did not return the position book, so the open position is unknown. Try again shortly."
                        .into(),
                ))
            }
        }
    }
    for p in &rows {
        let tok = text(p, "security_id");
        let sym = Some(text(p, "symbol"))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text(p, "trading_symbol"));
        let token_match = !target_token.is_empty() && tok == target_token;
        if (token_match || sym == brsymbol)
            && mapping::position_exchange(b.resolver(), p) == exchange.as_str()
        {
            return Ok(mapping::position_qty(p));
        }
    }
    Ok(0)
}

pub async fn get_holdings(b: &IndmoneyBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = b
        .account_call(Method::GET, "/portfolio/holdings", &[], None, auth)
        .await?;
    Ok(mapping::rows(&v)
        .iter()
        .map(|h| mapping::map_holding(b.resolver(), h))
        .collect())
}
