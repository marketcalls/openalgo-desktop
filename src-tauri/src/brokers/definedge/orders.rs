//! Orders and books (web `api/order_api.py`).
//!
//! * place `POST /placeorder` -> `{"status":"SUCCESS","order_id"}` (or the
//!   Noren `stat/norenordno`); HTTP 200 without an order id is a refusal.
//! * modify `POST /modify`, cancel `GET /cancel/{orderid}`.
//! * books `GET /orders`, `/trades`, `/positions`, `/holdings`.

use super::mapping::{self, rows};
use super::{broker_error, is_success, text, DefinedgeBroker, DefinedgeSession};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

async fn write(
    b: &DefinedgeBroker,
    s: &DefinedgeSession,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let url = format!("{}{}", b.urls.trade, path);
    let (status, txt) = b
        .send(method, &url, &s.api_session_key, body, true)
        .await?;
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(super::session_expired());
    }
    match serde_json::from_str::<Value>(&txt) {
        Ok(v) => Ok(v),
        Err(_) => {
            tracing::warn!(
                broker = "definedge",
                status = status.as_u16(),
                "Unreadable order response from Definedge"
            );
            Err(AppError::Broker(
                "Definedge did not confirm the order request. Check the order book before trying again."
                    .into(),
            ))
        }
    }
}

pub async fn place_order(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = DefinedgeSession::parse(auth)?;
    let body = mapping::place_body(o);
    let v = write(b, &s, Method::POST, "/placeorder", Some(&body)).await?;
    let id = if is_success(&v) {
        Some(text(&v, "norenordno"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| text(&v, "order_id"))
    } else {
        String::new()
    };
    if id.is_empty() {
        return Err(broker_error(&v, "Definedge did not accept the order."));
    }
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = DefinedgeSession::parse(auth)?;
    let body = mapping::modify_body(m);
    let v = write(b, &s, Method::POST, "/modify", Some(&body)).await?;
    if !is_success(&v) {
        return Err(broker_error(&v, "Definedge did not accept the change."));
    }
    let id = Some(text(&v, "order_id"))
        .filter(|x| !x.is_empty())
        .or_else(|| Some(text(&v, "norenordno")).filter(|x| !x.is_empty()))
        .unwrap_or_else(|| m.order_id.clone());
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn cancel_order(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = DefinedgeSession::parse(auth)?;
    let path = format!("/cancel/{}", urlencoding::encode(order_id));
    let v = write(b, &s, Method::GET, &path, None).await?;
    if text(&v, "status") != "SUCCESS" {
        return Err(broker_error(&v, "Definedge could not cancel the order."));
    }
    Ok(OrderResponse {
        order_id: Some(text(&v, "order_id"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| order_id.to_string()),
        message: None,
    })
}

async fn book(b: &DefinedgeBroker, auth: &AuthToken, path: &str) -> Result<Value> {
    let s = DefinedgeSession::parse(auth)?;
    let v = b.trade_json(&s, Method::GET, path, None).await?;
    let status = text(&v, "status");
    if status == "ERROR" || text(&v, "stat") == "Not_Ok" {
        return Err(broker_error(
            &v,
            "Definedge could not return your account data. Try again shortly.",
        ));
    }
    Ok(v)
}

/// Raw order rows (cancel-all reads the broker statuses).
async fn raw_orders(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let v = book(b, auth, "/orders").await?;
    let r = if v.get("orders").is_some() {
        rows(&v, "orders")
    } else {
        rows(&v, "orderbook")
    };
    Ok(r.to_vec())
}

pub async fn get_order_book(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let orders = raw_orders(b, auth).await?;
    Ok(orders
        .iter()
        .filter(|o| o.is_object())
        .map(|o| mapping::map_order(o, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = book(b, auth, "/trades").await?;
    Ok(rows(&v, "trades")
        .iter()
        .filter(|t| t.is_object())
        .map(|t| mapping::map_trade(t, b.resolver()))
        .collect())
}

pub async fn get_positions(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let v = book(b, auth, "/positions").await?;
    Ok(rows(&v, "positions")
        .iter()
        .filter(|p| p.is_object())
        .map(|p| mapping::map_position(p, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &DefinedgeBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = book(b, auth, "/holdings").await?;
    Ok(mapping::map_holdings(rows(&v, "holdings"), b.resolver()))
}

/// web `cancel_all_orders_api`: every order whose broker status is in the
/// cancellable set, one cancel each.
pub async fn cancel_all_orders(b: &DefinedgeBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let orders = raw_orders(b, auth).await?;
    let mut result = CancelAllResult::default();
    for o in orders.iter().filter(|o| mapping::is_cancellable(o)) {
        let id = mapping::order_id(o);
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

/// web `get_open_position`: net quantity of the broker symbol on this
/// exchange and product, 0 when flat.
pub async fn get_open_position(
    b: &DefinedgeBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let positions = get_positions(b, auth).await?;
    Ok(positions
        .iter()
        .find(|p| {
            p.symbol == symbol && p.exchange == exchange.as_str() && p.product == product.as_str()
        })
        .map(|p| i64::from(p.quantity))
        .unwrap_or(0))
}
