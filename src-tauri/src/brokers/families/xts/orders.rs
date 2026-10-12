//! Orders and books (web `api/order_api.py`).
//!
//! Corrections the audit asks for over the Python: cancel and modify
//! succeed on `type == "success"` (the Python checks a `status` key XTS
//! never sends), and the open-position lookup uses the XTS shape (done by
//! the trait default over the normalised position book).

use super::mapping;
use super::XtsBroker;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

fn result(v: &Value) -> &Value {
    v.get("result").unwrap_or(&Value::Null)
}

pub(crate) async fn place_order(
    b: &XtsBroker,
    auth: &AuthToken,
    order: &ResolvedOrder,
) -> Result<OrderResponse> {
    let payload = mapping::place_payload(order).ok_or_else(|| {
        AppError::Validation(format!(
            "{} orders cannot be placed on {} with {}.",
            order.symbol, order.exchange, b.cfg.name
        ))
    })?;
    place_raw(b, auth, &payload).await
}

async fn place_raw(b: &XtsBroker, auth: &AuthToken, payload: &Value) -> Result<OrderResponse> {
    let v = b
        .interactive(Method::POST, "/orders", auth, Some(payload))
        .await?;
    let id = mapping::order_id(result(&v).get("AppOrderID"));
    if id.is_empty() {
        // Accepted without an id: the order may exist (LOG-08).
        tracing::warn!(broker = b.cfg.id, "Order accepted without an order id");
        return Err(AppError::uncertain(
            format!(
                "{} accepted the order but returned no order id. Check the order book before placing it again.",
                b.cfg.name
            ),
            None,
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: Some(mapping::error_text(&v)).filter(|m| !m.is_empty()),
    })
}

pub(crate) async fn modify_order(
    b: &XtsBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let v = b
        .interactive(
            Method::PUT,
            "/orders",
            auth,
            Some(&mapping::modify_payload(m)),
        )
        .await?;
    let id = mapping::order_id(result(&v).get("AppOrderID"));
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            m.order_id.clone()
        } else {
            id
        },
        message: None,
    })
}

pub(crate) async fn cancel_order(
    b: &XtsBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let id = order_id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::Validation(format!(
            "{} is not a valid order id.",
            order_id
        )));
    }
    let path = format!("/orders?appOrderID={}", id);
    b.interactive(Method::DELETE, &path, auth, None).await?;
    Ok(OrderResponse {
        order_id: id.to_string(),
        message: None,
    })
}

/// web `close_all_positions`: one MARKET order per open position, using the
/// position's own segment, instrument id and product (no symbol lookup).
pub(crate) async fn close_all_positions(b: &XtsBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let v = b
        .interactive(
            Method::GET,
            "/portfolio/positions?dayOrNet=NetWise",
            auth,
            None,
        )
        .await?;
    let mut out = CloseAllResult::default();
    for p in mapping::position_list(result(&v)) {
        let qty = mapping::f(p, "Quantity") as i64;
        if qty == 0 {
            continue;
        }
        let segment = mapping::s(p, "ExchangeSegment");
        let Some(instrument) = mapping::position_instrument(p).cloned() else {
            out.failed
                .push(format!("{}: position has no instrument id", segment));
            continue;
        };
        let product = mapping::s(p, "ProductType");
        let payload = mapping::exit_payload(&segment, &instrument, &product, qty);
        let label = {
            let token = match &instrument {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let ex = mapping::oa_exchange(&segment);
            let sym = b
                .resolver()
                .by_token(&ex, &token)
                .map(|r| r.symbol)
                .unwrap_or_else(|| mapping::s(p, "TradingSymbol"));
            format!("{} ({})", sym, ex)
        };
        b.order_pacer.acquire().await;
        match place_raw(b, auth, &payload).await {
            Ok(r) => out.placed.push(r.order_id),
            Err(e) => out
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(out)
}

pub(crate) async fn get_order_book(b: &XtsBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let v = b.interactive(Method::GET, "/orders", auth, None).await?;
    Ok(mapping::orders(result(&v), b.resolver()))
}

pub(crate) async fn get_trade_book(b: &XtsBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = b
        .interactive(Method::GET, "/orders/trades", auth, None)
        .await?;
    Ok(mapping::trades(result(&v), b.resolver()))
}

pub(crate) async fn get_positions(b: &XtsBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let v = b
        .interactive(
            Method::GET,
            "/portfolio/positions?dayOrNet=NetWise",
            auth,
            None,
        )
        .await?;
    Ok(mapping::positions(result(&v), b.resolver()))
}

pub(crate) async fn get_holdings(b: &XtsBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = b
        .interactive(Method::GET, "/portfolio/holdings", auth, None)
        .await?;
    Ok(mapping::holdings(result(&v), b.resolver()))
}
