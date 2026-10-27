//! What the order tools read before the guard runs (the web's
//! `_last_traded_price`, `_available_funds`, `_analyzer_mode` and
//! `_open_position_quantity`), through the same in-process `/api/v1` as
//! every other tool.
//!
//! The guard does no I/O, so the numbers it compares against are fetched
//! here. The price and funds reads fail open: any failure answers `None`
//! and the guard skips those checks with a warning, because refusing an
//! order the trader approved over a quote that did not arrive is the worse
//! failure, and the broker runs its own checks. The analyzer read answers
//! `None` when it cannot be read, which the guard refuses when the trader
//! requires analyzer mode.

use super::AGENT_STRATEGY;
use crate::agent::contract::tool::ToolCtx;
use crate::agent::safety::risk::num::{py_decimal, py_str};
use crate::agent::safety::risk::ExistingOrder;
use crate::agent::tools::service::{unwrap, SERVICE_TIMEOUT};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

/// Where a quote may carry the last traded price, in order.
const LTP_KEYS: [&str; 4] = ["ltp", "last_price", "lastPrice", "last_traded_price"];

/// Where a funds reply may carry spendable cash, in order: a broker that
/// reports both cash and a wider limit is read as its cash.
const FUNDS_KEYS: [&str; 5] = [
    "availablecash",
    "available_cash",
    "availablemargin",
    "available_margin",
    "cash",
];

/// The reply's `data` object, or the reply itself.
fn source(body: &Value) -> &Value {
    match body.get("data") {
        Some(d @ Value::Object(_)) => d,
        _ => body,
    }
}

async fn read(ctx: &ToolCtx, label: &str, endpoint: &str, payload: Value) -> Option<Value> {
    let payload: Map<String, Value> = payload.as_object().cloned().unwrap_or_default();
    match unwrap(
        label,
        ctx.api_post(endpoint, payload, SERVICE_TIMEOUT).await,
    ) {
        Ok(body) if body.is_object() => Some(body),
        Ok(_) | Err(_) => None,
    }
}

/// The last traded price, as the quote carried it (so the guard prints it
/// as the web does), or `None` when there is no usable one.
pub(super) async fn last_traded_price(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
) -> Option<Value> {
    let Some(body) = read(
        ctx,
        "quotes_service.get_quotes",
        "quotes",
        json!({"symbol": symbol, "exchange": exchange}),
    )
    .await
    else {
        tracing::warn!(
            "Agent order tools got no usable quote for {} on {}; the guard's pricing checks will be skipped",
            symbol,
            exchange
        );
        return None;
    };
    let found = LTP_KEYS
        .iter()
        .filter_map(|k| source(&body).get(*k))
        .find(|v| py_decimal(v).is_some_and(|d| d > Decimal::ZERO))
        .cloned();
    if found.is_none() {
        tracing::warn!(
            "Agent order tools found no last traded price in the quote for {} on {}",
            symbol,
            exchange
        );
    }
    found
}

/// Spendable cash, or `None` when it could not be read.
pub(super) async fn available_funds(ctx: &ToolCtx) -> Option<Value> {
    let Some(body) = read(ctx, "funds_service.get_funds", "funds", json!({})).await else {
        tracing::warn!("Agent order tools got no usable funds reply; the guard's affordability check will be skipped");
        return None;
    };
    let found = FUNDS_KEYS
        .iter()
        .filter_map(|k| source(&body).get(*k))
        .find(|v| py_decimal(v).is_some())
        .cloned();
    if found.is_none() {
        tracing::warn!("Agent order tools found no cash figure in the funds reply");
    }
    found
}

/// The platform's analyzer toggle now (the trader may have moved it since
/// the run started), or `None` when it cannot be read.
pub(super) fn analyzer_mode(ctx: &ToolCtx) -> Option<bool> {
    match ctx.app.sqlite.get_analyze_mode() {
        Ok(on) => Some(on),
        Err(e) => {
            tracing::error!(
                "Agent order tools could not read the platform analyzer toggle: {}",
                e
            );
            None
        }
    }
}

/// The signed quantity held in one symbol and product, or `None` when it
/// could not be read. `None` means "send the square-off and let the broker
/// decide": the trader asked for the position to be closed, and a failed
/// read is not a reason to leave it open.
pub(super) async fn open_position_quantity(
    ctx: &ToolCtx,
    symbol: &str,
    exchange: &str,
    product: &str,
) -> Option<i64> {
    let body = read(
        ctx,
        "openposition_service.get_open_position",
        "openposition",
        json!({"strategy": AGENT_STRATEGY, "symbol": symbol, "exchange": exchange, "product": product}),
    )
    .await?;
    py_decimal(body.get("quantity")?)?.trunc().to_i64()
}

/// What the order book says about one order.
pub(super) enum Lookup {
    Found(ExistingOrder),
    NotFound,
    /// The book could not be read, or the order's row is incomplete.
    Unreadable,
}

/// The order a modification changes, from the order book (the sandbox's
/// in analyzer mode), never from the model's arguments.
pub(super) async fn order_record(ctx: &ToolCtx, order_id: &str) -> Lookup {
    let Some(body) = read(
        ctx,
        "orderbook_service.get_orderbook",
        "orderbook",
        json!({}),
    )
    .await
    else {
        tracing::warn!("Agent order tools could not read the order book to check a modification");
        return Lookup::Unreadable;
    };
    let Some(rows) = source(&body).get("orders").and_then(Value::as_array) else {
        return Lookup::Unreadable;
    };
    let Some(row) = rows
        .iter()
        .find(|r| r.get("orderid").map(|v| py_str(v).trim() == order_id) == Some(true))
    else {
        return Lookup::NotFound;
    };
    let field = |k: &str| {
        row.get(k)
            .map(|v| py_str(v).trim().to_string())
            .filter(|t| !t.is_empty())
    };
    match (
        field("symbol"),
        field("exchange"),
        field("action"),
        field("product"),
    ) {
        (Some(symbol), Some(exchange), Some(action), Some(product)) => {
            Lookup::Found(ExistingOrder {
                orderid: order_id.to_string(),
                symbol,
                exchange,
                action,
                product,
            })
        }
        _ => Lookup::Unreadable,
    }
}
