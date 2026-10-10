//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{self, text, text_any};
use super::{data, PocketfulBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::Value;

fn q(client_id: &str) -> String {
    urlencoding::encode(client_id).into_owned()
}

fn order_id_from(v: &Value, fallback: &str) -> String {
    let id = text(&v["data"], "oms_order_id");
    if id.is_empty() {
        fallback.to_string()
    } else {
        id
    }
}

/// The order type and price `transform_data` sends: a MARKET order becomes
/// a LIMIT at the protected price (web `utils/mpp_slab`) when a live price
/// is available, and stays MARKET otherwise.
pub fn market_protection(o: &ResolvedOrder, ltp: Option<f64>) -> (&'static str, f64) {
    if o.pricetype != PriceType::Market {
        return (mapping::order_type(o.pricetype), o.price);
    }
    match ltp.filter(|p| *p > 0.0) {
        Some(ltp) => {
            let tick = (o.instrument.tick_size > 0.0).then_some(o.instrument.tick_size);
            let price =
                protected_price(ltp, o.action, instrument_type_from_symbol(&o.symbol), tick);
            ("LIMIT", price)
        }
        None => ("MARKET", o.price),
    }
}

pub async fn place_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth).await?;
    let ltp = if o.pricetype == PriceType::Market {
        let ltp = data::ltp(b, auth, &o.instrument).await;
        if ltp.is_none() {
            tracing::warn!(
                "No live price for {} {}; sending a plain market order",
                o.exchange,
                o.symbol
            );
        }
        ltp
    } else {
        None
    };
    let (order_type, price) = market_protection(o, ltp);
    let body = mapping::place_payload(o, &client_id, order_type, price);
    let v = b
        .call(Method::POST, "/api/v1/orders", auth, Some(&body))
        .await?;
    let id = order_id_from(&v, "");
    if id.is_empty() {
        return Err(AppError::uncertain(
            "Pocketful accepted the request but returned no order id. Check the order book before retrying.",
            None,
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth).await?;
    let body = mapping::modify_payload(m, &client_id);
    let v = b
        .call(Method::PUT, "/api/v1/orders", auth, Some(&body))
        .await?;
    Ok(OrderResponse {
        order_id: order_id_from(&v, ""),
        message: None,
    })
}

pub async fn cancel_order(
    b: &PocketfulBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let client_id = b.client_id(auth).await?;
    let v = b
        .call(
            Method::DELETE,
            &format!(
                "/api/v1/orders/{}?client_id={}",
                urlencoding::encode(order_id),
                q(&client_id)
            ),
            auth,
            None,
        )
        .await?;
    Ok(OrderResponse {
        order_id: order_id_from(&v, order_id),
        message: None,
    })
}

/// One half of the order book (`type=completed` or `type=pending`).
async fn fetch_orders(
    b: &PocketfulBroker,
    auth: &AuthToken,
    client_id: &str,
    kind: &str,
) -> Result<Vec<Value>> {
    let v = b
        .call(
            Method::GET,
            &format!("/api/v1/orders?client_id={}&type={}", q(client_id), kind),
            auth,
            None,
        )
        .await?;
    Ok(mapping::list_at(&v, "orders").to_vec())
}

/// web `get_order_book`: completed + pending, merged. A half that fails is
/// read as empty (web), unless both fail.
async fn raw_orders(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let client_id = b.client_id(auth).await?;
    let completed = fetch_orders(b, auth, &client_id, "completed").await;
    let pending = fetch_orders(b, auth, &client_id, "pending").await;
    match (completed, pending) {
        (Err(e), Err(_)) => Err(e),
        (c, p) => {
            let mut rows = c.unwrap_or_default();
            rows.extend(p.unwrap_or_default());
            Ok(rows)
        }
    }
}

async fn raw_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Value>> {
    let client_id = b.client_id(auth).await?;
    let v = b
        .call(
            Method::GET,
            &format!("/api/v1/positions?client_id={}&type=live", q(&client_id)),
            auth,
            None,
        )
        .await?;
    Ok(mapping::list_at(&v, "positions").to_vec())
}

pub async fn get_order_book(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        &raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let client_id = b.client_id(auth).await?;
    let v = b
        .call(
            Method::GET,
            &format!("/api/v1/trades?client_id={}", q(&client_id)),
            auth,
            None,
        )
        .await?;
    Ok(mapping::map_trades(
        mapping::list_at(&v, "trades"),
        b.resolver(),
    ))
}

pub async fn get_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        &raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &PocketfulBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let client_id = b.client_id(auth).await?;
    let v = b
        .call(
            Method::GET,
            &format!("/api/v1/holdings?client_id={}", q(&client_id)),
            auth,
            None,
        )
        .await?;
    Ok(mapping::map_holdings(
        mapping::list_at(&v, "holdings"),
        b.resolver(),
    ))
}

/// web `cancel_all_orders_api`: the pending book, every cancellable row.
pub async fn cancel_all_orders(b: &PocketfulBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let client_id = b.client_id(auth).await?;
    let pending = fetch_orders(b, auth, &client_id, "pending").await?;
    let mut result = CancelAllResult::default();
    for o in pending.iter().filter(|o| mapping::is_cancellable(o)) {
        let id = mapping::order_id_of(o);
        if id.is_empty() {
            result.failed.push("unknown_id".into());
            continue;
        }
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

/// web `get_open_position`: match the broker symbol, exchange and product;
/// `quantity` first, then `net_quantity`.
pub async fn get_open_position(
    b: &PocketfulBroker,
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
        let ps = text_any(&p, &["tradingsymbol", "trading_symbol"]);
        if ps == br && text(&p, "exchange") == ex && text(&p, "product") == product.as_str() {
            return Ok(mapping::num_any(&p, &["quantity", "net_quantity"]).unwrap_or(0.0) as i64);
        }
    }
    Ok(0)
}

/// web `close_all_positions`: one MARKET exit per non-zero `net_quantity`.
pub async fn close_all_positions(b: &PocketfulBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        let qty = mapping::position_qty(&p);
        if qty == 0 {
            continue;
        }
        let exchange = text(&p, "exchange");
        let br = text_any(&p, &["trading_symbol", "tradingsymbol"]);
        let symbol = if br.is_empty() || exchange.is_empty() {
            br.clone()
        } else {
            symbols.oa_symbol_or_raw(&br, &exchange)
        };
        let label = format!("{} ({})", symbol, exchange);
        let product = {
            let pr = mapping::oa_product(&text(&p, "product"));
            if pr.is_empty() {
                "MIS".to_string()
            } else {
                pr
            }
        };
        let req = OrderRequest {
            symbol,
            exchange: exchange.clone(),
            side: if qty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(qty.abs()).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product,
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
