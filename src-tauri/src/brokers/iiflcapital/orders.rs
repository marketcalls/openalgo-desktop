//! Orders and books (web `api/order_api.py`).
//!
//! * place: `POST /orders` with a one-element list; success needs HTTP 200,
//!   an ok status and `result[0].brokerOrderId`.
//! * modify: `PUT /orders/{id}`; cancel: `DELETE /orders/{id}`.
//! * books: `GET /orders`, `/trades`, `/positions`, `/holdings`.

use super::mapping::{self, OrderFields};
use super::IiflCapitalBroker;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

async fn read_book(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    path: &str,
    what: &str,
) -> Result<Value> {
    let (status, data) = b.call(Method::GET, path, auth, None, true).await?;
    // A plain list is a book; so is an envelope with rows or an ok status.
    if status == StatusCode::OK
        && (data.is_array() || !mapping::book_rows(&data).is_empty() || mapping::is_ok(&data))
    {
        return Ok(data);
    }
    let why = mapping::message_of(&data).unwrap_or_else(|| format!("Failed to fetch {}", what));
    tracing::warn!(
        status = status.as_u16(),
        "IIFL Capital {} request failed: {}",
        what,
        why
    );
    Err(AppError::Broker(format!(
        "IIFL Capital could not return your {}: {}",
        what, why
    )))
}

fn log_rejections(rows: &[Value]) {
    for r in rows {
        if mapping::text(r.get("orderStatus")).eq_ignore_ascii_case("REJECTED") {
            tracing::warn!(
                "IIFL Capital rejected order {}: {}",
                mapping::text(mapping::first(r, &["brokerOrderId", "exchangeOrderId"])),
                mapping::text(r.get("rejectionReason"))
            );
        }
    }
}

/// Send one order body; returns the broker order id.
pub(super) async fn send_order(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    payload: Value,
) -> Result<String> {
    let body = Value::Array(vec![payload]);
    let (status, data) = b
        .call(Method::POST, "/orders", auth, Some(&body), false)
        .await?;
    let first = mapping::first_result(&data);
    let order_id = mapping::text(first.get("brokerOrderId"));
    let first_ok = first
        .get("status")
        .and_then(Value::as_str)
        .map(|s| matches!(s.to_ascii_lowercase().as_str(), "success" | "ok"))
        .unwrap_or(false);
    if status == StatusCode::OK && mapping::is_ok(&data) && first_ok && !order_id.is_empty() {
        return Ok(order_id);
    }
    let why = mapping::message_of(&data).unwrap_or_else(|| "Failed to place order".into());
    tracing::warn!(
        status = status.as_u16(),
        "IIFL Capital refused an order: {}",
        why
    );
    Err(AppError::Broker(why))
}

pub async fn place_order(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let fields = OrderFields {
        symbol: &o.symbol,
        action: o.action,
        pricetype: o.pricetype,
        price: o.price,
        trigger_price: o.trigger_price,
        tick_size: o.instrument.tick_size,
    };
    let payload = mapping::order_payload(
        o.token(),
        o.exchange.as_str(),
        &fields,
        o.quantity,
        o.product,
        o.validity,
        o.disclosed_quantity,
        None,
    )?;
    let order_id = send_order(b, auth, payload).await?;
    Ok(OrderResponse {
        order_id,
        message: None,
    })
}

pub async fn modify_order(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let id = mapping::safe_order_id(&m.order_id)?.to_string();
    let fields = OrderFields {
        symbol: &m.symbol,
        action: m.action,
        pricetype: m.pricetype,
        price: m.price,
        trigger_price: m.trigger_price,
        tick_size: m.instrument.tick_size,
    };
    let body = mapping::modify_payload(&fields, m.quantity, m.disclosed_quantity)?;
    let (status, data) = b
        .call(
            Method::PUT,
            &format!("/orders/{}", id),
            auth,
            Some(&body),
            false,
        )
        .await?;
    if status == StatusCode::OK && mapping::is_ok(&data) {
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    let why = mapping::message_of(&data).unwrap_or_else(|| "Failed to modify order".into());
    tracing::warn!(
        status = status.as_u16(),
        "IIFL Capital refused a modify: {}",
        why
    );
    Err(AppError::Broker(why))
}

pub async fn cancel_order(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let id = mapping::safe_order_id(order_id)?.to_string();
    let (status, data) = b
        .call(
            Method::DELETE,
            &format!("/orders/{}", id),
            auth,
            None,
            false,
        )
        .await?;
    if status == StatusCode::OK && mapping::is_ok(&data) {
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    let why = mapping::message_of(&data).unwrap_or_else(|| "Failed to cancel order".into());
    tracing::warn!(
        status = status.as_u16(),
        "IIFL Capital refused a cancel: {}",
        why
    );
    Err(AppError::Broker(why))
}

/// Cancel every order whose raw `orderStatus` is in the web's open set.
pub async fn cancel_all_orders(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let data = read_book(b, auth, "/orders", "order book").await?;
    let ids: Vec<String> = mapping::book_rows(&data)
        .iter()
        .filter(|r| mapping::is_open_status(&mapping::text(r.get("orderStatus"))))
        .map(|r| mapping::text(r.get("brokerOrderId")))
        .filter(|id| !id.is_empty())
        .collect();
    let mut out = CancelAllResult::default();
    for id in ids {
        match cancel_order(b, auth, &id).await {
            Ok(_) => out.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of IIFL Capital order {} failed: {}", id, e.code());
                out.failed.push(id)
            }
        }
    }
    Ok(out)
}

/// Square off every non-zero position with a MARKET order built from the
/// position row itself (web `close_all_positions`).
pub async fn close_all_positions(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
) -> Result<CloseAllResult> {
    let data = b.call(Method::GET, "/positions", auth, None, true).await?.1;
    let mut out = CloseAllResult::default();
    for row in mapping::book_rows(&data) {
        let net = mapping::int(row.get("netQuantity"));
        if net == 0 {
            continue;
        }
        let product = mapping::text(row.get("product"));
        let payload = json!({
            "instrumentId": mapping::text(row.get("instrumentId")),
            "exchange": mapping::text(row.get("exchange")),
            "transactionType": if net > 0 { "SELL" } else { "BUY" },
            "quantity": net.abs().to_string(),
            "orderComplexity": "REGULAR",
            "product": if product.is_empty() { "NORMAL".to_string() } else { product },
            "orderType": "MARKET",
            "validity": "DAY",
            "apiOrderSource": "openalgo",
            "orderTag": "close_all_positions",
        });
        let exchange = mapping::from_segment(&mapping::text(row.get("exchange")));
        let label = format!(
            "{} ({})",
            mapping::resolve_symbol(&row, &exchange, b.resolver()),
            exchange
        );
        match send_order(b, auth, payload).await {
            Ok(id) => out.placed.push(id),
            Err(e) => out
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(out)
}

/// Net quantity of one instrument (web `get_open_position`): the first row
/// whose symbol, segment and product match (blank segment/product match).
pub async fn get_open_position(
    b: &IiflCapitalBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let (_, data) = b.call(Method::GET, "/positions", auth, None, true).await?;
    if !(data.is_array() || mapping::is_ok(&data)) {
        let why = mapping::message_of(&data).unwrap_or_else(|| "no answer".into());
        return Err(AppError::Broker(format!(
            "IIFL Capital could not return your positions: {}",
            why
        )));
    }
    let row_info = b.resolver().by_symbol(exchange.as_str(), symbol);
    let br = row_info
        .as_ref()
        .map(|r| r.br_symbol().to_string())
        .unwrap_or_else(|| symbol.to_string());
    let token = row_info.map(|r| r.token).unwrap_or_default();
    let segment = mapping::to_segment(exchange.as_str());
    let bprod = mapping::product_to_broker(product);
    for row in mapping::book_rows(&data) {
        let rs = mapping::text(mapping::first(&row, &["tradingSymbol", "symbol"]));
        let rid = mapping::text(row.get("instrumentId"));
        let rx = mapping::text(row.get("exchange"));
        let rp = mapping::text(row.get("product"));
        let sym_ok = rs == br || rs == symbol || (!token.is_empty() && rid == token);
        let ex_ok = rx.is_empty() || rx.eq_ignore_ascii_case(&segment);
        let pr_ok = rp.is_empty() || rp.eq_ignore_ascii_case(bprod);
        if sym_ok && ex_ok && pr_ok {
            return Ok(mapping::int(mapping::first(
                &row,
                &["netQuantity", "quantity"],
            )));
        }
    }
    Ok(0)
}

pub async fn get_order_book(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let data = read_book(b, auth, "/orders", "order book").await?;
    let rows = mapping::book_rows(&data);
    log_rejections(&rows);
    Ok(rows
        .iter()
        .map(|r| mapping::order_row(r, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let data = read_book(b, auth, "/trades", "trade book").await?;
    Ok(mapping::book_rows(&data)
        .iter()
        .map(|r| mapping::trade_row(r, b.resolver()))
        .collect())
}

pub async fn get_positions(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let data = read_book(b, auth, "/positions", "positions").await?;
    Ok(mapping::book_rows(&data)
        .iter()
        .map(|r| mapping::position_row(r, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &IiflCapitalBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let data = read_book(b, auth, "/holdings", "holdings").await?;
    Ok(mapping::book_rows(&data)
        .iter()
        .filter_map(|r| mapping::holding_row(r, b.resolver()))
        .collect())
}
