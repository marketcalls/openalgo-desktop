//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`).

use super::mapping::{
    self, extract_order_id, product_code, UpstoxHolding, UpstoxOrder, UpstoxPosition, UpstoxTrade,
};
use super::{Category, UpstoxBroker};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Body of `POST /v3/order/place` (web `transform_data` + `place_order_api`).
///
/// Price only travels on LIMIT / SL and trigger only on SL / SL-M (Upstox
/// rejects a non-zero price on MARKET / SL-M with UDAPI1040). The web sends
/// every value as a string (`"is_amo": "false"`); this sends the JSON types
/// the Upstox v3 contract documents, which Upstox reads identically.
/// `market_protection` is left out so Upstox applies its own -1 default.
pub fn place_body(o: &ResolvedOrder) -> Value {
    let price = if matches!(o.pricetype, PriceType::Limit | PriceType::Sl) {
        o.price
    } else {
        0.0
    };
    let trigger = if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        o.trigger_price
    } else {
        0.0
    };
    json!({
        "quantity": o.quantity,
        "product": product_code(o.product),
        "validity": "DAY",
        "price": price,
        "tag": "openalgo",
        "instrument_token": o.token(),
        "order_type": mapping::order_type(o.pricetype),
        "transaction_type": o.action.as_str(),
        "disclosed_quantity": o.disclosed_quantity,
        "trigger_price": trigger,
        "is_amo": o.amo,
    })
}

/// Body of `PUT /v3/order/modify` (web `transform_modify_order_data`):
/// price and trigger pass through unzeroed, as on the web.
pub fn modify_body(m: &ResolvedModify) -> Value {
    json!({
        "quantity": m.quantity,
        "validity": "DAY",
        "price": m.price,
        "order_id": m.order_id,
        "order_type": mapping::order_type(m.pricetype),
        "disclosed_quantity": m.disclosed_quantity,
        "trigger_price": m.trigger_price,
    })
}

fn order_response(data: &Value, what: &str) -> Result<OrderResponse> {
    match extract_order_id(data) {
        Some(id) => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        None => {
            tracing::error!("Upstox accepted the {} but returned no order id", what);
            Err(AppError::uncertain(
                format!(
                    "Upstox accepted the {} but did not return an order id. Check the order book.",
                    what
                ),
                None,
            ))
        }
    }
}

pub async fn place_order(
    b: &UpstoxBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    if !o.token().contains('|') {
        return Err(AppError::Validation(format!(
            "The master contract has no Upstox instrument key for {}. Download the master contract again.",
            o.symbol
        )));
    }
    let body = place_body(o);
    let data = b
        .call(
            Method::POST,
            &b.hft("/v3/order/place"),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    order_response(&data, "order")
}

pub async fn modify_order(
    b: &UpstoxBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = modify_body(m);
    let data = b
        .call(
            Method::PUT,
            &b.hft("/v3/order/modify"),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    order_response(&data, "modification")
}

pub async fn cancel_order(
    b: &UpstoxBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let url = b.hft(&format!(
        "/v3/order/cancel?order_id={}",
        urlencoding::encode(order_id)
    ));
    let data = b
        .call(Method::DELETE, &url, auth, None, Category::Order)
        .await?;
    match extract_order_id(&data) {
        Some(id) => Ok(OrderResponse {
            order_id: id,
            message: None,
        }),
        None => Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        }),
    }
}

async fn read_list<T: DeserializeOwned>(
    b: &UpstoxBroker,
    auth: &AuthToken,
    path: &str,
) -> Result<Vec<T>> {
    let data = b
        .call(Method::GET, &b.api(path), auth, None, Category::Standard)
        .await?;
    if data.is_null() {
        return Ok(Vec::new());
    }
    serde_json::from_value(data).map_err(|e| {
        tracing::warn!("Upstox {} has an unexpected shape: {}", path, e);
        AppError::Broker("Upstox sent a book OpenAlgo could not read. Try again shortly.".into())
    })
}

pub(crate) async fn raw_orders(b: &UpstoxBroker, auth: &AuthToken) -> Result<Vec<UpstoxOrder>> {
    read_list(b, auth, "/v2/order/retrieve-all").await
}

pub(crate) async fn raw_positions(
    b: &UpstoxBroker,
    auth: &AuthToken,
) -> Result<Vec<UpstoxPosition>> {
    read_list(b, auth, "/v2/portfolio/short-term-positions").await
}

pub async fn get_order_book(b: &UpstoxBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &UpstoxBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows: Vec<UpstoxTrade> = read_list(b, auth, "/v2/order/trades/get-trades-for-day").await?;
    Ok(mapping::map_trades(rows, b.resolver()))
}

pub async fn get_positions(b: &UpstoxBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_holdings(b: &UpstoxBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let rows: Vec<UpstoxHolding> = read_list(b, auth, "/v2/portfolio/long-term-holdings").await?;
    Ok(mapping::map_holdings(rows, b.resolver()))
}

/// Cancel every raw `open` / `trigger pending` order, one call each (Upstox
/// has no multi-cancel the web uses).
pub async fn cancel_all_orders(b: &UpstoxBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        if !mapping::is_cancellable_raw(&o.status) {
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

/// Net quantity (web `get_open_position`): match the broker symbol, the
/// OpenAlgo exchange and the Upstox product code.
pub async fn get_open_position(
    b: &UpstoxBroker,
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
    let token = row.map(|r| r.token).unwrap_or_default();
    let code = product_code(product);
    for p in raw_positions(b, auth).await? {
        let psym = mapping::br_symbol(&p.trading_symbol, &p.tradingsymbol);
        let same_instrument = psym == br || (!token.is_empty() && p.instrument_token == token);
        if same_instrument && p.exchange == ex && p.product == code {
            return Ok(p.quantity);
        }
    }
    Ok(0)
}

/// Square off every open position at MARKET (web `close_all_positions`):
/// the symbol comes from the instrument key, the product from
/// `reverse_map_product_type`.
pub async fn close_all_positions(b: &UpstoxBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.quantity == 0 {
            continue;
        }
        let Some(row) = symbols.by_token(&p.exchange, &p.instrument_token) else {
            tracing::warn!(
                "No OpenAlgo symbol for Upstox instrument {} on {}; position skipped",
                p.instrument_token,
                p.exchange
            );
            result.failed.push(format!(
                "{} ({}): not in the master contract",
                mapping::br_symbol(&p.trading_symbol, &p.tradingsymbol),
                p.exchange
            ));
            continue;
        };
        let label = format!("{} ({})", row.symbol, p.exchange);
        let Some(product) = mapping::reverse_product(&p.exchange, &p.product) else {
            result
                .failed
                .push(format!("{}: unknown product {}", label, p.product));
            continue;
        };
        let req = OrderRequest {
            symbol: row.symbol.clone(),
            exchange: p.exchange.clone(),
            side: if p.quantity > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(p.quantity.abs()).unwrap_or(i32::MAX),
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
