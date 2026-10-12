//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, product_code, FyersHolding, FyersOrder, FyersPosition, FyersTrade};
use super::{code_message, fyers_error, is_ok, FyersBroker};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

const ORDERS_SYNC: &str = "/api/v3/orders/sync";

fn order_id(v: &Value) -> String {
    match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub async fn place_order(
    b: &FyersBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = mapping::place_order_body(o);
    let v = b.call(Method::POST, ORDERS_SYNC, auth, Some(&body)).await?;
    let id = order_id(&v);
    if id.is_empty() {
        return Err(AppError::uncertain(
            "Fyers accepted the order but returned no order id. Check the order book.",
            None,
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &FyersBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = mapping::modify_order_body(m);
    let v = b
        .call(Method::PATCH, ORDERS_SYNC, auth, Some(&body))
        .await?;
    let id = order_id(&v);
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            m.order_id.clone()
        } else {
            id
        },
        message: None,
    })
}

pub async fn cancel_order(
    b: &FyersBroker,
    auth: &AuthToken,
    order_id_in: &str,
) -> Result<OrderResponse> {
    let body = json!({ "id": order_id_in });
    let v = b
        .call(Method::DELETE, ORDERS_SYNC, auth, Some(&body))
        .await?;
    let id = order_id(&v);
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            order_id_in.to_string()
        } else {
            id
        },
        message: None,
    })
}

pub(crate) async fn raw_orders(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<FyersOrder>> {
    let v = b.call(Method::GET, "/api/v3/orders", auth, None).await?;
    Ok(mapping::rows(&v, "orderBook"))
}

pub(crate) async fn raw_positions(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<FyersPosition>> {
    let v = b.call(Method::GET, "/api/v3/positions", auth, None).await?;
    Ok(mapping::rows(&v, "netPositions"))
}

/// web `cancel_all_orders_api`: cancel orders whose Fyers status is 4
/// (trigger pending) or 6 (open).
pub async fn cancel_all_orders(b: &FyersBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        if o.status != 4 && o.status != 6 {
            continue;
        }
        if o.id.is_empty() {
            tracing::warn!("Skipping a Fyers order with no id");
            continue;
        }
        match cancel_order(b, auth, &o.id).await {
            Ok(_) => result.cancelled.push(o.id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.id, e.code());
                result.failed.push(o.id)
            }
        }
    }
    Ok(result)
}

/// web `get_open_position`: match the Fyers symbol and Fyers product on the
/// raw net positions, return `netQty`.
pub async fn get_open_position(
    b: &FyersBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let br = b
        .resolver()
        .br_symbol(symbol, exchange.as_str())
        .unwrap_or_else(|| symbol.to_string());
    let want = product_code(product);
    Ok(raw_positions(b, auth)
        .await?
        .into_iter()
        .find(|p| p.symbol == br && p.product_type == want)
        .map(|p| p.net_qty)
        .unwrap_or(0))
}

/// web `close_all_positions`: one `DELETE /api/v3/positions {"exit_all": 1}`.
/// The open positions are read first so the result names what was squared
/// off (Fyers returns no order ids for an exit-all).
pub async fn close_all_positions(b: &FyersBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let open: Vec<String> = raw_positions(b, auth)
        .await?
        .into_iter()
        .filter(|p| p.net_qty != 0)
        .map(|p| {
            let (s, e) = mapping::oa_identity(&p.symbol, p.exchange, p.segment, b.resolver());
            format!("{} ({})", s, e)
        })
        .collect();
    let mut result = CloseAllResult::default();
    if open.is_empty() {
        return Ok(result);
    }
    let body = json!({ "exit_all": 1 });
    // One request exits everything: an answer lost after sending, or a
    // server error, leaves every position's exit unknown (LOG-08 P1-5).
    let unknown = |result: &mut CloseAllResult, open: Vec<String>| {
        for l in open {
            result.failed.push(format!(
                "{}: {}",
                l,
                crate::brokers::common::outcome::UNCERTAIN_MESSAGE
            ));
            result.uncertain.push(l);
        }
    };
    let sent = crate::brokers::common::outcome::placing(b.raw(
        Method::DELETE,
        "/api/v3/positions",
        auth,
        Some(&body),
    ))
    .await;
    let (status, v) = match sent {
        Ok(x) => x,
        Err(e) if crate::brokers::common::outcome::uncertain_cause(&e).is_some() => {
            tracing::error!("Fyers exit-all has no definite answer: {}", e.code());
            unknown(&mut result, open);
            return Ok(result);
        }
        Err(e) => return Err(e),
    };
    if status.is_server_error() {
        tracing::error!(
            status = status.as_u16(),
            "Fyers exit-all answered a server error"
        );
        unknown(&mut result, open);
        return Ok(result);
    }
    if is_ok(&v) {
        result.placed = open;
    } else {
        let (code, message) = code_message(&v);
        let err = fyers_error(status.as_u16(), code, &message);
        if matches!(err, AppError::Auth(_)) {
            return Err(err);
        }
        tracing::warn!(code, "Fyers exit-all refused: {}", message);
        let reason = err.client_message();
        result.failed = open
            .into_iter()
            .map(|l| format!("{}: {}", l, reason))
            .collect();
    }
    Ok(result)
}

pub async fn get_order_book(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = b.call(Method::GET, "/api/v3/tradebook", auth, None).await?;
    Ok(mapping::map_trades(
        mapping::rows::<FyersTrade>(&v, "tradeBook"),
        b.resolver(),
    ))
}

pub async fn get_positions(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &FyersBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = b.call(Method::GET, "/api/v3/holdings", auth, None).await?;
    Ok(mapping::map_holdings(
        mapping::rows::<FyersHolding>(&v, "holdings"),
        b.resolver(),
    ))
}
