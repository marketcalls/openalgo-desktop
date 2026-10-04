//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, body_rows, int, text};
use super::{session, FivepaisaBroker, Session};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const PLACE: &str = "/VendorsAPI/Service1.svc/V1/PlaceOrderRequest";
pub const MODIFY: &str = "/VendorsAPI/Service1.svc/V1/ModifyOrderRequest";
pub const CANCEL: &str = "/VendorsAPI/Service1.svc/V1/CancelOrderRequest";
pub const ORDER_BOOK: &str = "/VendorsAPI/Service1.svc/V3/OrderBook";
pub const TRADE_BOOK: &str = "/VendorsAPI/Service1.svc/V1/TradeBook";
pub const POSITIONS: &str = "/VendorsAPI/Service1.svc/V2/NetPositionNetWise";
pub const HOLDINGS: &str = "/VendorsAPI/Service1.svc/V3/Holding";

/// web: the positions endpoint needs a 60 s timeout and up to 3 attempts.
const POSITIONS_TIMEOUT: Duration = Duration::from_secs(60);
const POSITIONS_ATTEMPTS: u32 = 3;

fn client_body(s: &Session) -> Value {
    json!({"ClientCode": s.client_code})
}

pub(crate) async fn order_book_raw(b: &FivepaisaBroker, s: &Session) -> Result<Vec<Value>> {
    let v = b.post(ORDER_BOOK, client_body(s), s).await?;
    Ok(body_rows(&v, "OrderBookDetail"))
}

/// The net position rows; a timeout is retried, other failures surface.
pub(crate) async fn positions_raw(b: &FivepaisaBroker, s: &Session) -> Result<Vec<Value>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match b
            .post_with(POSITIONS, client_body(s), s, POSITIONS_TIMEOUT)
            .await
        {
            Ok(v) => return Ok(body_rows(&v, "NetPositionDetail")),
            Err(AppError::Http(e)) if e.is_timeout() && attempt < POSITIONS_ATTEMPTS => {
                tracing::debug!(broker = "fivepaisa", "Positions timed out, retrying");
            }
            Err(e) => return Err(e),
        }
    }
}

/// LIMIT price for a MARKET order: the LTP plus/minus the MPP slab, rounded
/// to the tick (5paisa refuses plain market orders from API keys). Falls
/// back to the request price when no LTP is available, like the web.
async fn market_price(b: &FivepaisaBroker, auth: &AuthToken, o: &ResolvedOrder) -> f64 {
    let key = QuoteKey::new(o.exchange.as_str(), o.symbol.clone());
    match super::data::get_quote(b, auth, &key).await {
        Ok(q) if q.ltp > 0.0 => {
            let tick = (o.instrument.tick_size > 0.0).then_some(o.instrument.tick_size);
            protected_price(
                q.ltp,
                o.action,
                instrument_type_from_symbol(&o.symbol),
                tick,
            )
        }
        Ok(_) => {
            tracing::warn!(
                broker = "fivepaisa",
                "No LTP for {} on {}; MARKET order sent at the request price",
                o.symbol,
                o.exchange
            );
            o.price
        }
        Err(e) => {
            tracing::warn!(
                broker = "fivepaisa",
                "Quote for MARKET protection failed ({}); sent at the request price",
                e.code()
            );
            o.price
        }
    }
}

/// The price 5paisa receives for this order.
pub(crate) async fn order_price(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<f64> {
    match o.pricetype {
        PriceType::Market => Ok(market_price(b, auth, o).await),
        PriceType::SlM if o.trigger_price > 0.0 => mapping::slm_protected_price(
            &o.symbol,
            o.action,
            o.trigger_price,
            o.instrument.tick_size,
        ),
        _ => Ok(o.price),
    }
}

pub async fn place_order(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    let price = order_price(b, auth, o).await?;
    let body = mapping::place_body(o, price);
    let v = b.post(PLACE, body, &s).await?;
    match mapping::placed_order_id(&v) {
        Ok(id) => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        Err(reason) => {
            tracing::warn!(
                broker = "fivepaisa",
                "5paisa rejected the order: {}",
                reason
            );
            Err(AppError::Broker(reason))
        }
    }
}

pub async fn modify_order(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    let book = order_book_raw(b, &s).await?;
    let Some(row) = book.iter().find(|r| text(r, "BrokerOrderId") == m.order_id) else {
        return Err(AppError::NotFound(format!(
            "Order {} not found in order book",
            m.order_id
        )));
    };
    let exch_id = text(row, "ExchOrderID");
    if exch_id.is_empty() {
        return Err(AppError::Broker(
            "Exchange Order ID not found for this order".into(),
        ));
    }
    let v = b
        .post(MODIFY, mapping::modify_body(m, &exch_id), &s)
        .await?;
    let head_status = v.get("head").map(|h| text(h, "status")).unwrap_or_default();
    if head_status == "0" {
        let id = v
            .get("body")
            .map(|bd| text(bd, "BrokerOrderID"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| m.order_id.clone());
        Ok(OrderResponse {
            order_id: id,
            message: None,
        })
    } else {
        let msg = v
            .get("head")
            .map(|h| text(h, "statusDescription"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| "Failed to modify order".into());
        Err(AppError::Broker(msg))
    }
}

pub async fn cancel_order(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = session(auth)?;
    let book = order_book_raw(b, &s).await?;
    cancel_in_book(b, &s, &book, order_id).await
}

async fn cancel_in_book(
    b: &FivepaisaBroker,
    s: &Session,
    book: &[Value],
    order_id: &str,
) -> Result<OrderResponse> {
    let Some(row) = book
        .iter()
        .find(|r| text(r, "ExchOrderID") == order_id || text(r, "BrokerOrderId") == order_id)
    else {
        return Err(AppError::NotFound(format!("Order not found: {}", order_id)));
    };
    let exch_id = text(row, "ExchOrderID");
    if text(row, "OrderStatus") == "Pending" && exch_id.is_empty() {
        return Err(AppError::Broker(
            "Order is still pending at broker level. Cannot cancel until it reaches exchange."
                .into(),
        ));
    }
    let v = b.post(CANCEL, json!({"ExchOrderID": exch_id}), s).await?;
    if super::head_success(&v) {
        Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: Some("Order cancelled successfully".into()),
        })
    } else {
        let m = v
            .get("body")
            .map(|bd| text(bd, "Message"))
            .filter(|x| !x.is_empty())
            .unwrap_or_else(|| "Failed to cancel order".into());
        Err(AppError::Broker(m))
    }
}

/// web `cancel_all_orders_api`: rows whose raw status is `Pending` or
/// `Modified`, cancelled by broker order id.
pub async fn cancel_all_orders(b: &FivepaisaBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let s = session(auth)?;
    let book = order_book_raw(b, &s).await?;
    let mut out = CancelAllResult::default();
    for row in book
        .iter()
        .filter(|r| matches!(text(r, "OrderStatus").as_str(), "Pending" | "Modified"))
    {
        let id = text(row, "BrokerOrderId");
        match cancel_in_book(b, &s, &book, &id).await {
            Ok(_) => out.cancelled.push(id),
            Err(e) => {
                tracing::warn!(
                    broker = "fivepaisa",
                    "Cancel of {} failed: {}",
                    id,
                    e.code()
                );
                out.failed.push(id);
            }
        }
    }
    Ok(out)
}

/// web `close_all_positions`: a MARKET order against each non-zero row.
pub async fn close_all_positions(b: &FivepaisaBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let s = session(auth)?;
    let rows = positions_raw(b, &s).await?;
    let mut out = CloseAllResult::default();
    for row in rows {
        let net = int(&row, "NetQty");
        if net == 0 {
            continue;
        }
        let Some(exchange) =
            mapping::reverse_exchange(&text(&row, "Exch"), &text(&row, "ExchType"))
        else {
            continue;
        };
        let token = text(&row, "ScripCode");
        let label_symbol = b
            .resolver()
            .by_token(exchange, &token)
            .map(|r| r.symbol)
            .unwrap_or_else(|| text(&row, "ScripName"));
        let label = format!("{} ({})", label_symbol, exchange);
        let req = OrderRequest {
            symbol: label_symbol.clone(),
            exchange: exchange.to_string(),
            side: if net > 0 { "SELL" } else { "BUY" }.into(),
            quantity: net.unsigned_abs() as i32,
            price: 0.0,
            order_type: "MARKET".into(),
            product: mapping::reverse_product(&text(&row, "OrderFor"), exchange),
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
            Ok(r) => out.placed.push(r.order_id),
            Err(e) => out
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(out)
}

/// web `get_open_position`: match scrip code, exchange pair and the
/// `OrderFor` product code. A failed read is an error, not "flat".
pub async fn get_open_position(
    b: &FivepaisaBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let s = session(auth)?;
    let ex = exchange.as_str();
    let token = b
        .resolver()
        .token(symbol, ex)
        .ok_or_else(|| mapping::unknown_symbol(symbol, ex))?;
    let rows = positions_raw(b, &s).await?;
    let (ec, et, pc) = (
        mapping::exch_code(ex),
        mapping::exch_type(ex),
        mapping::product_code(product),
    );
    Ok(rows
        .iter()
        .find(|r| {
            text(r, "ScripCode") == token
                && text(r, "Exch") == ec
                && text(r, "ExchType") == et
                && text(r, "OrderFor") == pc
        })
        .map(|r| int(r, "NetQty"))
        .unwrap_or(0))
}

pub async fn get_order_book(b: &FivepaisaBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let s = session(auth)?;
    let rows = order_book_raw(b, &s).await?;
    Ok(rows
        .iter()
        .map(|r| mapping::to_order(b.resolver(), r))
        .collect())
}

pub async fn get_trade_book(b: &FivepaisaBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let s = session(auth)?;
    let v = b.post(TRADE_BOOK, client_body(&s), &s).await?;
    Ok(body_rows(&v, "TradeBookDetail")
        .iter()
        .map(|r| mapping::to_trade(b.resolver(), r))
        .collect())
}

pub async fn get_positions(b: &FivepaisaBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let s = session(auth)?;
    Ok(positions_raw(b, &s)
        .await?
        .iter()
        .map(|r| mapping::to_position(b.resolver(), r))
        .collect())
}

pub async fn get_holdings(b: &FivepaisaBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let s = session(auth)?;
    let v = b.post(HOLDINGS, client_body(&s), &s).await?;
    Ok(body_rows(&v, "Data")
        .iter()
        .map(|r| mapping::to_holding(b.resolver(), r))
        .collect())
}
