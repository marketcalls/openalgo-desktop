//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, s};
use super::{kotak_error, stat_ok, KotakBroker, KotakSession};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::brokers::Broker;
use crate::error::{AppError, Result};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::HashMap;

pub async fn place_order(
    b: &KotakBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let sess = KotakSession::parse(auth)?;
    let jdata = mapping::place_order_jdata(o)?;
    let (status, v) = b
        .trading_post(&sess, "/quick/order/rule/ms/place", &jdata)
        .await?;
    // web: `orderid = nOrdNo if stat == "Ok"`.
    if s(&v, "stat") == "Ok" {
        let id = s(&v, "nOrdNo");
        if !id.is_empty() {
            return Ok(OrderResponse {
                order_id: id,
                message: None,
            });
        }
        // Accepted without an id: the order may exist (LOG-08).
        return Err(AppError::uncertain(
            "Kotak accepted the order but returned no order number. Check the order book before placing it again.",
            None,
        ));
    }
    tracing::warn!(status = status.as_u16(), "Kotak refused an order");
    // Kotak's explicit failure (`stat: Not_Ok`, an error message) is a
    // definite refusal (LOG-08); a 2xx without one and a 5xx leave the
    // order unknown.
    let failed = s(&v, "stat").eq_ignore_ascii_case("not_ok")
        || !s(&v, "errMsg").is_empty()
        || !s(&v, "emsg").is_empty();
    Err(crate::brokers::common::outcome::not_accepted_err(
        status.as_u16(),
        failed,
        kotak_error(status, &v, "Kotak did not accept the order."),
    ))
}

pub async fn modify_order(
    b: &KotakBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let sess = KotakSession::parse(auth)?;
    let jdata = mapping::modify_order_jdata(m)?;
    let (status, v) = b
        .trading_post(&sess, "/quick/order/vr/modify", &jdata)
        .await?;
    if s(&v, "stat") == "Ok" {
        let id = s(&v, "nOrdNo");
        return Ok(OrderResponse {
            order_id: if id.is_empty() {
                m.order_id.clone()
            } else {
                id
            },
            message: None,
        });
    }
    tracing::warn!(status = status.as_u16(), "Kotak refused an order modify");
    Err(kotak_error(status, &v, "Failed to modify order"))
}

pub async fn cancel_order(
    b: &KotakBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let sess = KotakSession::parse(auth)?;
    let (status, v) = b
        .trading_post(
            &sess,
            "/quick/order/cancel",
            &json!({"on": order_id, "am": "NO"}),
        )
        .await?;
    if s(&v, "stat") == "Ok" {
        let id = s(&v, "nOrdNo");
        return Ok(OrderResponse {
            order_id: if id.is_empty() {
                order_id.to_string()
            } else {
                id
            },
            message: None,
        });
    }
    tracing::warn!(status = status.as_u16(), "Kotak refused a cancel");
    Err(kotak_error(status, &v, "Failed to cancel order"))
}

async fn book(b: &KotakBroker, auth: &AuthToken, path: &str) -> Result<Vec<Value>> {
    let sess = KotakSession::parse(auth)?;
    let (status, v) = b.trading_get(&sess, path).await?;
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(super::session_expired());
    }
    if !v.is_object() {
        return Err(AppError::Broker(
            "Kotak returned an unexpected book. Try again shortly.".into(),
        ));
    }
    if s(&v, "stat").eq_ignore_ascii_case("not_ok") {
        let e = kotak_error(status, &v, "");
        // A session error is an error; any other Not_Ok is an empty book
        // (web `map_order_data`).
        if matches!(e, AppError::Auth(_)) {
            return Err(e);
        }
        return Ok(Vec::new());
    }
    Ok(mapping::data_rows(&v))
}

/// The raw positions book. A payload that is not an object is an error,
/// never an empty book: close-all reads an empty book as nothing to close
/// (web `get_positions`).
pub(crate) async fn raw_positions(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let sess = KotakSession::parse(auth)?;
    let (status, v) = b.trading_get(&sess, "/quick/user/positions").await?;
    if !v.is_object() {
        tracing::warn!(
            status = status.as_u16(),
            "Kotak positions payload is not an object"
        );
        return Err(AppError::Broker(
            "Kotak did not return the position book. Try again shortly.".into(),
        ));
    }
    if !stat_ok(&v) {
        return Err(kotak_error(
            status,
            &v,
            "Kotak did not return the position book. Try again shortly.",
        ));
    }
    Ok(v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub async fn get_order_book(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let rows = book(b, auth, "/quick/user/orders").await?;
    Ok(mapping::map_orders(&rows, b.resolver()))
}

pub async fn get_trade_book(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = book(b, auth, "/quick/user/trades").await?;
    Ok(mapping::map_trades(&rows, b.resolver()))
}

/// web `_backfill_ltp`: one multiquote for the distinct instruments; best
/// effort, a failure leaves the LTPs at zero.
async fn backfill_ltp(
    b: &KotakBroker,
    auth: &AuthToken,
    rows: &[Value],
) -> HashMap<(String, String), f64> {
    let mut keys: Vec<QuoteKey> = Vec::new();
    for p in rows {
        let ex = mapping::row_exchange(&s(p, "exSeg"));
        let sym = mapping::openalgo_symbol(b.resolver(), p, &ex);
        if sym.is_empty() {
            continue;
        }
        let k = QuoteKey::new(ex, sym);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut out = HashMap::new();
    if keys.is_empty() {
        return out;
    }
    match b.get_multiquotes(auth, &keys).await {
        Ok(results) => {
            for r in results {
                if let Some(q) = r.data.filter(|q| q.ltp != 0.0) {
                    out.insert((r.symbol, r.exchange), q.ltp);
                }
            }
        }
        Err(e) => tracing::warn!("Could not backfill LTP for Kotak positions: {}", e.code()),
    }
    out
}

pub async fn get_positions(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let rows = raw_positions(b, auth).await?;
    let ltp = backfill_ltp(b, auth, &rows).await;
    Ok(rows
        .iter()
        .map(|p| {
            let ex = mapping::row_exchange(&s(p, "exSeg"));
            let sym = mapping::openalgo_symbol(b.resolver(), p, &ex);
            let last = ltp.get(&(sym, ex)).copied().unwrap_or(0.0);
            mapping::map_position(p, b.resolver(), last)
        })
        .collect())
}

pub async fn get_holdings(b: &KotakBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let sess = KotakSession::parse(auth)?;
    let (status, v) = b.trading_get(&sess, "/portfolio/v1/holdings").await?;
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(super::session_expired());
    }
    let rows = match v.get("data") {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Null) | None => {
            if s(&v, "stat").eq_ignore_ascii_case("not_ok") || v.get("fault").is_some() {
                let e = kotak_error(status, &v, "Kotak did not return holdings.");
                if matches!(e, AppError::Auth(_)) {
                    return Err(e);
                }
            }
            Vec::new()
        }
        Some(_) => Vec::new(),
    };
    Ok(rows
        .iter()
        .map(|h| mapping::map_holding(h, b.resolver()))
        .collect())
}

/// web `cancel_all_orders_api`: every order whose raw `ordSt` is `open` or
/// `trigger pending`.
pub async fn cancel_all_orders(b: &KotakBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in book(b, auth, "/quick/user/orders").await? {
        let st = s(&o, "ordSt");
        if st != "open" && st != "trigger pending" {
            continue;
        }
        let id = s(&o, "nOrdNo");
        match cancel_order(b, auth, &id).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", id, e.code());
                result.failed.push(id)
            }
        }
    }
    Ok(result)
}

/// web `get_open_position`: match `trdSym` (broker symbol), `exSeg` and
/// `prod`; net = day + carried legs.
pub async fn get_open_position(
    b: &KotakBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let br = b
        .resolver()
        .br_symbol(symbol, ex)
        .unwrap_or_else(|| symbol.to_string());
    let segment = mapping::reverse_map_exchange(ex).unwrap_or("");
    for p in raw_positions(b, auth).await? {
        if mapping::position_matches(&p, &br, segment, product.as_str()) {
            return Ok(mapping::net_quantity(&p));
        }
    }
    Ok(0)
}

/// web `close_all_positions`: one MARKET order per non-zero row.
pub async fn close_all_positions(b: &KotakBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        let net = mapping::net_quantity(&p);
        if net == 0 {
            continue;
        }
        let exchange = mapping::row_exchange(&s(&p, "exSeg"));
        let symbol = mapping::openalgo_symbol(&symbols, &p, &exchange);
        let label = format!("{} ({})", symbol, exchange);
        let req = OrderRequest {
            symbol,
            exchange: exchange.clone(),
            side: if net > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(net.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: s(&p, "prod"),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => crate::brokers::common::outcome::placing(place_order(b, auth, &o)).await,
            Err(e) => Err(e),
        };
        result.record(&label, placed);
    }
    Ok(result)
}
