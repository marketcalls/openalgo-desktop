//! Orders and books (web `api/order_api.py`).

use super::mapping::{
    self, mpp_place, noren_exchange, product_code, reverse_product, text, MppOrder, MppQuote,
};
use super::transport::{as_list, noren_error, session, stat_ok, Category, Session};
use super::NorenBroker;
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::Result;
use serde_json::{json, Value};

/// `{"uid","actid"}` for the book endpoints.
fn book_body(s: &Session) -> Value {
    json!({"uid": s.uid, "actid": s.uid})
}

pub(crate) async fn raw_book(b: &NorenBroker, s: &Session, endpoint: &str) -> Result<Vec<Value>> {
    let body = if endpoint == "/Holdings" {
        json!({"uid": s.uid, "actid": s.uid, "prd": "C"})
    } else {
        book_body(s)
    };
    b.post_list(endpoint, body, s, Category::Data).await
}

/// LTP and `ti` for MPP; `None` when the quote cannot be read.
async fn mpp_quote(b: &NorenBroker, s: &Session, o: &ResolvedOrder) -> Option<MppQuote> {
    let exch = noren_exchange(o.exchange.as_str());
    match super::data::quote_response(b, s, exch, o.token()).await {
        Ok(q) => {
            let tick = mapping::f(&q, "ti");
            Some(MppQuote {
                ltp: mapping::f(&q, "lp"),
                tick: (tick > 0.0).then_some(tick),
            })
        }
        Err(e) => {
            tracing::warn!(
                broker = b.cfg.id,
                "Price protection quote for {} failed: {}",
                o.symbol,
                e.code()
            );
            None
        }
    }
}

/// PlaceOrder `jData` for a resolved order, MPP applied.
pub(crate) async fn place_body(b: &NorenBroker, s: &Session, o: &ResolvedOrder) -> Value {
    let needs_quote = match o.pricetype {
        PriceType::Market => true,
        PriceType::SlM => b.cfg.mpp != super::MppScope::MarketOnly,
        _ => false,
    };
    let quote = if needs_quote {
        mpp_quote(b, s, o).await
    } else {
        None
    };
    let (prctyp, prc) = mpp_place(
        b.cfg.mpp,
        &MppOrder {
            symbol: &o.symbol,
            action: o.action,
            pricetype: o.pricetype,
            price: o.price,
            trigger: o.trigger_price,
        },
        quote,
        o.instrument.tick_size,
    );
    mapping::place_jdata(b.cfg, &s.uid, o, prctyp, &prc)
}

pub async fn place_order(
    b: &NorenBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    let s = session(b.cfg, auth)?;
    // The price-protection quote comes before the order is sent: a server
    // error it meets never counts against the order's answer (LOG-08).
    let body = crate::brokers::common::outcome::before_send(async {
        Ok::<_, crate::error::AppError>(place_body(b, &s, o).await)
    })
    .await?;
    let v = b.post_raw("/PlaceOrder", body, &s, Category::Order).await?;
    if stat_ok(&v) {
        let id = text(&v, "norenordno");
        if id.is_empty() {
            // Accepted without an order number: the order may exist.
            tracing::warn!(broker = b.cfg.id, "Order accepted without an order number");
            return Err(crate::error::AppError::uncertain(
                format!(
                    "{} accepted the order but returned no order number. Check the order book before placing it again.",
                    b.cfg.name
                ),
                None,
            ));
        }
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    let e = super::transport::emsg(&v);
    tracing::warn!(broker = b.cfg.id, "Order refused: {}", e);
    // A readable refusal (LOG-08); unknown after a server error.
    Err(crate::brokers::common::outcome::definite(noren_error(
        b.cfg.name, &e,
    )))
}

pub async fn modify_order(
    b: &NorenBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let s = session(b.cfg, auth)?;
    let body = mapping::modify_jdata(b.cfg, &s.uid, m);
    let v = b
        .post_raw("/ModifyOrder", body, &s, Category::Order)
        .await?;
    if stat_ok(&v) {
        return Ok(OrderResponse {
            order_id: m.order_id.clone(),
            message: None,
        });
    }
    let e = super::transport::emsg(&v);
    tracing::warn!(broker = b.cfg.id, "Modify refused: {}", e);
    Err(noren_error(b.cfg.name, &e))
}

pub async fn cancel_order(
    b: &NorenBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let s = session(b.cfg, auth)?;
    let v = b
        .post_raw(
            "/CancelOrder",
            json!({"uid": s.uid, "norenordno": order_id}),
            &s,
            Category::Order,
        )
        .await?;
    if stat_ok(&v) {
        return Ok(OrderResponse {
            order_id: order_id.to_string(),
            message: None,
        });
    }
    let msg = (b.cfg.hooks.cancel_error)(&v).unwrap_or_else(|| "Failed to cancel order".into());
    tracing::warn!(broker = b.cfg.id, "Cancel of {} refused: {}", order_id, msg);
    Err(noren_error(b.cfg.name, &msg))
}

pub async fn cancel_all_orders(b: &NorenBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let s = session(b.cfg, auth)?;
    let mut result = CancelAllResult::default();
    for o in raw_book(b, &s, "/OrderBook").await? {
        if mapping::normalize_status(&text(&o, "status")) != "open" {
            continue;
        }
        let id = text(&o, "norenordno");
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
    b: &NorenBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let s = session(b.cfg, auth)?;
    let br = b
        .resolver()
        .br_symbol(symbol, exchange.as_str())
        .unwrap_or_else(|| symbol.to_string());
    let prd = product_code(product);
    for p in raw_book(b, &s, "/PositionBook").await? {
        if text(&p, "tsym") == br && text(&p, "exch") == exchange.as_str() && text(&p, "prd") == prd
        {
            return Ok(mapping::i(&p, "netqty"));
        }
    }
    Ok(0)
}

pub async fn close_all_positions(b: &NorenBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let s = session(b.cfg, auth)?;
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in raw_book(b, &s, "/PositionBook").await? {
        let qty = mapping::i(&p, "netqty");
        if qty == 0 {
            continue;
        }
        let exch = text(&p, "exch");
        let symbol = mapping::oa_symbol(&symbols, &exch, &text(&p, "token"), &text(&p, "tsym"));
        let label = format!("{} ({})", symbol, exch);
        let Some(product) = reverse_product(&text(&p, "prd")) else {
            result
                .failed
                .push(format!("{}: unknown product {}", label, text(&p, "prd")));
            continue;
        };
        let req = OrderRequest {
            symbol,
            exchange: exch.clone(),
            side: if qty > 0 { "SELL" } else { "BUY" }.into(),
            quantity: i32::try_from(qty.abs()).unwrap_or(i32::MAX),
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

pub async fn get_order_book(b: &NorenBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let s = session(b.cfg, auth)?;
    let rows = raw_book(b, &s, "/OrderBook").await?;
    Ok(rows
        .iter()
        .map(|o| mapping::map_order(b.cfg, o, b.resolver()))
        .collect())
}

pub async fn get_trade_book(b: &NorenBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let s = session(b.cfg, auth)?;
    let rows = raw_book(b, &s, "/TradeBook").await?;
    Ok(rows
        .iter()
        .map(|t| mapping::map_trade(b.cfg, t, b.resolver()))
        .collect())
}

pub async fn get_positions(b: &NorenBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    let s = session(b.cfg, auth)?;
    let rows = raw_book(b, &s, "/PositionBook").await?;
    Ok(rows
        .iter()
        .map(|p| mapping::map_position(b.cfg, p, b.resolver()))
        .collect())
}

pub async fn get_holdings(b: &NorenBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let s = session(b.cfg, auth)?;
    let v = b
        .post_raw(
            "/Holdings",
            json!({"uid": s.uid, "actid": s.uid, "prd": "C"}),
            &s,
            Category::Data,
        )
        .await?;
    let rows = as_list(b.cfg.name, "/Holdings", v)?;
    Ok(mapping::map_holdings(b.cfg, &rows, b.resolver()))
}
