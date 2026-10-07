//! Orders and books (web `api/order_api.py`).
//!
//! Writes go to `/sentinel/orders/{create,modify,cancel}` with the item
//! wrapped in `{"orders": [..]}`; success is 200/201 (cancel also 204) and
//! the order id is `orders[0].intentOrderId`. Orders and trades both come
//! from the bucketed `GET /sentinel/orders`.

use super::mapping;
use super::{refused, NubraBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

/// The order buckets that hold working orders (web `_WORKING_BUCKETS`).
const WORKING_BUCKETS: &[&str] = &["open", "gtt"];

fn intent_id(v: &Value) -> Option<String> {
    v.get("orders")
        .and_then(|o| o.get(0))
        .and_then(|o| o.get("intentOrderId"))
        .and_then(|x| match x {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
}

fn numeric_order_id(order_id: &str) -> Result<i64> {
    order_id.trim().parse::<i64>().map_err(|_| {
        AppError::Validation(format!("Order id {} is not a Nubra order id.", order_id))
    })
}

pub async fn place_order(
    b: &NubraBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let ref_id = mapping::ref_id(o.token()).ok_or_else(|| {
        AppError::Validation(format!(
            "{} on {} has no Nubra instrument id. Download the master contract again.",
            o.symbol, o.exchange
        ))
    })?;
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) && mapping::paise(o.trigger_price) == 0
    {
        return Err(AppError::Validation(format!(
            "{} orders need a trigger price for {} on {}.",
            o.pricetype, o.symbol, o.exchange
        )));
    }
    let body = json!({"orders": [mapping::place_item(o, ref_id)]});
    let (status, v) = b
        .call(Method::POST, "/sentinel/orders/create", auth, Some(&body))
        .await?;
    match intent_id(&v) {
        Some(id) if matches!(status, 200 | 201) => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        _ => {
            tracing::warn!(status, "Nubra refused an order");
            Err(refused(&v, status))
        }
    }
}

pub async fn modify_order(
    b: &NubraBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let id = numeric_order_id(&m.order_id)?;
    let body = json!({"orders": [mapping::modify_item(m, id)]});
    let (status, v) = b
        .call(Method::POST, "/sentinel/orders/modify", auth, Some(&body))
        .await?;
    if matches!(status, 200 | 201) {
        return Ok(OrderResponse {
            order_id: intent_id(&v).unwrap_or_else(|| m.order_id.clone()),
            message: None,
        });
    }
    tracing::warn!(status, "Nubra refused a modify");
    Err(refused(&v, status))
}

pub async fn cancel_order(
    b: &NubraBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let id = numeric_order_id(order_id)?;
    let body = json!({"orders": [{"orderId": id}]});
    let (status, v) = b
        .call(Method::POST, "/sentinel/orders/cancel", auth, Some(&body))
        .await?;
    if matches!(status, 200 | 201 | 204) {
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    tracing::warn!(status, "Nubra refused a cancel");
    Err(refused(&v, status))
}

/// Raw bucketed order response.
async fn raw_orders(b: &NubraBroker, auth: &AuthToken) -> Result<Value> {
    b.get_ok("/sentinel/orders", auth).await
}

pub async fn cancel_all_orders(b: &NubraBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let resp = raw_orders(b, auth).await?;
    let mut out = CancelAllResult::default();
    for (_, o) in mapping::flatten_buckets(&resp, Some(WORKING_BUCKETS)) {
        let id = match o.get("intentOrderId") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        b.loop_pacer.acquire().await;
        match cancel_order(b, auth, &id).await {
            Ok(_) => out.cancelled.push(id),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("Nubra cancel of {} failed: {}", id, e.code());
                out.failed.push(id)
            }
        }
    }
    Ok(out)
}

async fn raw_positions(b: &NubraBroker, auth: &AuthToken) -> Result<Value> {
    b.get_ok("/sentinel/portfolio/positions", auth).await
}

pub async fn close_all_positions(b: &NubraBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let resp = raw_positions(b, auth).await?;
    let symbols = b.resolver().clone();
    let mut out = CloseAllResult::default();
    for p in mapping::positions_list(&resp) {
        let net = mapping::position_net_qty(&p);
        if net == 0 {
            continue;
        }
        let (symbol, exchange) = mapping::resolve_position(&symbols, &p);
        let label = format!("{} ({})", symbol, exchange);
        let req = OrderRequest {
            symbol: symbol.clone(),
            exchange: exchange.clone(),
            side: if net > 0 { "SELL" } else { "BUY" }.to_string(),
            quantity: i32::try_from(net.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".to_string(),
            product: mapping::product_from(
                p.get("deliveryType").and_then(Value::as_str).unwrap_or(""),
            )
            .to_string(),
            validity: "DAY".to_string(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        b.loop_pacer.acquire().await;
        let outcome = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(order) => place_order(b, auth, &order).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(r) => out.placed.push(r.order_id),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => out
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(out)
}

pub async fn get_open_position(
    b: &NubraBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let resp = raw_positions(b, auth).await?;
    let symbols = b.resolver();
    for p in mapping::positions_list(&resp) {
        let (s, e) = mapping::resolve_position(symbols, &p);
        let prod =
            mapping::product_from(p.get("deliveryType").and_then(Value::as_str).unwrap_or(""));
        if s == symbol && e == exchange.as_str() && prod == product.as_str() {
            return Ok(mapping::position_net_qty(&p));
        }
    }
    Ok(0)
}

pub async fn get_order_book(b: &NubraBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let resp = raw_orders(b, auth).await?;
    Ok(mapping::order_book(&resp, b.resolver()))
}

pub async fn get_trade_book(b: &NubraBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let resp = raw_orders(b, auth).await?;
    Ok(mapping::trade_book(&resp, b.resolver()))
}

pub async fn get_positions(b: &NubraBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let resp = raw_positions(b, auth).await?;
    Ok(mapping::positions(&resp, b.resolver()))
}

pub async fn get_holdings(b: &NubraBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let resp = b.get_ok("/sentinel/portfolio/holdings", auth).await?;
    Ok(mapping::holdings(&resp, b.resolver()))
}
