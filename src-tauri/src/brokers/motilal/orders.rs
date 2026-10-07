//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, token_text, vi, vs};
use super::{motilal_error, paths, MotilalBroker, MotilalSession};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::mpp;
use crate::brokers::types::*;
use crate::brokers::Broker;
use crate::error::{AppError, Result};
use serde_json::{json, Value};

/// Shares -> lots (web `_resolve_lotsize` + the multiple check): derivatives
/// without a lot size are refused rather than guessed.
pub fn quantity_in_lots(symbol: &str, exchange: &str, quantity: i64, lot_size: i32) -> Result<i64> {
    let lot = if lot_size > 0 {
        i64::from(lot_size)
    } else if mapping::is_derivative_exchange(exchange) {
        return Err(AppError::Validation(format!(
            "The lot size of {} on {} is not known, so the order was not sent. Download the master contract again and retry.",
            symbol, exchange
        )));
    } else {
        1
    };
    if quantity % lot != 0 {
        return Err(AppError::Validation(format!(
            "Quantity {} is not a multiple of the lot size {}. Use {}, {}, {} and so on.",
            quantity,
            lot,
            lot,
            lot * 2,
            lot * 3
        )));
    }
    Ok(quantity / lot)
}

/// Order type and limit price after Market Price Protection (web
/// `transform_data`): MARKET -> LIMIT at LTP +/- slab when an LTP is known,
/// SL-M -> STOPLOSS with a limit at trigger +/- slab.
pub fn protected_order(o: &ResolvedOrder, ltp: Option<f64>) -> (&'static str, f64) {
    let tick = Some(o.instrument.tick_size).filter(|t| *t > 0.0);
    let itype = mpp::instrument_type_from_symbol(&o.symbol);
    match o.pricetype {
        PriceType::Market => match ltp.filter(|l| *l > 0.0) {
            Some(l) => ("LIMIT", mpp::protected_price(l, o.action, itype, tick)),
            None => {
                tracing::warn!("No LTP for {}; sending the MARKET order as is", o.symbol);
                ("MARKET", o.price)
            }
        },
        PriceType::SlM if o.trigger_price > 0.0 => (
            "STOPLOSS",
            mpp::protected_price(o.trigger_price, o.action, itype, tick),
        ),
        other => (mapping::map_order_type(other), o.price),
    }
}

/// web `place_order_api` payload.
pub fn place_order_body(o: &ResolvedOrder, ordertype: &str, price: f64, lots: i64) -> Value {
    let token: i64 = o.token().trim().parse().unwrap_or(0);
    json!({
        "exchange": mapping::map_exchange(o.exchange.as_str()),
        "symboltoken": token,
        "buyorsell": o.action.as_str(),
        "ordertype": ordertype,
        "producttype": mapping::map_product_type(o.product.as_str(), o.exchange.as_str()),
        "orderduration": "DAY",
        "price": price,
        "triggerprice": o.trigger_price,
        "quantityinlot": lots,
        "disclosedquantity": o.disclosed_quantity,
        "amoorder": "N",
    })
}

pub async fn place_order(
    b: &MotilalBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = MotilalSession::parse(auth)?;
    if o.token().trim().parse::<i64>().is_err() {
        return Err(AppError::Validation(format!(
            "Symbol {} has no Motilal Oswal scrip code. Download the master contract again.",
            o.symbol
        )));
    }
    let lots = quantity_in_lots(
        &o.symbol,
        o.exchange.as_str(),
        o.quantity,
        o.instrument.lot_size,
    )?;
    let ltp = if o.pricetype == PriceType::Market {
        match b
            .get_quote(auth, &QuoteKey::new(o.exchange.as_str(), &o.symbol))
            .await
        {
            Ok(q) => Some(q.ltp),
            Err(e) => {
                tracing::warn!(
                    "Price protection quote for {} failed: {}",
                    o.symbol,
                    e.code()
                );
                None
            }
        }
    } else {
        None
    };
    let (ordertype, price) = protected_order(o, ltp);
    let body = place_order_body(o, ordertype, price, lots);
    let v = b.post(&s, paths::PLACE, Some(&body)).await?;
    let id = vs(&v, "uniqueorderid").ok_or_else(|| {
        AppError::Broker("Motilal Oswal accepted the order but returned no order id.".into())
    })?;
    Ok(OrderResponse {
        order_id: id,
        message: vs(&v, "message"),
    })
}

/// web `_resolve_lastmodifiedtime`.
pub fn last_modified_time(row: &Value) -> String {
    let clean = |k: &str| vs(row, k).filter(|v| v != "0");
    clean("lastmodifiedtime")
        .or_else(|| clean("recordinserttime"))
        .or_else(|| clean("entrydatetime"))
        .unwrap_or_else(|| vs(row, "lastmodifiedtime").unwrap_or_default())
}

/// web `_fetch_order_details`: the by-id endpoint, then the order book.
async fn order_details(b: &MotilalBroker, s: &MotilalSession, order_id: &str) -> Result<Value> {
    match b
        .post_raw(
            s,
            paths::ORDER_DETAIL,
            Some(&json!({"uniqueorderid": order_id})),
        )
        .await
    {
        Ok((_, v)) if super::is_success(&v) => {
            let rows = v
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(r) = rows
                .into_iter()
                .find(|r| r.is_object() && vs(r, "uniqueorderid").is_none_or(|id| id == order_id))
            {
                return Ok(r);
            }
        }
        Ok(_) => tracing::warn!("Order detail refused; falling back to the order book"),
        Err(e) => tracing::warn!(
            "Order detail failed ({}); falling back to the order book",
            e.code()
        ),
    }
    let book = raw_book(b, s, paths::ORDER_BOOK).await?;
    book.into_iter()
        .find(|r| vs(r, "uniqueorderid").as_deref() == Some(order_id))
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "Order {} was not found in the order book.",
                order_id
            ))
        })
}

/// web `transform_modify_order_data`.
pub fn modify_order_body(m: &ResolvedModify, lots: i64, last_modified: &str, traded: i64) -> Value {
    json!({
        "uniqueorderid": m.order_id,
        "newordertype": mapping::map_order_type(m.pricetype),
        "neworderduration": "DAY",
        "newprice": m.price,
        "newtriggerprice": m.trigger_price,
        "newquantityinlot": lots,
        "newdisclosedquantity": m.disclosed_quantity,
        "newgoodtilldate": "",
        "lastmodifiedtime": last_modified,
        "qtytradedtoday": traded,
    })
}

pub async fn modify_order(
    b: &MotilalBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = MotilalSession::parse(auth)?;
    let details = order_details(b, &s, &m.order_id).await?;
    let lmt = last_modified_time(&details);
    let traded = vi(&details, "qtytradedtoday");
    let lots = quantity_in_lots(
        &m.symbol,
        m.exchange.as_str(),
        m.quantity,
        m.instrument.lot_size,
    )?;
    let body = modify_order_body(m, lots, &lmt, traded);
    b.post(&s, paths::MODIFY, Some(&body)).await?;
    Ok(OrderResponse {
        order_id: m.order_id.clone(),
        message: None,
    })
}

pub async fn cancel_order(
    b: &MotilalBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = MotilalSession::parse(auth)?;
    let (status, v) = b
        .post_raw(&s, paths::CANCEL, Some(&json!({"uniqueorderid": order_id})))
        .await?;
    if !super::is_success(&v) {
        return Err(motilal_error(
            status,
            &v,
            "Motilal Oswal could not cancel the order.",
        ));
    }
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

/// A book's `data` rows (null data is an empty book).
pub(crate) async fn raw_book(
    b: &MotilalBroker,
    s: &MotilalSession,
    path: &str,
) -> Result<Vec<Value>> {
    let v = b.post(s, path, None).await?;
    Ok(v.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// web `cancel_all_orders_api`: rows whose raw status is confirm, sent,
/// open or partial.
pub fn is_cancellable(row: &Value) -> bool {
    matches!(
        vs(row, "orderstatus")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "confirm" | "sent" | "open" | "partial"
    )
}

pub async fn cancel_all_orders(b: &MotilalBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let s = MotilalSession::parse(auth)?;
    let book = raw_book(b, &s, paths::ORDER_BOOK).await?;
    let mut out = CancelAllResult::default();
    for row in book.iter().filter(|r| is_cancellable(r)) {
        let Some(id) = vs(row, "uniqueorderid") else {
            continue;
        };
        match cancel_order(b, auth, &id).await {
            Ok(_) => out.cancelled.push(id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", id, e.code());
                out.failed.push(id)
            }
        }
    }
    Ok(out)
}

/// web `close_all_positions`: one MARKET exit per non-flat row, product
/// from the reverse map.
pub async fn close_all_positions(b: &MotilalBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let s = MotilalSession::parse(auth)?;
    let rows = raw_book(b, &s, paths::POSITIONS).await?;
    let mut out = CloseAllResult::default();
    for r in rows.iter().filter(|r| r.is_object()) {
        let net = vi(r, "buyquantity") - vi(r, "sellquantity");
        if net == 0 {
            continue;
        }
        let exchange =
            mapping::reverse_map_exchange(&vs(r, "exchange").unwrap_or_default()).to_string();
        let token = token_text(r, "symboltoken");
        let Some(row) = b.resolver().by_token(&exchange, &token) else {
            tracing::warn!("No master row for scrip {} on {}", token, exchange);
            out.failed.push(format!(
                "{} ({}): the instrument is not in the master contract",
                vs(r, "symbol").unwrap_or(token),
                exchange
            ));
            continue;
        };
        let req = OrderRequest {
            symbol: row.symbol.clone(),
            exchange: exchange.clone(),
            side: if net > 0 { "SELL" } else { "BUY" }.to_string(),
            quantity: net.unsigned_abs() as i32,
            price: 0.0,
            order_type: PriceType::Market.as_str().to_string(),
            product: mapping::reverse_map_product_type(&vs(r, "productname").unwrap_or_default())
                .to_string(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let label = format!("{} ({})", row.symbol, exchange);
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

/// web `get_open_position`: match on scrip code, Motilal exchange and the
/// Motilal product the order would be placed with.
pub async fn get_open_position(
    b: &MotilalBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let Some(row) = b.resolver().by_symbol(exchange.as_str(), symbol) else {
        tracing::warn!(
            "No master row for {} on {}; treating as flat",
            symbol,
            exchange
        );
        return Ok(0);
    };
    let s = MotilalSession::parse(auth)?;
    let rows = raw_book(b, &s, paths::POSITIONS).await?;
    let mo_exchange = mapping::map_exchange(exchange.as_str());
    let mo_product = mapping::position_product(product, exchange);
    Ok(rows
        .iter()
        .find(|r| {
            token_text(r, "symboltoken") == row.token
                && vs(r, "exchange").as_deref() == Some(mo_exchange)
                && vs(r, "productname").as_deref() == Some(mo_product)
        })
        .map(|r| vi(r, "buyquantity") - vi(r, "sellquantity"))
        .unwrap_or(0))
}

pub async fn get_order_book(b: &MotilalBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let s = MotilalSession::parse(auth)?;
    let rows = raw_book(b, &s, paths::ORDER_BOOK).await?;
    Ok(mapping::map_orders(&rows, b.resolver()))
}

pub async fn get_trade_book(b: &MotilalBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let s = MotilalSession::parse(auth)?;
    let rows = raw_book(b, &s, paths::TRADE_BOOK).await?;
    Ok(mapping::map_trades(&rows, b.resolver()))
}

pub async fn get_positions(b: &MotilalBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let s = MotilalSession::parse(auth)?;
    let rows = raw_book(b, &s, paths::POSITIONS).await?;
    Ok(mapping::map_positions(&rows, b.resolver()))
}

pub async fn get_holdings(b: &MotilalBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let s = MotilalSession::parse(auth)?;
    let v = b.post(&s, paths::HOLDINGS, Some(&json!({}))).await?;
    let rows = v
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(mapping::map_holdings(&rows, b.resolver()))
}
