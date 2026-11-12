//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{
    self, holding_rows, oa_exchange, parse_orders, paytm_exchange, paytm_order_type, paytm_product,
    paytm_side, segment, PaytmOrder, PaytmPosition,
};
use super::PaytmBroker;
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::position_read::unread;
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

/// Statuses of an order Paytm still works, upper-cased: what `modify_order`
/// and `cancel_order` accept (web `MODIFIABLE_STATUSES`, and
/// `cancellable_statuses` since #2374).
const WORKING: &[&str] = &["OPEN", "TRIGGER PENDING", "MODIFIED", "PENDING"];

fn working(o: &PaytmOrder) -> bool {
    WORKING.contains(&o.status.trim().to_ascii_uppercase().as_str())
}

/// SL and SL-M, which Paytm takes as `SL` and `SLM` with a trigger price
/// (web `STOP_ORDER_TYPES`).
fn is_stop(p: PriceType) -> bool {
    matches!(p, PriceType::Sl | PriceType::SlM)
}

/// web `require_trigger_price` (#2404): an SL or SL-M order needs a finite
/// trigger price above 0, or it is refused before it is sent. Without one
/// the web sent a protective stop as a market order that executed at once.
fn require_trigger(pricetype: PriceType, trigger: f64) -> Result<()> {
    if is_stop(pricetype) && !(trigger.is_finite() && trigger > 0.0) {
        return Err(AppError::Validation(format!(
            "A trigger price is required for an {} order. Set the trigger price and place the order again.",
            pricetype.as_str()
        )));
    }
    Ok(())
}

/// The body `place_order_api` posts (web `transform_data`): a stop order
/// carries its trigger price, and an SL-M, which executes at market once
/// triggered, the price "0" (the web's string).
pub fn place_order_body(o: &ResolvedOrder) -> Result<Value> {
    require_trigger(o.pricetype, o.trigger_price)?;
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
    if is_stop(o.pricetype) {
        body["trigger_price"] = json!(o.trigger_price);
    }
    if o.pricetype == PriceType::SlM {
        body["price"] = json!("0");
    }
    Ok(body)
}

/// Fields of a book row echoed back verbatim (strings stay strings).
fn echo(o: &PaytmOrder, key: &str) -> Value {
    o.raw.get(key).cloned().unwrap_or(Value::Null)
}

/// The body `modify_order` posts: the book row's identity fields plus the
/// new quantity, price, trigger, product and order type. An order that
/// executes at market (MKT, SLM) sends the price "0", the web's string; a
/// stop order needs its trigger price (#2404).
pub fn modify_order_body(m: &ResolvedModify, o: &PaytmOrder) -> Result<Value> {
    require_trigger(m.pricetype, m.trigger_price)?;
    let at_market = matches!(m.pricetype, PriceType::Market | PriceType::SlM);
    Ok(json!({
        "order_no": o.order_no,
        "exchange": echo(o, "exchange"),
        "segment": echo(o, "segment"),
        "security_id": echo(o, "security_id"),
        "quantity": m.quantity,
        "price": if at_market { json!("0") } else { json!(m.price) },
        "trigger_price": m.trigger_price,
        "validity": "DAY",
        "product": paytm_product(m.product),
        "order_type": paytm_order_type(m.pricetype),
        "txn_type": echo(o, "txn_type"),
        "source": "N",
        "off_mkt_flag": if o.off_mkt_flag.is_empty() { "N".to_string() } else { o.off_mkt_flag.clone() },
        "serial_no": echo(o, "serial_no"),
        "group_id": echo(o, "group_id"),
    }))
}

/// The body `cancel_order` posts (web copies the book row, `ext_info`
/// included when the row has it, unchanged).
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
    if let Some(ext) = o.raw.get("ext_info") {
        body["ext_info"] = ext.clone();
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
    let body = place_order_body(o)?;
    let env = b.call(Method::POST, PLACE, auth, Some(&body)).await?;
    let id = order_no(&env.data).ok_or_else(|| {
        // Accepted without a number: the order may exist (LOG-08).
        tracing::warn!("Paytm Money accepted an order without an order number");
        AppError::uncertain(
            "Paytm Money accepted the order but returned no order number. Check the order book before placing it again.",
            None,
        )
    })?;
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub(crate) async fn raw_orders(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<PaytmOrder>> {
    let env = b.call(Method::GET, ORDERS, auth, None).await?;
    parse_orders(&env.data).ok_or_else(|| book_read::unreadable("paytm", Book::Orders))
}

pub(crate) async fn raw_positions(b: &PaytmBroker, auth: &AuthToken) -> Result<Vec<PaytmPosition>> {
    positions_read(b, auth, || book_read::unreadable("paytm", Book::Positions)).await
}

/// The position book (BR-03): a list whose every row reads and carries
/// its net quantity, or `unread()`.
async fn positions_read(
    b: &PaytmBroker,
    auth: &AuthToken,
    unread: impl Fn() -> AppError,
) -> Result<Vec<PaytmPosition>> {
    let env = b.call(Method::GET, POSITIONS, auth, None).await?;
    let rows = env.data.as_array().ok_or_else(&unread)?;
    if !rows.iter().all(mapping::has_quantity) {
        tracing::warn!("A Paytm Money position row without its quantity");
        return Err(unread());
    }
    book_read::typed(rows, unread)
}

/// web `modify_order` / `cancel_order`: 404 "Order <id> not found".
fn not_found(order_id: &str) -> AppError {
    AppError::NotFound(format!("Order {} not found", order_id))
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
    if !working(o) {
        return Err(AppError::Validation(format!(
            "Order {} cannot be modified. Current status: {}",
            m.order_id, o.status
        )));
    }
    let body = modify_order_body(m, o)?;
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
    if !working(o) {
        return Err(AppError::Validation(format!(
            "Order {} cannot be cancelled. Current status: {}",
            order_id, o.status
        )));
    }
    cancel_row(b, auth, o).await
}

pub async fn cancel_all_orders(b: &PaytmBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    // Every working order, as a single cancel takes them. The web's
    // `cancel_all_orders_api` still selects `Pending` alone, so an Open
    // order survives its cancel-all: a gap not copied.
    for o in raw_orders(b, auth).await?.iter().filter(|o| working(o)) {
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
    for p in positions_read(b, auth, || unread("Paytm Money")).await? {
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
        let placed = crate::brokers::common::outcome::placing(async {
            let env = b.call(Method::POST, PLACE, auth, Some(&body)).await?;
            // Accepted without a number: the exit may exist (LOG-08).
            order_no(&env.data)
                .map(|order_id| OrderResponse {
                    order_id,
                    message: None,
                })
                .ok_or_else(|| {
                    AppError::uncertain(
                        "Paytm Money accepted the exit but returned no order number. Check the order book.",
                        None,
                    )
                })
        })
        .await;
        result.record(&label, placed);
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
    Ok(positions_read(b, auth, || unread("Paytm Money"))
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
    let rows =
        holding_rows(&env.data).ok_or_else(|| book_read::unreadable("paytm", Book::Holdings))?;
    Ok(mapping::map_holdings(&rows, b.resolver()))
}
