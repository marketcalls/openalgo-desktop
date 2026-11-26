//! Orders, books, funds and margin (web `api/order_api.py`, `funds.py`,
//! `margin_api.py`).

use super::mapping;
use super::{session, FirstockBroker, Session};
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product};
use crate::brokers::common::position_read::unread;
use crate::brokers::families::noren::mapping::{
    escape_tsym, f, i, mpp_margin, num, oa_symbol, product_code, reverse_product, text, MppOrder,
    MppQuote,
};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// The rows of a success: `data` as a list, or one row as an object.
/// Firstock sends `data: []` for an empty book, so anything else is a book
/// that was not read (BR-03).
fn data_list(v: &Value) -> Option<Vec<Value>> {
    match v.get("data") {
        Some(Value::Array(a)) => Some(a.clone()),
        Some(o @ Value::Object(_)) => Some(vec![o.clone()]),
        _ => None,
    }
}

/// The book an endpoint answers (for the trader-facing error).
fn book_of(endpoint: &str) -> Book {
    match endpoint {
        "/positionBook" => Book::Positions,
        "/tradeBook" => Book::Trades,
        "/holdings" => Book::Holdings,
        _ => Book::Orders,
    }
}

/// A book; a failed answer that says "no data" is an empty book. Every
/// position row must carry `netQuantity` (BR-03).
async fn book(b: &FirstockBroker, s: &Session, endpoint: &str) -> Result<Vec<Value>> {
    book_with(b, s, endpoint, || {
        book_read::unreadable("firstock", book_of(endpoint))
    })
    .await
}

/// The position book for a decision to trade: refused with the web's
/// position-read sentence when it cannot be read.
async fn positions(b: &FirstockBroker, s: &Session) -> Result<Vec<Value>> {
    book_with(b, s, "/positionBook", || unread("Firstock")).await
}

async fn book_with(
    b: &FirstockBroker,
    s: &Session,
    endpoint: &str,
    unread: impl Fn() -> AppError,
) -> Result<Vec<Value>> {
    let v = b.call(endpoint, json!({}), s).await?;
    if super::is_success(&v) {
        let rows = data_list(&v).ok_or_else(&unread)?;
        if endpoint == "/positionBook" {
            book_read::require_quantities(&rows, &["netQuantity"], unread)?;
        }
        return Ok(rows);
    }
    // The position book's empty answer is read as the web's position read
    // reads it (its markers, Firstock's message fields); "not found" alone
    // could be any refusal.
    let empty = if endpoint == "/positionBook" {
        crate::brokers::common::position_read::says_no_positions(&v, &["error.message", "message"])
    } else {
        let m = super::error_message(&v).to_ascii_lowercase();
        m.contains("no data") || m.contains("not found")
    };
    if empty {
        return Ok(Vec::new());
    }
    Err(super::firstock_error(&v))
}

/// LTP for MPP (`None` when the quote cannot be read). Read before the
/// order is sent: a server error on the quote never counts against the
/// order's own answer (review P2-3). Only MARKET reads it: an SL-M is
/// priced off its trigger (web #2390).
async fn ltp(b: &FirstockBroker, s: &Session, exchange: &str, brsymbol: &str) -> Option<f64> {
    let q = crate::brokers::common::outcome::before_send(super::data::quote_data(
        b, s, exchange, brsymbol,
    ))
    .await
    .ok()?;
    Some(f(&q, "lastTradedPrice"))
}

pub async fn place_order(
    b: &FirstockBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    let quote = if o.pricetype == PriceType::Market {
        ltp(b, &s, o.exchange.as_str(), o.brsymbol()).await
    } else {
        None
    };
    let v = b
        .call_ok("/placeOrder", mapping::place_body(o, quote)?, &s)
        .await?;
    let id = v
        .get("data")
        .map(|d| text(d, "orderNumber"))
        .unwrap_or_default();
    if id.is_empty() {
        // Accepted without an order number: the order may exist (LOG-08).
        tracing::warn!("Firstock accepted an order without an order number");
        return Err(AppError::uncertain(
            "Firstock accepted the order but returned no order number. Check the order book before placing it again.",
            None,
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &FirstockBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    let quote = if m.pricetype == PriceType::Market {
        ltp(b, &s, m.exchange.as_str(), m.brsymbol()).await
    } else {
        None
    };
    b.call_ok("/modifyOrder", mapping::modify_body(m, quote)?, &s)
        .await?;
    Ok(OrderResponse {
        order_id: m.order_id.clone(),
        message: None,
    })
}

pub async fn cancel_order(
    b: &FirstockBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    b.call_ok("/cancelOrder", json!({"orderNumber": order_id}), &s)
        .await?;
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

/// Cancel every order whose raw status is `OPEN` or `TRIGGER_PENDING`.
pub async fn cancel_all_orders(b: &FirstockBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let s = session(auth)?;
    let mut r = CancelAllResult::default();
    for o in book(b, &s, "/orderBook").await? {
        let st = text(&o, "status").to_ascii_uppercase();
        if st != "OPEN" && st != "TRIGGER_PENDING" && st != "TRIGGER PENDING" {
            continue;
        }
        let id = text(&o, "orderNumber");
        match cancel_order(b, auth, &id).await {
            Ok(_) => r.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", id, e.code());
                r.failed.push(id)
            }
        }
    }
    Ok(r)
}

pub async fn get_open_position(
    b: &FirstockBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let s = session(auth)?;
    let br = escape_tsym(
        &b.resolver()
            .br_symbol(symbol, exchange.as_str())
            .unwrap_or_else(|| symbol.to_string()),
    );
    let prd = product_code(product);
    for p in positions(b, &s).await? {
        if text(&p, "tradingSymbol") == br
            && text(&p, "exchange") == exchange.as_str()
            && text(&p, "product") == prd
        {
            return Ok(i(&p, "netQuantity"));
        }
    }
    Ok(0)
}

pub async fn close_all_positions(b: &FirstockBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let s = session(auth)?;
    let symbols = b.resolver().clone();
    let mut r = CloseAllResult::default();
    for p in positions(b, &s).await? {
        let qty = i(&p, "netQuantity");
        if qty == 0 {
            continue;
        }
        let exch = text(&p, "exchange");
        let symbol = oa_symbol(
            &symbols,
            &exch,
            &text(&p, "token"),
            &text(&p, "tradingSymbol"),
        );
        let label = format!("{} ({})", symbol, exch);
        let product = reverse_product(&text(&p, "product")).unwrap_or("MIS");
        let req = OrderRequest {
            symbol,
            exchange: exch,
            side: if qty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(qty.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: product.into(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => crate::brokers::common::outcome::placing(place_order(b, auth, &o)).await,
            Err(e) => Err(e),
        };
        r.record(&label, placed);
    }
    Ok(r)
}

pub async fn get_order_book(b: &FirstockBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let s = session(auth)?;
    Ok(book(b, &s, "/orderBook")
        .await?
        .iter()
        .map(|o| mapping::map_order(o, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &FirstockBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let s = session(auth)?;
    Ok(book(b, &s, "/tradeBook")
        .await?
        .iter()
        .map(|t| mapping::map_trade(t, b.resolver()))
        .collect())
}

pub async fn get_positions(b: &FirstockBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let s = session(auth)?;
    Ok(book(b, &s, "/positionBook")
        .await?
        .iter()
        .map(|p| mapping::map_position(p, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &FirstockBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let s = session(auth)?;
    let rows = book(b, &s, "/holdings").await?;
    Ok(mapping::map_holdings(&rows, b.resolver()))
}

pub async fn get_funds(b: &FirstockBroker, auth: &AuthToken) -> Result<Funds> {
    let s = session(auth)?;
    let v = b.call_ok("/limit", json!({}), &s).await?;
    let data = book_read::object(v.get("data"), "firstock", Book::Funds)?;
    Ok(mapping::funds_from(data))
}

pub async fn calculate_margin(
    b: &FirstockBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let s = session(auth)?;
    let mut built = Vec::new();
    for l in legs {
        let Some(row) = b.resolver().by_symbol(&l.key.exchange, &l.key.symbol) else {
            tracing::warn!("Margin leg skipped, symbol not found: {}", l.key.symbol);
            continue;
        };
        let quote = if matches!(l.pricetype, PriceType::Market | PriceType::SlM) {
            ltp(b, &s, &l.key.exchange, row.br_symbol())
                .await
                .map(|ltp| MppQuote {
                    ltp,
                    tick: (row.tick_size > 0.0).then_some(row.tick_size),
                })
        } else {
            None
        };
        let (pt, prc) = mpp_margin(
            &MppOrder {
                symbol: &l.key.symbol,
                action: l.action,
                pricetype: l.pricetype,
                price: l.price,
                trigger: l.trigger_price,
            },
            quote,
        )?;
        built.push(json!({
            "exchange": l.key.exchange,
            "tradingSymbol": escape_tsym(row.br_symbol()),
            "quantity": l.quantity.to_string(),
            "price": prc,
            "triggerPrice": num(l.trigger_price),
            "product": product_code(l.product),
            "transactionType": if l.action == Action::Buy { "B" } else { "S" },
            "priceType": pt,
        }));
    }
    let body = mapping::basket_body(built).ok_or_else(|| {
        AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        )
    })?;
    let v = b.call_ok("/basketMargin", body, &s).await?;
    let d = v.get("data").cloned().unwrap_or_default();
    Ok(MarginResult {
        total_margin_required: f(&d, "TradedMargin"),
        span_margin: 0.0,
        exposure_margin: 0.0,
    })
}
