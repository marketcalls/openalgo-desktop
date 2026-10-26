//! Orders and books (web `api/order_api.py`, `mapping/transform_data.py`,
//! https://vortex.rupeezy.in/docs/latest/regular-order/).

use super::books::{self, VortexHolding, VortexOrder, VortexPosition, VortexTrade};
use super::client::{order_path, Answer, Call};
use super::data::num;
use super::mapping::{
    self, from_vortex_quantity, is_market, is_stop, oa_exchange, product_from_vortex, row_ticker,
    split_ticker, to_vortex_quantity,
};
use super::RupeezyBroker;
use crate::brokers::common::book_read::{self, Book};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::outcome;
use crate::brokers::common::position_read::unread;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

const REGULAR: &str = "/trading/orders/regular";
const POSITIONS: &str = "/trading/portfolio/positions";
/// The order book pages with limit/offset (offset from 1; the web reads it
/// as a record offset, which only matters past 500 orders a day).
const ORDER_PAGE_SIZE: usize = 500;
const MAX_ORDER_PAGES: usize = 20;
/// `/data/quotes` takes up to 1000 tickers a call.
pub(crate) const QUOTE_BATCH: usize = 1000;

/// The master's lot size for the MCX conversion (1 when it has none).
pub(crate) fn lot_of(row: &SymToken) -> i64 {
    i64::from(row.lot_size).max(1)
}

/// The rows under the first of `keys` that holds a non-empty list (the
/// web's `payload.get("orders") or payload.get("data")`). `None` when none
/// of the keys holds a list, `null` included, or when a row does not read:
/// a book that was not read, never an empty one (BR-03). The web's `or []`
/// also reads `null` as no orders, but no recorded Vortex reply and no
/// Vortex document sends `null` for an empty book (`[]` is the empty one),
/// so a `null` list is not believed: cancel all would cancel nothing and
/// call it done.
pub(crate) fn rows_of<T: DeserializeOwned>(v: &Value, keys: &[&str]) -> Option<Vec<T>> {
    let lists: Vec<&Vec<Value>> = keys
        .iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_array))
        .collect();
    let first = lists.first()?;
    let list = lists.iter().find(|l| !l.is_empty()).unwrap_or(first);
    list.iter()
        .map(|r| {
            r.is_object()
                .then(|| serde_json::from_value(r.clone()).ok())
                .flatten()
        })
        .collect()
}

/// The order id in a `{data: {order_id}}` answer.
fn order_id_of(v: &Value) -> String {
    match v.get("data").and_then(|d| d.get("order_id")) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn price_fields(m: &mut Map<String, Value>, variety: &str, price: f64, trigger: f64) {
    // Market orders go with price 0 (Vortex docs); only stops carry a
    // trigger.
    m.insert(
        "price".into(),
        json!(if is_market(variety) { 0.0 } else { price }),
    );
    m.insert(
        "trigger_price".into(),
        json!(if is_stop(variety) { trigger } else { 0.0 }),
    );
}

/// The `order_identifier` the web sends: the request's strategy, else
/// `openalgo`, cut to Vortex's 50 characters (`str(strategy or
/// "openalgo")[:50]`). Vortex returns it on the order book.
pub(crate) fn order_identifier(strategy: &str) -> String {
    let s = if strategy.is_empty() {
        "openalgo"
    } else {
        strategy
    };
    s.chars().take(50).collect()
}

/// The strategy close-all sends its exits with (web `close_all_positions`).
const SQUARE_OFF: &str = "Squareoff";

/// The place-order body (web `transform_data`). Quantities go in Vortex
/// units: lots on MCX, refused when not a whole number of lots.
pub(crate) fn place_order_body(o: &ResolvedOrder, strategy: &str) -> Result<Value> {
    let row = &o.instrument;
    let (brex, lot) = (row.br_exchange(), lot_of(row));
    let variety = mapping::variety(o.pricetype);
    let mut m = Map::new();
    m.insert("ticker".into(), json!(row_ticker(row)));
    m.insert("transaction_type".into(), json!(o.action.as_str()));
    m.insert("product".into(), json!(mapping::product(o.product)));
    m.insert("variety".into(), json!(variety));
    m.insert(
        "quantity".into(),
        json!(to_vortex_quantity(o.quantity, brex, lot)?),
    );
    price_fields(&mut m, variety, o.price, o.trigger_price);
    m.insert(
        "disclosed_quantity".into(),
        json!(to_vortex_quantity(o.disclosed_quantity, brex, lot)?),
    );
    m.insert("validity".into(), json!("DAY"));
    m.insert("is_amo".into(), json!(false));
    m.insert("order_identifier".into(), json!(order_identifier(strategy)));
    Ok(Value::Object(m))
}

/// The modify body without `traded_quantity`, which is read from the
/// order's latest state just before sending (web
/// `transform_modify_order_data`).
pub(crate) fn modify_order_body(m: &ResolvedModify, traded_quantity: i64) -> Result<Value> {
    let row = &m.instrument;
    let (brex, lot) = (row.br_exchange(), lot_of(row));
    let variety = mapping::variety(m.pricetype);
    let mut b = Map::new();
    b.insert("variety".into(), json!(variety));
    b.insert(
        "quantity".into(),
        json!(to_vortex_quantity(m.quantity, brex, lot)?),
    );
    // Vortex grows the order by whatever traded unless this is current.
    b.insert("traded_quantity".into(), json!(traded_quantity));
    price_fields(&mut b, variety, m.price, m.trigger_price);
    b.insert(
        "disclosed_quantity".into(),
        json!(to_vortex_quantity(m.disclosed_quantity, brex, lot)?),
    );
    b.insert("validity".into(), json!("DAY"));
    Ok(Value::Object(b))
}

/// Place one order tagged with its strategy (`""`: `openalgo`).
pub async fn place_order(
    b: &RupeezyBroker,
    auth: &AuthToken,
    o: &ResolvedOrder,
    strategy: &str,
) -> Result<OrderResponse> {
    let body = place_order_body(o, strategy)?;
    let a = b
        .call(auth, Call::write(Method::POST, REGULAR, Some(&body)))
        .await?;
    if a.succeeded() {
        let id = order_id_of(&a.body);
        if id.is_empty() {
            // Accepted with no id: the order may exist (LOG-08).
            tracing::warn!("Rupeezy accepted an order without an order id");
            return Err(AppError::uncertain(
                "Rupeezy accepted the order but returned no order id. Check the order book before placing it again.",
                None,
            ));
        }
        return Ok(OrderResponse {
            order_id: id,
            message: None,
        });
    }
    Err(not_placed(&a))
}

/// A placement answer without an order id: refused only when Vortex said so
/// on a 2xx/4xx; a 5xx or a 2xx without its failure marker stays unknown.
fn not_placed(a: &Answer) -> AppError {
    let msg = a.message();
    tracing::warn!(
        status = a.status.as_u16(),
        "Rupeezy did not place an order: {}",
        msg
    );
    outcome::not_accepted(
        a.status.as_u16(),
        a.failed(),
        if msg.is_empty() {
            "Rupeezy did not accept the order.".to_string()
        } else {
            msg
        },
    )
}

/// The order's newest history row (Vortex lists newest first). Live Vortex
/// answers under `orders`, the docs say `data`.
async fn latest_order_state(
    b: &RupeezyBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<VortexOrder> {
    let a = b
        .call(auth, Call::get(&order_path("/trading/orders", order_id)))
        .await?;
    if !a.succeeded() {
        // Unreadable is not missing: a session or server error says
        // nothing about the order.
        return Err(a.refused("an order's history"));
    }
    rows_of::<VortexOrder>(&a.body, &["orders", "data"])
        .and_then(|rows| rows.into_iter().next())
        .ok_or_else(|| AppError::NotFound(format!("Order {} was not found.", order_id)))
}

pub async fn modify_order(
    b: &RupeezyBroker,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    // Refuse a quantity Vortex cannot take before reading anything.
    modify_order_body(m, 0)?;
    let state = latest_order_state(b, auth, &m.order_id).await?;
    let body = modify_order_body(m, state.traded_quantity)?;
    let a = b
        .call(
            auth,
            Call::write(Method::PUT, &order_path(REGULAR, &m.order_id), Some(&body)),
        )
        .await?;
    if !a.succeeded() {
        return Err(a.refused("an order change"));
    }
    let id = order_id_of(&a.body);
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
    b: &RupeezyBroker,
    auth: &AuthToken,
    order_id: &str,
) -> Result<OrderResponse> {
    let a = b
        .call(
            auth,
            Call::write(Method::DELETE, &order_path(REGULAR, order_id), None),
        )
        .await?;
    if !a.succeeded() {
        return Err(a.refused("a cancel"));
    }
    Ok(OrderResponse {
        order_id: order_id.to_string(),
        message: None,
    })
}

/// Today's orders, page by page, read with `unread` when a page came back
/// without its order list (BR-03: a page missing would hide open orders).
async fn fetch_orders(
    b: &RupeezyBroker,
    auth: &AuthToken,
    unread: impl Fn() -> AppError,
) -> Result<Vec<VortexOrder>> {
    let mut out = Vec::new();
    for page in 0..MAX_ORDER_PAGES {
        let q = [
            ("limit", ORDER_PAGE_SIZE.to_string()),
            ("offset", (page * ORDER_PAGE_SIZE + 1).to_string()),
        ];
        let v = b.ok(auth, Call::get_with("/trading/orders", &q)).await?;
        let Some(batch) = rows_of::<VortexOrder>(&v, &["orders", "data"]) else {
            tracing::error!("Rupeezy answered the order book without its orders");
            return Err(unread());
        };
        let n = batch.len();
        out.extend(batch);
        if n < ORDER_PAGE_SIZE {
            break;
        }
    }
    Ok(out)
}

pub async fn get_order_book(b: &RupeezyBroker, auth: &AuthToken) -> Result<Vec<Order>> {
    let rows = fetch_orders(b, auth, || book_read::unreadable("rupeezy", Book::Orders)).await?;
    Ok(books::map_orders(&rows, b.resolver()))
}

/// Cancel every open or trigger-pending order. An order book that cannot
/// be read is an error, never "nothing to cancel".
pub async fn cancel_all_orders(b: &RupeezyBroker, auth: &AuthToken) -> Result<CancelAllResult> {
    let orders = fetch_orders(b, auth, || {
        AppError::Broker(
            "Rupeezy did not return your order book, so no order was cancelled. Check your orders and try again."
                .into(),
        )
    })
    .await?;
    let mut result = CancelAllResult::default();
    for o in orders.iter().filter(|o| mapping::is_open(&o.status)) {
        match cancel_order(b, auth, &o.order_id).await {
            Ok(_) => result.cancelled.push(o.order_id.clone()),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.order_id, e.code());
                result.failed.push(o.order_id.clone())
            }
        }
    }
    Ok(result)
}

/// Today's trades. Rows that arrive without a ticker are completed from the
/// order book by order id, at the cost of one more call; if the order book
/// cannot be read the trades are shown as they came.
pub async fn get_trade_book(b: &RupeezyBroker, auth: &AuthToken) -> Result<Vec<Trade>> {
    let v = b.ok(auth, Call::get("/trading/trades")).await?;
    let mut trades: Vec<VortexTrade> = rows_of(&v, &["trades", "data"])
        .ok_or_else(|| book_read::unreadable("rupeezy", Book::Trades))?;
    if trades.iter().any(|t| t.ticker.trim().is_empty()) {
        match fetch_orders(b, auth, || book_read::unreadable("rupeezy", Book::Orders)).await {
            Ok(orders) => {
                let by_id: HashMap<&str, &VortexOrder> =
                    orders.iter().map(|o| (o.order_id.as_str(), o)).collect();
                for t in trades.iter_mut() {
                    if let Some(o) = by_id.get(t.order_id.as_str()) {
                        t.fill_from(o);
                    }
                }
            }
            Err(e) => tracing::warn!("Rupeezy trade book: order book unavailable ({})", e.code()),
        }
    }
    Ok(books::map_trades(&trades, b.resolver()))
}

fn is_busy(e: &AppError) -> bool {
    matches!(e, AppError::Busy(_))
}

/// `{ticker: ltp}` from `/data/quotes` in `ltp` mode, 1000 tickers a call.
/// Best effort: a failed batch leaves its tickers out, so their prices show
/// as unknown rather than failing the book. A request refused by the
/// pacer is passed on.
pub(crate) async fn fetch_ltps(
    b: &RupeezyBroker,
    auth: &AuthToken,
    tickers: &[String],
) -> Result<HashMap<String, f64>> {
    let unique: Vec<&String> = tickers
        .iter()
        .filter(|t| !t.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut out = HashMap::new();
    for batch in unique.chunks(QUOTE_BATCH) {
        let mut q: Vec<(&str, String)> = batch.iter().map(|t| ("q", (*t).clone())).collect();
        q.push(("mode", "ltp".into()));
        let v = match b.ok(auth, Call::get_with("/data/quotes", &q)).await {
            Ok(v) => v,
            Err(e) if is_busy(&e) => return Err(e),
            Err(e) => {
                tracing::warn!("Rupeezy price lookup failed: {}", e.code());
                continue;
            }
        };
        if let Some(data) = v.get("data").and_then(Value::as_object) {
            for (ticker, quote) in data {
                let ltp = num(quote.get("last_trade_price"));
                if ltp != 0.0 {
                    out.insert(ticker.clone(), ltp);
                }
            }
        }
    }
    Ok(out)
}

/// `data.net` of a positions answer (Vortex's empty book is `net: []`),
/// read with `unread` when it is not a list or a row has no readable
/// quantity: never an empty book or a flat position (BR-03).
fn net_rows(v: &Value, unread: impl Fn() -> AppError) -> Result<Vec<VortexPosition>> {
    let rows = match v.get("data") {
        Some(d @ Value::Object(_)) => book_read::list(d, "net", &unread)?,
        Some(Value::Array(rows)) => rows.as_slice(),
        _ => return Err(unread()),
    };
    book_read::require_quantities(rows, &["quantity"], &unread)?;
    book_read::typed(rows, unread)
}

/// The position book for a decision to trade (smart-order sizing, close
/// all): the answer must succeed and carry `data.net`, or no order is sent.
/// No quotes are read: an order should not wait on a price.
async fn strict_positions(b: &RupeezyBroker, auth: &AuthToken) -> Result<Vec<VortexPosition>> {
    let v = b.ok(auth, Call::get(POSITIONS)).await?;
    net_rows(&v, || unread("Rupeezy"))
}

/// The position book as the Positions page and funds read it: open rows
/// priced from one batched quotes call.
pub(crate) async fn priced_positions(
    b: &RupeezyBroker,
    auth: &AuthToken,
) -> Result<Vec<VortexPosition>> {
    let v = b.ok(auth, Call::get(POSITIONS)).await?;
    let mut rows = net_rows(&v, || book_read::unreadable("rupeezy", Book::Positions))?;
    let open: Vec<String> = rows
        .iter()
        .filter(|p| p.quantity != 0)
        .map(|p| p.ticker.clone())
        .collect();
    if !open.is_empty() {
        let ltps = fetch_ltps(b, auth, &open).await?;
        for p in rows.iter_mut().filter(|p| p.quantity != 0) {
            if let Some(ltp) = ltps.get(&p.ticker) {
                p.ltp = *ltp;
            }
        }
    }
    Ok(rows)
}

pub async fn get_positions(b: &RupeezyBroker, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(books::map_positions(
        &priced_positions(b, auth).await?,
        b.resolver(),
    ))
}

/// Holdings. A row without Vortex's `last_price` is priced from one
/// batched quotes call.
pub async fn get_holdings(b: &RupeezyBroker, auth: &AuthToken) -> Result<Vec<Holding>> {
    let a = b
        .call(auth, Call::get("/trading/portfolio/holdings"))
        .await?;
    if !a.status.is_success() || a.failed() {
        return Err(a.refused("the holdings"));
    }
    let mut rows: Vec<VortexHolding> = rows_of(&a.body, &["data"])
        .ok_or_else(|| book_read::unreadable("rupeezy", Book::Holdings))?;
    let unpriced: Vec<String> = rows
        .iter()
        .filter(|h| h.last_price == 0.0)
        .filter_map(|h| h.listing().map(|l| l.ticker.clone()))
        .collect();
    if !unpriced.is_empty() {
        let ltps = fetch_ltps(b, auth, &unpriced).await?;
        for h in rows.iter_mut().filter(|h| h.last_price == 0.0) {
            let ltp = h.listing().and_then(|l| ltps.get(&l.ticker)).copied();
            if let Some(ltp) = ltp {
                h.last_price = ltp;
            }
        }
    }
    Ok(books::map_holdings(&rows, b.resolver()))
}

/// Net quantity in OpenAlgo units for a symbol and product, matched on the
/// Vortex ticker and Vortex product (web `get_open_position`).
pub async fn get_open_position(
    b: &RupeezyBroker,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let row = b.instrument(&QuoteKey::new(exchange.as_str(), symbol))?;
    let ticker = row_ticker(&row);
    let wanted = mapping::product(product);
    Ok(strict_positions(b, auth)
        .await?
        .iter()
        .find(|p| p.ticker == ticker && p.product.eq_ignore_ascii_case(wanted))
        .map(|p| from_vortex_quantity(p.quantity, row.br_exchange(), lot_of(&row)))
        .unwrap_or(0))
}

/// The MARKET exit for one open position; an MCX row without its own lot
/// size takes the master's, so two lots leave as 200 units, not 2.
fn exit_request(p: &VortexPosition, row: &SymToken, exchange: &str) -> OrderRequest {
    let lot = if p.lot_size > 0 {
        p.lot_size
    } else {
        lot_of(row)
    };
    let units = from_vortex_quantity(p.quantity, &p.exchange, lot);
    OrderRequest {
        symbol: row.symbol.clone(),
        exchange: exchange.to_string(),
        side: if units > 0 { "SELL" } else { "BUY" }.into(),
        quantity: i32::try_from(units.abs()).unwrap_or(i32::MAX),
        price: 0.0,
        order_type: PriceType::Market.as_str().into(),
        product: product_from_vortex(exchange, &p.product),
        validity: "DAY".into(),
        trigger_price: None,
        disclosed_quantity: None,
        amo: false,
    }
}

/// Square off every open net position at MARKET. A position book that
/// cannot be read is an error, not "no positions"; each exit is recorded
/// as accepted, refused (with Vortex's reason) or without a definite
/// answer (LOG-08).
pub async fn close_all_positions(b: &RupeezyBroker, auth: &AuthToken) -> Result<CloseAllResult> {
    let positions = strict_positions(b, auth).await?;
    let symbols = b.resolver().clone();
    let mut result = CloseAllResult::default();
    for p in positions.iter().filter(|p| p.quantity != 0) {
        let exchange = oa_exchange(&p.exchange);
        let Some(row) = symbols.by_brsymbol(&exchange, split_ticker(&p.ticker)) else {
            tracing::error!(
                "Square-off skipped: {} is not in the master contract",
                p.ticker
            );
            result
                .failed
                .push(format!("{}: not in the master contract", p.ticker));
            continue;
        };
        let label = format!("{} ({})", row.symbol, exchange);
        let req = exit_request(p, &row, &exchange);
        let placed = match ResolvedOrder::resolve(&req, &symbols) {
            Ok(o) => outcome::placing(place_order(b, auth, &o, SQUARE_OFF)).await,
            Err(e) => Err(e),
        };
        result.record(&label, placed);
    }
    Ok(result)
}
