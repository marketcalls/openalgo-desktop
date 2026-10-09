//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, s};
use super::{is_success, message, refusal, MstockBroker};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::position_read::{says_no_positions, unread};
use crate::brokers::types::*;
use crate::error::Result;
use reqwest::Method;
use serde_json::Value;

pub const PLACE_PATH: &str = "/orders/regular";
pub const CANCEL_ALL_PATH: &str = "/orders/cancelall";
pub const ORDER_BOOK_PATH: &str = "/orders";
pub const TRADE_BOOK_PATH: &str = "/tradebook";
pub const POSITIONS_PATH: &str = "/portfolio/positions";
pub const HOLDINGS_PATH: &str = "/portfolio/holdings";

/// web `place_order_api`: success only when the payload says so and carries
/// an order id (mStock answers HTTP 200 for RMS rejections too).
pub async fn place_order(
    b: &MstockBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = mapping::place_order_body(o);
    let v = b.call(Method::POST, PLACE_PATH, auth, Some(&body)).await?;
    match mapping::extract_order_id(&v) {
        Some(id) => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        None => {
            tracing::warn!("mStock refused the order: {}", message(&v));
            Err(refusal(&v, "mStock did not accept the order."))
        }
    }
}

/// web `modify_order`: `PUT /orders/regular/{id}`; success when `status`
/// is true or `message == "SUCCESS"`; the answer's id, else the request's.
pub async fn modify_order(
    b: &MstockBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = mapping::modify_order_body(m);
    let path = format!("{}/{}", PLACE_PATH, m.order_id);
    let v = b.call(Method::PUT, &path, auth, Some(&body)).await?;
    if !(is_success(&v) || message(&v) == "SUCCESS") {
        tracing::warn!("mStock refused the modification: {}", message(&v));
        return Err(refusal(&v, "mStock did not accept the modification."));
    }
    let id = v
        .get("data")
        .filter(|d| d.is_object())
        .map(|d| s(d, "orderid"))
        .filter(|x| !x.is_empty())
        .unwrap_or_else(|| m.order_id.clone());
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

/// web `cancel_order`: `DELETE /orders/regular/{id}` with
/// `{"variety":"NORMAL","orderid"}` in the body.
pub async fn cancel_order(
    b: &MstockBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let body = mapping::cancel_order_body(order_id);
    let path = format!("{}/{}", PLACE_PATH, order_id);
    let v = b.call(Method::DELETE, &path, auth, Some(&body)).await?;
    if is_success(&v) || message(&v) == "SUCCESS" {
        Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        })
    } else {
        tracing::warn!("mStock refused cancel of {}: {}", order_id, message(&v));
        Err(refusal(&v, "mStock did not cancel the order."))
    }
}

/// A book call: a refusal that only says the book is empty reads as empty.
async fn book(b: &MstockBroker, auth: &AuthToken, path: &str) -> Result<Value> {
    let v = b.call(Method::GET, path, auth, None).await?;
    if is_success(&v) || v.get("data").is_some_and(Value::is_array) {
        return Ok(v);
    }
    let msg = message(&v).to_ascii_lowercase();
    if v.as_object().is_some_and(|o| o.is_empty())
        || msg.contains("no data")
        || msg.contains("not found")
        || msg.contains("no record")
    {
        return Ok(Value::Object(Default::default()));
    }
    Err(refusal(
        &v,
        "mStock did not return your account details. Try again shortly.",
    ))
}

/// web `cancel_all_orders_api`: list the cancellable orders, then one
/// `POST /orders/cancelall` (no body) when there is any.
pub async fn cancel_all_orders(b: &MstockBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let book = book(b, auth, ORDER_BOOK_PATH).await?;
    let pending: Vec<String> = mapping::rows(&book)
        .iter()
        .filter(|o| mapping::is_cancellable(&s(o, "status")))
        .map(|o| s(o, "orderid"))
        .filter(|id| !id.is_empty())
        .collect();
    if pending.is_empty() {
        return Ok(CancelAllResult::default());
    }
    let ok = match b.call(Method::POST, CANCEL_ALL_PATH, auth, None).await {
        Ok(v) => {
            let ok = is_success(&v)
                || v.get("status").and_then(Value::as_str) == Some("success")
                || message(&v) == "SUCCESS";
            if !ok {
                tracing::warn!("mStock refused cancel all: {}", message(&v));
            }
            ok
        }
        Err(e) => {
            tracing::warn!("mStock cancel all failed: {}", e.code());
            false
        }
    };
    Ok(if ok {
        CancelAllResult {
            cancelled: pending,
            failed: Vec::new(),
        }
    } else {
        CancelAllResult {
            cancelled: Vec::new(),
            failed: pending,
        }
    })
}

pub(crate) async fn raw_positions(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    Ok(mapping::rows(&book(b, auth, POSITIONS_PATH).await?))
}

/// web `_position_book_ok`: mStock marks a read that worked with `status`
/// true, as a bool or as the text "true" (or "success").
pub fn positions_ok(v: &Value) -> bool {
    match v.get("status") {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(t)) => matches!(t.to_ascii_lowercase().as_str(), "true" | "success"),
        _ => false,
    }
}

/// The rows for a smart order (web `read_position_book`, #2116): a book
/// mStock did not confirm refuses the order instead of reading as flat,
/// unless its `message` says the book is empty. An empty or unreadable
/// body is not a read (the Positions page still shows it as empty).
async fn positions_strict(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let v = b.call(Method::GET, POSITIONS_PATH, auth, None).await?;
    if positions_ok(&v) {
        return Ok(mapping::rows(&v));
    }
    if says_no_positions(&v, &["message"]) {
        return Ok(Vec::new());
    }
    tracing::error!("mStock position book not confirmed: {}", message(&v));
    Err(unread("mStock by Mirae Asset"))
}

/// web `get_open_position`: match the instrument token, exchange and mStock
/// product on the raw book; `netqty`, 0 when absent. The exchange is
/// compared after the derivative fix, so NFO / BFO positions (reported as
/// NSE / BSE) match too.
pub async fn get_open_position(
    b: &MstockBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let Some(token) = b.resolver().token(symbol, exchange.as_str()) else {
        tracing::warn!("Token not found for {} on {}", symbol, exchange);
        return Ok(0);
    };
    let producttype = mapping::map_product_type(product);
    Ok(positions_strict(b, auth)
        .await?
        .iter()
        .find(|p| {
            s(p, "symboltoken") == token
                && mapping::oa_exchange(&s(p, "exchange"), &s(p, "instrumenttype"))
                    == exchange.as_str()
                && s(p, "producttype") == producttype
        })
        .map(|p| mapping::i(p, "netqty"))
        .unwrap_or(0))
}

/// web `close_all_positions`: one MARKET order per non-zero `netqty`, the
/// symbol found by token on the OpenAlgo exchange (NFO / BFO for
/// derivatives), the product reverse-mapped.
pub async fn close_all_positions(b: &MstockBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        let netqty = mapping::i(&p, "netqty");
        if netqty == 0 {
            continue;
        }
        let exchange = mapping::oa_exchange(&s(&p, "exchange"), &s(&p, "instrumenttype"));
        let token = s(&p, "symboltoken");
        let Some(row) = symbols.by_token(&exchange, &token) else {
            tracing::warn!("Symbol not found for token {} on {}", token, exchange);
            result.failed.push(format!(
                "token {} ({}): the instrument is not in the master contract. Download the master contract again.",
                token, exchange
            ));
            continue;
        };
        let label = format!("{} ({})", row.symbol, exchange);
        let req = OrderRequest {
            symbol: row.symbol.clone(),
            exchange: exchange.clone(),
            side: if netqty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(netqty.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: mapping::reverse_map_product_type(&s(&p, "producttype"))
                .unwrap_or("MIS")
                .into(),
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
            Ok(r) => result.placed.push(r.order_id),
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

pub async fn get_order_book(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let v = book(b, auth, ORDER_BOOK_PATH).await?;
    Ok(mapping::map_orders(&v, b.resolver()))
}

pub async fn get_trade_book(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = book(b, auth, TRADE_BOOK_PATH).await?;
    Ok(mapping::map_trades(&v, b.resolver()))
}

pub async fn get_positions(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let v = book(b, auth, POSITIONS_PATH).await?;
    Ok(mapping::map_positions(&v, b.resolver()))
}

pub async fn get_holdings(b: &MstockBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let v = book(b, auth, HOLDINGS_PATH).await?;
    Ok(mapping::map_holdings(&v, b.resolver()))
}
