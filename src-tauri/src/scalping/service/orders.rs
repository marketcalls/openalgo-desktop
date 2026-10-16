//! Scalping orders: the lot rules on an entry, entries, and the
//! freeze-safe risk-reducing exits (one leg, every leg, every open order).

use super::{
    allowed_products, err, is_derivative, is_order_exchange, mode_name, py_int, text,
    MAX_ORDER_QUANTITY, VALID_ACTIONS,
};
use crate::scalping::monitor::{exit_chunk, MAX_LOTS, SCALPING_STRATEGY};
use crate::scalping::unconfirmed::{self, NewPending};
use crate::services::core::{analyze_mode, Reply};
use crate::services::order_service::Route;
use crate::state::AppState;
use serde_json::{json, Map, Value};

/// The lot rules on an entry: whole lots, at most `MAX_LOTS`, under the
/// exchange freeze.
pub fn validate_quantity(
    ctx: &AppState,
    symbol: &str,
    exchange: &str,
    quantity: i64,
) -> Option<String> {
    let lot = ctx
        .symbols
        .by_symbol(exchange, symbol)
        .map(|r| i64::from(r.lot_size))
        .filter(|l| *l > 0);
    let Some(lot) = lot else {
        return Some(format!(
            "Unknown symbol or lot size unavailable: {}",
            symbol
        ));
    };
    if quantity % lot != 0 {
        return Some(format!(
            "quantity must be a whole number of lots (lot size {})",
            lot
        ));
    }
    if quantity > MAX_LOTS * lot {
        return Some(format!("quantity exceeds the {}-lot cap", MAX_LOTS));
    }
    let freeze = ctx.freeze.for_option(symbol, exchange);
    if freeze > 0 && quantity > freeze {
        return Some(format!(
            "quantity exceeds the exchange freeze limit ({})",
            freeze
        ));
    }
    None
}

struct OrderFields {
    symbol: String,
    exchange: String,
    action: String,
    product: String,
    quantity: i64,
}

fn order_fields(body: &Map<String, Value>, default_product: &str) -> Result<OrderFields, Reply> {
    let symbol = text(body, "symbol");
    let exchange = text(body, "exchange").to_uppercase();
    let action = text(body, "action").to_uppercase();
    let product = {
        let p = text(body, "product").to_uppercase();
        if p.is_empty() {
            default_product.to_string()
        } else {
            p
        }
    };
    let quantity = py_int(body.get("quantity")).unwrap_or(0);
    if symbol.is_empty() {
        return Err(err(400, "symbol is required"));
    }
    if !is_order_exchange(&exchange) {
        return Err(err(400, format!("Invalid exchange: {}", exchange)));
    }
    if !VALID_ACTIONS.contains(&action.as_str()) {
        return Err(err(400, format!("Invalid action: {}", action)));
    }
    if !allowed_products(&exchange).contains(&product.as_str()) {
        return Err(err(
            400,
            format!("Invalid product for {}: {}", exchange, product),
        ));
    }
    if quantity <= 0 {
        return Err(err(400, "quantity must be positive"));
    }
    if quantity > MAX_ORDER_QUANTITY {
        return Err(err(400, "quantity exceeds the safety limit"));
    }
    Ok(OrderFields {
        symbol,
        exchange,
        action,
        product,
        quantity,
    })
}

fn accepted(r: &Reply) -> bool {
    r.is_success() && r.body.get("status").and_then(Value::as_str) != Some("error")
}

/// `POST /scalping/api/order`: one MARKET entry.
pub async fn place(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let f = match order_fields(body, "MIS") {
        Ok(f) => f,
        Err(r) => return r,
    };
    if is_derivative(&f.exchange) {
        if let Some(lots) = body.get("lots").filter(|v| !v.is_null()) {
            let Ok(lots) = py_int(Some(lots)) else {
                return err(400, "lots must be an integer");
            };
            if !(1..=MAX_LOTS).contains(&lots) {
                return err(400, format!("lots must be between 1 and {}", MAX_LOTS));
            }
        }
        if let Some(e) = validate_quantity(ctx, &f.symbol, &f.exchange, f.quantity) {
            return err(400, e);
        }
    }
    let req = json!({
        "strategy": SCALPING_STRATEGY,
        "symbol": f.symbol,
        "exchange": f.exchange,
        "action": f.action,
        "pricetype": "MARKET",
        "product": f.product,
        "quantity": f.quantity,
        "price": 0,
        "trigger_price": 0,
        "disclosed_quantity": 0,
    });
    // The side is decided once, before the order goes out, and the list row
    // names that side: read after the await, a toggle flipped while the
    // order was in flight tagged a live entry as a sandbox one (LOG-09).
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Some(r) =
        unconfirmed::refuse_repeat(ctx, mode_name(analyze), &f.symbol, &f.exchange, &f.product)
    {
        return r;
    }
    let r = crate::services::order_service::place_order_decided(
        ctx,
        &req,
        Route::INTERNAL,
        analyze,
        true,
    )
    .await;
    if r.is_uncertain() {
        return unconfirmed::note(
            ctx,
            &NewPending {
                mode: mode_name(analyze),
                kind: "entry",
                symbol: &f.symbol,
                exchange: &f.exchange,
                product: &f.product,
                action: &f.action,
                quantity: f.quantity,
                client_tag: unconfirmed::client_tag_of(&r).as_deref(),
            },
        );
    }
    if accepted(&r) {
        let mode = mode_name(analyze);
        if let Err(e) = ctx
            .scalping
            .store
            .track(&f.symbol, &f.exchange, &f.product, mode)
        {
            tracing::error!("Scalping list could not record {}: {}", f.symbol, e);
        }
    }
    r
}

/// The freeze-safe, whole-lot risk-reducing exit (web `_reducing_exit`), to
/// the side the caller decided (`analyze`: the sandbox).
pub async fn reducing_exit(
    ctx: &AppState,
    analyze: bool,
    symbol: &str,
    exchange: &str,
    product: &str,
    action: &str,
    quantity: i64,
) -> Reply {
    let mode = mode_name(analyze);
    if let Some(r) = unconfirmed::refuse_repeat(ctx, mode, symbol, exchange, product) {
        return r;
    }
    let exit = |quantity, client_tag| NewPending {
        mode,
        kind: "exit",
        symbol,
        exchange,
        product,
        action,
        quantity,
        client_tag,
    };
    let chunk = match exit_chunk(&ctx.symbols, &ctx.freeze, symbol, exchange, quantity) {
        Ok(c) => c,
        Err(e) => return err(400, e),
    };
    if let Some(c) = chunk.filter(|c| quantity > *c) {
        let req = json!({
            "strategy": SCALPING_STRATEGY,
            "symbol": symbol,
            "exchange": exchange,
            "action": action,
            "quantity": quantity,
            "splitsize": c,
            "pricetype": "MARKET",
            "product": product,
        });
        let reply = crate::services::batch_order_service::split_order_decided(
            ctx,
            &req,
            Route::INTERNAL,
            analyze,
        )
        .await;
        if !reply.is_uncertain() {
            return reply;
        }
        // The split stopped at the first chunk with no definite answer and
        // sent nothing after it: that chunk may be at the broker.
        let q = unconfirmed::uncertain_chunk(&reply).unwrap_or(quantity);
        return unconfirmed::note(ctx, &exit(q, None));
    }
    let req = json!({
        "strategy": SCALPING_STRATEGY,
        "symbol": symbol,
        "exchange": exchange,
        "action": action,
        "pricetype": "MARKET",
        "product": product,
        "quantity": quantity,
        "price": 0,
        "trigger_price": 0,
        "disclosed_quantity": 0,
    });
    let r = crate::services::order_service::place_order_decided(
        ctx,
        &req,
        Route::INTERNAL,
        analyze,
        true,
    )
    .await;
    if r.is_uncertain() {
        let tag = unconfirmed::client_tag_of(&r);
        return unconfirmed::note(ctx, &exit(quantity, tag.as_deref()));
    }
    r
}

/// `POST /scalping/api/close_leg`.
pub async fn close_leg(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    let f = match order_fields(body, "NRML") {
        Ok(f) => f,
        Err(r) => return r,
    };
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    reducing_exit(
        ctx,
        analyze,
        &f.symbol,
        &f.exchange,
        &f.product,
        &f.action,
        f.quantity,
    )
    .await
}

/// `POST /scalping/api/close_all`: only the scalping list's open positions.
pub async fn close_all(ctx: &AppState) -> Reply {
    // One side for the list, the position book and every exit.
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let mode = mode_name(analyze);
    let tracked = ctx.scalping.store.tracked(Some(mode)).unwrap_or_else(|e| {
        tracing::error!("Scalping list could not be read: {}", e);
        Vec::new()
    });
    if tracked.is_empty() {
        return Reply::ok(json!({
            "status": "success",
            "message": "No scalping positions to close",
            "results": [],
        }));
    }
    let book = crate::services::account_service::positionbook_on(ctx, analyze).await;
    if !accepted(&book) {
        let msg = book.message();
        let msg = if msg.is_empty() {
            "positionbook unavailable".to_string()
        } else {
            msg
        };
        return err(502, format!("Could not fetch positions to close: {}", msg));
    }
    let mut results = Vec::new();
    let mut closed = 0;
    for t in tracked {
        let net =
            crate::scalping::monitor::net_quantity(&book.body, &t.symbol, &t.exchange, &t.product);
        if net == 0 {
            continue;
        }
        let action = if net > 0 { "SELL" } else { "BUY" };
        let r = reducing_exit(
            ctx,
            analyze,
            &t.symbol,
            &t.exchange,
            &t.product,
            action,
            net.abs(),
        )
        .await;
        let ok = accepted(&r);
        results.push(json!({
            "symbol": t.symbol,
            "status": if ok { "success" } else { "error" },
            "message": r.body.get("message").cloned().unwrap_or(Value::Null),
        }));
        if ok {
            closed += 1;
        }
    }
    Reply::ok(json!({
        "status": "success",
        "message": format!("Closed {} scalping position(s)", closed),
        "results": results,
    }))
}

/// `POST /scalping/api/cancel_all`.
pub async fn cancel_all(ctx: &AppState) -> Reply {
    crate::services::order_service::cancel_all_orders(ctx, &json!({}), Route::INTERNAL).await
}
