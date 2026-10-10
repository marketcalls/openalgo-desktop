//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{self, KiteHolding, KiteOrder, KitePositions, KiteTrade};
use super::{Body, Category, ZerodhaBroker};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::common::master_contract::format_strike;
use crate::brokers::common::outcome;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;

#[derive(Deserialize)]
struct OrderId {
    #[serde(deserialize_with = "crate::brokers::common::de::string_lenient")]
    order_id: String,
}

/// Number as the web sends it in a form (`"0"`, `"1500.5"`).
pub(crate) fn num(v: f64) -> String {
    format_strike(v)
}

fn mcx_lot(inst: &crate::brokers::common::SymToken) -> Option<i64> {
    (inst.lot_size > 0).then_some(i64::from(inst.lot_size))
}

/// The form `place_order_api` posts (web `transform_data`). `tag` is the
/// order's own client tag. The web sends the constant `openalgo`; a tag per
/// order is what lets an order whose placement had no definite answer be
/// found in the order book (LOG-08, a deliberate departure).
pub fn place_order_form(o: &ResolvedOrder, tag: &str) -> Result<Vec<(&'static str, String)>> {
    let br = o.brsymbol().to_string();
    let ex = o.exchange.as_str();
    let lot = mcx_lot(&o.instrument);
    let qty = mapping::to_kite_quantity(o.quantity, &br, ex, lot, "Quantity")?;
    let dq = mapping::to_kite_quantity(o.disclosed_quantity, &br, ex, lot, "Disclosed quantity")?;
    Ok(vec![
        ("tradingsymbol", br.clone()),
        ("exchange", o.brexchange().to_string()),
        ("transaction_type", o.action.as_str().to_string()),
        ("order_type", o.pricetype.as_str().to_string()),
        ("quantity", qty.to_string()),
        ("product", o.product.as_str().to_string()),
        ("price", num(o.price)),
        ("trigger_price", num(o.trigger_price)),
        ("disclosed_quantity", dq.to_string()),
        ("validity", "DAY".to_string()),
        ("market_protection", "-1".to_string()),
        ("tag", tag.to_string()),
    ])
}

/// The form `modify_order` puts (web `transform_modify_order_data`).
pub fn modify_order_form(m: &ResolvedModify) -> Result<Vec<(&'static str, String)>> {
    let br = m.brsymbol().to_string();
    let ex = m.exchange.as_str();
    let lot = mcx_lot(&m.instrument);
    let qty = mapping::to_kite_quantity(m.quantity, &br, ex, lot, "Quantity")?;
    let dq = mapping::to_kite_quantity(m.disclosed_quantity, &br, ex, lot, "Disclosed quantity")?;
    let mut form = vec![
        ("order_type", m.pricetype.as_str().to_string()),
        ("quantity", qty.to_string()),
        (
            "price",
            if m.price != 0.0 {
                num(m.price)
            } else {
                "0".into()
            },
        ),
        ("disclosed_quantity", dq.to_string()),
        ("validity", "DAY".to_string()),
    ];
    if m.trigger_price != 0.0 {
        form.push(("trigger_price", num(m.trigger_price)));
    }
    Ok(form)
}

pub async fn place_order(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let tag = outcome::new_client_tag();
    let form = place_order_form(o, &tag)?;
    let variety = if o.amo { "amo" } else { "regular" };
    let placed = b
        .call::<OrderId>(
            Method::POST,
            &format!("/orders/{}", variety),
            auth,
            Body::Form(&form),
            Category::Order,
        )
        .await
        .map(|r| OrderResponse {
            order_id: r.order_id,
            message: None,
        });
    outcome::with_client_tag(placed, &tag)
}

/// The order book with each order's tag (the reconciler's lookup).
pub async fn get_order_book_tagged(
    b: &ZerodhaBroker,
    auth: &AuthToken,
) -> Result<Vec<TaggedOrder>> {
    let rows = raw_orders(b, auth).await?;
    let tags: Vec<Option<String>> = rows
        .iter()
        .map(|r| Some(r.tag.trim().to_string()).filter(|t| !t.is_empty()))
        .collect();
    Ok(mapping::map_orders(rows, b.resolver())
        .into_iter()
        .zip(tags)
        .map(|(order, client_tag)| TaggedOrder { order, client_tag })
        .collect())
}

pub async fn modify_order(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let form = modify_order_form(m)?;
    let r: OrderId = b
        .call(
            Method::PUT,
            &format!("/orders/regular/{}", urlencoding::encode(&m.order_id)),
            auth,
            Body::Form(&form),
            Category::Order,
        )
        .await?;
    Ok(OrderResponse {
        order_id: r.order_id,
        message: None,
    })
}

pub async fn cancel_order(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let r: OrderId = b
        .call(
            Method::DELETE,
            &format!("/orders/regular/{}", urlencoding::encode(order_id)),
            auth,
            Body::None,
            Category::Order,
        )
        .await?;
    Ok(OrderResponse {
        order_id: r.order_id,
        message: None,
    })
}

async fn raw_orders(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<KiteOrder>> {
    let env = b
        .call_raw::<Vec<KiteOrder>>(Method::GET, "/orders", auth, Body::None, Category::Other)
        .await?;
    Ok(env.data.unwrap_or_default())
}

/// Raw Kite net positions (MCX quantities in contracts).
pub(crate) async fn raw_positions(
    b: &ZerodhaBroker,
    auth: &AuthToken,
) -> Result<Vec<mapping::KitePosition>> {
    let env = b
        .call_raw::<KitePositions>(
            Method::GET,
            "/portfolio/positions",
            auth,
            Body::None,
            Category::Other,
        )
        .await?;
    Ok(env.data.and_then(|d| d.net).unwrap_or_default())
}

pub async fn cancel_all_orders(b: &ZerodhaBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        // web filters Kite's own strings
        if o.status != "OPEN" && o.status != "TRIGGER PENDING" {
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

pub async fn get_open_position(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let ex = exchange.as_str();
    let row = b.resolver().by_symbol(ex, symbol);
    let br = row
        .as_ref()
        .map(|r| r.br_symbol().to_string())
        .unwrap_or_else(|| symbol.to_string());
    let lot = row.as_ref().and_then(mcx_lot);
    for p in raw_positions(b, auth).await? {
        if p.tradingsymbol == br && p.exchange == ex && p.product == product.as_str() {
            // This value decides whether to trade, so an unknown MCX size is
            // an error, never a factor of 1.
            if mapping::units_per_contract(&br, ex, lot).is_none() {
                return Err(AppError::Validation(format!(
                    "Cannot read the open position in {}: MCX has revised its contract size and the master contract has no row for this expiry. Re-download the master contract, then retry.",
                    br
                )));
            }
            return Ok(mapping::from_kite_quantity(p.quantity, &br, ex, lot));
        }
    }
    Ok(0)
}

pub async fn close_all_positions(b: &ZerodhaBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.quantity == 0 {
            continue;
        }
        let lot = symbols
            .by_brsymbol(&p.exchange, &p.tradingsymbol)
            .and_then(|r| mcx_lot(&r));
        let qty = mapping::from_kite_quantity(p.quantity, &p.tradingsymbol, &p.exchange, lot).abs();
        let symbol = symbols.oa_symbol_or_raw(&p.tradingsymbol, &p.exchange);
        let label = format!("{} ({})", symbol, p.exchange);
        let req = OrderRequest {
            symbol,
            exchange: p.exchange.clone(),
            side: if p.quantity > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(qty).unwrap_or(i32::MAX),
            price: 0.0,
            order_type: "MARKET".into(),
            product: p.product.clone(),
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

pub async fn get_order_book(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let env = b
        .call_raw::<Vec<KiteTrade>>(Method::GET, "/trades", auth, Body::None, Category::Other)
        .await?;
    Ok(mapping::map_trades(
        env.data.unwrap_or_default(),
        b.resolver(),
    ))
}

pub async fn get_positions(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let env = b
        .call_raw::<Vec<KiteHolding>>(
            Method::GET,
            "/portfolio/holdings",
            auth,
            Body::None,
            Category::Other,
        )
        .await?;
    Ok(mapping::map_holdings(
        env.data.unwrap_or_default(),
        b.resolver(),
    ))
}
