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
use super::order_service::{route_to_pending, Route};
use crate::brokers::types::{GttOrder, GttRequest, GttTriggerType, QuoteKey};
use crate::error::AppError;
use crate::events::{Event, GttKind};
use crate::sandbox::types::dec_from_f64;
use crate::state::AppState;
use serde_json::{json, Value};

fn opt_dec(v: &Value, k: &str) -> Option<rust_decimal::Decimal> {
    v.get(k)
        .and_then(Value::as_f64)
        .filter(|x| *x != 0.0)
        .map(dec_from_f64)
}

/// The sandbox request for a loaded GTT body.
pub fn sandbox_gtt(req: &Value) -> crate::sandbox::GttRequest {
    crate::sandbox::GttRequest {
        trigger_type: s(req, "trigger_type"),
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        action: s(req, "action").to_ascii_uppercase(),
        product: s(req, "product"),
        quantity: i(req, "quantity"),
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
    }
}

/// The broker request for a loaded GTT body.
pub fn broker_gtt(req: &Value) -> Result<GttRequest, AppError> {
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
        quantity: i(req, "quantity"),
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
        AppError::Broker(m) | AppError::Validation(m) => Reply::error(400, m.clone()),
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
        let reply = match ctx.sandbox.place_gtt(sandbox_gtt(req)).await {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => Reply::sandbox(&e),
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
        Ok(g) => h.broker.place_gtt(&h.auth, &g).await,
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
    if !analyze && route.semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Modify GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.",
        );
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
        let reply = match ctx.sandbox.modify_gtt(&trigger_id, sandbox_gtt(req)).await {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => Reply::sandbox(&e),
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
        Ok(g) => h.broker.modify_gtt(&h.auth, &trigger_id, &g).await,
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
    if !analyze && route.semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Cancel GTT order is not allowed in Semi-Auto mode. Switch to Auto mode.",
        );
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
    let reply = match h.broker.cancel_gtt(&h.auth, &trigger_id).await {
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
        let g = sandbox_gtt(&req);
        assert_eq!(g.action, "SELL");
        assert_eq!(g.quantity, 2);
        let b = broker_gtt(&req).unwrap();
        assert_eq!(b.trigger_type, GttTriggerType::Oco);
        assert_eq!(b.target, 1001.0);
    }
}
