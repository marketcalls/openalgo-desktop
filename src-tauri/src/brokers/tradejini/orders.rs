//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).
//!
//! * place `POST /api/oms/place-order` (form): `symId, qty, side, type,
//!   product, validity, [limitPrice], [trigPrice], [discQty], [amo=true],
//!   [mktProt=2 for market/stopmarket]` -> `d.orderId`.
//! * modify `PUT /api/oms/modify-order` (form): `symId, orderId, qty, type,
//!   validity, side, [limitPrice], [trigPrice], [discQty]`.
//! * cancel `DELETE /api/oms/cancel-order?orderId=`.
//! * books `GET /api/oms/{orders,trades,positions,holdings}?symDetails=true`.

use super::mapping::{self, py_float};
use super::{Body, TradejiniBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

/// Place-order form (web `transform_data`). Zero prices and quantities are
/// left out, as the web intends with its "remove optional fields" pass.
pub fn place_order_form(o: &ResolvedOrder) -> Vec<(&'static str, String)> {
    let kind = mapping::order_type(o.pricetype);
    let mut f: Vec<(&'static str, String)> = vec![
        ("symId", o.brsymbol().to_string()),
        ("qty", o.quantity.to_string()),
        (
            "side",
            if o.action == crate::brokers::common::mapping::Action::Buy {
                "buy"
            } else {
                "sell"
            }
            .to_string(),
        ),
        ("type", kind.to_string()),
        ("product", mapping::product(o.product).to_string()),
    ];
    if o.price != 0.0 {
        f.push(("limitPrice", py_float(o.price)));
    }
    if o.trigger_price != 0.0 {
        f.push(("trigPrice", py_float(o.trigger_price)));
    }
    f.push((
        "validity",
        mapping::validity(o.validity, o.exchange).to_string(),
    ));
    if o.disclosed_quantity != 0 {
        f.push(("discQty", o.disclosed_quantity.to_string()));
    }
    if o.amo {
        f.push(("amo", "true".to_string()));
    }
    if matches!(kind, "market" | "stopmarket") {
        f.push(("mktProt", "2".to_string()));
    }
    f
}

/// Modify-order form (web `transform_modify_order_data`). The total
/// quantity is filled + new; OpenAlgo's modify request carries no filled
/// quantity, so it is the new quantity.
pub fn modify_order_form(m: &ResolvedModify) -> Vec<(&'static str, String)> {
    let mut f: Vec<(&'static str, String)> = vec![
        ("symId", m.brsymbol().to_string()),
        ("orderId", m.order_id.clone()),
        ("qty", m.quantity.to_string()),
        ("type", mapping::order_type(m.pricetype).to_string()),
        ("validity", "day".to_string()),
        ("side", m.action.as_str().to_ascii_lowercase()),
    ];
    if matches!(m.pricetype, PriceType::Limit | PriceType::Sl) {
        f.push(("limitPrice", py_float(m.price)));
    }
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        f.push(("trigPrice", py_float(m.trigger_price)));
    }
    if m.disclosed_quantity != 0 {
        f.push(("discQty", m.disclosed_quantity.to_string()));
    }
    f
}

fn d_order_id(v: &Value) -> Option<String> {
    v.get("d")
        .map(|d| mapping::s(d, "orderId"))
        .filter(|s| !s.is_empty())
}

pub async fn place_order(
    b: &TradejiniBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let form = place_order_form(o);
    let v = b
        .call(
            Method::POST,
            "/api/oms/place-order",
            &[],
            auth,
            Body::Form(&form),
        )
        .await?;
    let id = d_order_id(&v).ok_or_else(|| {
        tracing::warn!("Tradejini accepted an order without an order id");
        AppError::Broker("Tradejini did not return an order id for this order.".into())
    })?;
    let msg = v.get("d").map(|d| mapping::s(d, "msg"));
    Ok(OrderResponse {
        order_id: id,
        message: msg.filter(|m| !m.is_empty()),
    })
}

pub async fn modify_order(
    b: &TradejiniBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let form = modify_order_form(m);
    let v = b
        .call(
            Method::PUT,
            "/api/oms/modify-order",
            &[],
            auth,
            Body::Form(&form),
        )
        .await?;
    Ok(OrderResponse {
        order_id: d_order_id(&v).unwrap_or_else(|| m.order_id.clone()),
        message: Some("Order modified successfully".into()),
    })
}

pub async fn cancel_order(
    b: &TradejiniBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let v = b
        .call(
            Method::DELETE,
            "/api/oms/cancel-order",
            &[("orderId", order_id.to_string())],
            auth,
            Body::None,
        )
        .await?;
    Ok(OrderResponse {
        order_id: d_order_id(&v).unwrap_or_else(|| order_id.to_string()),
        message: Some("Order cancelled successfully".into()),
    })
}

/// web `cancel_all_orders_api`: order book, then cancel every order whose
/// status is OPEN, TRIGGER PENDING, MODIFIED or PENDING.
pub async fn cancel_all_orders(b: &TradejiniBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let book = get_order_book(b, auth).await?;
    let mut out = CancelAllResult::default();
    for o in book
        .into_iter()
        .filter(|o| mapping::is_cancellable(&o.status))
    {
        match cancel_order(b, auth, &o.order_id).await {
            Ok(_) => out.cancelled.push(o.order_id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.order_id, e.code());
                out.failed.push(o.order_id)
            }
        }
    }
    Ok(out)
}

/// web `get_open_position`: the first non-zero row of this symbol on this
/// exchange. The web matches on symbol and exchange only (the product is
/// mapped but not compared), and so does this.
pub async fn get_open_position(
    b: &TradejiniBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    _product: Product,
) -> Result<i64> {
    let want = symbol.trim().to_ascii_uppercase();
    let positions = get_positions(b, auth).await?;
    Ok(positions
        .iter()
        .find(|p| {
            p.symbol.trim().eq_ignore_ascii_case(&want)
                && p.exchange.trim().eq_ignore_ascii_case(exchange.as_str())
                && p.quantity != 0
        })
        .map(|p| i64::from(p.quantity))
        .unwrap_or(0))
}

fn sym_details() -> [(&'static str, String); 1] {
    [("symDetails", "true".to_string())]
}

pub async fn get_order_book(b: &TradejiniBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let v = b
        .call(
            Method::GET,
            "/api/oms/orders",
            &sym_details(),
            auth,
            Body::None,
        )
        .await?;
    Ok(mapping::rows(&v)
        .iter()
        .map(|o| mapping::order_row(o, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &TradejiniBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = b
        .call(
            Method::GET,
            "/api/oms/trades",
            &sym_details(),
            auth,
            Body::None,
        )
        .await?;
    Ok(mapping::rows(&v)
        .iter()
        .map(|t| mapping::trade_row(t, b.resolver()))
        .collect())
}

pub async fn get_positions(b: &TradejiniBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let v = b
        .call(
            Method::GET,
            "/api/oms/positions",
            &sym_details(),
            auth,
            Body::None,
        )
        .await?;
    Ok(mapping::rows(&v)
        .iter()
        .map(|p| mapping::position_row(p, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &TradejiniBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = b
        .call(
            Method::GET,
            "/api/oms/holdings",
            &sym_details(),
            auth,
            Body::None,
        )
        .await?;
    // `d.holdings[]`; `d` may be the string "No Holdings".
    let list = match v.get("d").and_then(|d| d.get("holdings")) {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    Ok(list
        .iter()
        .filter_map(|h| mapping::holding_row(h, b.resolver()))
        .collect())
}
