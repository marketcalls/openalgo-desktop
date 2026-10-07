//! Books and account (web `orderbook_service.py`, `tradebook_service.py`,
//! `positionbook_service.py`, `holdings_service.py`, `funds_service.py`,
//! `orderstatus_service.py`, `openposition_service.py`, `pnl_symbols.py`).
//!
//! In analyzer mode every call is answered by the sandbox engine with its
//! analyze-mode shape (`"mode":"analyze"`); live calls go to the broker and
//! are shaped like the web's broker `transform_*` functions. Books always
//! carry OpenAlgo symbols (the adapters translate).

use super::core::{broker_handle, float, is_analyze, round2, s, BrokerHandle, Reply, UNEXPECTED};
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::{ExactRow, Funds, Holding, Order, Position, Trade};
use crate::state::AppState;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Value};

fn sandbox<T: serde::Serialize>(r: crate::sandbox::SbResult<T>) -> Reply {
    match r {
        Ok(v) => Reply::from_ser(&v),
        Err(e) => Reply::sandbox(&e),
    }
}

fn broker_fail(e: &crate::error::AppError) -> Reply {
    match e {
        crate::error::AppError::Unsupported(_) => Reply::error(501, e.client_message()),
        _ => {
            tracing::warn!("Broker book request failed: {}", e.code());
            Reply::error(500, e.client_message())
        }
    }
}

fn handle(ctx: &AppState) -> Result<BrokerHandle, Reply> {
    broker_handle(ctx)
}

// ------------------------------------------------------------------ shaping

/// Web orderbook row (`transform_order_data` + `format_order_data`).
pub fn order_row(o: &Order) -> Value {
    let market = o.order_type.eq_ignore_ascii_case("MARKET");
    json!({
        "symbol": o.symbol,
        "exchange": o.exchange,
        "action": o.side,
        "quantity": o.quantity,
        "price": if market { 0.0 } else { round2(o.price) },
        "trigger_price": round2(o.trigger_price),
        "pricetype": o.order_type,
        "product": o.product,
        "orderid": o.order_id,
        "order_status": o.status,
        "timestamp": o.order_timestamp,
    })
}

/// Web `calculate_order_statistics`.
pub fn order_statistics(orders: &[Order]) -> Value {
    let count = |f: &dyn Fn(&Order) -> bool| orders.iter().filter(|o| f(o)).count() as i64;
    json!({
        "total_buy_orders": count(&|o| o.side.eq_ignore_ascii_case("BUY")),
        "total_sell_orders": count(&|o| o.side.eq_ignore_ascii_case("SELL")),
        "total_completed_orders": count(&|o| o.status == "complete"),
        "total_open_orders": count(&|o| o.status == "open"),
        "total_rejected_orders": count(&|o| o.status == "rejected"),
    })
}

pub fn trade_row(t: &Trade) -> Value {
    json!({
        "symbol": t.symbol,
        "exchange": t.exchange,
        "product": t.product,
        "action": t.side,
        "quantity": t.quantity,
        "average_price": round2(t.average_price),
        "trade_value": round2(t.trade_value),
        "orderid": t.order_id,
        "tradeid": t.trade_id,
        "timestamp": t.timestamp,
    })
}

pub fn position_row(p: &Position) -> Value {
    json!({
        "symbol": p.symbol,
        "exchange": p.exchange,
        "product": p.product,
        "quantity": p.quantity,
        "pnl": round2(p.pnl),
        "average_price": format!("{:.2}", p.average_price),
        "ltp": round2(p.ltp),
    })
}

pub fn holding_row(h: &Holding) -> Value {
    let pnlpercent = if h.average_price == 0.0 || h.ltp == 0.0 {
        0.0
    } else {
        round2((h.ltp - h.average_price) / h.average_price * 100.0)
    };
    json!({
        "symbol": h.symbol,
        "exchange": h.exchange,
        "quantity": h.quantity,
        "product": h.product,
        "average_price": float(h.average_price),
        "ltp": float(h.ltp),
        "pnl": round2(h.pnl),
        "pnlpercent": pnlpercent,
    })
}

pub fn holdings_statistics(h: &[Holding]) -> Value {
    let stats = crate::brokers::types::PortfolioStats::from_holdings(h);
    json!({
        "totalholdingvalue": round2(stats.totalholdingvalue),
        "totalinvvalue": round2(stats.totalinvvalue),
        "totalprofitandloss": round2(stats.totalprofitandloss),
        "totalpnlpercentage": round2(stats.totalpnlpercentage),
    })
}

/// Live funds: the web's broker modules format every value to two decimals
/// as strings.
pub fn live_funds(f: &Funds) -> Value {
    let used = if f.utilised_debits != 0.0 {
        f.utilised_debits
    } else {
        f.used_margin
    };
    json!({
        "availablecash": format!("{:.2}", f.available_cash),
        "collateral": format!("{:.2}", f.collateral),
        "m2mrealized": format!("{:.2}", f.m2m_realized),
        "m2munrealized": format!("{:.2}", f.m2m_unrealized),
        "utiliseddebits": format!("{:.2}", used),
    })
}

// ------------------------------------------------------------- crypto sizes
// A crypto venue (Delta Exchange) reports exact, possibly fractional sizes.
// The web's Delta mapping returns them as Python floats (`float(size)`) for
// positions and trades and as the raw size for orders, and adds the
// contract multiplier as `lot_size` to position rows. Only `CRYPTO` rows of
// a `crypto` venue take this branch; every other book is unchanged.

fn crypto_venue(h: &BrokerHandle) -> bool {
    h.broker.broker_type() == "crypto"
}

fn is_crypto_row(exchange: &str) -> bool {
    exchange == "CRYPTO"
}

/// Python `float(size)`.
pub fn crypto_float(d: Decimal) -> Value {
    json!(d.to_f64().unwrap_or(0.0))
}

/// The raw order size: an integer for whole contracts, else a float.
pub fn crypto_size(d: Decimal) -> Value {
    match d.fract().is_zero().then(|| d.to_i64()).flatten() {
        Some(n) => json!(n),
        None => crypto_float(d),
    }
}

fn set(row: &mut Value, key: &str, v: Value) {
    if let Some(m) = row.as_object_mut() {
        m.insert(key.into(), v);
    }
}

pub fn exact_order_row(e: &ExactRow<Order>) -> Value {
    let mut row = order_row(&e.row);
    if is_crypto_row(&e.row.exchange) {
        set(&mut row, "quantity", crypto_size(e.quantity));
    }
    row
}

pub fn exact_trade_row(e: &ExactRow<Trade>) -> Value {
    let mut row = trade_row(&e.row);
    if is_crypto_row(&e.row.exchange) {
        set(&mut row, "quantity", crypto_float(e.quantity));
    }
    row
}

pub fn exact_position_row(e: &ExactRow<Position>, symbols: &SymbolResolver) -> Value {
    let mut row = position_row(&e.row);
    if is_crypto_row(&e.row.exchange) {
        set(&mut row, "quantity", crypto_float(e.quantity));
        let lot = symbols
            .contract_value(&e.row.symbol, &e.row.exchange)
            .filter(|v| *v > 0.0)
            .unwrap_or(1.0);
        set(&mut row, "lot_size", json!(lot));
    }
    row
}

// ------------------------------------------------------------------ endpoints

/// `orderbook`.
pub async fn orderbook(ctx: &AppState) -> Reply {
    if is_analyze(ctx) {
        return sandbox(ctx.sandbox.orderbook().await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if crypto_venue(&h) {
        return match h.broker.get_order_book_exact(&h.auth).await {
            Ok(rows) => {
                let orders: Vec<Order> = rows.iter().map(|e| e.row.clone()).collect();
                Reply::ok(json!({"status": "success", "data": {
                    "orders": rows.iter().map(exact_order_row).collect::<Vec<_>>(),
                    "statistics": order_statistics(&orders),
                }}))
            }
            Err(e) => broker_fail(&e),
        };
    }
    match h.broker.get_order_book(&h.auth).await {
        Ok(orders) => Reply::ok(json!({"status": "success", "data": {
            "orders": orders.iter().map(order_row).collect::<Vec<_>>(),
            "statistics": order_statistics(&orders),
        }})),
        Err(e) => broker_fail(&e),
    }
}

/// `tradebook`.
pub async fn tradebook(ctx: &AppState) -> Reply {
    if is_analyze(ctx) {
        return sandbox(ctx.sandbox.tradebook().await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if crypto_venue(&h) {
        return match h.broker.get_trade_book_exact(&h.auth).await {
            Ok(t) => Reply::ok(json!({"status": "success",
                "data": t.iter().map(exact_trade_row).collect::<Vec<_>>()})),
            Err(e) => broker_fail(&e),
        };
    }
    match h.broker.get_trade_book(&h.auth).await {
        Ok(t) => Reply::ok(json!({"status": "success",
            "data": t.iter().map(trade_row).collect::<Vec<_>>()})),
        Err(e) => broker_fail(&e),
    }
}

/// `positionbook`.
pub async fn positionbook(ctx: &AppState) -> Reply {
    if is_analyze(ctx) {
        return sandbox(ctx.sandbox.positionbook().await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if crypto_venue(&h) {
        return match h.broker.get_positions_exact(&h.auth).await {
            Ok(p) => Reply::ok(json!({"status": "success",
                "data": p.iter().map(|e| exact_position_row(e, &ctx.symbols)).collect::<Vec<_>>()})),
            Err(e) => broker_fail(&e),
        };
    }
    match h.broker.get_positions(&h.auth).await {
        Ok(p) => Reply::ok(json!({"status": "success",
            "data": p.iter().map(position_row).collect::<Vec<_>>()})),
        Err(e) => broker_fail(&e),
    }
}

/// `holdings`.
pub async fn holdings(ctx: &AppState) -> Reply {
    if is_analyze(ctx) {
        return sandbox(ctx.sandbox.holdings().await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match h.broker.get_holdings(&h.auth).await {
        Ok(rows) => Reply::ok(json!({"status": "success", "data": {
            "holdings": rows.iter().map(holding_row).collect::<Vec<_>>(),
            "statistics": holdings_statistics(&rows),
        }})),
        Err(e) => broker_fail(&e),
    }
}

/// `funds`.
pub async fn funds(ctx: &AppState) -> Reply {
    if is_analyze(ctx) {
        return sandbox(ctx.sandbox.funds().await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match h.broker.get_funds(&h.auth).await {
        Ok(f) => Reply::ok(json!({"status": "success", "data": live_funds(&f)})),
        Err(e) => {
            tracing::warn!("Funds request failed: {}", e.code());
            Reply::error(500, e.client_message())
        }
    }
}

/// `orderstatus`.
pub async fn order_status(ctx: &AppState, req: &Value) -> Reply {
    let orderid = s(req, "orderid");
    if is_analyze(ctx) && !orderid.is_empty() {
        return sandbox(ctx.sandbox.order_status(&orderid).await);
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let orders = match h.broker.get_order_book(&h.auth).await {
        Ok(o) => o,
        Err(e) => return broker_fail(&e),
    };
    let Some(order) = orders.iter().find(|o| o.order_id == orderid) else {
        return Reply::error(404, format!("Order {} not found", orderid));
    };
    let mut average_price = 0.0;
    if order.status.eq_ignore_ascii_case("complete") {
        if let Ok(trades) = h.broker.get_trade_book(&h.auth).await {
            if let Some(t) = trades.iter().find(|t| t.order_id == orderid) {
                average_price = t.average_price;
            }
        }
    }
    let mut row = order_row(order);
    if let Some(m) = row.as_object_mut() {
        m.insert("average_price".into(), float(average_price));
    }
    Reply::ok(json!({"status": "success", "data": row}))
}

/// `openposition`.
pub async fn open_position(ctx: &AppState, req: &Value) -> Reply {
    let (symbol, exchange, product) = (s(req, "symbol"), s(req, "exchange"), s(req, "product"));
    if is_analyze(ctx) {
        return sandbox(
            ctx.sandbox
                .open_position(&symbol, &exchange, &product)
                .await,
        );
    }
    let h = match handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if crypto_venue(&h) {
        // Web: the position book's `quantity` (a float for crypto), or 0.
        return match h.broker.get_positions_exact(&h.auth).await {
            Ok(rows) => {
                let q = rows
                    .iter()
                    .find(|e| {
                        e.row.symbol == symbol
                            && e.row.exchange == exchange
                            && e.row.product == product
                    })
                    .map(|e| crypto_float(e.quantity))
                    .unwrap_or(json!(0));
                Reply::ok(json!({"quantity": q, "status": "success"}))
            }
            Err(e) => Reply::error(500, e.client_message()),
        };
    }
    match h.broker.get_positions(&h.auth).await {
        Ok(rows) => {
            let q = rows
                .iter()
                .find(|p| p.symbol == symbol && p.exchange == exchange && p.product == product)
                .map(|p| p.quantity)
                .unwrap_or(0);
            Reply::ok(json!({"quantity": q, "status": "success"}))
        }
        Err(e) => Reply::error(500, e.client_message()),
    }
}

/// `pnl/symbols` (sandbox only, as on the web).
pub async fn pnl_symbols(ctx: &AppState) -> Reply {
    sandbox(ctx.sandbox.pnl_symbols().await)
}

pub const PNL_LIVE_MESSAGE: &str = "This endpoint is only available in sandbox/analyzer mode";

/// Funds for the dashboard route (`/auth/dashboard-data`): the data object
/// and, in analyzer mode, `Some("analyze")`.
pub async fn funds_payload(ctx: &AppState) -> crate::error::Result<(Value, Option<&'static str>)> {
    let r = funds(ctx).await;
    if !r.is_success() {
        return Err(crate::error::AppError::Broker(if r.message().is_empty() {
            UNEXPECTED.to_string()
        } else {
            r.message()
        }));
    }
    let mode = r
        .body
        .get("mode")
        .and_then(Value::as_str)
        .map(|_| "analyze");
    Ok((r.body.get("data").cloned().unwrap_or(Value::Null), mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(side: &str, status: &str, pricetype: &str) -> Order {
        Order {
            order_id: "1".into(),
            exchange_order_id: None,
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: side.into(),
            quantity: 10,
            filled_quantity: 0,
            pending_quantity: 10,
            price: 801.256,
            trigger_price: 0.0,
            average_price: 0.0,
            order_type: pricetype.into(),
            product: "MIS".into(),
            status: status.into(),
            validity: "DAY".into(),
            order_timestamp: "2026-10-05 10:00:00".into(),
            exchange_timestamp: None,
            rejection_reason: None,
        }
    }

    #[test]
    fn live_order_rows_match_the_web_transform() {
        let r = order_row(&order("BUY", "open", "LIMIT"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 11);
        assert_eq!(r["price"], json!(801.26));
        assert_eq!(
            order_row(&order("BUY", "complete", "MARKET"))["price"],
            json!(0.0)
        );
        let st = order_statistics(&[
            order("BUY", "open", "LIMIT"),
            order("SELL", "complete", "MARKET"),
            order("SELL", "rejected", "MARKET"),
        ]);
        assert_eq!(
            st,
            json!({"total_buy_orders": 1, "total_sell_orders": 2, "total_completed_orders": 1,
                "total_open_orders": 1, "total_rejected_orders": 1})
        );
    }

    #[test]
    fn live_funds_are_two_decimal_strings() {
        let f = Funds {
            available_cash: 125000.5,
            used_margin: 2500.25,
            collateral: 1000.0,
            ..Default::default()
        };
        assert_eq!(
            live_funds(&f),
            json!({"availablecash": "125000.50", "collateral": "1000.00", "m2mrealized": "0.00",
                "m2munrealized": "0.00", "utiliseddebits": "2500.25"})
        );
    }
}
