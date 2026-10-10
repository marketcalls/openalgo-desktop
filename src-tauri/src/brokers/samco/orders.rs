//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, text, Resolved};
use super::{data, is_success, samco_error, SamcoBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::position_read::{says_no_positions, unread};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

/// MPP for MARKET / SL-M, fetching the LTP for a MARKET order.
#[allow(clippy::too_many_arguments)]
async fn resolve(
    b: &SamcoBroker,
    auth: &AuthToken,
    key: &QuoteKey,
    instrument_tick: f64,
    pricetype: PriceType,
    action: crate::brokers::common::mapping::Action,
    price: f64,
    trigger_price: f64,
) -> Result<Resolved> {
    let ltp = if pricetype == PriceType::Market {
        match data::get_quote(b, auth, key).await {
            Ok(q) => Some(q.ltp),
            Err(e) => {
                tracing::warn!(
                    "Samco quote for market price protection failed: {}",
                    e.code()
                );
                return Err(AppError::Broker(format!(
                    "MARKET order failed: the live price of {} could not be read, so a protected price could not be set. Try again, or place a LIMIT order.",
                    key.symbol
                )));
            }
        }
    } else {
        None
    };
    mapping::resolve_order_type(
        pricetype,
        action,
        &key.symbol,
        price,
        trigger_price,
        Some(instrument_tick),
        ltp,
    )
    .map_err(AppError::Validation)
}

pub async fn place_order(
    b: &SamcoBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let key = QuoteKey::new(o.exchange.as_str(), &o.symbol);
    let r = resolve(
        b,
        auth,
        &key,
        o.instrument.tick_size,
        o.pricetype,
        o.action,
        o.price,
        o.trigger_price,
    )
    .await?;
    let body = mapping::place_body(o, &r);
    let (status, v) = b
        .send(Method::POST, "/order/placeOrder", auth, Some(&body))
        .await?;
    if is_success(&v) {
        let id = text(v.get("orderNumber"));
        if !id.is_empty() {
            return Ok(OrderResponse {
                order_id: id,
                message: None,
            });
        }
    }
    tracing::warn!(status = status.as_u16(), "Samco refused an order");
    Err(samco_error(status, &v, "Samco did not accept the order."))
}

pub async fn modify_order(
    b: &SamcoBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let key = QuoteKey::new(m.exchange.as_str(), &m.symbol);
    let r = resolve(
        b,
        auth,
        &key,
        m.instrument.tick_size,
        m.pricetype,
        m.action,
        m.price,
        m.trigger_price,
    )
    .await?;
    let body = mapping::modify_body(m, &r);
    let path = format!("/order/modifyOrder/{}", super::url_quote(m.order_id.trim()));
    let (status, v) = b.send(Method::PUT, &path, auth, Some(&body)).await?;
    if is_success(&v) {
        // The documented key is `orderNumber`; the actual body says
        // `ordernumber`. Fall back to the id we were given.
        let id = [text(v.get("orderNumber")), text(v.get("ordernumber"))]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| m.order_id.clone());
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    Err(samco_error(status, &v, "Failed to modify order"))
}

pub async fn cancel_order(
    b: &SamcoBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let path = format!(
        "/order/cancelOrder?orderNumber={}",
        super::url_quote(order_id.trim())
    );
    let (status, v) = b.send(Method::DELETE, &path, auth, None).await?;
    if is_success(&v) {
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    Err(samco_error(status, &v, "Failed to cancel order"))
}

/// A `Failure` answer whose short `statusMessage` says the book is empty
/// (`position_read` phrases, or "no order" for the order book). Anything
/// else, including an empty or unreadable body, is not an empty book.
fn says_empty_book(v: &Value) -> bool {
    if says_no_positions(v, &["statusMessage"]) {
        return true;
    }
    let msg = text(v.get("statusMessage"));
    msg.chars().count() <= 200 && msg.to_ascii_lowercase().contains("no order")
}

/// Book reads (BR-02): a confirmed `Success`, the details array itself, or
/// a `Failure` that says the book is empty; anything else (an empty or
/// non-JSON body, an error page, another `Failure`) is an error, never an
/// empty book.
fn book_or_error(status: reqwest::StatusCode, v: Value, key: &str) -> Result<Value> {
    if is_success(&v) || v.get(key).is_some_and(Value::is_array) {
        return Ok(v);
    }
    if says_empty_book(&v) {
        return Ok(Value::Null);
    }
    tracing::warn!(
        status = status.as_u16(),
        "Samco book not confirmed: {}",
        text(v.get("statusMessage"))
    );
    Err(samco_error(status, &v, "Samco could not load this book."))
}

pub async fn get_order_book(b: &SamcoBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let (status, v) = b.send(Method::GET, "/order/orderBook", auth, None).await?;
    let v = book_or_error(status, v, "orderBookDetails")?;
    Ok(mapping::order_book(&v, b.resolver()))
}

pub async fn get_trade_book(b: &SamcoBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let (status, v) = b.send(Method::GET, "/trade/tradeBook", auth, None).await?;
    let v = book_or_error(status, v, "tradeBookDetails")?;
    Ok(mapping::trade_book(&v, b.resolver()))
}

async fn raw_positions(
    b: &SamcoBroker,
    auth: &AuthToken,
    kind: &str,
) -> Result<(reqwest::StatusCode, Value)> {
    b.send(
        Method::GET,
        &format!("/position/getPositions?positionType={}", kind),
        auth,
        None,
    )
    .await
}

/// The position book is the DAY book (web `get_positions` default).
pub async fn get_positions(b: &SamcoBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let (status, v) = raw_positions(b, auth, "DAY").await?;
    let v = book_or_error(status, v, "positionDetails")?;
    Ok(mapping::positions(&v, b.resolver()))
}

pub async fn get_holdings(b: &SamcoBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let (status, v) = b
        .send(Method::GET, "/holding/getHoldings", auth, None)
        .await?;
    let v = book_or_error(status, v, "holdingDetails")?;
    Ok(mapping::holdings(&v, b.resolver()))
}

/// web `cancel_all_orders_api`: every order whose raw status is open,
/// pending or trigger pending. BR-02: an order book Samco did not confirm
/// is an error, never "nothing to cancel" (the web returns no orders and
/// reports success): only a `Success` with the order list, or a `Failure`
/// that says the book is empty, is read; an expired session keeps its
/// sign-in message.
pub async fn cancel_all_orders(b: &SamcoBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let (status, v) = b.send(Method::GET, "/order/orderBook", auth, None).await?;
    let mut result = CancelAllResult::default();
    let orders: Vec<Value> = match v.get("orderBookDetails") {
        Some(Value::Array(a)) if is_success(&v) => a.clone(),
        _ if !is_success(&v) && says_empty_book(&v) => Vec::new(),
        _ => {
            tracing::error!(
                status = status.as_u16(),
                "Samco order book not confirmed for cancel all: {}",
                text(v.get("statusMessage"))
            );
            return Err(match samco_error(status, &v, "") {
                e @ AppError::Auth(_) => e,
                _ => AppError::Broker(
                    "Samco could not load the order book, so no order was cancelled. Check your orders on Samco and try again."
                        .into(),
                ),
            });
        }
    };
    let ids: Vec<String> = orders
        .iter()
        .filter(|o| {
            matches!(
                text(o.get("orderStatus")).to_ascii_lowercase().as_str(),
                "open" | "pending" | "trigger pending"
            )
        })
        .map(|o| text(o.get("orderNumber")))
        .filter(|id| !id.trim().is_empty())
        .collect();
    for id in ids {
        match cancel_order(b, auth, &id).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Samco cancel of order {} failed: {}", id, e.code());
                result.failed.push(id);
            }
        }
    }
    Ok(result)
}

/// web `_collect_open_positions`: DAY then NET, deduplicated on
/// (tradingSymbol, exchange, productCode) with DAY winning. Returns the
/// books that could not be read.
pub async fn collect_open_positions(
    b: &SamcoBroker,
    auth: &AuthToken,
) -> (Vec<Value>, Vec<&'static str>) {
    let mut merged: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut failed = Vec::new();
    for kind in ["DAY", "NET"] {
        let v = match raw_positions(b, auth, kind).await {
            Ok((_, v)) if is_success(&v) => v,
            Ok(_) => {
                tracing::warn!("Samco {} positions could not be read", kind);
                failed.push(kind);
                continue;
            }
            Err(e) => {
                tracing::warn!("Samco {} positions failed: {}", kind, e.code());
                failed.push(kind);
                continue;
            }
        };
        for p in v
            .get("positionDetails")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let k = (
                text(p.get("tradingSymbol")),
                text(p.get("exchange")),
                text(p.get("productCode")),
            );
            if seen.insert(k) {
                merged.push(p);
            }
        }
    }
    (merged, failed)
}

/// web `close_all_positions`: refuse when either book is unreadable, then
/// one MARKET exit per open position.
pub async fn close_all_positions(b: &SamcoBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let (positions, failed_types) = collect_open_positions(b, auth).await;
    if !failed_types.is_empty() {
        return Err(AppError::Broker(format!(
            "Could not read the {} position book, so positions may remain open. No square-off was attempted. Please retry.",
            failed_types.join(" and ")
        )));
    }
    let mut result = CloseAllResult::default();
    for p in positions {
        let net = mapping::signed_net(&p);
        if net == 0 {
            continue;
        }
        let (symbol, exchange) = mapping::oa_symbol(
            b.resolver(),
            &text(p.get("tradingSymbol")),
            &text(p.get("exchange")),
        );
        let label = format!("{} ({})", symbol, exchange);
        let req = OrderRequest {
            symbol,
            exchange,
            // A long (BUY) position is closed with a SELL and vice versa.
            side: if text(p.get("transactionType")) == "SELL" {
                "BUY"
            } else {
                "SELL"
            }
            .into(),
            quantity: net.unsigned_abs() as i32,
            price: 0.0,
            order_type: "MARKET".into(),
            product: mapping::reverse_product(&text(p.get("productCode"))).into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let outcome = match ResolvedOrder::resolve(&req, b.resolver()) {
            Ok(o) => place_order(b, auth, &o).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(r) => result.placed.push(r.order_id),
            Err(e) => result
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(result)
}

/// web `_position_book_ok`: Samco says `status` "Success" for a read that
/// worked and "Failure" otherwise; an empty body is not a read.
pub fn positions_ok(v: &Value) -> bool {
    v.get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("success"))
}

/// web `get_open_position`: the DAY book row with the same trading symbol,
/// exchange and product, signed by `transactionType`. A book Samco did not
/// confirm refuses the order unless its `statusMessage` says the book is
/// empty (web `read_position_book`, #2116).
pub async fn get_open_position(
    b: &SamcoBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let br = b
        .resolver()
        .br_symbol(symbol, exchange.as_str())
        .unwrap_or_else(|| symbol.to_string());
    let (status, v) = raw_positions(b, auth, "DAY").await?;
    if !positions_ok(&v) {
        if says_no_positions(&v, &["statusMessage"]) {
            return Ok(0);
        }
        // Never report flat on a book we could not read (web #2116): an
        // expired session keeps its own sign-in message.
        tracing::error!(
            "Samco position book not confirmed: {}",
            text(v.get("statusMessage"))
        );
        return Err(match samco_error(status, &v, "") {
            e @ AppError::Auth(_) => e,
            _ => unread("Samco"),
        });
    }
    Ok(v.get("positionDetails")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter().find(|p| {
                text(p.get("tradingSymbol")) == br
                    && mapping::oa_exchange(&text(p.get("exchange"))) == exchange.as_str()
                    && text(p.get("productCode")) == product.as_str()
            })
        })
        .map(mapping::signed_net)
        .unwrap_or(0))
}
