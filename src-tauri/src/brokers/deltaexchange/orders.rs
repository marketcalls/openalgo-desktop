//! Orders, books and leverage (web `api/order_api.py`).

use super::mapping::{self, DeltaFill, DeltaOrder, DeltaPosition, RawPosition, WalletBalance};
use super::DeltaBroker;
use crate::brokers::common::mapping::{Action, Exchange, PriceType, Product, Validity};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Deserialize)]
struct Placed {
    #[serde(
        default,
        deserialize_with = "crate::brokers::common::de::string_lenient"
    )]
    id: String,
    #[serde(
        default,
        deserialize_with = "crate::brokers::common::de::string_lenient"
    )]
    product_id: String,
}

/// `POST /v2/orders` with an exact size. Returns the composite order id.
pub async fn place_order(
    b: &DeltaBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
    q: &CryptoQuantity,
) -> Result<OrderResponse> {
    let payload = mapping::place_payload(o, q)?;
    let placed: Placed = b
        .signed(auth, Method::POST, "/v2/orders", &[], Some(&payload))
        .await?;
    let product_id = if placed.product_id.is_empty() {
        payload["product_id"].to_string()
    } else {
        placed.product_id
    };
    if placed.id.is_empty() {
        return Err(AppError::Broker(
            "Delta Exchange accepted the order but returned no order id. Check the order book."
                .into(),
        ));
    }
    Ok(OrderResponse {
        order_id: format!("{}:{}", product_id, placed.id),
        message: None,
    })
}

/// `PUT /v2/orders`. Echoes the OpenAlgo order id like the web.
pub async fn modify_order(
    b: &DeltaBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
    q: &CryptoQuantity,
) -> Result<OrderResponse> {
    let payload = mapping::modify_payload(m, q)?;
    let _: Value = b
        .signed(auth, Method::PUT, "/v2/orders", &[], Some(&payload))
        .await?;
    Ok(OrderResponse {
        order_id: m.order_id.clone(),
        message: None,
    })
}

/// `DELETE /v2/orders` with `{id, product_id}` from the composite id.
pub async fn cancel_order(
    b: &DeltaBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let body = mapping::cancel_body(order_id)?;
    if body.get("product_id").is_none() {
        tracing::warn!("Delta Exchange cancel with a bare order id; the product is unknown");
    }
    let _: Value = b
        .signed(auth, Method::DELETE, "/v2/orders", &[], Some(&body))
        .await?;
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

/// Bulk `DELETE /v2/orders/all`; when Delta refuses it, cancel each open or
/// pending order one by one (web `cancel_all_orders_api`). The bulk path
/// reports `["all"]` like the web.
pub async fn cancel_all_orders(b: &DeltaBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let body = json!({
        "cancel_limit_orders": true,
        "cancel_stop_orders": true,
        "cancel_reduce_only_orders": true,
    });
    match b
        .signed_envelope(auth, Method::DELETE, "/v2/orders/all", &[], Some(&body))
        .await
    {
        Ok(_) => {
            return Ok(CancelAllResult {
                cancelled: vec!["all".into()],
                failed: Vec::new(),
            })
        }
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => tracing::warn!(
            "Delta Exchange bulk cancel failed ({}); cancelling one by one",
            e.code()
        ),
    }
    let open: Vec<DeltaOrder> = b
        .signed(
            auth,
            Method::GET,
            "/v2/orders",
            &[("state", "open".into())],
            None,
        )
        .await?;
    let mut result = CancelAllResult::default();
    for o in open
        .iter()
        .filter(|o| matches!(o.state.as_str(), "open" | "pending"))
    {
        let id = o.composite_id();
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

/// `POST /v2/products/{product_id}/orders/leverage {"leverage": "10"}`.
pub async fn set_leverage(
    b: &DeltaBroker,
    auth: &AuthToken,
    inst: &SymbolData,
    leverage: u32,
) -> Result<()> {
    let path = format!("/v2/products/{}/orders/leverage", product_id(inst)?);
    let body = json!({"leverage": leverage.to_string()});
    let _: Value = b
        .signed(auth, Method::POST, &path, &[], Some(&body))
        .await?;
    tracing::debug!(
        "Delta Exchange leverage set to {}x for {}",
        leverage,
        inst.symbol
    );
    Ok(())
}

/// `GET /v2/products/{product_id}/orders/leverage` -> `result.leverage`.
pub async fn get_leverage(b: &DeltaBroker, auth: &AuthToken, inst: &SymbolData) -> Result<f64> {
    let path = format!("/v2/products/{}/orders/leverage", product_id(inst)?);
    let v: Value = b.signed(auth, Method::GET, &path, &[], None).await?;
    match v.get("leverage") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        AppError::Broker("Delta Exchange did not report the leverage for this contract.".into())
    })
}

fn product_id(inst: &SymbolData) -> Result<i64> {
    inst.token.trim().parse::<i64>().map_err(|_| {
        AppError::Validation(format!(
            "{} has no Delta Exchange product id. Download the master contract again from the broker page.",
            inst.symbol
        ))
    })
}

/// Today's orders, open and historical (web `get_order_book`).
pub async fn get_order_book(b: &DeltaBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(get_order_book_exact(b, auth)
        .await?
        .into_iter()
        .map(|o| o.row)
        .collect())
}

/// `get_order_book` with exact sizes.
pub async fn get_order_book_exact(
    b: &DeltaBroker,
    auth: &AuthToken,
) -> Result<Vec<ExactRow<Order>>> {
    let mut rows: Vec<DeltaOrder> = b
        .signed(
            auth,
            Method::GET,
            "/v2/orders",
            &[("state", "open".into())],
            None,
        )
        .await?;
    let history: Vec<DeltaOrder> = b
        .signed(auth, Method::GET, "/v2/orders/history", &[], None)
        .await?;
    rows.extend(history);
    let today = mapping::ist_date(b.now());
    let symbols = b.resolver();
    let mut seen = HashSet::new();
    Ok(rows
        .iter()
        .filter(|o| mapping::is_on_ist_day(&o.created_at, today))
        .filter(|o| seen.insert(o.composite_id()))
        .map(|o| mapping::map_order_exact(o, symbols))
        .collect())
}

/// Today's fills (web `get_trade_book`).
pub async fn get_trade_book(b: &DeltaBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    Ok(get_trade_book_exact(b, auth)
        .await?
        .into_iter()
        .map(|t| t.row)
        .collect())
}

/// `get_trade_book` with exact sizes.
pub async fn get_trade_book_exact(
    b: &DeltaBroker,
    auth: &AuthToken,
) -> Result<Vec<ExactRow<Trade>>> {
    let fills: Vec<DeltaFill> = b.signed(auth, Method::GET, "/v2/fills", &[], None).await?;
    let today = mapping::ist_date(b.now());
    let symbols = b.resolver();
    Ok(fills
        .iter()
        .filter(|t| mapping::is_on_ist_day(&t.created_at, today))
        .map(|t| mapping::map_trade_exact(t, symbols))
        .collect())
}

/// Derivative positions plus spot wallet balances (web `get_positions`).
/// With `strict`, a half that cannot be read is an error (a missing half
/// reads as flat to a smart order); otherwise it is logged and left out,
/// which is right for the position book.
pub async fn raw_positions(
    b: &DeltaBroker,
    auth: &AuthToken,
    strict: bool,
) -> Result<Vec<RawPosition>> {
    let mut out = Vec::new();
    match b
        .signed::<Vec<DeltaPosition>>(auth, Method::GET, "/v2/positions/margined", &[], None)
        .await
    {
        Ok(p) => out.extend(p.iter().map(RawPosition::from)),
        Err(e) if strict || matches!(e, AppError::Auth(_)) => return Err(e),
        Err(e) => tracing::warn!("Delta Exchange positions could not be read: {}", e.code()),
    }
    match b
        .signed::<Vec<WalletBalance>>(auth, Method::GET, "/v2/wallet/balances", &[], None)
        .await
    {
        Ok(w) => out.extend(mapping::spot_positions(&w)),
        Err(e) if strict || matches!(e, AppError::Auth(_)) => return Err(e),
        Err(e) => tracing::warn!(
            "Delta Exchange wallet balances could not be read: {}",
            e.code()
        ),
    }
    Ok(out)
}

pub async fn get_positions(b: &DeltaBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let raw = raw_positions(b, auth, false).await?;
    let symbols = b.resolver();
    Ok(raw
        .iter()
        .filter_map(|p| mapping::map_position(p, symbols))
        .collect())
}

/// Every position with its exact size, fractional spot balances included.
pub async fn get_positions_exact(
    b: &DeltaBroker,
    auth: &AuthToken,
) -> Result<Vec<ExactRow<Position>>> {
    let raw = raw_positions(b, auth, false).await?;
    let symbols = b.resolver();
    Ok(raw
        .iter()
        .map(|p| mapping::map_position_exact(p, symbols))
        .collect())
}

/// Net size of one instrument, matched on the broker symbol whatever the
/// product (web `get_open_position`: Delta has no product types). Whole
/// units; a fractional spot balance is truncated.
pub async fn get_open_position(
    b: &DeltaBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
) -> Result<i64> {
    let br = b
        .resolver()
        .br_symbol(symbol, exchange.as_str())
        .unwrap_or_else(|| symbol.to_string());
    let raw = raw_positions(b, auth, true).await?;
    Ok(raw
        .iter()
        .find(|p| p.product_symbol == br)
        .map(|p| {
            use rust_decimal::prelude::ToPrimitive;
            p.size.trunc().to_i64().unwrap_or(0)
        })
        .unwrap_or(0))
}

/// Square off every derivative position and spot balance with a market
/// order of its exact size (web `close_all_positions`).
pub async fn close_all_positions(b: &DeltaBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let raw = raw_positions(b, auth, false).await?;
    let symbols = b.resolver();
    let mut result = CloseAllResult::default();
    for p in raw.iter().filter(|p| !p.size.is_zero()) {
        let Some(row) = mapping::position_row(p, symbols) else {
            result.failed.push(format!(
                "{} (CRYPTO): not in the master contract; download it again from the broker page",
                p.product_symbol
            ));
            continue;
        };
        let label = format!("{} (CRYPTO)", row.symbol);
        let Some(q) = CryptoQuantity::from_decimal(p.size.abs()) else {
            continue;
        };
        let order = ResolvedOrder {
            symbol: row.symbol.clone(),
            exchange: Exchange::Crypto,
            action: if p.size.is_sign_positive() {
                Action::Sell
            } else {
                Action::Buy
            },
            quantity: q.truncated(),
            price: 0.0,
            trigger_price: 0.0,
            pricetype: PriceType::Market,
            product: if p.is_spot {
                Product::Cnc
            } else {
                Product::Nrml
            },
            validity: Validity::Day,
            disclosed_quantity: 0,
            amo: false,
            instrument: row,
        };
        match place_order(b, auth, &order, &q).await {
            Ok(r) => result.placed.push(r.order_id),
            Err(e) => result
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(result)
}
