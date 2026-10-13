//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, DhanHolding, DhanOrder, DhanPosition, DhanTrade};
use super::{session_expired, Category, DhanBroker, DhanSession};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::brokers::Broker;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::collections::HashMap;

fn order_id_of(v: &Value) -> Option<String> {
    match v.get("orderId") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn refused(v: &Value, what: &str) -> AppError {
    super::dhan_error(v).unwrap_or_else(|| AppError::Broker(format!("Dhan did not {}.", what)))
}

pub async fn place_order(
    b: &DhanBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = DhanSession::parse(auth)?;
    let cid = s.require_client_id()?.to_string();
    let body = mapping::place_order_body(o, &cid, !b.is_sandbox())?;
    let (status, v) = b
        .send(
            Method::POST,
            "/v2/orders",
            &s,
            Some(&body),
            Some(&cid),
            Category::Trade,
        )
        .await?;
    // web: HTTP 200/201 with `orderId`.
    if matches!(status, StatusCode::OK | StatusCode::CREATED) {
        if let Some(id) = order_id_of(&v) {
            return Ok(OrderResponse {
                order_id: id,
                message: None,
            });
        }
        // Accepted without an id: the order may exist (LOG-08).
        tracing::warn!(
            status = status.as_u16(),
            "Dhan accepted an order without an id"
        );
        return Err(AppError::uncertain(
            "Dhan accepted the order but returned no order id. Check the order book before placing it again.",
            None,
        ));
    }
    tracing::warn!(status = status.as_u16(), "Dhan refused an order");
    // Only Dhan's error body is a refusal (LOG-08); a 2xx such as 202
    // without one, and a 5xx, leave the order unknown.
    let failed = super::dhan_error(&v).is_some();
    Err(crate::brokers::common::outcome::not_accepted_err(
        status.as_u16(),
        failed,
        refused(&v, "accept the order"),
    ))
}

pub async fn modify_order(
    b: &DhanBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = DhanSession::parse(auth)?;
    let cid = s.require_client_id()?.to_string();
    let body = mapping::modify_order_body(m, &cid, !b.is_sandbox())?;
    let (status, v) = b
        .send(
            Method::PUT,
            &format!("/v2/orders/{}", urlencoding::encode(&m.order_id)),
            &s,
            Some(&body),
            None,
            Category::Trade,
        )
        .await?;
    match order_id_of(&v) {
        Some(id) if super::dhan_error(&v).is_none() => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        _ => {
            tracing::warn!(status = status.as_u16(), "Dhan refused an order modify");
            Err(refused(&v, "modify the order"))
        }
    }
}

pub async fn cancel_order(
    b: &DhanBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = DhanSession::parse(auth)?;
    let (status, v) = b
        .send(
            Method::DELETE,
            &format!("/v2/orders/{}", urlencoding::encode(order_id)),
            &s,
            None,
            None,
            Category::Trade,
        )
        .await?;
    if let Some(e) = super::dhan_error(&v) {
        tracing::warn!(status = status.as_u16(), "Dhan refused a cancel");
        return Err(e);
    }
    // web: any non-empty body is a success.
    let empty = match &v {
        Value::Null => true,
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    };
    if empty || !status.is_success() {
        return Err(AppError::Broker("Dhan did not cancel the order.".into()));
    }
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

async fn get_rows<T: serde::de::DeserializeOwned>(
    b: &DhanBroker,
    s: &DhanSession,
    path: &str,
) -> Result<Vec<T>> {
    let (status, v) = b
        .send(Method::GET, path, s, None, None, Category::Trade)
        .await?;
    if let Some(e) = super::dhan_error(&v) {
        tracing::warn!(status = status.as_u16(), "Dhan refused {}", path);
        return Err(e);
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(session_expired());
    }
    mapping::rows(v)
}

pub(crate) async fn raw_orders(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<DhanOrder>> {
    let s = DhanSession::parse(auth)?;
    get_rows(b, &s, "/v2/orders").await
}

pub(crate) async fn raw_positions(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<DhanPosition>> {
    let s = DhanSession::parse(auth)?;
    get_rows(b, &s, "/v2/positions").await
}

pub async fn get_order_book(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let s = DhanSession::parse(auth)?;
    let rows: Vec<DhanTrade> = get_rows(b, &s, "/v2/trades").await?;
    Ok(mapping::map_trades(rows, b.resolver()))
}

/// LTPs for book rows through the multiquote path (web fetches them through
/// the quotes service because `/positions` and `/holdings` carry none).
/// Best effort: a failure leaves the LTPs at zero.
async fn ltp_map(b: &DhanBroker, auth: &AuthToken, keys: Vec<QuoteKey>) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    if keys.is_empty() {
        return out;
    }
    match b.get_multiquotes(auth, &keys).await {
        Ok(results) => {
            for r in results {
                if let Some(q) = r.data {
                    out.insert(mapping::ltp_key(&r.exchange, &r.symbol), q.ltp);
                }
            }
        }
        Err(e) => tracing::warn!("Dhan LTP backfill failed: {}", e.code()),
    }
    out
}

pub async fn get_positions(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let rows = raw_positions(b, auth).await?;
    let mut keys: Vec<QuoteKey> = rows
        .iter()
        .map(|p| {
            let ex = mapping::map_exchange(&p.exchange_segment);
            let sym = mapping::resolve_symbol(b.resolver(), &p.security_id, &ex, &p.trading_symbol);
            QuoteKey::new(ex, sym)
        })
        .collect();
    keys.sort_by(|a, c| (&a.exchange, &a.symbol).cmp(&(&c.exchange, &c.symbol)));
    keys.dedup();
    let ltp = ltp_map(b, auth, keys).await;
    Ok(mapping::map_positions(rows, b.resolver(), &ltp))
}

pub async fn get_holdings(b: &DhanBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let s = DhanSession::parse(auth)?;
    let (status, v) = b
        .send(Method::GET, "/v2/holdings", &s, None, None, Category::Trade)
        .await?;
    if mapping::is_no_holdings(&v) {
        return Ok(Vec::new());
    }
    if let Some(e) = super::dhan_error(&v) {
        tracing::warn!(status = status.as_u16(), "Dhan refused holdings");
        return Err(e);
    }
    let rows: Vec<DhanHolding> = mapping::rows(v)?;
    let mut keys: Vec<QuoteKey> = rows
        .iter()
        .map(|h| {
            let (ex, sym) = mapping::holding_listing(b.resolver(), h);
            QuoteKey::new(ex, sym)
        })
        .collect();
    keys.sort_by(|a, c| (&a.exchange, &a.symbol).cmp(&(&c.exchange, &c.symbol)));
    keys.dedup();
    let ltp = ltp_map(b, auth, keys).await;
    Ok(mapping::map_holdings(rows, b.resolver(), &ltp))
}

/// web `cancel_all_orders_api`: only `PENDING` orders are cancelled.
pub async fn cancel_all_orders(b: &DhanBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        if o.order_status != "PENDING" {
            continue;
        }
        match cancel_order(b, auth, &o.order_id).await {
            Ok(_) => result.cancelled.push(o.order_id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.order_id, e.code());
                result.failed.push(o.order_id)
            }
        }
    }
    Ok(result)
}

/// web `get_open_position`: match segment and product, then the security id
/// (the trading symbol when the master has no token).
pub async fn get_open_position(
    b: &DhanBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let row = b.resolver().by_symbol(ex, symbol);
    let segment = mapping::exchange_segment(ex).unwrap_or("");
    let want_product = mapping::product_type(product);
    for p in raw_positions(b, auth).await? {
        if p.exchange_segment != segment || p.product_type != want_product {
            continue;
        }
        let matched = match &row {
            Some(r) => p.security_id.trim() == r.token,
            None => p.trading_symbol == symbol,
        };
        if matched {
            return Ok(p.net_qty);
        }
    }
    Ok(0)
}

/// web `close_all_positions`: one MARKET order per open row, symbol from the
/// security id, product reverse-mapped.
pub async fn close_all_positions(b: &DhanBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.net_qty == 0 {
            continue;
        }
        let exchange = mapping::map_exchange(&p.exchange_segment);
        let symbol =
            mapping::resolve_symbol(&symbols, &p.security_id, &exchange, &p.trading_symbol);
        let label = format!("{} ({})", symbol, exchange);
        let req = OrderRequest {
            symbol,
            exchange: exchange.clone(),
            side: if p.net_qty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(p.net_qty.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: mapping::reverse_product(&p.product_type)
                .unwrap_or("")
                .to_string(),
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
            Ok(r) if !r.order_id.is_empty() => result.placed.push(r.order_id),
            Ok(_) => result.failed.push(format!("{}: order was refused", label)),
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
