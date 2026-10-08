//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, AngelOrder, AngelPortfolio, AngelPosition, AngelTrade};
use super::{angel_error, AngelBroker, Category};
use crate::brokers::common::de::string_lenient;
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;

pub const PLACE_PATH: &str = "/rest/secure/angelbroking/order/v1/placeOrder";
pub const MODIFY_PATH: &str = "/rest/secure/angelbroking/order/v1/modifyOrder";
pub const CANCEL_PATH: &str = "/rest/secure/angelbroking/order/v1/cancelOrder";
pub const ORDER_BOOK_PATH: &str = "/rest/secure/angelbroking/order/v1/getOrderBook";
pub const TRADE_BOOK_PATH: &str = "/rest/secure/angelbroking/order/v1/getTradeBook";
pub const POSITIONS_PATH: &str = "/rest/secure/angelbroking/order/v1/getPosition";
pub const HOLDINGS_PATH: &str = "/rest/secure/angelbroking/portfolio/v1/getAllHolding";

#[derive(Deserialize, Default)]
#[serde(default)]
struct OrderIdData {
    #[serde(deserialize_with = "string_lenient")]
    orderid: String,
}

pub async fn place_order(
    b: &AngelBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let body = mapping::place_order_body(o);
    let data: Option<OrderIdData> = b
        .call(Method::POST, PLACE_PATH, auth, Some(&body), Category::Order)
        .await?;
    let id = data
        .map(|d| d.orderid)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Broker("Angel One did not return an order id.".into()))?;
    Ok(OrderResponse {
        order_id: id,
        message: None,
    })
}

pub async fn modify_order(
    b: &AngelBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = mapping::modify_order_body(m);
    let env = b
        .call_env::<OrderIdData>(
            Method::POST,
            MODIFY_PATH,
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    // web: `status == "true" or message == "SUCCESS"`.
    if !(env.status || env.message == "SUCCESS") {
        tracing::warn!(code = %env.errorcode, "Angel One refused modify: {}", env.message);
        return Err(angel_error(&env.errorcode, &env.message));
    }
    Ok(OrderResponse {
        order_id: env
            .data
            .map(|d| d.orderid)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| m.order_id.clone()),
        message: None,
    })
}

pub async fn cancel_order(
    b: &AngelBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let body = mapping::cancel_order_body(order_id);
    b.call::<OrderIdData>(
        Method::POST,
        CANCEL_PATH,
        auth,
        Some(&body),
        Category::Order,
    )
    .await?;
    // web returns the requested id on success.
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

pub(crate) async fn raw_orders(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<AngelOrder>> {
    Ok(
        b.call::<Vec<AngelOrder>>(Method::GET, ORDER_BOOK_PATH, auth, None, Category::Other)
            .await?
            .unwrap_or_default(),
    )
}

pub(crate) async fn raw_positions(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<AngelPosition>> {
    Ok(
        b.call::<Vec<AngelPosition>>(Method::GET, POSITIONS_PATH, auth, None, Category::Other)
            .await?
            .unwrap_or_default(),
    )
}

/// web `cancel_all_orders_api`: cancel rows whose raw status is `open` or
/// `trigger pending`.
pub async fn cancel_all_orders(b: &AngelBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(b, auth).await? {
        if o.status != "open" && o.status != "trigger pending" {
            continue;
        }
        match cancel_order(b, auth, &o.orderid).await {
            Ok(_) => result.cancelled.push(o.orderid),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.orderid, e.code());
                result.failed.push(o.orderid)
            }
        }
    }
    Ok(result)
}

/// web `get_open_position`: the OpenAlgo symbol converted to Angel's
/// tradingsymbol, matched with exchange and Angel producttype on the raw
/// position book; `netqty`, 0 when not found.
pub async fn get_open_position(
    b: &AngelBroker,
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
    let producttype = mapping::map_product_type(product.as_str());
    Ok(raw_positions(b, auth)
        .await?
        .into_iter()
        .find(|p| p.tradingsymbol == br && p.exchange == ex && p.producttype == producttype)
        .map(|p| p.netqty)
        .unwrap_or(0))
}

/// web `close_all_positions`: one MARKET order per non-zero net position,
/// symbol by token, product reverse-mapped.
pub async fn close_all_positions(b: &AngelBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_positions(b, auth).await? {
        if p.netqty == 0 {
            continue;
        }
        let symbol = mapping::oa_symbol(&symbols, &p.symboltoken, &p.tradingsymbol, &p.exchange);
        let label = format!("{} ({})", symbol, p.exchange);
        let product = mapping::reverse_map_product_type(&p.producttype)
            .unwrap_or("MIS")
            .to_string();
        let req = OrderRequest {
            symbol,
            exchange: p.exchange.clone(),
            side: if p.netqty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(p.netqty.abs()).unwrap_or(i32::MAX),
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
            Ok(r) => result.placed.push(r.order_id),
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

pub async fn get_order_book(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        raw_orders(b, auth).await?,
        b.resolver(),
    ))
}

pub async fn get_trade_book(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let rows = b
        .call::<Vec<AngelTrade>>(Method::GET, TRADE_BOOK_PATH, auth, None, Category::Other)
        .await?
        .unwrap_or_default();
    Ok(mapping::map_trades(rows, b.resolver()))
}

pub async fn get_positions(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(mapping::map_positions(
        raw_positions(b, auth).await?,
        b.resolver(),
    ))
}

pub(crate) async fn raw_portfolio(b: &AngelBroker, auth: &AuthToken) -> Result<AngelPortfolio> {
    Ok(
        b.call::<AngelPortfolio>(Method::GET, HOLDINGS_PATH, auth, None, Category::Other)
            .await?
            .unwrap_or_default(),
    )
}

pub async fn get_holdings(b: &AngelBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    Ok(mapping::map_holdings(
        raw_portfolio(b, auth).await?,
        b.resolver(),
    ))
}

/// Holdings with Angel's own `totalholding` figures (web
/// `calculate_portfolio_statistics`).
pub async fn get_holdings_with_totals(b: &AngelBroker, auth: &AuthToken) -> Result<HoldingsBook> {
    let p = raw_portfolio(b, auth).await?;
    let totals = p.stats();
    Ok(HoldingsBook {
        holdings: mapping::map_holdings(p, b.resolver()),
        totals: Some(totals),
    })
}
