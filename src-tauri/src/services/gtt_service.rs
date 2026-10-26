//! GTT orders (web `place_gtt_order_service.py`, `modify_gtt_order_service.py`,
//! `cancel_gtt_order_service.py`, `gtt_orderbook_service.py`).
//!
//! Analyzer mode uses the sandbox GTT engine; live mode the broker's GTT
//! API (501 when the broker has none). Failed sandbox modifies answer with
//! the sandbox's own status, not the web's 500 (web defect, see fixtures
//! README).

use super::core::{
    analyze_mode, analyzer_request, broker_handle, f, i, meta, mode_of, publish, s, safe_request,
    Reply,
};
use super::order_service::{fractional_refusal, route_to_pending, Route};
use super::schemas::{GTT_QUANTITY_NOT_POSITIVE, MAX_BROKER_QUANTITY, QUANTITY_TOO_LARGE};
use crate::brokers::types::{GttOrder, GttRequest, GttTriggerType, QuoteKey};
use crate::error::AppError;
use crate::events::{Event, GttKind};
use crate::sandbox::types::dec_from_f64;
use crate::services::latency_probe::broker_call;
use crate::state::AppState;
use serde_json::{json, Value};

fn opt_dec(v: &Value, k: &str) -> Option<rust_decimal::Decimal> {
    v.get(k)
        .and_then(Value::as_f64)
        .filter(|x| *x != 0.0)
        .map(dec_from_f64)
}

// No GTT carries a fractional size: the sandbox's and every broker's GTT
// take whole units (Delta, the CRYPTO venue, has none). A fractional CRYPTO
// quantity is refused, as `placeorder` refuses it where it cannot be
// carried, never truncated to a GTT for 0.

/// The quantity of a GTT, or the refusal of one no broker takes (API-02,
/// PROD-TR-05): below 1 (a blank or unreadable one reads as 0), or above a
/// 32-bit integer, as `placeorder` refuses it at dispatch. The `/api/v1`
/// schema refuses both first; the GTT page's modify reaches here without
/// one, and a 0 there would rewrite the GTT's legs and release its margin.
fn gtt_quantity(req: &Value) -> Result<i64, Reply> {
    if let Some(refused) = fractional_refusal(req) {
        return Err(refused);
    }
    let quantity = i(req, "quantity");
    if quantity < 1 {
        return Err(Reply::error(400, GTT_QUANTITY_NOT_POSITIVE));
    }
    if quantity > MAX_BROKER_QUANTITY {
        return Err(Reply::error(400, QUANTITY_TOO_LARGE));
    }
    Ok(quantity)
}

/// The sandbox request for a loaded GTT body.
pub fn sandbox_gtt(req: &Value) -> Result<crate::sandbox::GttRequest, Reply> {
    let quantity = gtt_quantity(req)?;
    Ok(crate::sandbox::GttRequest {
        trigger_type: s(req, "trigger_type"),
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        action: s(req, "action").to_ascii_uppercase(),
        product: s(req, "product"),
        quantity,
        pricetype: s(req, "pricetype"),
        price: opt_dec(req, "price"),
        triggerprice_sl: opt_dec(req, "triggerprice_sl"),
        triggerprice_tg: opt_dec(req, "triggerprice_tg"),
        stoploss: opt_dec(req, "stoploss"),
        target: opt_dec(req, "target"),
        strategy: req
            .get("strategy")
            .and_then(Value::as_str)
            .map(str::to_string),
        expires_at: req
            .get("expires_at")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The broker request for a loaded GTT body.
pub fn broker_gtt(req: &Value) -> Result<GttRequest, AppError> {
    let quantity = gtt_quantity(req).map_err(|refused| AppError::Validation(refused.message()))?;
    let parse = |k: &str| AppError::Validation(format!("Invalid {}", k));
    Ok(GttRequest {
        key: QuoteKey::new(s(req, "exchange"), s(req, "symbol")),
        trigger_type: if s(req, "trigger_type") == "OCO" {
            GttTriggerType::Oco
        } else {
            GttTriggerType::Single
        },
        action: s(req, "action")
            .to_ascii_uppercase()
            .parse()
            .map_err(|_| parse("action"))?,
        product: s(req, "product").parse().map_err(|_| parse("product"))?,
        quantity,
        pricetype: s(req, "pricetype")
            .parse()
            .map_err(|_| parse("pricetype"))?,
        price: f(req, "price"),
        trigger_price: f(req, "trigger_price"),
        triggerprice_sl: f(req, "triggerprice_sl"),
        stoploss: f(req, "stoploss"),
        triggerprice_tg: f(req, "triggerprice_tg"),
        target: f(req, "target"),
        last_price: None,
    })
}

fn not_supported(broker: &str) -> Reply {
    Reply::error(
        501,
        format!("GTT orders are not supported for broker '{}' yet", broker),
    )
}

fn gtt_event(
    kind: GttKind,
    analyze: bool,
    api_type: &str,
    req: &Value,
    reply: &Reply,
    trigger_id: &str,
) -> Event {
    let request = if analyze {
        analyzer_request(req, api_type)
    } else {
        safe_request(req)
    };
    Event::Gtt {
        kind,
        meta: meta(mode_of(analyze), api_type, request, &reply.body),
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        trigger_id: trigger_id.to_string(),
        triggered_order_id: String::new(),
    }
}

fn live_error(e: &AppError, internal: &str, broker: &str) -> Reply {
    match e {
        AppError::Unsupported(_) => not_supported(broker),
        AppError::Broker(m) | AppError::Refused(m) | AppError::Validation(m) => {
            Reply::error(400, m.clone())
        }
        // Not sent: the web's busy refusal (docs-20).
        AppError::Busy(_) => Reply::broker_failure(e, 429),
        AppError::NotFound(m) => Reply::error(404, m.clone()),
        _ => {
            tracing::error!("GTT call failed: {}", e);
            Reply::error(500, internal)
        }
    }
}

/// `placegttorder`.
pub async fn place_gtt(ctx: &AppState, req: &Value) -> Reply {
    place_gtt_with(ctx, req, Route::API).await
}

/// `placegttorder` for a caller that says how it routes (the Action
/// Center executes an approved GTT with [`Route::INTERNAL`]).
pub async fn place_gtt_with(ctx: &AppState, req: &Value, route: Route) -> Reply {
    if let Some(r) = route_to_pending(ctx, "placegttorder", req, route) {
        return r;
    }
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if analyze {
        let reply = match sandbox_gtt(req) {
            Ok(g) => match ctx.sandbox.place_gtt(g).await {
                Ok(r) => Reply::from_ser(&r),
                Err(e) => Reply::sandbox(&e),
            },
            Err(refused) => refused,
        };
        let id = s(&reply.body, "trigger_id");
        let kind = if reply.is_success() {
            GttKind::Placed
        } else {
            GttKind::Failed
        };
        publish(
            ctx,
            gtt_event(kind, true, "placegttorder", req, &reply, &id),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !h.broker.capabilities().gtt {
        let reply = not_supported(&h.id);
        publish(
            ctx,
            gtt_event(GttKind::Failed, false, "placegttorder", req, &reply, ""),
        );
        return reply;
    }
    let result = match broker_gtt(req) {
        Ok(g) => broker_call(&h.id, h.broker.place_gtt(&h.auth, &g)).await,
        Err(e) => Err(e),
    };
    let reply = match result {
        Ok(r) => Reply::ok(json!({"status": "success", "trigger_id": r.trigger_id})),
        Err(e) => live_error(&e, "Failed to place GTT due to internal error", &h.id),
    };
    let id = s(&reply.body, "trigger_id");
    let kind = if reply.is_success() {
        GttKind::Placed
    } else {
        GttKind::Failed
    };
    publish(
        ctx,
        gtt_event(kind, false, "placegttorder", req, &reply, &id),
    );
    reply
}

/// `modifygttorder`.
pub async fn modify_gtt(ctx: &AppState, req: &Value) -> Reply {
    modify_gtt_with(ctx, req, Route::API).await
}

/// `modifygttorder`; [`Route::INTERNAL`] (a page action) is not subject to
/// the Semi-Auto block.
pub async fn modify_gtt_with(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let trigger_id = s(req, "trigger_id");
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if let Some(reply) = route.semi_auto_refusal(
        ctx,
        analyze,
        "Modify GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.",
    ) {
        publish(
            ctx,
            gtt_event(
                GttKind::ModifyFailed,
                false,
                "modifygttorder",
                req,
                &reply,
                &trigger_id,
            ),
        );
        return reply;
    }
    if analyze {
        let reply = match sandbox_gtt(req) {
            Ok(g) => match ctx.sandbox.modify_gtt(&trigger_id, g).await {
                Ok(r) => Reply::from_ser(&r),
                Err(e) => Reply::sandbox(&e),
            },
            Err(refused) => refused,
        };
        let kind = if reply.is_success() {
            GttKind::Modified
        } else {
            GttKind::ModifyFailed
        };
        publish(
            ctx,
            gtt_event(kind, true, "modifygttorder", req, &reply, &trigger_id),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !h.broker.capabilities().gtt {
        let reply = not_supported(&h.id);
        publish(
            ctx,
            gtt_event(
                GttKind::ModifyFailed,
                false,
                "modifygttorder",
                req,
                &reply,
                &trigger_id,
            ),
        );
        return reply;
    }
    let result = match broker_gtt(req) {
        Ok(g) => broker_call(&h.id, h.broker.modify_gtt(&h.auth, &trigger_id, &g)).await,
        Err(e) => Err(e),
    };
    let reply = match result {
        Ok(r) => Reply::ok(json!({"status": "success",
            "trigger_id": if r.trigger_id.is_empty() { trigger_id.clone() } else { r.trigger_id }})),
        Err(e) => live_error(&e, "Failed to modify GTT due to internal error", &h.id),
    };
    let kind = if reply.is_success() {
        GttKind::Modified
    } else {
        GttKind::ModifyFailed
    };
    publish(
        ctx,
        gtt_event(kind, false, "modifygttorder", req, &reply, &trigger_id),
    );
    reply
}

/// `cancelgttorder`.
pub async fn cancel_gtt(ctx: &AppState, req: &Value) -> Reply {
    cancel_gtt_with(ctx, req, Route::API).await
}

/// `cancelgttorder`; [`Route::INTERNAL`] (a page action) is not subject to
/// the Semi-Auto block.
pub async fn cancel_gtt_with(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let trigger_id = s(req, "trigger_id");
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if trigger_id.is_empty() {
        return Reply::error(400, "trigger_id is missing");
    }
    if let Some(reply) = route.semi_auto_refusal(
        ctx,
        analyze,
        "Cancel GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.",
    ) {
        publish(
            ctx,
            gtt_event(
                GttKind::CancelFailed,
                false,
                "cancelgttorder",
                req,
                &reply,
                &trigger_id,
            ),
        );
        return reply;
    }
    if analyze {
        let reply = match ctx.sandbox.cancel_gtt(&trigger_id).await {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => Reply::sandbox(&e),
        };
        let kind = if reply.is_success() {
            GttKind::Cancelled
        } else {
            GttKind::CancelFailed
        };
        publish(
            ctx,
            gtt_event(kind, true, "cancelgttorder", req, &reply, &trigger_id),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !h.broker.capabilities().gtt {
        let reply = not_supported(&h.id);
        publish(
            ctx,
            gtt_event(
                GttKind::CancelFailed,
                false,
                "cancelgttorder",
                req,
                &reply,
                &trigger_id,
            ),
        );
        return reply;
    }
    let reply = match broker_call(&h.id, h.broker.cancel_gtt(&h.auth, &trigger_id)).await {
        Ok(_) => Reply::ok(json!({"status": "success", "trigger_id": trigger_id})),
        Err(e) => live_error(&e, "Failed to cancel GTT due to internal error", &h.id),
    };
    let kind = if reply.is_success() {
        GttKind::Cancelled
    } else {
        GttKind::CancelFailed
    };
    publish(
        ctx,
        gtt_event(kind, false, "cancelgttorder", req, &reply, &trigger_id),
    );
    reply
}

fn live_row(g: &GttOrder) -> Value {
    json!({
        "trigger_id": g.trigger_id,
        "trigger_type": g.trigger_type,
        "status": g.status,
        "symbol": g.symbol,
        "exchange": g.exchange,
        "trigger_prices": g.trigger_prices,
        "last_price": g.last_price,
        "legs": g.legs.iter().map(|l| json!({
            "action": l.action, "quantity": l.quantity, "price": l.price,
            "pricetype": l.pricetype, "product": l.product,
        })).collect::<Vec<_>>(),
        "created_at": g.created_at,
        "updated_at": g.updated_at,
        "expires_at": g.expires_at,
    })
}

/// Active entries first, otherwise in the order given (web `_active_first`).
pub fn active_first(rows: &mut [Value]) {
    rows.sort_by_key(|r| {
        !r.get("status")
            .and_then(Value::as_str)
            .map(|s| s.eq_ignore_ascii_case("active"))
            .unwrap_or(false)
    });
}

/// `gttorderbook`.
pub async fn gtt_orderbook(ctx: &AppState, status: &str) -> Reply {
    let all = status.eq_ignore_ascii_case("all");
    let analyze = match analyze_mode(ctx) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if analyze {
        let mut reply = match ctx
            .sandbox
            .gtt_orderbook(if all { None } else { Some("active") })
            .await
        {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => return Reply::sandbox(&e),
        };
        if let Some(rows) = reply.body.get_mut("data").and_then(Value::as_array_mut) {
            active_first(rows);
        }
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    if !h.broker.capabilities().gtt {
        return not_supported(&h.id);
    }
    match h.broker.get_gtt_book(&h.auth, all).await {
        Ok(rows) => {
            let mut data: Vec<Value> = rows.iter().map(live_row).collect();
            active_first(&mut data);
            Reply::ok(json!({"status": "success", "data": data}))
        }
        Err(e) => live_error(&e, "Failed to fetch GTT orderbook", &h.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::order_service::FRACTIONAL_REFUSED;

    #[test]
    fn active_entries_come_first_stably() {
        let mut rows = vec![
            json!({"trigger_id": "a", "status": "cancelled"}),
            json!({"trigger_id": "b", "status": "active"}),
            json!({"trigger_id": "c", "status": "triggered"}),
            json!({"trigger_id": "d", "status": "active"}),
        ];
        active_first(&mut rows);
        let ids: Vec<&str> = rows
            .iter()
            .map(|r| r["trigger_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["b", "d", "a", "c"]);
    }

    #[test]
    fn requests_translate() {
        let req = json!({"trigger_type": "OCO", "symbol": "SBIN", "exchange": "NSE", "action": "sell",
            "product": "CNC", "quantity": 2, "pricetype": "LIMIT", "price": 900.0,
            "triggerprice_sl": 850.0, "stoploss": 849.0, "triggerprice_tg": 1000.0, "target": 1001.0,
            "trigger_price": 1000.0, "strategy": "s"});
        let g = sandbox_gtt(&req).unwrap();
        assert_eq!(g.action, "SELL");
        assert_eq!(g.quantity, 2);
        let b = broker_gtt(&req).unwrap();
        assert_eq!(b.trigger_type, GttTriggerType::Oco);
        assert_eq!(b.target, 1001.0);
    }

    /// A fractional CRYPTO quantity is refused by both conversions, never
    /// truncated to a GTT for 0.
    #[test]
    fn a_fractional_quantity_makes_no_gtt() {
        let req = json!({"trigger_type": "SINGLE", "symbol": "BTCUSDFUT", "exchange": "CRYPTO",
            "action": "BUY", "product": "NRML", "quantity": 0.5, "pricetype": "LIMIT",
            "price": 60000.0, "triggerprice_sl": 59000.0, "trigger_price": 59000.0});
        let refused = sandbox_gtt(&req).unwrap_err();
        assert_eq!(
            (refused.status, refused.message()),
            (400, FRACTIONAL_REFUSED.to_string())
        );
        match broker_gtt(&req) {
            Err(AppError::Validation(m)) => assert_eq!(m, FRACTIONAL_REFUSED),
            other => panic!("{:?}", other.map(|g| g.quantity)),
        }
        let whole = json!({"exchange": "CRYPTO", "quantity": 2.0, "action": "BUY",
            "product": "NRML", "pricetype": "LIMIT"});
        assert_eq!(sandbox_gtt(&whole).unwrap().quantity, 2);
        assert_eq!(broker_gtt(&whole).unwrap().quantity, 2);
    }

    /// A quantity no broker takes is refused by both conversions, on every
    /// exchange (PROD-TR-05): the GTT page's modify has no schema in front
    /// of it. The largest one a broker takes passes unchanged.
    #[test]
    fn a_quantity_no_broker_takes_makes_no_gtt() {
        for exchange in ["NSE", "CRYPTO"] {
            let gtt = |quantity: u64| {
                json!({"exchange": exchange, "quantity": quantity, "action": "BUY",
                    "product": "NRML", "pricetype": "LIMIT"})
            };
            let refused = sandbox_gtt(&gtt(2_147_483_648)).unwrap_err();
            assert_eq!(
                (refused.status, refused.message()),
                (400, QUANTITY_TOO_LARGE.to_string())
            );
            match broker_gtt(&gtt(2_147_483_648)) {
                Err(AppError::Validation(m)) => assert_eq!(m, QUANTITY_TOO_LARGE),
                other => panic!("{:?}", other.map(|g| g.quantity)),
            }
            let largest = i64::from(i32::MAX);
            assert_eq!(sandbox_gtt(&gtt(2_147_483_647)).unwrap().quantity, largest);
            assert_eq!(broker_gtt(&gtt(2_147_483_647)).unwrap().quantity, largest);
        }
    }

    /// A GTT quantity below 1 is refused by both conversions (PROD-TR-05):
    /// the GTT page's modify turns a blank or unreadable quantity into 0,
    /// and a negative one would reach the sandbox or the broker unchanged.
    #[test]
    fn a_quantity_below_one_makes_no_gtt() {
        for quantity in [json!(0), json!(-5), json!(null), json!("abc")] {
            let req = json!({"exchange": "NSE", "quantity": quantity, "action": "BUY",
                "product": "NRML", "pricetype": "LIMIT"});
            let refused = sandbox_gtt(&req).unwrap_err();
            assert_eq!(
                (refused.status, refused.message()),
                (400, GTT_QUANTITY_NOT_POSITIVE.to_string()),
                "{quantity}"
            );
            match broker_gtt(&req) {
                Err(AppError::Validation(m)) => assert_eq!(m, GTT_QUANTITY_NOT_POSITIVE),
                other => panic!("{quantity}: {:?}", other.map(|g| g.quantity)),
            }
        }
        let one = json!({"exchange": "NSE", "quantity": 1, "action": "BUY",
            "product": "NRML", "pricetype": "LIMIT"});
        assert_eq!(sandbox_gtt(&one).unwrap().quantity, 1);
        assert_eq!(broker_gtt(&one).unwrap().quantity, 1);
    }
}
