//! Order actions from the pages (web `blueprints/orders.py` session routes:
//! `/close_position`, `/close_all_positions`, `/cancel_all_orders`,
//! `/cancel_order`, `/modify_order`, `/modify_gtt_order`,
//! `/cancel_gtt_order`).
//!
//! They run through the `/api/v1` services with [`Route::INTERNAL`]: the
//! trader is acting in the app, so Semi-Auto never queues or blocks them,
//! and analyzer mode still routes them to the sandbox. Results and events
//! are the services' own.

use super::core::{broker_handle, meta, publish, Reply};
use super::order_service::{
    self as orders, place_live, smart_decision, stripe, Route, SmartDecision,
};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::{ExactRow, Position};
use crate::events::{Event, Mode};
use crate::state::AppState;
use rust_decimal::Decimal;
use serde_json::{json, Map, Value};

pub const AUTH_ERROR: &str = "Authentication error";

/// The web's guard on the live session routes.
pub fn require_broker(ctx: &AppState) -> Option<Reply> {
    (!ctx.is_broker_connected()).then(|| Reply::error(401, AUTH_ERROR))
}

/// A page-sent value as the services read it: numeric strings become
/// numbers (the web passes these straight to the broker layer, which casts).
fn coerce(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                json!(i)
            } else if let Ok(f) = t.parse::<f64>() {
                json!(f)
            } else {
                json!(s)
            }
        }
        Some(other) => other.clone(),
    }
}

fn text(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(s.trim()),
        Some(other) => json!(other.to_string()),
    }
}

/// `/close_position`: exit one position with a smart order to zero.
pub async fn close_position(ctx: &AppState, symbol: &str, exchange: &str, product: &str) -> Reply {
    let order = json!({
        "strategy": "UI Exit Position",
        "exchange": exchange,
        "symbol": symbol,
        "action": "BUY",
        "product": product,
        "pricetype": "MARKET",
        "quantity": 0,
        "price": 0.0,
        "trigger_price": 0.0,
        "disclosed_quantity": 0,
        "position_size": 0,
    });
    if super::core::is_analyze(ctx) {
        // Web: the sandbox smart order with quantity 0 and position size 0.
        return orders::place_smart_order(ctx, &order, Route::INTERNAL).await;
    }
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let (Ok(ex), Ok(pr)) = (exchange.parse::<Exchange>(), product.parse::<Product>()) else {
        return Reply::error(
            400,
            "Choose a valid exchange and product for this position.",
        );
    };
    // Decide once, under the position's lock (shared with smart orders).
    let key = format!("{}:{}:{}", ex, symbol, pr);
    let _guard = stripe(&key).lock().await;
    let decision = if ex == Exchange::Crypto && h.broker.broker_type() == "crypto" {
        // A crypto position can be fractional (a spot balance): close its
        // exact size, carried as decimal text.
        match h.broker.get_positions_exact(&h.auth).await {
            Ok(rows) => crypto_close_decision(&rows, symbol, exchange, product),
            Err(e) => return orders::broker_error_reply(&e, "Failed to close position."),
        }
    } else {
        let current = match h.broker.get_open_position(&h.auth, symbol, ex, pr).await {
            Ok(q) => q,
            Err(e) => return orders::broker_error_reply(&e, "Failed to close position."),
        };
        match smart_decision(current, 0, 0, "BUY") {
            SmartDecision::NoAction(msg) => Err(msg),
            SmartDecision::Place { action, quantity } => Ok((action, json!(quantity))),
        }
    };
    match decision {
        Err(msg) => Reply::error(400, msg),
        Ok((action, quantity)) => {
            let mut req = order.clone();
            if let Some(m) = req.as_object_mut() {
                m.insert("action".into(), json!(action));
                m.insert("quantity".into(), quantity);
            }
            let placed = place_live(&h, ctx, &req).await;
            let orderid = placed
                .body
                .get("orderid")
                .and_then(Value::as_str)
                .filter(|o| !o.is_empty())
                .map(str::to_string);
            match orderid {
                Some(id) if placed.is_success() => {
                    let message = "Position close order placed successfully.";
                    let reply =
                        Reply::ok(json!({"status": "success", "message": message, "orderid": id}));
                    let mut request = order.clone();
                    if let Some(m) = request.as_object_mut() {
                        m.insert("api_type".into(), json!("closeposition"));
                    }
                    publish(
                        ctx,
                        Event::PositionClosed {
                            meta: meta(Mode::Live, "closeposition", request, &reply.body),
                            message: Some(message.to_string()),
                        },
                    );
                    reply
                }
                _ => {
                    let msg = placed.message();
                    let status = if placed.status >= 400 {
                        placed.status
                    } else {
                        400
                    };
                    Reply::error(
                        status,
                        if msg.is_empty() {
                            "Failed to close position (broker did not return order ID).".to_string()
                        } else {
                            msg
                        },
                    )
                }
            }
        }
    }
}

/// The exit for one crypto position: the opposite side for its exact size,
/// or the smart order's "no open position" answer.
pub fn crypto_close_decision(
    rows: &[ExactRow<Position>],
    symbol: &str,
    exchange: &str,
    product: &str,
) -> Result<(String, Value), &'static str> {
    let size = rows
        .iter()
        .find(|e| e.row.symbol == symbol && e.row.exchange == exchange && e.row.product == product)
        .map(|e| e.quantity)
        .unwrap_or(Decimal::ZERO);
    let sign = i64::from(size > Decimal::ZERO) - i64::from(size < Decimal::ZERO);
    match smart_decision(sign, 0, 0, "BUY") {
        SmartDecision::NoAction(msg) => Err(msg),
        SmartDecision::Place { action, .. } => {
            Ok((action, json!(size.abs().normalize().to_string())))
        }
    }
}

/// `/close_all_positions`.
pub async fn close_all_positions(ctx: &AppState) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let r = orders::close_position(ctx, &json!({}), Route::INTERNAL).await;
    if r.status == 200 && r.body.get("status").and_then(Value::as_str) != Some("error") {
        let msg = r
            .body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("All Open Positions Squared Off")
            .to_string();
        return Reply::ok(json!({"status": "success", "message": msg}));
    }
    r
}

/// `/cancel_all_orders`.
pub async fn cancel_all_orders(ctx: &AppState) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let r = orders::cancel_all_orders(ctx, &json!({}), Route::INTERNAL).await;
    if r.status != 200 || r.body.get("status").and_then(Value::as_str) == Some("error") {
        return r;
    }
    let list = |k: &str| {
        r.body
            .get(k)
            .cloned()
            .filter(Value::is_array)
            .unwrap_or(json!([]))
    };
    let (canceled, failed) = (list("canceled_orders"), list("failed_cancellations"));
    let (nc, nf) = (
        canceled.as_array().map_or(0, Vec::len),
        failed.as_array().map_or(0, Vec::len),
    );
    if nc > 0 || nf == 0 {
        let mut message = format!("Successfully canceled {} orders", nc);
        if nf > 0 {
            message.push_str(&format!(" (Failed to cancel {} orders)", nf));
        }
        return Reply::ok(json!({
            "status": "success",
            "message": message,
            "canceled_orders": canceled,
            "failed_cancellations": failed,
        }));
    }
    Reply::ok(json!({"status": "info", "message": "No open orders to cancel"}))
}

/// `/cancel_order`.
pub async fn cancel_order(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let orderid = text(body.get("orderid"));
    if orderid.as_str().is_none_or(str::is_empty) {
        return Reply::error(400, "Order ID is required");
    }
    orders::cancel_order(ctx, &json!({"orderid": orderid}), Route::INTERNAL).await
}

/// `/modify_order`.
pub async fn modify_order(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let orderid = text(body.get("orderid"));
    if orderid.as_str().is_none_or(str::is_empty) {
        return Reply::error(400, "Order ID is required");
    }
    let req = json!({
        "orderid": orderid,
        "symbol": text(body.get("symbol")),
        "exchange": text(body.get("exchange")),
        "action": text(body.get("action")),
        "product": text(body.get("product")),
        "pricetype": text(body.get("pricetype")),
        "price": coerce(body.get("price")),
        "quantity": coerce(body.get("quantity")),
        "disclosed_quantity": coerce(Some(body.get("disclosed_quantity").unwrap_or(&json!(0)))),
        "trigger_price": coerce(Some(body.get("trigger_price").unwrap_or(&json!(0)))),
    });
    orders::modify_order(ctx, &req, Route::INTERNAL).await
}

fn trigger_id(body: &Map<String, Value>) -> Option<String> {
    match body.get("trigger_id") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        _ => None,
    }
}

/// `/modify_gtt_order`: the flat replacement body plus `trigger_id`.
pub async fn modify_gtt_order(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let Some(tid) = trigger_id(body) else {
        return Reply::error(400, "trigger_id is required");
    };
    let req = json!({
        "trigger_id": tid,
        "strategy": body.get("strategy").cloned().unwrap_or(json!("GTT Modify")),
        "symbol": text(body.get("symbol")),
        "exchange": text(body.get("exchange")),
        "trigger_type": text(body.get("trigger_type")),
        "action": text(body.get("action")),
        "product": text(body.get("product")),
        "quantity": coerce(body.get("quantity")),
        "pricetype": body.get("pricetype").map(|v| text(Some(v))).unwrap_or(json!("LIMIT")),
        "price": coerce(body.get("price")),
        "triggerprice_sl": coerce(body.get("triggerprice_sl")),
        "triggerprice_tg": coerce(body.get("triggerprice_tg")),
        "stoploss": coerce(body.get("stoploss")),
        "target": coerce(body.get("target")),
    });
    super::gtt_service::modify_gtt_with(ctx, &req, Route::INTERNAL).await
}

/// `/cancel_gtt_order`.
pub async fn cancel_gtt_order(ctx: &AppState, body: &Map<String, Value>) -> Reply {
    if let Some(r) = require_broker(ctx) {
        return r;
    }
    let Some(tid) = trigger_id(body) else {
        return Reply::error(400, "trigger_id is required");
    };
    super::gtt_service::cancel_gtt_with(ctx, &json!({"trigger_id": tid}), Route::INTERNAL).await
}
