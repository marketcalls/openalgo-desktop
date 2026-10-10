//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{self, ArrowHolding, ArrowOrder, ArrowPosition, ArrowTrade};
use super::{arrow_error, message_of, session_expired, ArrowBroker, Category};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::master_contract::format_strike;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};

/// Number as the web stringifies it (`"0"`, `"1500.5"`).
pub(crate) fn num(v: f64) -> String {
    format_strike(v)
}

fn is_stop(p: PriceType) -> bool {
    matches!(p, PriceType::Sl | PriceType::SlM)
}

/// The place-order body (web `transform_data`, `transform_data.py:66-95`).
/// Plain market orders are disabled on Arrow, so `MKT` carries `mpp: true`
/// (Arrow routes it as a protected limit); stop orders carry `triggerPrice`
/// (the field name the official SDK sends).
pub fn place_order_body(o: &ResolvedOrder) -> Value {
    let order = mapping::order_type(o.pricetype);
    let mut m = Map::new();
    m.insert("exchange".into(), json!(o.exchange.as_str()));
    m.insert("symbol".into(), json!(o.brsymbol()));
    m.insert("quantity".into(), json!(o.quantity.to_string()));
    m.insert(
        "transactionType".into(),
        json!(mapping::side_code(o.action)),
    );
    m.insert("order".into(), json!(order));
    m.insert("product".into(), json!(mapping::product_code(o.product)));
    m.insert("price".into(), json!(num(o.price)));
    m.insert("validity".into(), json!("DAY"));
    m.insert(
        "disclosedQty".into(),
        json!(o.disclosed_quantity.to_string()),
    );
    m.insert("remarks".into(), json!("openalgo"));
    if o.pricetype == PriceType::Market {
        m.insert("mpp".into(), json!(true));
    }
    if is_stop(o.pricetype) {
        m.insert("triggerPrice".into(), json!(num(o.trigger_price)));
    }
    Value::Object(m)
}

/// The modify body (web `transform_modify_order_data`): the place shape
/// minus side and remarks.
pub fn modify_order_body(m: &ResolvedModify) -> Value {
    let mut b = Map::new();
    b.insert("exchange".into(), json!(m.exchange.as_str()));
    b.insert("symbol".into(), json!(m.brsymbol()));
    b.insert("quantity".into(), json!(m.quantity.to_string()));
    b.insert("order".into(), json!(mapping::order_type(m.pricetype)));
    b.insert("product".into(), json!(mapping::product_code(m.product)));
    b.insert("price".into(), json!(num(m.price)));
    b.insert("validity".into(), json!("DAY"));
    b.insert(
        "disclosedQty".into(),
        json!(m.disclosed_quantity.to_string()),
    );
    if is_stop(m.pricetype) {
        b.insert("triggerPrice".into(), json!(num(m.trigger_price)));
    }
    Value::Object(b)
}

/// The order number of an answer: a string (trimmed) or a number; `false`,
/// an object or anything else is no order number (12-U1).
fn order_no(data: &Value) -> String {
    match data.get("orderNo") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub async fn place_order(
    b: &ArrowBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = place_order_body(o);
    let data = b
        .call(
            Method::POST,
            "/order/regular",
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    let order_id = order_no(&data);
    if order_id.is_empty() {
        tracing::warn!("Arrow answered an order without an order number");
        return Err(AppError::Broker(
            "Arrow accepted the request but returned no order number. Check the order book before retrying."
                .into(),
        ));
    }
    Ok(OrderResponse {
        order_id,
        message: None,
    })
}

pub async fn modify_order(
    b: &ArrowBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = modify_order_body(m);
    let data = b
        .call(
            Method::PATCH,
            &format!("/order/regular/{}", urlencoding::encode(&m.order_id)),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    let id = order_no(&data);
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            m.order_id.clone()
        } else {
            id
        },
        message: None,
    })
}

/// Cancel answers with a plain string on success, so success is the HTTP
/// status, not a JSON envelope (`order_api.py:305-318`).
pub async fn cancel_order(
    b: &ArrowBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let url = format!(
        "{}/order/regular/{}",
        b.urls().rest,
        urlencoding::encode(order_id)
    );
    let resp = b
        .send(Method::DELETE, &url, auth, None, Category::Order)
        .await?;
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(session_expired());
    }
    let bytes = resp.bytes().await.map_err(|e| super::redact(e.into()))?;
    if status.is_success() {
        // A JSON envelope that says otherwise still counts as a refusal.
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if matches!(v.get("status").and_then(Value::as_str), Some(s) if s != "success") {
                return Err(arrow_error(&message_of(&v)));
            }
        }
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    let msg = serde_json::from_slice::<Value>(&bytes)
        .map(|v| message_of(&v))
        .unwrap_or_default();
    tracing::warn!(
        status = status.as_u16(),
        "Arrow refused cancel of {}: {}",
        order_id,
        msg
    );
    Err(arrow_error(&msg))
}

pub(crate) async fn raw_orders(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<ArrowOrder>> {
    let data = b
        .call(Method::GET, "/user/orders", auth, None, Category::Other)
        .await?;
    Ok(mapping::rows(data, "order"))
}

pub(crate) async fn raw_positions(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<ArrowPosition>> {
    let data = b
        .call(Method::GET, "/user/positions", auth, None, Category::Other)
        .await?;
    Ok(mapping::rows(data, "position"))
}

pub async fn cancel_all_orders(b: &ArrowBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        if !mapping::is_cancellable(&o.order_status) {
            continue;
        }
        let id = o.order_id().to_string();
        if id.is_empty() {
            continue;
        }
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

/// Net quantity matched on broker symbol, exchange and Arrow product code
/// (`order_api.py:129-154`).
pub async fn get_open_position(
    b: &ArrowBroker,
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
    let code = mapping::product_code(product);
    Ok(raw_positions(b, auth)
        .await?
        .into_iter()
        .find(|p| p.symbol == br && p.exchange == ex && p.product == code)
        .map(|p| p.qty)
        .unwrap_or(0))
}

/// Square off every non-zero position at MARKET (`order_api.py:258-302`).
pub async fn close_all_positions(b: &ArrowBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.qty == 0 {
            continue;
        }
        let symbol = symbols.oa_symbol_or_raw(&p.symbol, &p.exchange);
        let label = format!("{} ({})", symbol, p.exchange);
        let req = OrderRequest {
            symbol,
            exchange: p.exchange.clone(),
            side: if p.qty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(p.qty.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: mapping::product_from_arrow(&p.product),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => place_order(b, auth, &o).await,
            Err(e) => Err(e),
        };
        match placed {
            Ok(r) if !r.order_id.is_empty() => result.placed.push(r.order_id),
            Ok(_) => result.failed.push(format!("{}: order was refused", label)),
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

pub async fn get_order_book(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let data = b
        .call(Method::GET, "/user/trades", auth, None, Category::Other)
        .await?;
    Ok(mapping::map_trades(
        mapping::rows::<ArrowTrade>(data, "trade"),
        b.resolver(),
    ))
}

pub async fn get_positions(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &ArrowBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let data = b
        .call(Method::GET, "/user/holdings", auth, None, Category::Other)
        .await?;
    Ok(mapping::map_holdings(
        mapping::rows::<ArrowHolding>(data, "holding"),
        b.resolver(),
    ))
}
