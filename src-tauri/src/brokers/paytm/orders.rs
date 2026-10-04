//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{
    self, holding_rows, oa_exchange, parse_orders, paytm_exchange, paytm_order_type, paytm_product,
    paytm_side, segment, PaytmOrder, PaytmPosition,
};
use super::PaytmBroker;
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

pub const PLACE: &str = "/orders/v1/place/regular";
pub const MODIFY: &str = "/orders/v1/modify/regular";
pub const CANCEL: &str = "/orders/v1/cancel/regular";
pub const ORDERS: &str = "/orders/v1/user/orders";
pub const POSITIONS: &str = "/orders/v1/position";
pub const HOLDINGS: &str = "/holdings/v1/get-user-holdings-data";

/// Statuses `modify_order` accepts (web `MODIFIABLE_STATUSES`).
const MODIFIABLE: &[&str] = &["OPEN", "TRIGGER PENDING", "MODIFIED", "PENDING"];

/// The body `place_order_api` posts (web `transform_data`). Trigger price
/// is sent for stop orders, which Paytm needs to accept them.
pub fn place_order_body(o: &ResolvedOrder) -> Value {
    let mut body = json!({
        "security_id": o.token(),
        "exchange": paytm_exchange(o.exchange),
        "txn_type": paytm_side(o.action),
        "order_type": paytm_order_type(o.pricetype),
        "quantity": o.quantity,
        "product": paytm_product(o.product),
        "price": o.price,
        "validity": "DAY",
        "segment": segment(o.exchange),
        "source": "M",
    });
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        body["trigger_price"] = json!(o.trigger_price);
    }
    body
}

/// Fields of a book row echoed back verbatim (strings stay strings).
fn echo(o: &PaytmOrder, key: &str) -> Value {
    o.raw.get(key).cloned().unwrap_or(Value::Null)
}

/// The body `modify_order` posts: the book row's identity fields plus the
/// new quantity, price, trigger, product and order type.
pub fn modify_order_body(m: &ResolvedModify, o: &PaytmOrder) -> Value {
    let market = m.pricetype == PriceType::Market;
    json!({
        "order_no": o.order_no,
        "exchange": echo(o, "exchange"),
        "segment": echo(o, "segment"),
        "security_id": echo(o, "security_id"),
        "quantity": m.quantity,
        "price": if market { 0.0 } else { m.price },
        "trigger_price": m.trigger_price,
        "validity": "DAY",
        "product": paytm_product(m.product),
        "order_type": paytm_order_type(m.pricetype),
        "txn_type": echo(o, "txn_type"),
        "source": "N",
        "off_mkt_flag": if o.off_mkt_flag.is_empty() { "N".to_string() } else { o.off_mkt_flag.clone() },
        "serial_no": echo(o, "serial_no"),
        "group_id": echo(o, "group_id"),
    })
}

/// The body `cancel_order` posts (web copies the book row).
pub fn cancel_order_body(o: &PaytmOrder) -> Value {
    let mut body = json!({"order_no": o.order_no, "source": "N"});
    for k in [
        "txn_type",
        "exchange",
        "segment",
        "product",
        "security_id",
        "quantity",
        "validity",
        "order_type",
        "price",
        "off_mkt_flag",
        "mkt_type",
        "serial_no",
        "group_id",
    ] {
        body[k] = echo(o, k);
    }
    body
}

/// Order number from `data[0].order_no`.
fn order_no(data: &Value) -> Option<String> {
    let v = data.get(0).and_then(|r| r.get("order_no"))?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub async fn place_order(
    b: &PaytmBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = place_order_body(o);
    let env = b.call(Method::POST, PLACE, auth, Some(&body)).await?;
    let id = order_no(&env.data).ok_or_else(|| {
        AppError::Broker("Paytm Money accepted the order but returned no order number.".into())
    })?;
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub(crate) async fn raw_orders(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<PaytmOrder>> {
    let env = b.call(Method::GET, ORDERS, auth, None).await?;
    Ok(parse_orders(&env.data))
}

pub(crate) async fn raw_positions(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<PaytmPosition>> {
    let env = b.call(Method::GET, POSITIONS, auth, None).await?;
    Ok(env.rows())
}

fn not_found(order_id: &str) -> AppError {
    AppError::NotFound(format!(
        "Order {} was not found in today's Paytm Money order book.",
        order_id
    ))
}

pub async fn modify_order(
    b: &PaytmBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let book = raw_orders(b, auth).await?;
    let o = book
        .iter()
        .find(|o| o.order_no == m.order_id)
        .ok_or_else(|| not_found(&m.order_id))?;
    if !MODIFIABLE.contains(&o.status.trim().to_ascii_uppercase().as_str()) {
        return Err(AppError::Validation(format!(
            "Order {} cannot be modified. Current status: {}",
            m.order_id, o.status
        )));
    }
    let body = modify_order_body(m, o);
    let env = b.call(Method::POST, MODIFY, auth, Some(&body)).await?;
    Ok(OrderResponse {
        order_id: order_no(&env.data).unwrap_or_else(|| m.order_id.clone()),
        message: Some("Order modified successfully".into()),
    })
}

async fn cancel_row(b: &PaytmBroker, auth: &AuthToken, o: &PaytmOrder) -> Result<OrderResponse> {
    let body = cancel_order_body(o);
    let env = b.call(Method::POST, CANCEL, auth, Some(&body)).await?;
    Ok(OrderResponse {
        order_id: order_no(&env.data).unwrap_or_else(|| o.order_no.clone()),
        message: None,
    })
}

/// Paytm cancels only orders whose `status` is `Pending` (web
/// `cancel_order`).
fn cancellable(o: &PaytmOrder) -> bool {
    o.status == "Pending"
}

pub async fn cancel_order(
    b: &PaytmBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let book = raw_orders(b, auth).await?;
    let o = book
        .iter()
        .find(|o| o.order_no == order_id)
        .ok_or_else(|| not_found(order_id))?;
    if !cancellable(o) {
        return Err(AppError::Validation(format!(
            "Order {} cannot be cancelled. Current status: {}",
            order_id, o.status
        )));
    }
    cancel_row(b, auth, o).await
}

pub async fn cancel_all_orders(b: &PaytmBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await?.iter().filter(|o| cancellable(o)) {
        match cancel_row(b, auth, o).await {
            Ok(_) => result.cancelled.push(o.order_no.clone()),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.order_no, e.code());
                result.failed.push(o.order_no.clone())
            }
        }
    }
    Ok(result)
}

/// The market exit the web sends for one position (straight from the row:
/// its own `security_id`, parent exchange and product).
pub fn close_position_body(p: &PaytmPosition) -> Option<Value> {
    let qty = p.quantity();
    if qty == 0 || p.security_id.is_empty() {
        return None;
    }
    let derivative = p.instrument.contains("OPT") || p.instrument.contains("FUT");
    Some(json!({
        "security_id": p.security_id,
        "exchange": p.exchange,
        "txn_type": if qty > 0 { "S" } else { "B" },
        "order_type": "MKT",
        "quantity": qty.abs(),
        "product": p.product,
        "price": 0,
        "validity": "DAY",
        "segment": if derivative { "D" } else { "E" },
        "source": "M",
    }))
}

pub async fn close_all_positions(b: &PaytmBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.quantity() == 0 {
            continue;
        }
        let exchange = oa_exchange(&p.exchange, &p.instrument);
        let label = format!(
            "{} ({})",
            mapping::oa_symbol(b.resolver(), &p.security_id, &exchange),
            exchange
        );
        let Some(body) = close_position_body(&p) else {
            result
                .failed
                .push(format!("{}: the position has no instrument id", label));
            continue;
        };
        match b.call(Method::POST, PLACE, auth, Some(&body)).await {
            Ok(env) => match order_no(&env.data) {
                Some(id) => result.placed.push(id),
                None => result.failed.push(format!("{}: order was refused", label)),
            },
            Err(e) => {
                tracing::error!("Square-off failed for {}: {}", label, e.code());
                result
                    .failed
                    .push(format!("{}: {}", label, e.client_message()))
            }
        }
    }
    Ok(result)
}

/// Net quantity of one instrument: the position row with the master
/// `security_id` on the same OpenAlgo exchange and product.
pub async fn get_open_position(
    b: &PaytmBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let Some(row) = b.resolver().by_symbol(ex, symbol) else {
        return Err(AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            symbol, ex
        )));
    };
    let want_product = paytm_product(product);
    Ok(raw_positions(b, auth)
        .await?
        .iter()
        .find(|p| {
            p.security_id == row.token
                && oa_exchange(&p.exchange, &p.instrument) == ex
                && p.product == want_product
        })
        .map(PaytmPosition::quantity)
        .unwrap_or(0))
}

pub async fn get_order_book(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        &raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    Ok(mapping::map_trades(
        &raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_positions(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        &raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let env = b.call(Method::GET, HOLDINGS, auth, None).await?;
    Ok(mapping::map_holdings(
        &holding_rows(&env.data),
        b.resolver(),
    ))
}
