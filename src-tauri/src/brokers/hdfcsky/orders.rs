//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::data;
use super::mapping::{self, s, to_oa_exchange, unwrap_rows};
use super::{broker_error, message_of, HdfcSkyBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::common::streaming::now_ms;
use crate::brokers::types::*;
use crate::error::Result;
use reqwest::{Method, StatusCode};
use serde_json::Value;

/// The order type and price actually sent: merchant keys may only place
/// LIMIT orders, so MARKET becomes LIMIT off the live LTP and SL-M becomes
/// SL off the trigger, each with the MPP buffer. Without a reference price
/// the original type goes out so the broker's own refusal surfaces.
pub(crate) async fn protected_order(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<(&'static str, f64)> {
    let limit_type = match o.pricetype {
        PriceType::Market => "LIMIT",
        PriceType::SlM => "SL",
        other => return Ok((mapping::order_type(other), o.price)),
    };
    let base = if o.pricetype == PriceType::SlM {
        o.trigger_price
    } else {
        data::ltp_of_row(b, auth, &o.instrument).await?.0
    };
    if base <= 0.0 {
        tracing::warn!(
            "No reference price for {}:{}; sending the order unprotected",
            o.exchange,
            o.symbol
        );
        return Ok((mapping::order_type(o.pricetype), o.price));
    }
    let tick = (o.instrument.tick_size > 0.0).then_some(o.instrument.tick_size);
    let price = protected_price(base, o.action, instrument_type_from_symbol(&o.symbol), tick);
    Ok((limit_type, price))
}

pub async fn place_order(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let (order_type, price) = protected_order(b, auth, o).await?;
    let sess = HdfcSkyBroker::session(auth)?;
    let body = mapping::place_body(
        o,
        &sess.client_id,
        order_type,
        price,
        mapping::ORDER_IDS.next(now_ms()),
    );
    let (status, v) = b
        .send(
            Method::POST,
            "/oapi/v1/orders",
            auth,
            &[],
            false,
            Some(&body),
        )
        .await?;
    if v.get("status").and_then(Value::as_str) == Some("success") {
        let id = s(v.get("data").unwrap_or(&Value::Null), "oms_order_id");
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    // HDFC Sky can refuse with HTTP 200; that is still a refusal.
    let msg = message_of(&v);
    tracing::warn!(
        status = status.as_u16(),
        "HDFC Sky refused the order: {}",
        msg
    );
    Err(broker_error(&msg))
}

pub async fn modify_order(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let sess = HdfcSkyBroker::session(auth)?;
    let body = mapping::modify_body(m, &sess.client_id);
    let v = b
        .call(
            Method::PUT,
            "/oapi/v1/orders",
            auth,
            &[],
            false,
            Some(&body),
        )
        .await?;
    let id = s(v.get("data").unwrap_or(&Value::Null), "oms_order_id");
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
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    b.call(
        Method::DELETE,
        &format!("/oapi/v1/orders/{}", urlencoding::encode(order_id)),
        auth,
        &[("execution_type", "REGULAR".to_string())],
        true,
        None,
    )
    .await?;
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

async fn orders_of(b: &HdfcSkyBroker, auth: &AuthToken, kind: &str) -> Result<Vec<Value>> {
    let v = b
        .call(
            Method::GET,
            "/oapi/v1/orders",
            auth,
            &[("type", kind.to_string())],
            true,
            None,
        )
        .await?;
    Ok(unwrap_rows(&v, "orders"))
}

/// Pending and completed books merged (one failing half is skipped, as on
/// the web; both failing is reported).
pub(crate) async fn raw_orders(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut first_err = None;
    let mut ok = 0;
    for kind in ["pending", "completed"] {
        match orders_of(b, auth, kind).await {
            Ok(r) => {
                ok += 1;
                rows.extend(r)
            }
            Err(e) => {
                tracing::warn!("HDFC Sky {} order book unavailable: {}", kind, e.code());
                if matches!(e, crate::error::AppError::Auth(_)) {
                    return Err(e);
                }
                first_err.get_or_insert(e);
            }
        }
    }
    match (ok, first_err) {
        (0, Some(e)) => Err(e),
        _ => Ok(rows),
    }
}

pub(crate) async fn raw_positions(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let v = b
        .call(
            Method::GET,
            "/oapi/v1/positions",
            auth,
            &[("type", "historical".to_string())],
            true,
            None,
        )
        .await?;
    Ok(unwrap_rows(&v, "positions"))
}

pub async fn cancel_all_orders(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in orders_of(b, auth, "pending").await? {
        if !mapping::is_cancellable(&s(&o, "order_status")) {
            continue;
        }
        let id = s(&o, "oms_order_id");
        match cancel_order(b, auth, &id).await {
            Ok(_) => result.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", id, e.code());
                result.failed.push(id)
            }
        }
    }
    Ok(result)
}

pub async fn get_open_position(
    b: &HdfcSkyBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let br = b
        .resolver()
        .br_symbol(symbol, ex)
        .unwrap_or_else(|| symbol.to_string());
    for p in raw_positions(b, auth).await? {
        if s(&p, "trading_symbol") == br
            && to_oa_exchange(&s(&p, "exchange")) == ex
            && s(&p, "product").eq_ignore_ascii_case(product.as_str())
        {
            return Ok(mapping::i(&p, "net_quantity"));
        }
    }
    Ok(0)
}

pub async fn close_all_positions(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        let net = mapping::i(&p, "net_quantity");
        if net == 0 {
            continue;
        }
        let exchange = to_oa_exchange(&s(&p, "exchange"));
        let br = s(&p, "trading_symbol");
        let symbol = symbols.oa_symbol_or_raw(&br, &exchange);
        let label = format!("{} ({})", symbol, exchange);
        let product = mapping::reverse_product(&s(&p, "product")).unwrap_or("MIS");
        let req = OrderRequest {
            symbol,
            exchange,
            side: if net > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(net.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: product.into(),
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

pub async fn get_order_book(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        &raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = b
        .call(Method::GET, "/oapi/v1/trades", auth, &[], true, None)
        .await?;
    Ok(mapping::map_trades(
        &unwrap_rows(&v, "trades"),
        b.resolver(),
    ))
}

pub async fn get_positions(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        &raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &HdfcSkyBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let (status, v) = b
        .send(Method::GET, "/oapi/v1/holdings", auth, &[], true, None)
        .await?;
    // An empty demat account answers with an error envelope on some days;
    // only a real refusal is an error.
    if v.get("status").and_then(Value::as_str) != Some("success") && status != StatusCode::OK {
        return Err(broker_error(&message_of(&v)));
    }
    Ok(mapping::map_holdings(
        &unwrap_rows(&v, "holdings"),
        b.resolver(),
    ))
}
