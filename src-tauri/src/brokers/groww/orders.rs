//! Orders and books (web `api/order_api.py`, aligned with Groww's API docs
//! in #2194).

use super::mapping::{
    self, GrowwHolding, GrowwOrder, GrowwPosition, GrowwTrade, SEGMENT_CASH, SEGMENT_FNO,
};
use super::{error_message, groww_error, in_transit, Category, GrowwCore, Reply};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::common::redact::url_safe_error;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Orders per page of `/v1/order/list` (Groww's documented maximum).
pub const PAGE_SIZE: usize = 100;
/// Fills per page of `/v1/order/trades/{id}` (Groww's cap; an order can
/// have more, so every page is read).
pub const TRADES_PAGE_SIZE: usize = 50;
/// Pages read per list at most, so a broker that never returns a short page
/// cannot loop forever.
const MAX_PAGES: usize = 100;
/// Instruments per `/v1/live-data/ltp` call.
pub const LTP_BATCH: usize = 50;
/// A smart order whose position could not be read (web
/// `utils/position_read.py`): nothing is sent.
pub const POSITION_UNREAD: &str = "OpenAlgo could not read your open position from Groww, so no order was sent. Check your positions and try again.";

/// The `/v1/order/create` body (web `direct_place_order_api`). SL is a
/// stop-limit order: it carries both its limit price and its trigger.
pub fn place_order_body(o: &ResolvedOrder, reference_id: &str) -> Result<Value> {
    let ex = o.exchange.as_str();
    let mut m = Map::new();
    m.insert("trading_symbol".into(), json!(o.brsymbol()));
    m.insert("quantity".into(), json!(o.quantity));
    m.insert("validity".into(), json!(mapping::validity(o.validity)?));
    m.insert("exchange".into(), json!(mapping::order_exchange(ex)?));
    m.insert("segment".into(), json!(mapping::order_segment(ex)?));
    m.insert("product".into(), json!(mapping::product(o.product)));
    m.insert("order_type".into(), json!(mapping::order_type(o.pricetype)));
    m.insert("transaction_type".into(), json!(o.action.as_str()));
    m.insert("order_reference_id".into(), json!(reference_id));
    if matches!(o.pricetype, PriceType::Limit | PriceType::Sl) {
        m.insert("price".into(), json!(o.price));
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        m.insert("trigger_price".into(), json!(o.trigger_price));
    }
    Ok(Value::Object(m))
}

/// The `/v1/order/modify` body (web `direct_modify_order`): price only for
/// LIMIT / SL and when set, trigger only for SL / SL-M and when set.
pub fn modify_order_body(m: &ResolvedModify) -> Result<Value> {
    let mut b = Map::new();
    b.insert("groww_order_id".into(), json!(m.order_id));
    b.insert("order_type".into(), json!(mapping::order_type(m.pricetype)));
    b.insert(
        "segment".into(),
        json!(mapping::order_segment(m.exchange.as_str())?),
    );
    if m.quantity <= 0 {
        return Err(AppError::Validation(
            "Quantity must be greater than zero.".into(),
        ));
    }
    b.insert("quantity".into(), json!(m.quantity));
    if matches!(m.pricetype, PriceType::Limit | PriceType::Sl) && m.price != 0.0 {
        b.insert("price".into(), json!(m.price));
    }
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) && m.trigger_price != 0.0 {
        b.insert("trigger_price".into(), json!(m.trigger_price));
    }
    Ok(Value::Object(b))
}

/// Inputs refused before anything is sent (web: unsupported exchange or
/// validity, unknown symbol, a trading symbol shared by several series).
pub fn validate(core: &GrowwCore, o: &ResolvedOrder) -> Result<()> {
    let ex = o.exchange.as_str();
    // An exchange Groww cannot trade is the reason to give first.
    mapping::order_exchange(ex)?;
    mapping::validity(o.validity)?;
    if o.quantity <= 0 {
        return Err(AppError::Validation(
            "Quantity must be greater than zero.".into(),
        ));
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) && o.trigger_price <= 0.0 {
        return Err(AppError::Validation(
            "Trigger price is required for Stop Loss orders.".into(),
        ));
    }
    if o.instrument.brsymbol.trim().is_empty() {
        // Groww's trading_symbol comes from its instrument file; a guessed
        // symbol could name a different contract.
        return Err(AppError::Validation(format!(
            "{} is not in the {} master contract. Check the symbol, or download the master contract again.",
            o.symbol, ex
        )));
    }
    let trading_symbol = o.brsymbol();
    let sharing = core
        .symbols
        .snapshot()
        .rows()
        .iter()
        .filter(|r| r.brsymbol == trading_symbol && r.exchange == ex)
        .count();
    if sharing > 1 {
        // Groww lists some NSE bonds under one trading_symbol for several
        // series (IMC1 for N1/N2/N3); an order carries only that symbol.
        return Err(AppError::Validation(format!(
            "{} cannot be ordered through Groww's API: Groww lists {} series under the same trading symbol {} and an order cannot say which one. Place it in the Groww app instead.",
            o.symbol, sharing, trading_symbol
        )));
    }
    Ok(())
}

fn id_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// What a `/v1/order/create` reply means (web `direct_place_order_api`):
/// success only on HTTP 200 + `SUCCESS` with a `groww_order_id` and an
/// order that has not already `FAILED` / been `REJECTED`.
pub fn place_outcome(r: &Reply) -> Result<OrderResponse> {
    if !r.is_success() {
        let m = r.error_message();
        if r.status.is_success() || (r.status.is_client_error() && r.status.as_u16() != 429) {
            return Err(AppError::Broker(if m.is_empty() {
                "Groww did not accept the order.".into()
            } else {
                m
            }));
        }
        if r.status.is_server_error() {
            // A failure on Groww's side does not say the order was not
            // taken: never invite a blind retry.
            tracing::error!(
                status = r.status.as_u16(),
                "Groww order reply was a server error: {}",
                m
            );
            return Err(AppError::uncertain(
                "Groww did not confirm the order. Check the order book before placing it again.",
                None,
            ));
        }
        return Err(groww_error(r));
    }
    let p = r.payload();
    let status = id_text(p.get("order_status")).to_ascii_uppercase();
    if status == "FAILED" || status == "REJECTED" {
        tracing::warn!("Groww {} the order: {}", status, r.body);
        let remark = id_text(p.get("remark"));
        return Err(AppError::Broker(if remark.is_empty() {
            "Groww rejected the order".into()
        } else {
            remark
        }));
    }
    let id = id_text(p.get("groww_order_id"));
    if id.is_empty() {
        tracing::error!("Groww order reply without groww_order_id: {}", r.body);
        return Err(AppError::uncertain(
            "Groww did not return an order ID. Check the order book before placing the order again.",
            None,
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: p
            .get("order_status")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub async fn place_order(
    core: &GrowwCore,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    validate(core, o)?;
    let reference = mapping::new_reference_id(chrono::Local::now().date_naive());
    let body = place_order_body(o, &reference)?;
    let r = core
        .send(
            Method::POST,
            "/v1/order/create",
            auth,
            Some(&body),
            Category::Order,
        )
        .await
        .map_err(|e| in_transit(e, "place"))?;
    place_outcome(&r)
}

/// Success only when Groww says so (HTTP 200 + `SUCCESS`); Groww's
/// `error.message` otherwise.
pub async fn modify_order(
    core: &GrowwCore,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = modify_order_body(m)?;
    let payload = core
        .call(
            Method::POST,
            "/v1/order/modify",
            auth,
            Some(&body),
            Category::Order,
        )
        .await
        .map_err(|e| in_transit(e, "modify"))?;
    Ok(OrderResponse {
        order_id: m.order_id.clone(),
        message: Some(
            payload
                .get("order_status")
                .and_then(Value::as_str)
                .unwrap_or("MODIFICATION_REQUESTED")
                .to_string(),
        ),
    })
}

/// Segments to try for an order id (web `_cancel_segments`): its own, read
/// from the order book; CASH then FNO when it is not there.
async fn segments_of(core: &GrowwCore, auth: &AuthToken, order_id: &str) -> Result<Vec<String>> {
    match raw_orders(core, auth).await {
        Ok(book) => {
            if let Some(o) = book.iter().find(|o| o.groww_order_id == order_id) {
                if o.segment == SEGMENT_CASH || o.segment == SEGMENT_FNO {
                    return Ok(vec![o.segment.clone()]);
                }
            }
        }
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => tracing::warn!(
            "Groww order book unreadable while looking up {}: {}",
            order_id,
            e.code()
        ),
    }
    Ok(vec![SEGMENT_CASH.into(), SEGMENT_FNO.into()])
}

/// Cancel (web `cancel_order`): success only on HTTP 200 + `SUCCESS`. A
/// cancel sent to the wrong segment is refused by Groww, so trying the
/// second segment is harmless.
pub async fn cancel_order(
    core: &GrowwCore,
    auth: &AuthToken,
    order_id: &str,
    segment: Option<&str>,
) -> Result<OrderResponse> {
    let segments = match segment {
        Some(s) => vec![s.to_string()],
        None => segments_of(core, auth, order_id).await?,
    };
    let mut last = None;
    for seg in &segments {
        let body = json!({"segment": seg, "groww_order_id": order_id});
        let r = core
            .send(
                Method::POST,
                "/v1/order/cancel",
                auth,
                Some(&body),
                Category::Order,
            )
            .await
            .map_err(|e| in_transit(e, "cancel"))?;
        if r.is_success() {
            return Ok(OrderResponse {
                order_id: order_id.to_string(),
                message: r
                    .payload()
                    .get("order_status")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        tracing::warn!(
            status = r.status.as_u16(),
            "Groww refused cancel of {} in {}: {}",
            order_id,
            seg,
            r.error_message()
        );
        last = Some(r);
    }
    Err(match last {
        Some(r) if !r.error_message().is_empty() => AppError::Broker(r.error_message()),
        Some(r) => groww_error(&r),
        None => AppError::Broker(format!("Groww could not cancel order {}.", order_id)),
    })
}

/// Every page of a Groww list endpoint (web `_get_paged`). `Err` is a
/// fatal failure (session gone); the inner error is Groww's reason for a
/// page it refused.
async fn paged<T: serde::de::DeserializeOwned>(
    core: &GrowwCore,
    auth: &AuthToken,
    path: &str,
    segment: &str,
    list_key: &str,
    page_size: usize,
) -> Result<std::result::Result<Vec<T>, String>> {
    let mut out = Vec::new();
    for page in 0..MAX_PAGES {
        let url = format!(
            "{}{}segment={}&page={}&page_size={}",
            path,
            if path.contains('?') { "&" } else { "?" },
            segment,
            page,
            page_size
        );
        let r = core
            .send(Method::GET, &url, auth, None, Category::NonTrading)
            .await?;
        if !r.is_success() {
            // Groww's reason, empty when it gave none (`send` has logged
            // the status).
            return Ok(Err(r.error_message()));
        }
        let list: Vec<T> = r
            .payload()
            .get(list_key)
            .cloned()
            .filter(|v| !v.is_null())
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let n = list.len();
        out.extend(list);
        if n < page_size {
            break;
        }
    }
    Ok(Ok(out))
}

/// Both segments' orders (web `direct_get_order_book`). A CASH read that
/// fails is an error, not an empty book; an FNO read that fails is skipped,
/// since it fails on accounts without F&O.
pub(crate) async fn raw_orders(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<GrowwOrder>> {
    let mut all = Vec::new();
    for segment in [SEGMENT_CASH, SEGMENT_FNO] {
        match paged::<GrowwOrder>(
            core,
            auth,
            "/v1/order/list",
            segment,
            "order_list",
            PAGE_SIZE,
        )
        .await?
        {
            Ok(list) => all.extend(list),
            Err(why) if mapping::says_no_positions(&why) => {}
            Err(why) if segment == SEGMENT_CASH => {
                tracing::error!("Groww order list (CASH) could not be read: {}", why);
                return Err(AppError::Broker(if why.is_empty() {
                    "Groww did not return your order book. Try again in a moment.".into()
                } else {
                    format!("Could not read the Groww order book: {}", why)
                }));
            }
            Err(why) => {
                tracing::warn!("Groww order list (FNO) could not be read, skipped: {}", why)
            }
        }
    }
    Ok(all)
}

pub async fn get_order_book(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        &raw_orders(core, auth).await?,
        &core.symbols,
    ))
}

/// Cancel every open order (web `cancel_all_orders_api`). An unreadable
/// order book is an error, not "nothing to cancel".
pub async fn cancel_all_orders(core: &GrowwCore, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(core, auth).await? {
        if !mapping::is_cancellable(&o.order_status) || o.groww_order_id.is_empty() {
            continue;
        }
        let seg =
            (o.segment == SEGMENT_CASH || o.segment == SEGMENT_FNO).then_some(o.segment.as_str());
        match cancel_order(core, auth, &o.groww_order_id, seg).await {
            Ok(_) => result.cancelled.push(o.groww_order_id),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.groww_order_id, e.code());
                result.failed.push(o.groww_order_id)
            }
        }
    }
    Ok(result)
}

/// Every fill of one order (web `get_order_trades`), in OpenAlgo terms.
/// `Ok(Err(reason))` when Groww did not return them.
async fn order_trades(
    core: &GrowwCore,
    auth: &AuthToken,
    order: &GrowwOrder,
) -> Result<std::result::Result<Vec<Trade>, String>> {
    let segments = if order.segment == SEGMENT_CASH || order.segment == SEGMENT_FNO {
        vec![order.segment.clone()]
    } else {
        segments_of(core, auth, &order.groww_order_id).await?
    };
    let path = format!(
        "/v1/order/trades/{}",
        urlencoding::encode(&order.groww_order_id)
    );
    let mut why = format!(
        "Groww returned no trades for order {}",
        order.groww_order_id
    );
    for seg in &segments {
        match paged::<GrowwTrade>(core, auth, &path, seg, "trade_list", TRADES_PAGE_SIZE).await? {
            Ok(list) => {
                return Ok(Ok(list
                    .iter()
                    .map(|t| mapping::map_trade(t, &order.groww_order_id, seg, &core.symbols))
                    .collect()))
            }
            Err(e) if !e.is_empty() => why = e,
            Err(_) => {}
        }
    }
    Ok(Err(why))
}

/// Every fill of the day (web `get_trade_book`). Groww has no account-wide
/// trade list, so the order book names the orders that filled
/// (`filled_quantity > 0`) and each one's fills are read. Nothing is
/// synthesised: if Groww cannot return an order's trades, the trade book is
/// an error rather than an invented fill.
pub async fn get_trade_book(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Trade>> {
    let orders = raw_orders(core, auth).await?;
    let mut out = Vec::new();
    let mut failed = 0usize;
    for o in orders
        .iter()
        .filter(|o| o.filled_quantity > 0 && !o.groww_order_id.is_empty())
    {
        match order_trades(core, auth, o).await? {
            Ok(t) => out.extend(t),
            Err(why) => {
                tracing::error!(
                    "Groww trades for order {} could not be read: {}",
                    o.groww_order_id,
                    why
                );
                failed += 1;
            }
        }
    }
    if failed > 0 {
        return Err(AppError::Broker(format!(
            "Groww did not return the trades for {} filled order(s). Try again shortly; the order book shows their fills.",
            failed
        )));
    }
    Ok(out)
}

/// Last traded prices from `/v1/live-data/ltp` (web `_live_prices`), 50
/// instruments per call, keyed `EXCHANGE_TRADINGSYMBOL`. A batch Groww
/// refuses is logged and left out: no price rather than a wrong one.
pub(crate) async fn live_prices(
    core: &GrowwCore,
    auth: &AuthToken,
    keys_by_segment: &[(&str, Vec<String>)],
) -> Result<HashMap<String, f64>> {
    let mut prices = HashMap::new();
    for (segment, keys) in keys_by_segment {
        let mut unique: Vec<&String> = Vec::new();
        for k in keys {
            if !unique.contains(&k) {
                unique.push(k);
            }
        }
        for batch in unique.chunks(LTP_BATCH) {
            let joined: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
            let path = format!(
                "/v1/live-data/ltp?segment={}&exchange_symbols={}",
                segment,
                urlencoding::encode(&joined.join(","))
            );
            let r = match core
                .send(Method::GET, &path, auth, None, Category::Live)
                .await
            {
                Ok(r) => r,
                Err(AppError::Http(e)) => {
                    // As the web does: no price rather than a failed book.
                    tracing::warn!(
                        "Groww LTP for {} could not be read: {}",
                        segment,
                        url_safe_error(&e)
                    );
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !r.is_success() || !r.payload().is_object() {
                tracing::warn!(
                    "Groww LTP for {} refused: {}",
                    segment,
                    error_message(&r.body)
                );
                continue;
            }
            for k in batch {
                let v = super::data::to_f64(r.payload().get(k.as_str()));
                if v > 0.0 && v.is_finite() {
                    prices.insert((*k).clone(), v);
                }
            }
        }
    }
    Ok(prices)
}

/// The position book (web `get_positions`).
pub(crate) struct PositionRead {
    pub rows: Vec<Position>,
    /// Segments a strict read could not read (FNO only; a CASH failure in a
    /// strict read is an error).
    pub failed: Vec<&'static str>,
}

/// One segment's raw positions; `Ok(Err(reason))` when Groww refused.
async fn segment_positions(
    core: &GrowwCore,
    auth: &AuthToken,
    segment: &str,
) -> Result<std::result::Result<Vec<GrowwPosition>, String>> {
    let r = core
        .send(
            Method::GET,
            &format!("/v1/positions/user?segment={}", segment),
            auth,
            None,
            Category::NonTrading,
        )
        .await?;
    if !r.is_success() {
        let m = r.error_message();
        if mapping::says_no_positions(&m) {
            return Ok(Ok(Vec::new()));
        }
        return Ok(Err(if m.is_empty() {
            format!("HTTP {}", r.status.as_u16())
        } else {
            m
        }));
    }
    Ok(Ok(r
        .payload()
        .get("positions")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default()))
}

/// Both segments' positions (web `get_positions(strict, include_ltp)`).
/// `strict` (the smart-order read) reports an unreadable CASH segment as an
/// error and an unreadable FNO segment in `failed`; otherwise a refused
/// segment is logged and left out. `include_ltp` adds live prices and P&L.
pub(crate) async fn read_positions(
    core: &GrowwCore,
    auth: &AuthToken,
    strict: bool,
    include_ltp: bool,
) -> Result<PositionRead> {
    let mut raw: Vec<(GrowwPosition, &'static str)> = Vec::new();
    match segment_positions(core, auth, SEGMENT_CASH).await? {
        Ok(rows) => raw.extend(rows.into_iter().map(|p| (p, SEGMENT_CASH))),
        Err(why) if strict => {
            tracing::error!("Groww position book incomplete: CASH segment: {}", why);
            return Err(AppError::Broker(POSITION_UNREAD.into()));
        }
        Err(why) => tracing::warn!("Groww CASH positions could not be read: {}", why),
    }
    let mut failed = Vec::new();
    match segment_positions(core, auth, SEGMENT_FNO).await? {
        Ok(rows) => raw.extend(rows.into_iter().map(|p| (p, SEGMENT_FNO))),
        Err(why) => {
            tracing::warn!("Groww FNO positions could not be read: {}", why);
            if strict {
                failed.push(SEGMENT_FNO);
            }
        }
    }
    let mut rows: Vec<Position> = raw
        .iter()
        .map(|(p, seg)| mapping::map_position(p, seg, &core.symbols))
        .collect();
    if include_ltp {
        let key = |p: &GrowwPosition| mapping::ltp_key(&p.exchange, &p.trading_symbol);
        let mut wanted: Vec<(&str, Vec<String>)> = Vec::new();
        for ((p, seg), row) in raw.iter().zip(&rows) {
            if row.quantity == 0 || p.trading_symbol.is_empty() {
                continue;
            }
            let seg: &str = if p.segment.is_empty() {
                seg
            } else {
                &p.segment
            };
            match wanted.iter_mut().find(|(s, _)| *s == seg) {
                Some((_, v)) => v.push(key(p)),
                None => wanted.push((seg, vec![key(p)])),
            }
        }
        if !wanted.is_empty() {
            let prices = live_prices(core, auth, &wanted).await?;
            for ((p, _), row) in raw.iter().zip(rows.iter_mut()) {
                mapping::attach_ltp(row, prices.get(&key(p)).copied());
            }
        }
    }
    Ok(PositionRead { rows, failed })
}

pub async fn get_positions(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(read_positions(core, auth, false, true).await?.rows)
}

/// Square off every open position at MARKET (web `close_all_positions`):
/// the position book read without live prices, one exit order each for
/// the OpenAlgo symbol, exchange and product of the row.
pub async fn close_all_positions(core: &GrowwCore, auth: &AuthToken) -> Result<CloseAllResult> {
    let positions = read_positions(core, auth, false, false).await?.rows;
    let mut result = CloseAllResult::default();
    for p in positions.into_iter().filter(|p| p.quantity != 0) {
        let label = format!("{} ({})", p.symbol, p.exchange);
        let req = OrderRequest {
            symbol: p.symbol.clone(),
            exchange: p.exchange.clone(),
            side: if p.quantity > 0 { "SELL" } else { "BUY" }.into(),
            quantity: p.quantity.abs(),
            price: 0.0,
            order_type: PriceType::Market.as_str().into(),
            product: p.product.clone(),
            validity: "DAY".into(),
            trigger_price: None,
            disclosed_quantity: None,
            amo: false,
        };
        let placed = match ResolvedOrder::resolve(&req, &core.symbols) {
            Ok(o) => place_order(core, auth, &o).await,
            Err(e) => Err(e),
        };
        match placed {
            Ok(r) if !r.order_id.is_empty() => result.placed.push(r.order_id),
            Ok(_) => result.failed.push(format!("{}: order was refused", label)),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => result
                .failed
                .push(format!("{}: {}", label, e.client_message())),
        }
    }
    Ok(result)
}

/// Net quantity for a smart order (web `get_open_position`): a strict read
/// without prices. A failed read of the segment the symbol trades in is an
/// error, never a flat 0 (web `PositionReadError`).
pub async fn get_open_position(
    core: &GrowwCore,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let read = read_positions(core, auth, true, false).await?;
    if !read.failed.is_empty() {
        let covered = mapping::segment_of(exchange);
        if covered.is_none_or(|s| read.failed.contains(&s)) {
            tracing::error!(
                "Groww position book incomplete ({:?} not read); open position of {} on {} unknown",
                read.failed,
                symbol,
                exchange.as_str()
            );
            return Err(AppError::Broker(POSITION_UNREAD.into()));
        }
    }
    Ok(read
        .rows
        .iter()
        .find(|p| {
            p.symbol == symbol && p.exchange == exchange.as_str() && p.product == product.as_str()
        })
        .map(|p| i64::from(p.quantity))
        .unwrap_or(0))
}

/// Holdings (web `get_holdings`): exchange from the master contract, live
/// price from `/v1/live-data/ltp`, P&L only for a priced holding.
pub async fn get_holdings_book(core: &GrowwCore, auth: &AuthToken) -> Result<HoldingsBook> {
    let r = core
        .send(
            Method::GET,
            "/v1/holdings/user",
            auth,
            None,
            Category::NonTrading,
        )
        .await?;
    if !r.is_success() {
        return Err(groww_error(&r));
    }
    let rows: Vec<GrowwHolding> = r
        .payload()
        .get("holdings")
        .cloned()
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let keys: Vec<String> = rows
        .iter()
        .filter_map(|h| {
            mapping::holding_exchange(h, &core.symbols)
                .map(|ex| mapping::ltp_key(ex, &h.trading_symbol))
        })
        .collect();
    let prices = if keys.is_empty() {
        HashMap::new()
    } else {
        live_prices(core, auth, &[(SEGMENT_CASH, keys)]).await?
    };
    let holdings: Vec<Holding> = rows
        .iter()
        .map(|h| {
            let ltp = mapping::holding_exchange(h, &core.symbols).and_then(|ex| {
                prices
                    .get(&mapping::ltp_key(ex, &h.trading_symbol))
                    .copied()
            });
            mapping::map_holding(h, &core.symbols, ltp)
        })
        .collect();
    let totals = mapping::holdings_stats(&holdings);
    Ok(HoldingsBook {
        holdings,
        totals: Some(totals),
    })
}

pub async fn get_holdings(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Holding>> {
    Ok(get_holdings_book(core, auth).await?.holdings)
}
