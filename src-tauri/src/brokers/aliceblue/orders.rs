//! Orders and books (web `api/order_api.py`).
//!
//! Place, modify and cancel are not rate limited (AliceBlue does not limit
//! them); every book read draws on the 1800-per-15-minutes budget.

use super::mapping::{self, s};
use super::{broker_error, status_ok, text, AliceBlueBroker};
use crate::brokers::common::mapping::{Exchange, OrderStatus, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

pub const PLACE: &str = "/open-api/od/v1/orders/placeorder";
pub const MODIFY: &str = "/open-api/od/v1/orders/modify";
pub const CANCEL: &str = "/open-api/od/v1/orders/cancel";
pub const ORDER_BOOK: &str = "/open-api/od/v1/orders/book";
pub const TRADE_BOOK: &str = "/open-api/od/v1/orders/trades";
pub const POSITIONS: &str = "/open-api/od/v1/positions";
pub const HOLDINGS: &str = "/open-api/od/v1/holdings/CNC";

/// Which book a refusal came from: decides whether AliceBlue's answer
/// means "empty" (web `get_order_book` / `get_trade_book` /
/// `get_positions` / `get_holdings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Book {
    Orders,
    Trades,
    /// The Positions page and close-all (`strict=False`).
    Positions,
    /// The smart-order read (`strict=True`): a failed read is an error.
    PositionsStrict,
    Holdings,
}

/// Whether a non-`Ok` answer with this message is the broker's way of
/// saying the book is empty. The web matches the message text; AliceBlue
/// also answers with the bare code, so the codes of the same sentences
/// count too.
pub fn means_empty(book: Book, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let has = |code: &str| message.contains(code);
    match book {
        Book::Orders => {
            message.contains("Failed to retrieve")
                || lower.contains("no orders")
                || has("EC915")
                || has("EC916")
        }
        Book::Trades => {
            message.contains("No trades")
                || lower.contains("not found")
                || has("EC926")
                || has("EC927")
        }
        Book::Positions => {
            message.contains("No position")
                || lower.contains("not found")
                || message.contains("Failed to retrieve")
                || has("EC919")
                || has("EC920")
        }
        Book::PositionsStrict => {
            (message.contains("No position") || lower.contains("not found") || has("EC920"))
                && !has("EC919")
                && !message.contains("Failed to retrieve")
        }
        Book::Holdings => {
            message.contains("No holding")
                || lower.contains("not found")
                || message.contains("Failed to retrieve")
                || has("EC921")
                || has("EC922")
        }
    }
}

/// Rows of a book, or empty when AliceBlue says there are none.
pub async fn read_book(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    path: &str,
    book: Book,
) -> Result<Vec<Value>> {
    let (_, v) = b.call(Method::GET, path, auth, None, true).await?;
    if status_ok(&v) {
        return Ok(match v.get("result") {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        });
    }
    let msg = text(v.get("message"));
    if means_empty(book, &msg) {
        tracing::debug!("AliceBlue reports an empty book: {}", msg);
        return Ok(Vec::new());
    }
    tracing::warn!("AliceBlue book read refused: {}", mapping::describe(&msg));
    Err(broker_error(
        &v,
        "AliceBlue could not return the book. Try again shortly.",
    ))
}

pub async fn place_order(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = json!([mapping::place_payload(o)]);
    let (_, v) = b
        .call(Method::POST, PLACE, auth, Some(&body), false)
        .await?;
    if !status_ok(&v) {
        tracing::warn!(
            "AliceBlue refused an order: {}",
            mapping::describe(&s(&v, "message"))
        );
        return Err(broker_error(&v, "AliceBlue did not accept the order."));
    }
    let first = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null);
    let order_id = s(&first, "brokerOrderId");
    let result_status = s(&first, "status");
    if !result_status.is_empty() && result_status != "Ok" && order_id.is_empty() {
        tracing::warn!(
            "AliceBlue refused an order ({}): {}",
            result_status,
            mapping::describe(&s(&first, "message"))
        );
        return Err(broker_error(&first, "AliceBlue did not accept the order."));
    }
    if order_id.is_empty() {
        return Err(AppError::Broker(
            "AliceBlue accepted the request but returned no order id. Check the order book before retrying."
                .into(),
        ));
    }
    Ok(OrderResponse {
        order_id,
        message: None,
    })
}

pub async fn modify_order(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = mapping::modify_payload(m);
    let (_, v) = b
        .call(Method::POST, MODIFY, auth, Some(&body), false)
        .await?;
    if !status_ok(&v) {
        return Err(broker_error(&v, "Failed to modify order"));
    }
    let id = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|r| s(r, "brokerOrderId"))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| m.order_id.clone());
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn cancel_order(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let body = json!({ "brokerOrderId": order_id });
    let (_, v) = b
        .call(Method::POST, CANCEL, auth, Some(&body), false)
        .await?;
    if !status_ok(&v) {
        return Err(broker_error(&v, "Failed to cancel order"));
    }
    let id = v
        .get("result")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(|r| s(r, "brokerOrderId"))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| order_id.to_string());
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn get_order_book(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let rows = read_book(b, auth, ORDER_BOOK, Book::Orders).await?;
    Ok(rows
        .iter()
        .map(|r| mapping::order_from(r, &b.symbols))
        .collect())
}

pub async fn get_trade_book(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = read_book(b, auth, TRADE_BOOK, Book::Trades).await?;
    Ok(rows
        .iter()
        .map(|r| mapping::trade_from(r, &b.symbols))
        .collect())
}

pub async fn get_positions(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    positions(b, auth, Book::Positions).await
}

async fn positions(b: &AliceBlueBroker, auth: &AuthToken, book: Book) -> Result<Vec<Position>> {
    let rows = read_book(b, auth, POSITIONS, book).await?;
    Ok(rows
        .iter()
        .map(|r| mapping::position_from(r, &b.symbols))
        .collect())
}

pub async fn get_holdings(b: &AliceBlueBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let rows = read_book(b, auth, HOLDINGS, Book::Holdings).await?;
    Ok(rows
        .iter()
        .filter_map(|r| mapping::holding_from(r, &b.symbols))
        .collect())
}

/// web `get_open_position`: the strict read, so a failed position book
/// is an error rather than a flat position.
pub async fn get_open_position(
    b: &AliceBlueBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let book = positions(b, auth, Book::PositionsStrict).await?;
    Ok(book
        .iter()
        .find(|p| {
            p.symbol == symbol && p.exchange == exchange.as_str() && p.product == product.as_str()
        })
        .map(|p| i64::from(p.quantity))
        .unwrap_or(0))
}

/// web `cancel_all_orders_api`: an unreadable order book cancels nothing.
pub async fn cancel_all_orders(b: &AliceBlueBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let book = match get_order_book(b, auth).await {
        Ok(book) => book,
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => {
            tracing::warn!(
                "AliceBlue order book unavailable for cancel-all: {}",
                e.code()
            );
            return Ok(CancelAllResult::default());
        }
    };
    let mut out = CancelAllResult::default();
    for o in book {
        let pending = o
            .status
            .parse::<OrderStatus>()
            .map(OrderStatus::is_pending)
            .unwrap_or(false);
        if !pending {
            continue;
        }
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

/// web `close_all_positions`: one MARKET exit per open position, product
/// from the position; an unreadable book reads as no positions.
pub async fn close_all_positions(b: &AliceBlueBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let book = match get_positions(b, auth).await {
        Ok(book) => book,
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => {
            tracing::warn!(
                "AliceBlue position book unavailable for close-all: {}",
                e.code()
            );
            return Ok(CloseAllResult::default());
        }
    };
    let mut out = CloseAllResult::default();
    for p in book.into_iter().filter(|p| p.quantity != 0) {
        let label = format!("{} ({})", p.symbol, p.exchange);
        let req = OrderRequest {
            symbol: p.symbol.clone(),
            exchange: p.exchange.clone(),
            side: if p.quantity > 0 { "SELL" } else { "BUY" }.to_string(),
            quantity: p.quantity.abs(),
            price: 0.0,
            order_type: PriceType::Market.as_str().to_string(),
            product: p.product.clone(),
            validity: "DAY".to_string(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let outcome = match ResolvedOrder::resolve(&req, &b.symbols) {
            Ok(order) => place_order(b, auth, &order).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(r) => out.placed.push(r.order_id),
            Err(e) => out
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(out)
}
