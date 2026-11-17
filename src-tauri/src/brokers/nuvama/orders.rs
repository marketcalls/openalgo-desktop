//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).
//!
//! Routes (SDK `constants/router.py`, under the EQ host):
//! `POST trade/placetrade/v1/{userid}/`, `PUT trade/modifytrade/v1/{userid}/`,
//! `PUT trade/canceltrade/v1/{userid}/`, `GET order/book/{userid}/v1/`,
//! `GET tradebook/v1/{userid}/`, `GET positions/net/{userid}/`,
//! `GET holdings/v1/rmsholdings/{userid}/`.
//!
//! Modify and cancel need the order's instrument, side, product and
//! quantity, so both read the order's row from the order book first.
//! MARKET and SL-M go out as LIMIT and STOP_LIMIT priced with Market Price
//! Protection: MARKET off the LTP, SL-M off its own trigger, so the stop's
//! limit is never on the wrong side of it (Nuvama refused an LTP-priced
//! one with 16315). A MARKET without a usable quote goes out as MARKET.

use super::books::{self, NuvamaBook};
use super::client::{Call, Lane};
use super::mapping::{self, nuvama_exchange, product_from_nuvama, text};
use super::session::NuvamaSession;
use super::NuvamaBroker;
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::common::outcome;
use crate::brokers::common::position_read;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

/// A float as Python's `str()` writes it (`100.0`, `1503.45`): the web
/// sends every number as text.
pub(crate) fn py_float(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{:.1}", v)
    } else {
        format!("{}", v)
    }
}

/// A book value as the web's `str(value)` passes it on.
fn py_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Number(n)) if n.is_f64() => n.as_f64().map(py_float).unwrap_or_default(),
        other => text(other),
    }
}

/// `{eq}<route>` with the account in its `{userid}` segment.
fn route(b: &NuvamaBroker, s: &NuvamaSession, template: &str) -> String {
    format!(
        "{}{}",
        b.urls.eq,
        template.replace("{userid}", &urlencoding::encode(&s.account_id))
    )
}

fn book_route(which: NuvamaBook) -> &'static str {
    match which {
        NuvamaBook::Orders => "order/book/{userid}/v1/",
        NuvamaBook::Trades => "tradebook/v1/{userid}/",
        NuvamaBook::Positions => "positions/net/{userid}/",
        NuvamaBook::Holdings => "holdings/v1/rmsholdings/{userid}/",
    }
}

/// The rows of one book, each an object.
pub(crate) async fn read_book(
    b: &NuvamaBroker,
    auth: &AuthToken,
    which: NuvamaBook,
) -> Result<Vec<Value>> {
    let s = b.session(auth)?;
    let a = b
        .call(
            &s,
            Call::get(route(b, &s, book_route(which)), Lane::Account),
        )
        .await?;
    let rows = books::rows(&a, which)?;
    books::objects(&rows, which.book())?;
    Ok(rows)
}

pub async fn get_order_book(b: &NuvamaBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let rows = read_book(b, auth, NuvamaBook::Orders).await?;
    Ok(rows
        .iter()
        .map(|r| books::order_row(r, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &NuvamaBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = read_book(b, auth, NuvamaBook::Trades).await?;
    Ok(rows
        .iter()
        .map(|r| books::trade_row(r, b.resolver()))
        .collect())
}

/// The position rows, every one with a readable `ntQty`.
async fn position_rows(b: &NuvamaBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let rows = read_book(b, auth, NuvamaBook::Positions).await?;
    book_read::require_quantities(&rows, &["ntQty"], || books::unread(Book::Positions))?;
    Ok(rows)
}

pub async fn get_positions(b: &NuvamaBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let rows = position_rows(b, auth).await?;
    Ok(rows
        .iter()
        .map(|r| books::position_row(r, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &NuvamaBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    Ok(get_holdings_with_totals(b, auth).await?.holdings)
}

pub async fn get_holdings_with_totals(b: &NuvamaBroker, auth: &AuthToken) -> Result<HoldingsBook> {
    let rows = read_book(b, auth, NuvamaBook::Holdings).await?;
    Ok(HoldingsBook {
        holdings: rows
            .iter()
            .filter_map(|r| books::holding_row(r, b.resolver()))
            .collect(),
        totals: Some(books::portfolio_totals(&rows)),
    })
}

/// Net quantity of one instrument and product (web `get_open_position`):
/// the position row with its token and product. A book that cannot be
/// read refuses the smart order (web `refuse_smart_order_on_read_failure`).
pub async fn get_open_position(
    b: &NuvamaBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let token = b
        .resolver()
        .token(symbol, exchange.as_str())
        .ok_or_else(|| {
            AppError::Validation(format!(
                "{} is not in the Nuvama master contract for {}. Download the master contract again and check the symbol.",
                symbol, exchange
            ))
        })?;
    let rows = position_rows(b, auth).await.map_err(|e| match e {
        AppError::Auth(_) => e,
        other => {
            tracing::warn!("Nuvama position read failed: {}", other.code());
            position_read::unread("Nuvama")
        }
    })?;
    let row = rows.iter().find(|r| {
        text(r.get("sym")) == token
            && product_from_nuvama(&text(r.get("prdCode"))) == product.as_str()
    });
    Ok(row.and_then(books::net_quantity).unwrap_or(0))
}

/// What an order asks for, as priced.
struct Terms {
    pricetype: PriceType,
    action: Action,
    price: f64,
    trigger: f64,
}

/// `(ordTyp, lmPrc)` with Market Price Protection (web
/// `apply_market_protection`).
async fn priced(
    b: &NuvamaBroker,
    auth: &AuthToken,
    row: &SymbolData,
    key: QuoteKey,
    terms: Terms,
) -> Result<(&'static str, String)> {
    let Terms {
        pricetype,
        action,
        price,
        trigger,
    } = terms;
    let tick = Some(row.tick_size).filter(|t| *t > 0.0);
    let itype = instrument_type_from_symbol(&key.symbol);
    match pricetype {
        PriceType::Limit | PriceType::Sl => Ok((mapping::order_type(pricetype), py_float(price))),
        PriceType::SlM => {
            if trigger <= 0.0 {
                return Err(AppError::Validation(
                    "An SL-M order needs a trigger price.".into(),
                ));
            }
            // The trigger is on a tick already; without a tick size the
            // trigger itself is the limit (still a valid stop).
            let limit = match tick {
                Some(_) => protected_price(trigger, action, itype, tick)?,
                None => trigger,
            };
            Ok(("STOP_LIMIT", py_float(limit)))
        }
        PriceType::Market => {
            let ltp = outcome::before_send(super::data::get_quote(b, auth, &key))
                .await
                .map(|q| q.ltp)
                .unwrap_or_else(|e| {
                    tracing::warn!("MPP: no Nuvama quote for {}: {}", key.symbol, e.code());
                    0.0
                });
            if ltp > 0.0 {
                let limit = protected_price(ltp, action, itype, tick)?;
                return Ok(("LIMIT", py_float(limit)));
            }
            tracing::warn!("MPP: no LTP for {}; sending MARKET unconverted", key.symbol);
            Ok(("MARKET", "0".into()))
        }
    }
}

/// The place-order body (web `transform_data`): every number as text.
pub(crate) fn place_body(o: &ResolvedOrder, ord_typ: &str, lm_prc: String) -> Value {
    let row = &o.instrument;
    json!({
        "trdSym": row.brsymbol,
        "exc": nuvama_exchange(o.exchange.as_str()),
        "action": o.action.as_str(),
        "dur": "DAY",
        "ordTyp": ord_typ,
        "qty": o.quantity.to_string(),
        "dscQty": o.disclosed_quantity.to_string(),
        "sym": row.token,
        "mktPro": "",
        "lmPrc": lm_prc,
        "trgPrc": py_float(o.trigger_price),
        "prdCode": mapping::product(o.product),
        "posSqr": "N",
        "minQty": "0",
        "ordSrc": "API",
        "vnCode": "",
        "rmk": "",
        "flQty": "0",
    })
}

/// The order id of a place answer: `data.oid`, or a sliced order's first
/// slice (web `_order_id_from`).
fn order_id_of(body: &Value) -> String {
    let data = body.get("data");
    let id = data.and_then(|d| d.get("oid")).or_else(|| {
        data.and_then(|d| d.get("ord"))
            .and_then(Value::as_array)
            .and_then(|o| o.first())
            .and_then(|o| o.get("oid"))
    });
    text(id)
}

pub async fn place_order(
    b: &NuvamaBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = b.session(auth)?;
    let key = QuoteKey::new(o.exchange.as_str(), &o.symbol);
    let (ord_typ, lm_prc) = priced(
        b,
        auth,
        &o.instrument,
        key,
        Terms {
            pricetype: o.pricetype,
            action: o.action,
            price: o.price,
            trigger: o.trigger_price,
        },
    )
    .await?;
    let body = place_body(o, ord_typ, lm_prc);
    let url = route(b, &s, "trade/placetrade/v1/{userid}/");
    let a = b
        .call(&s, Call::send(Method::POST, url, &body, Lane::Orders))
        .await?;
    let id = if a.ok() {
        order_id_of(&a.body)
    } else {
        String::new()
    };
    if !id.is_empty() {
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    if a.ok() {
        tracing::warn!("Nuvama accepted an order without an order id");
        return Err(AppError::uncertain(
            "Nuvama accepted the order but returned no order id. Check the order book before placing it again.",
            None,
        ));
    }
    let message = a.message("Nuvama did not accept the order");
    tracing::warn!(
        status = a.status,
        code = a.error_code().unwrap_or(""),
        "Nuvama did not place an order: {}",
        message
    );
    let failed = a.body.get("error").is_some_and(|e| !e.is_null());
    Err(outcome::not_accepted(a.status, failed, message))
}

/// One order's row in today's order book (web `_find_order`).
async fn find_order(b: &NuvamaBroker, auth: &AuthToken, order_id: &str) -> Result<Value> {
    let rows = read_book(b, auth, NuvamaBook::Orders).await?;
    rows.into_iter()
        .find(|r| text(r.get("ordID")) == order_id)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "Order {} is not in today's Nuvama order book.",
                order_id
            ))
        })
}

fn raw(row: &Value, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

/// `curQty`: the order's quantity as the book sent it.
fn current_quantity(row: &Value) -> String {
    let set = |k: &str| {
        row.get(k)
            .filter(|v| !v.is_null() && v.as_str() != Some("") && v.as_f64() != Some(0.0))
    };
    let v = set("ntQty").or_else(|| set("reqQty"));
    match v {
        Some(v) => py_text(Some(v)),
        None => "0".into(),
    }
}

/// The modify body (web `transform_modify_order_data`), from the order's
/// book row and the new terms.
pub(crate) fn modify_body(m: &ResolvedModify, row: &Value, ord_typ: &str, lm_prc: String) -> Value {
    let dur = text(row.get("dur"));
    let mut b = Map::new();
    b.insert("trdSym".into(), raw(row, "trdSym"));
    b.insert("exc".into(), raw(row, "exc"));
    b.insert("action".into(), json!(side_of(row, m.action)));
    b.insert(
        "dur".into(),
        json!(if dur.is_empty() { "DAY".into() } else { dur }),
    );
    b.insert("flQty".into(), json!("0"));
    b.insert("ordTyp".into(), json!(ord_typ));
    b.insert("qty".into(), json!(m.quantity.to_string()));
    b.insert("dscQty".into(), json!(m.disclosed_quantity.to_string()));
    b.insert("sym".into(), raw(row, "sym"));
    b.insert("mktPro".into(), json!(""));
    b.insert("lmPrc".into(), json!(lm_prc));
    b.insert("trgPrc".into(), json!(py_float(m.trigger_price)));
    b.insert("prdCode".into(), json!(mapping::product(m.product)));
    b.insert("dtDays".into(), json!(""));
    b.insert("nstOID".into(), raw(row, "ordID"));
    b.insert("curQty".into(), json!(current_quantity(row)));
    Value::Object(b)
}

/// The order's side from its row, else the request's.
fn side_of(row: &Value, fallback: Action) -> String {
    let s = mapping::side(&text(row.get("trsTyp")));
    if s.is_empty() {
        fallback.as_str().to_string()
    } else {
        s
    }
}

/// The cancel body (web `transform_cancel_order_data`).
pub(crate) fn cancel_body(row: &Value) -> Value {
    json!({
        "trdSym": raw(row, "trdSym"),
        "sym": raw(row, "sym"),
        "action": mapping::side(&text(row.get("trsTyp"))),
        "nstOID": raw(row, "ordID"),
        "exc": raw(row, "exc"),
        "prdCode": raw(row, "prdCode"),
        "ordTyp": raw(row, "ordTyp"),
        "curQty": current_quantity(row),
        "flQty": "0",
    })
}

/// A modify or cancel answer: the order id on success, Nuvama's words
/// otherwise.
fn changed(a: &super::client::Answer, order_id: &str, what: &str) -> Result<OrderResponse> {
    if a.ok() {
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    let message = a.message(&format!("Nuvama did not {} the order", what));
    tracing::warn!(
        status = a.status,
        code = a.error_code().unwrap_or(""),
        "Nuvama did not {} order {}: {}",
        what,
        order_id,
        message
    );
    if a.status >= 500 {
        return Err(AppError::Broker(
            crate::brokers::common::http::SERVER_ERROR.into(),
        ));
    }
    Err(AppError::Broker(message))
}

pub async fn modify_order(
    b: &NuvamaBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let row = find_order(b, auth, &m.order_id).await?;
    let s = b.session(auth)?;
    let action = match side_of(&row, m.action).as_str() {
        "SELL" => Action::Sell,
        _ => Action::Buy,
    };
    let key = QuoteKey::new(m.exchange.as_str(), &m.symbol);
    let (ord_typ, lm_prc) = priced(
        b,
        auth,
        &m.instrument,
        key,
        Terms {
            pricetype: m.pricetype,
            action,
            price: m.price,
            trigger: m.trigger_price,
        },
    )
    .await?;
    let body = modify_body(m, &row, ord_typ, lm_prc);
    let url = route(b, &s, "trade/modifytrade/v1/{userid}/");
    let a = b
        .call(&s, Call::send(Method::PUT, url, &body, Lane::Orders))
        .await?;
    changed(&a, &m.order_id, "modify")
}

async fn cancel_row(b: &NuvamaBroker, s: &NuvamaSession, row: &Value) -> Result<OrderResponse> {
    let body = cancel_body(row);
    let url = route(b, s, "trade/canceltrade/v1/{userid}/");
    let a = b
        .call(s, Call::send(Method::PUT, url, &body, Lane::Orders))
        .await?;
    changed(&a, &text(row.get("ordID")), "cancel")
}

pub async fn cancel_order(
    b: &NuvamaBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let row = find_order(b, auth, order_id).await?;
    let s = b.session(auth)?;
    cancel_row(b, &s, &row).await
}

/// Cancel every open or trigger-pending order from one read of the book
/// (web `cancel_all_orders_api`).
pub async fn cancel_all_orders(b: &NuvamaBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let rows = read_book(b, auth, NuvamaBook::Orders).await?;
    let s = b.session(auth)?;
    let mut result = CancelAllResult::default();
    for row in rows {
        let status = mapping::normalize_status(&text(row.get("sts")));
        if !matches!(status.as_str(), "open" | "trigger pending") {
            continue;
        }
        let id = text(row.get("ordID"));
        match cancel_row(b, &s, &row).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Nuvama cancel of {} failed: {}", id, e.code());
                result.failed.push(id);
            }
        }
    }
    Ok(result)
}
