//! Orders, order and trade books, positions and holdings (web
//! `api/order_api.py`, marketcalls/openalgo#2467):
//!
//! ```text
//! POST   /api/v1/orders                     place-regular-order.md
//! PUT    /api/v1/orders                     modify-regular-order.md
//! DELETE /api/v1/orders/<oms_order_id>      cancel-regular-order.md
//! GET    /api/v1/orders?type=pending        fetch-pending-orders.md
//! GET    /api/v1/orders?type=completed      fetch-completed-orders.md
//! GET    /api/v1/trades                     trade-book.md
//! GET    /api/v1/positions?type=historical  fetch-position-netwise.md
//! GET    /api/v1/holdings                   fetch-demat-holdings.md
//! ```
//!
//! Every call carries the session's client id. Positions are read netwise,
//! not daywise: a carry-forward NRML or CNC position from an earlier day is
//! one a smart order and close-all must see.
//!
//! A placement has three outcomes (LOG-08): the order id is accepted;
//! Pocketful's explicit failure, a 4xx and an order refused before sending
//! (it could not be priced) are refused; a lost answer, a 5xx, an
//! unreadable answer and a success without an id are uncertain.

use super::client::{Answer, Lane, Request};
use super::mapping::{self, text};
use super::pricing::{self, Ask};
use super::PocketfulBroker;
use crate::brokers::common::book_read;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::outcome;
use crate::brokers::common::position_read::unread;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

const ORDERS: &str = "/api/v1/orders";
const TRADES: &str = "/api/v1/trades";
const POSITIONS: &str = "/api/v1/positions";
const HOLDINGS: &str = "/api/v1/holdings";
/// fetch-position-netwise.md (the SDK's `api.portfolio.positions.netwise`).
const POSITIONS_TYPE: &str = "historical";
/// The execution types the regular cancel takes (cancel-regular-order.md,
/// cancel-amo-order.md); CO and BO legs cancel elsewhere.
const CANCELLABLE: [&str; 2] = ["REGULAR", "AMO"];

/// The sentence a trader reads when Pocketful did not do what was asked
/// (web `_refusal`). `after_write`: the request changes something, so an
/// unreadable answer may mean it went through.
fn refusal_text(a: &Answer, subject: &str, after_write: bool) -> String {
    if a.session_refused() {
        return format!(
            "Pocketful did not accept your session for the {}. Log in to Pocketful again from OpenAlgo, then try again.",
            subject
        );
    }
    if a.status == StatusCode::TOO_MANY_REQUESTS {
        return format!(
            "Pocketful refused the {} because too many requests were sent in a short time. Wait a moment and try again.",
            subject
        );
    }
    if a.body.as_ref().is_none_or(|b| !b.is_object()) {
        return if after_write {
            format!(
                "Pocketful sent an answer to the {} that OpenAlgo could not read, so it may have gone through. Check the order book before trying again.",
                subject
            )
        } else {
            format!(
                "Pocketful sent an answer to the {} that OpenAlgo could not read. Try again in a moment.",
                subject
            )
        };
    }
    match a.message() {
        m if m.is_empty() => format!(
            "Pocketful refused the {} without giving a reason. Try again in a moment.",
            subject
        ),
        m => format!("Pocketful refused the {}: {}", subject, m),
    }
}

/// A read or write Pocketful answered without doing it, as an error: a
/// session refusal is a session error, everything else Pocketful's words.
pub(super) fn refused(a: &Answer, subject: &str, after_write: bool) -> AppError {
    tracing::warn!(
        status = a.status.as_u16(),
        "Pocketful refused the {}: {}",
        subject,
        a.message()
    );
    let text = refusal_text(a, subject, after_write);
    if a.session_refused() {
        AppError::Auth(text)
    } else {
        AppError::Broker(text)
    }
}

fn unrecognised(subject: &str) -> AppError {
    tracing::warn!("Pocketful sent the {} in an unknown shape", subject);
    AppError::Broker(format!(
        "Pocketful sent the {} in a form OpenAlgo does not recognise. Try again in a moment.",
        subject
    ))
}

/// The rows of a book answer (web `_list_of`): `data` itself (`key` None)
/// or `data[key]`. An empty object or list and a null list are an empty
/// book, as the web reads them; a success without `data` at all, or a list
/// of the wrong type, is a book that was not read (BR-03).
fn rows_of(a: &Answer, key: Option<&str>, subject: &str) -> Result<Vec<Value>> {
    let Some(data) = a.payload() else {
        return Err(unrecognised(subject));
    };
    let empty = |v: &Value| {
        v.is_null()
            || v.as_array().is_some_and(Vec::is_empty)
            || v.as_object().is_some_and(serde_json::Map::is_empty)
    };
    if empty(data) {
        return Ok(Vec::new());
    }
    let list = match key {
        None => data,
        Some(k) => match data.get(k) {
            Some(Value::Null) => return Ok(Vec::new()),
            Some(v) => v,
            None if data.is_object() => return Ok(Vec::new()),
            None => return Err(unrecognised(subject)),
        },
    };
    match list.as_array() {
        Some(rows) => Ok(rows.iter().filter(|r| r.is_object()).cloned().collect()),
        None => Err(unrecognised(subject)),
    }
}

/// GET a book and return its rows (web `_read_list`).
async fn read_list(
    b: &PocketfulBroker,
    auth: &AuthToken,
    req: Request,
    key: Option<&str>,
    subject: &str,
) -> Result<Vec<Value>> {
    let client_id = b.client_id(auth)?;
    let a = match b
        .send(Lane::Data, auth, req.query("client_id", client_id))
        .await
    {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Pocketful {} could not be read: {}", subject, e.code());
            return Err(match e {
                AppError::Busy(_) | AppError::Auth(_) => e,
                _ => AppError::Broker(format!(
                    "OpenAlgo could not reach Pocketful for the {}. Check your connection and try again.",
                    subject
                )),
            });
        }
    };
    if !a.ok() {
        return Err(refused(&a, subject, false));
    }
    rows_of(&a, key, subject)
}

async fn order_list(b: &PocketfulBroker, auth: &AuthToken, kind: &str) -> Result<Vec<Value>> {
    read_list(
        b,
        auth,
        Request::get(ORDERS).query("type", kind),
        Some("orders"),
        "order book",
    )
    .await
}

/// The netwise position rows as Pocketful sent them (no row dropped, none
/// judged): the decisions below and the funds' M2M read their own fields.
pub(crate) async fn read_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    read_list(
        b,
        auth,
        Request::get(POSITIONS).query("type", POSITIONS_TYPE),
        None,
        "position book",
    )
    .await
}

/// The order book: the pending and completed lists. A failure of either
/// half fails the book, so a missing pending list never reads as "no open
/// orders".
pub async fn get_order_book(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let pending = order_list(b, auth, "pending").await?;
    let completed = order_list(b, auth, "completed").await?;
    Ok(mapping::map_orders(&pending, &completed, b.resolver()))
}

/// Executed trades, under `data.orders` (trade-book.md).
pub async fn get_trade_book(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = read_list(b, auth, Request::get(TRADES), Some("orders"), "trade book").await?;
    Ok(mapping::map_trades(&rows, b.resolver()))
}

/// The position book. Every row must carry its net quantity: a row without
/// one hides a position and is never read as flat (BR-03).
pub async fn get_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let rows = read_positions(b, auth).await?;
    book_read::require_quantities(&rows, &["net_quantity"], || {
        book_read::unreadable("pocketful", book_read::Book::Positions)
    })?;
    Ok(mapping::map_positions(&rows, b.resolver()))
}

/// Demat holdings, under `data.holdings`.
pub async fn get_holdings(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let rows = read_list(
        b,
        auth,
        Request::get(HOLDINGS),
        Some("holdings"),
        "holdings",
    )
    .await?;
    Ok(mapping::map_holdings(&rows, b.resolver()))
}

/// What a placement answered: the order id, or why not (see the module
/// doc for the three outcomes).
fn placement(a: &Answer) -> Result<String> {
    if a.session_refused() {
        return Err(AppError::Auth(refusal_text(a, "order", true)));
    }
    if a.status == StatusCode::TOO_MANY_REQUESTS {
        return Err(outcome::busy(refusal_text(a, "order", true)));
    }
    if a.ok() {
        let id = a
            .payload()
            .map(|d| text(d, "oms_order_id"))
            .unwrap_or_default();
        if id.is_empty() {
            tracing::warn!("Pocketful accepted an order without an order id");
            return Err(AppError::uncertain(outcome::UNCERTAIN_MESSAGE, None));
        }
        return Ok(id);
    }
    tracing::warn!(
        status = a.status.as_u16(),
        "Pocketful did not accept an order: {}",
        a.message()
    );
    let readable = a.body.as_ref().is_some_and(Value::is_object);
    Err(outcome::not_accepted(
        a.status.as_u16(),
        readable && a.failed(),
        refusal_text(a, "order", true),
    ))
}

/// Place a regular order (web `place_order_api`). MARKET and SL-M are
/// priced first; one that cannot be priced is refused before sending.
pub async fn place_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth)?;
    let priced = pricing::price(b, auth, &Ask::order(o)).await?;
    let body = mapping::place_body(o, &client_id, &priced);
    let a = b
        .send(
            Lane::Orders,
            auth,
            Request::new(Method::POST, ORDERS).json(body),
        )
        .await?;
    Ok(OrderResponse {
        order_id: placement(&a)?,
        message: None,
    })
}

/// A write whose request failed in transport: never sent when the
/// connection did not open, otherwise it may have gone through (web
/// `modify_order` / `cancel_order` handlers).
fn lost(e: AppError, not_sent: &str, may_have: &str) -> AppError {
    match e {
        AppError::Http(h) => {
            tracing::warn!("A Pocketful write failed in transport: {}", h);
            if outcome::after_send(&h) {
                AppError::Broker(may_have.into())
            } else {
                AppError::Broker(not_sent.into())
            }
        }
        other => other,
    }
}

/// Modify an order: `PUT /api/v1/orders` with the place body and its
/// `oms_order_id`, priced like a new order (web `modify_order`).
pub async fn modify_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth)?;
    let priced = pricing::price(b, auth, &Ask::modify(m)).await?;
    let body = mapping::modify_body(m, &client_id, &priced);
    let a = b
        .send(
            Lane::Orders,
            auth,
            Request::new(Method::PUT, ORDERS).json(body),
        )
        .await
        .map_err(|e| {
            lost(
                e,
                "OpenAlgo could not reach Pocketful, so the change was not sent. Check your connection and try again.",
                "The connection to Pocketful dropped after the change was sent, so it may have gone through. Check the order book.",
            )
        })?;
    if !a.ok() {
        return Err(refused(&a, "change to the order", true));
    }
    let id = a
        .payload()
        .map(|d| text(d, "oms_order_id"))
        .unwrap_or_default();
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            m.order_id.clone()
        } else {
            id
        },
        message: None,
    })
}

/// Cancel one order with its execution type (web `cancel_order`). The docs
/// send the fields as a JSON body, the SDK as query parameters: both are
/// sent. The order id is encoded, so it cannot leave the orders path.
async fn cancel_with(
    b: &PocketfulBroker,
    auth: &AuthToken,
    order_id: &str,
    execution_type: &str,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth)?;
    let req = Request::new(
        Method::DELETE,
        format!("{}/{}", ORDERS, urlencoding::encode(order_id)),
    )
    .query("client_id", client_id.clone())
    .query("execution_type", execution_type)
    .json(json!({
        "oms_order_id": order_id,
        "client_id": client_id,
        "execution_type": execution_type,
    }));
    let a = b.send(Lane::Orders, auth, req).await.map_err(|e| {
        lost(
            e,
            "OpenAlgo could not reach Pocketful, so the order was not cancelled. Check your connection and try again.",
            "The connection to Pocketful dropped after the cancel was sent, so it may have gone through. Check the order book.",
        )
    })?;
    if !a.ok() {
        return Err(refused(&a, "cancel", true));
    }
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

pub async fn cancel_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    cancel_with(b, auth, order_id, "REGULAR").await
}

/// Cancel every order in the pending list (web `cancel_all_orders_api`). A
/// list that cannot be read fails the whole request; an order the regular
/// cancel does not take (a CO or BO leg) is counted as failed.
pub async fn cancel_all_orders(b: &PocketfulBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for order in order_list(b, auth, "pending").await? {
        let id = text(&order, "oms_order_id");
        if id.is_empty() {
            tracing::warn!("Cancel all: a pending Pocketful order has no order id");
            result.failed.push("unknown".into());
            continue;
        }
        let execution_type = match text(&order, "execution_type").to_ascii_uppercase() {
            t if t.is_empty() => "REGULAR".to_string(),
            t => t,
        };
        if !CANCELLABLE.contains(&execution_type.as_str()) {
            tracing::warn!(
                "Cancel all: {} is a {} order, not cancelled",
                id,
                execution_type
            );
            result.failed.push(id);
            continue;
        }
        match cancel_with(b, auth, &id, &execution_type).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel all: {} failed: {}", id, e.code());
                result.failed.push(id);
            }
        }
    }
    Ok(result)
}

/// A position row's net quantity, `None` when it cannot be read.
fn net_quantity(p: &Value) -> Option<i64> {
    book_read::quantity(p, "net_quantity")
}

/// Whether a position row is the instrument: its token, or its trading
/// symbol, is the master's (web `_same_instrument`).
fn same_instrument(p: &Value, brsymbol: &str, token: &str) -> bool {
    let row_token = match text(p, "instrument_token") {
        t if t.is_empty() => text(p, "token"),
        t => t,
    };
    (!token.is_empty() && row_token == token)
        || (!brsymbol.is_empty() && text(p, "trading_symbol") == brsymbol)
}

/// Net quantity of one OpenAlgo symbol on one exchange and product (web
/// `get_open_position`). A book that cannot be read, or a matching row
/// without a readable net quantity, refuses: a position OpenAlgo cannot
/// read never sizes an order as flat.
pub async fn get_open_position(
    b: &PocketfulBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let row = b.resolver().by_symbol(ex, symbol);
    let brsymbol = row.as_ref().map(|r| r.brsymbol.clone()).unwrap_or_default();
    let token = row.map(|r| r.token).unwrap_or_default();
    let rows = read_positions(b, auth).await.map_err(|e| {
        tracing::warn!("Pocketful positions not read for a decision: {}", e.code());
        unread("Pocketful")
    })?;
    for p in &rows {
        if !text(p, "exchange").eq_ignore_ascii_case(ex) || !same_instrument(p, &brsymbol, &token) {
            continue;
        }
        if mapping::oa_product(&text(p, "product")) != product.as_str() {
            continue;
        }
        return net_quantity(p).ok_or_else(|| {
            tracing::warn!(
                "A Pocketful position row for {} {} has no readable net quantity",
                ex,
                symbol
            );
            unread("Pocketful")
        });
    }
    Ok(0)
}

/// The square-off order for a position row, or why none can be sent.
fn square_off_request(
    p: &Value,
    symbol: String,
    net: i64,
) -> std::result::Result<OrderRequest, String> {
    let raw = text(p, "product");
    let Some(product) = mapping::openalgo_product(&raw) else {
        return Err(format!(
            "it is held under the Pocketful product {}, which OpenAlgo does not close. Close it at Pocketful.",
            raw
        ));
    };
    Ok(OrderRequest {
        symbol,
        exchange: text(p, "exchange"),
        side: if net > 0 { "SELL" } else { "BUY" }.into(),
        quantity: i32::try_from(net.unsigned_abs()).unwrap_or(i32::MAX),
        price: 0.0,
        order_type: "MARKET".into(),
        product: product.into(),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    })
}

/// Square off every open net position, each with a protected LIMIT order
/// (web `close_all_positions`). A position that cannot be read, priced or
/// closed is reported and the others are still closed, so the trader is
/// never told everything was closed when it was not.
pub async fn close_all_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let rows = read_positions(b, auth).await.map_err(|e| {
        tracing::warn!("Close all: Pocketful positions not read: {}", e.code());
        unread("Pocketful")
    })?;
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in &rows {
        let net = net_quantity(p);
        if net == Some(0) {
            continue;
        }
        let exchange = text(p, "exchange");
        let symbol = mapping::row_symbol(
            &symbols,
            &text(p, "trading_symbol"),
            &text(p, "instrument_token"),
            &exchange,
        );
        let label = format!("{} ({})", symbol, exchange);
        let Some(net) = net else {
            result.failed.push(format!(
                "{}: OpenAlgo could not read its quantity from Pocketful. Close it at Pocketful.",
                label
            ));
            continue;
        };
        let req = match square_off_request(p, symbol, net) {
            Ok(r) => r,
            Err(reason) => {
                result.failed.push(format!("{}: {}", label, reason));
                continue;
            }
        };
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => outcome::placing(place_order(b, auth, &o)).await,
            Err(e) => Err(e),
        };
        result.record(&label, placed);
    }
    Ok(result)
}
