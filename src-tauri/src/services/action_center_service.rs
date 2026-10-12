//! Action Center (web `services/action_center_service.py`,
//! `services/pending_order_execution_service.py` and the Action Center
//! routes of `blueprints/orders.py`).
//!
//! An approved order is sent at most once. Approval is a compare-and-set
//! (pending -> approved) and execution claims the row (`broker_status`
//! empty -> `submitting`) in one conditional `UPDATE` before anything is
//! dispatched, so two approvals racing for one order (a double click, Approve
//! on one screen and Approve All on another) dispatch it once. Every path
//! after the claim writes a final broker status; a row left in `submitting`
//! by a crash is never resent automatically.
//!
//! Approved orders run through the same services as `/api/v1`, with
//! [`Route::INTERNAL`] so they are not queued again; analyzer mode at the
//! moment of approval decides sandbox or live, as on the web.

use super::core::{publish, s, Reply};
use super::order_service::Route;
use crate::db::sqlite::action_center::{self as store, PendingOrderRow, NO_ACTION, SUBMITTING};
use crate::events::Event;
use crate::state::AppState;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{json, Map, Value};

pub const ALREADY_HANDLED_MESSAGE: &str =
    "This order was already approved or rejected. Check the Action Center.";
pub const ALREADY_SUBMITTING_STATUS: u16 = 409;
pub const ALREADY_SUBMITTING_MESSAGE: &str =
    "This order is already being sent to your broker. Check the order book before approving it again.";
pub const CLAIM_FAILED_MESSAGE: &str = "The order was not sent to your broker, because OpenAlgo could not record it as being sent. It is back in the pending list: approve it again in a moment.";
pub const CLAIM_FAILED_STUCK_MESSAGE: &str = "The order was not sent to your broker, because OpenAlgo could not record it as being sent. Place it again from your trading platform.";
const RETURNED_TO_PENDING: &str = "returned_to_pending";

// ------------------------------------------------------------------ display

fn seconds_since(stored: Option<&str>, now: DateTime<Utc>) -> Value {
    let Some(t) = stored else {
        return Value::Null;
    };
    let parsed = NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S"));
    match parsed {
        Ok(at) => {
            let secs = (now.naive_utc() - at).num_milliseconds() as f64 / 1000.0;
            json!(secs.max(0.0))
        }
        Err(_) => Value::Null,
    }
}

fn get_or(d: &Value, k: &str, default: Value) -> Value {
    d.get(k).cloned().unwrap_or(default)
}

/// Python `str()` of a JSON scalar.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => other.to_string(),
    }
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// Web `parse_pending_order`: one row for display.
pub fn parse_pending_order(row: &PendingOrderRow, now: DateTime<Utc>) -> Value {
    let data: Value = match serde_json::from_str::<Value>(&row.order_data) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            tracing::warn!("Pending order {} has unreadable order data", row.id);
            return json!({
                "id": row.id, "api_type": row.api_type, "strategy": "Error",
                "symbol": "Error parsing order", "exchange": "-", "action": "-",
                "quantity": "-", "price": "-", "price_type": "-", "product_type": "-",
                "error": "The stored order could not be read.",
            });
        }
    };
    let raw: Map<String, Value> = data
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(k, _)| k.as_str() != "apikey" && k.as_str() != "api_key")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut out = json!({
        "id": row.id,
        "user_id": row.user_id,
        "api_type": row.api_type,
        "status": row.status,
        "created_at_ist": row.created_at_ist,
        "approved_at_ist": row.approved_at_ist,
        "approved_age_seconds": seconds_since(row.approved_at.as_deref(), now),
        "approved_by": row.approved_by,
        "rejected_at_ist": row.rejected_at_ist,
        "rejected_by": row.rejected_by,
        "rejected_reason": row.rejected_reason,
        "broker_order_id": row.broker_order_id,
        "broker_status": row.broker_status,
        "strategy": get_or(&data, "strategy", json!("N/A")),
        "raw_order_data": Value::Object(raw),
    });
    let g = |k: &str, d: &str| get_or(&data, k, json!(d));
    let extra = match row.api_type.as_str() {
        "optionsmultiorder" => {
            let legs = data
                .get("legs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let set = |k: &str, d: &str| {
                let mut v: Vec<String> = legs
                    .iter()
                    .map(|l| py_str(&get_or(l, k, json!(d))).to_uppercase())
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            let one_or = |v: Vec<String>, many: &str| {
                if v.len() == 1 {
                    v[0].clone()
                } else {
                    many.to_string()
                }
            };
            json!({
                "symbol": format!("{} ({} legs)", py_str(&g("underlying", "")), legs.len()),
                "exchange": g("exchange", ""),
                "action": one_or(set("action", ""), "MULTI"),
                "quantity": legs.iter().map(|l| int_of(l.get("quantity"))).sum::<i64>(),
                "price": "Multiple",
                "trigger_price": "Multiple",
                "price_type": one_or(set("pricetype", "MARKET"), "Multiple"),
                "product_type": one_or(set("product", "MIS"), "Multiple"),
            })
        }
        "optionsorder" => json!({
            "symbol": format!("{} {} {}", py_str(&g("underlying", "")), py_str(&g("offset", "")),
                py_str(&g("option_type", ""))),
            "exchange": g("exchange", ""),
            "action": g("action", ""),
            "quantity": g("quantity", ""),
            "price": g("price", "0"),
            "trigger_price": g("trigger_price", "0"),
            "price_type": g("pricetype", "MARKET"),
            "product_type": g("product", ""),
        }),
        "placegttorder" => {
            let trigger = if py_str(&g("trigger_type", "")).to_uppercase() == "OCO" {
                json!(format!(
                    "{} / {}",
                    py_str(&get_or(&data, "triggerprice_sl", json!(0))),
                    py_str(&get_or(&data, "triggerprice_tg", json!(0)))
                ))
            } else {
                get_or(&data, "trigger_price", json!(0))
            };
            json!({
                "symbol": g("symbol", ""),
                "exchange": g("exchange", ""),
                "action": g("action", ""),
                "quantity": g("quantity", ""),
                "price": g("price", "0"),
                "trigger_price": trigger,
                "price_type": g("pricetype", "LIMIT"),
                "product_type": g("product", ""),
            })
        }
        "basketorder" => {
            let orders = data
                .get("orders")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let first = |k: &str| {
                if orders.len() > 1 {
                    json!("Multiple")
                } else {
                    orders
                        .first()
                        .map(|o| get_or(o, k, json!("")))
                        .unwrap_or(json!(""))
                }
            };
            json!({
                "symbol": format!("Basket ({} orders)", orders.len()),
                "exchange": first("exchange"),
                "action": first("action"),
                "quantity": orders.iter().map(|o| int_of(o.get("quantity"))).sum::<i64>().to_string(),
                "price": "Multiple",
                "trigger_price": "0",
                "price_type": "Multiple",
                "product_type": "Multiple",
            })
        }
        "splitorder" => json!({
            "symbol": g("symbol", ""),
            "exchange": g("exchange", ""),
            "action": g("action", ""),
            "quantity": format!("{} (split: {})", py_str(&g("quantity", "")), py_str(&g("splitsize", ""))),
            "price": g("price", "0"),
            "trigger_price": g("trigger_price", "0"),
            "price_type": data.get("pricetype").cloned().unwrap_or_else(|| g("price_type", "MARKET")),
            "product_type": data.get("product").cloned().unwrap_or_else(|| g("product_type", "")),
        }),
        "smartorder" => json!({
            "symbol": g("symbol", ""),
            "exchange": g("exchange", ""),
            "action": g("action", ""),
            "quantity": g("quantity", ""),
            "price": g("price", "0"),
            "trigger_price": g("trigger_price", "0"),
            "price_type": data.get("pricetype").cloned().unwrap_or_else(|| g("price_type", "MARKET")),
            "product_type": data.get("product").cloned().unwrap_or_else(|| g("product_type", "")),
        }),
        _ => json!({
            "symbol": g("symbol", ""),
            "exchange": g("exchange", ""),
            "action": g("action", ""),
            "quantity": g("quantity", ""),
            "price": g("price", "0"),
            "trigger_price": g("trigger_price", "0"),
            "price_type": data.get("price_type").cloned().unwrap_or_else(|| g("pricetype", "MARKET")),
            "product_type": data.get("product_type").cloned().unwrap_or_else(|| g("product", "")),
        }),
    };
    if let (Some(o), Value::Object(e)) = (out.as_object_mut(), extra) {
        o.extend(e);
    }
    out
}

/// Web `calculate_action_center_stats`.
pub fn statistics(orders: &[Value]) -> Value {
    let mut st: Map<String, Value> = Map::new();
    let keys = [
        "total_pending",
        "total_approved",
        "total_rejected",
        "total_buy_orders",
        "total_sell_orders",
        "total_placeorder",
        "total_smartorder",
        "total_basketorder",
        "total_splitorder",
        "total_optionsorder",
        "total_optionsmultiorder",
        "total_placegttorder",
    ];
    let mut counts = [0i64; 12];
    for o in orders {
        match o.get("status").and_then(Value::as_str) {
            Some("pending") => counts[0] += 1,
            Some("approved") => counts[1] += 1,
            Some("rejected") => counts[2] += 1,
            _ => {}
        }
        match o
            .get("action")
            .and_then(Value::as_str)
            .map(str::to_uppercase)
            .as_deref()
        {
            Some("BUY") => counts[3] += 1,
            Some("SELL") => counts[4] += 1,
            _ => {}
        }
        let t = o
            .get("api_type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(i) = keys[5..]
            .iter()
            .position(|k| k.strip_prefix("total_") == Some(t))
        {
            counts[5 + i] += 1;
        }
    }
    for (k, c) in keys.iter().zip(counts) {
        st.insert((*k).to_string(), json!(c));
    }
    Value::Object(st)
}

/// Web `get_action_center_data`: statistics over every order, the list
/// filtered by status (`None` = all).
pub fn data(ctx: &AppState, user: &str, status: Option<&str>) -> crate::error::Result<Value> {
    let now = ctx.now();
    let conn = ctx.sqlite.conn()?;
    let all: Vec<Value> = store::list(&conn, user, None)?
        .iter()
        .map(|r| parse_pending_order(r, now))
        .collect();
    let stats = statistics(&all);
    let orders = match status {
        Some(st) => store::list(&conn, user, Some(st))?
            .iter()
            .map(|r| parse_pending_order(r, now))
            .collect(),
        None => all,
    };
    Ok(json!({"orders": orders, "statistics": stats}))
}

// ------------------------------------------------------------------ execution

/// Outcome of [`execute_approved`]: like the web's `(success, data, status)`.
#[derive(Debug, Clone)]
pub struct Execution {
    pub success: bool,
    pub body: Value,
    pub status: u16,
}

impl Execution {
    fn fail(status: u16, msg: &str) -> Self {
        Self {
            success: false,
            body: json!({"status": "error", "message": msg}),
            status,
        }
    }

    fn message(&self) -> String {
        s(&self.body, "message")
    }
}

fn mark(ctx: &AppState, id: i64, order_id: Option<&str>, status: &str) {
    let r = ctx
        .sqlite
        .conn()
        .and_then(|c| store::update_broker_status(&c, id, order_id, status));
    if let Err(e) = r {
        tracing::error!("Could not record the result of pending order {}: {}", id, e);
    }
}

/// Leaf order results of basket, split and multi-leg replies.
fn flatten(results: &[Value], out: &mut Vec<Value>) {
    for r in results {
        if !r.is_object() {
            continue;
        }
        match r.get("split_results").and_then(Value::as_array) {
            Some(inner) => flatten(inner, out),
            None => out.push(r.clone()),
        }
    }
}

fn summarize_ids(ids: &[String]) -> String {
    let joined = ids.join(",");
    if joined.len() <= 255 {
        return joined;
    }
    let s = format!("{} (+{} more)", ids[0], ids.len() - 1);
    s.chars().take(255).collect()
}

async fn dispatch(ctx: &AppState, api_type: &str, data: &Value) -> Option<Reply> {
    use super::{
        batch_order_service as batch, gtt_service as gtt, options_order_service as opt,
        order_service as ord,
    };
    let r = Route::INTERNAL;
    Some(match api_type {
        "placeorder" => ord::place_order(ctx, data, r).await,
        "smartorder" => ord::place_smart_order(ctx, data, r).await,
        "basketorder" => batch::basket_order(ctx, data, r).await,
        "splitorder" => batch::split_order(ctx, data, r).await,
        "optionsorder" => opt::options_order(ctx, data, r).await,
        "optionsmultiorder" => opt::options_multi_order(ctx, data, r).await,
        "placegttorder" => gtt::place_gtt_with(ctx, data, r).await,
        _ => return None,
    })
}

/// Web `execute_approved_order`: claim, dispatch once, record the result.
pub async fn execute_approved(ctx: &AppState, id: i64) -> Execution {
    let row = match ctx.sqlite.conn().and_then(|c| store::get(&c, id)) {
        Ok(Some(r)) => r,
        Ok(None) => return Execution::fail(404, "Pending order not found"),
        Err(e) => {
            tracing::error!("Could not read pending order {}: {}", id, e);
            return Execution::fail(500, "The order could not be read. Try again.");
        }
    };
    if row.status != "approved" {
        return Execution::fail(
            400,
            &format!("Order cannot be executed (status: {})", row.status),
        );
    }
    // Claim before anything can reach the broker.
    match ctx.sqlite.conn().and_then(|c| store::claim(&c, id)) {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                "Pending order {} is already being executed; not sending it again",
                id
            );
            return Execution::fail(ALREADY_SUBMITTING_STATUS, ALREADY_SUBMITTING_MESSAGE);
        }
        Err(e) => {
            tracing::error!("Could not claim pending order {}: {}", id, e);
            let back = ctx
                .sqlite
                .conn()
                .and_then(|c| store::return_to_pending(&c, id, None))
                .unwrap_or(false);
            if back {
                let mut ex = Execution::fail(500, CLAIM_FAILED_MESSAGE);
                if let Some(m) = ex.body.as_object_mut() {
                    m.insert(RETURNED_TO_PENDING.into(), json!(true));
                }
                return ex;
            }
            return Execution::fail(500, CLAIM_FAILED_STUCK_MESSAGE);
        }
    }
    let data: Value = match serde_json::from_str(&row.order_data) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            mark(ctx, id, None, "rejected");
            return Execution::fail(
                500,
                "The stored order could not be read, so it was not sent.",
            );
        }
    };
    if !ctx.is_broker_connected() {
        tracing::warn!("Pending order {} not sent: no broker session", id);
        mark(ctx, id, None, "rejected");
        return Execution::fail(403, "Authentication failed");
    }
    tracing::info!("Executing approved order {} ({})", id, row.api_type);
    let Some(reply) = dispatch(ctx, &row.api_type, &data).await else {
        mark(ctx, id, None, "rejected");
        return Execution::fail(400, &format!("Unknown order type: {}", row.api_type));
    };
    // An order (or a leg of a basket or split) with no definite answer may
    // be at the broker: the claim's `submitting` mark stays, so the order is
    // shown unconfirmed and never sent again (LOG-08 P1-5).
    let uncertain = reply.placement.as_ref().is_some_and(|p| p.is_uncertain());
    let mut ex = Execution {
        success: reply.is_success(),
        body: reply.body,
        status: reply.status,
    };
    if !ex.success && uncertain {
        tracing::error!(
            "Approved order {} has no definite answer from the broker; it stays unconfirmed and is not sent again",
            id
        );
        return ex;
    }
    if !ex.success {
        mark(ctx, id, None, "rejected");
        tracing::warn!("Approved order {} was refused: {}", id, ex.message());
        return ex;
    }
    if row.api_type == "placegttorder" {
        let trigger = match ex.body.get("trigger_id") {
            Some(Value::String(t)) if !t.is_empty() => t.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => {
                mark(ctx, id, None, "rejected");
                if let Some(m) = ex.body.as_object_mut() {
                    m.insert("status".into(), json!("error"));
                    m.insert(
                        "message".into(),
                        json!("GTT placement succeeded without a trigger ID"),
                    );
                }
                ex.success = false;
                ex.status = 502;
                return ex;
            }
        };
        mark(ctx, id, Some(&trigger), "open");
        if let Some(m) = ex.body.as_object_mut() {
            m.insert("broker_order_id".into(), json!(trigger));
        }
        return ex;
    }
    if let Some(results) = ex.body.get("results").and_then(Value::as_array).cloned() {
        let mut leaves = Vec::new();
        flatten(&results, &mut leaves);
        let ok = |r: &Value| {
            r.get("status").and_then(Value::as_str) == Some("success")
                && r.get("orderid").is_some_and(|o| !o.is_null() && o != "")
        };
        let ids: Vec<String> = leaves
            .iter()
            .filter(|r| ok(r))
            .map(|r| match &r["orderid"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        if uncertain {
            tracing::error!(
                "Approved order {} has legs with no definite answer; it stays unconfirmed and is not sent again",
                id
            );
            let summary = summarize_ids(&ids);
            mark(
                ctx,
                id,
                (!ids.is_empty()).then_some(summary.as_str()),
                SUBMITTING,
            );
            return ex;
        }
        if ids.is_empty() {
            mark(ctx, id, None, "rejected");
            if let Some(m) = ex.body.as_object_mut() {
                m.insert("status".into(), json!("error"));
                m.insert(
                    "message".into(),
                    json!("No child orders were submitted successfully"),
                );
            }
            ex.success = false;
            ex.status = 502;
            return ex;
        }
        let partial = leaves.iter().any(|r| !ok(r));
        let summary = summarize_ids(&ids);
        mark(
            ctx,
            id,
            Some(&summary),
            if partial { "partial" } else { "open" },
        );
        if let Some(m) = ex.body.as_object_mut() {
            m.insert("broker_order_id".into(), json!(summary));
            m.insert("broker_order_ids".into(), json!(ids));
        }
        return ex;
    }
    if let Some(orderid) = ex.body.get("orderid").map(|o| match o {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }) {
        let status = super::account_service::order_status(ctx, &json!({"orderid": orderid})).await;
        let actual = if status.is_success() {
            status
                .body
                .get("data")
                .and_then(|d| d.get("status"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        mark(ctx, id, Some(&orderid), actual.as_deref().unwrap_or("open"));
        return ex;
    }
    // A success with no order id: a smart order whose position already
    // matched sends nothing, but the claim still needs a final status.
    let last = if row.api_type == "smartorder" {
        NO_ACTION
    } else {
        "open"
    };
    mark(ctx, id, None, last);
    ex
}

// ------------------------------------------------------------------ routes

fn updated(ctx: &AppState, payload: Value) {
    publish(ctx, Event::PendingOrderUpdated { payload });
}

fn already_handled(ctx: &AppState, id: i64, user: &str) -> bool {
    match ctx.sqlite.conn().and_then(|c| store::get(&c, id)) {
        Ok(Some(r)) => r.user_id == user && r.status != "pending",
        Ok(None) => false,
        Err(e) => {
            tracing::error!("Could not re-read pending order {}: {}", id, e);
            false
        }
    }
}

fn approve_row(ctx: &AppState, id: i64, user: &str) -> bool {
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::approve(&c, id, user, user, ctx.now()))
    {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("Could not approve pending order {}: {}", id, e);
            false
        }
    }
}

/// `POST /action-center/approve/<id>`.
pub async fn approve(ctx: &AppState, id: i64, user: &str) -> Reply {
    if !approve_row(ctx, id, user) {
        if already_handled(ctx, id, user) {
            return Reply::error(409, ALREADY_HANDLED_MESSAGE);
        }
        return Reply::error(400, "Failed to approve order");
    }
    let ex = execute_approved(ctx, id).await;
    if !ex.success && ex.status == ALREADY_SUBMITTING_STATUS {
        return Reply::error(ALREADY_SUBMITTING_STATUS, ex.message());
    }
    if !ex.success && ex.body.get(RETURNED_TO_PENDING) == Some(&json!(true)) {
        updated(
            ctx,
            json!({"action": "returned", "order_id": id, "user_id": user}),
        );
        return Reply::error(ex.status, ex.message());
    }
    updated(
        ctx,
        json!({"action": "approved", "order_id": id, "user_id": user}),
    );
    if ex.success {
        let bid = ["broker_order_id", "orderid", "trigger_id"]
            .iter()
            .find_map(|k| {
                ex.body
                    .get(*k)
                    .filter(|v| !v.is_null() && *v != "")
                    .cloned()
            })
            .unwrap_or(Value::Null);
        return Reply::ok(json!({
            "status": "success",
            "message": "Order approved and executed successfully",
            "broker_order_id": bid,
        }));
    }
    Reply::new(
        ex.status,
        json!({
            "status": "warning",
            "message": "Order approved but execution failed",
            "error": ex.body.get("message").cloned().unwrap_or(Value::Null),
        }),
    )
}

/// `POST /action-center/reject/<id>`.
pub fn reject(ctx: &AppState, id: i64, user: &str, reason: &str) -> Reply {
    let ok = ctx
        .sqlite
        .conn()
        .and_then(|c| store::reject(&c, id, reason, user, user, ctx.now()));
    match ok {
        Ok(true) => {
            updated(
                ctx,
                json!({"action": "rejected", "order_id": id, "user_id": user}),
            );
            Reply::ok(json!({"status": "success", "message": "Order rejected successfully"}))
        }
        Ok(false) => Reply::error(400, "Failed to reject order"),
        Err(e) => {
            tracing::error!("Could not reject pending order {}: {}", id, e);
            Reply::error(400, "Failed to reject order")
        }
    }
}

/// `DELETE /action-center/delete/<id>` (only orders no longer pending).
pub fn delete(ctx: &AppState, id: i64, user: &str) -> Reply {
    match ctx.sqlite.conn().and_then(|c| store::delete(&c, id, user)) {
        Ok(true) => {
            updated(
                ctx,
                json!({"action": "deleted", "order_id": id, "user_id": user}),
            );
            Reply::ok(json!({"status": "success", "message": "Order deleted successfully"}))
        }
        Ok(false) => Reply::error(400, "Failed to delete order"),
        Err(e) => {
            tracing::error!("Could not delete pending order {}: {}", id, e);
            Reply::error(400, "Failed to delete order")
        }
    }
}

/// `GET /action-center/count`.
pub fn count(ctx: &AppState, user: &str) -> Reply {
    let n = ctx
        .sqlite
        .conn()
        .and_then(|c| store::pending_count(&c, user))
        .unwrap_or_else(|e| {
            tracing::error!("Could not count pending orders: {}", e);
            0
        });
    Reply::ok(json!({"count": n}))
}

/// `POST /action-center/approve-all`.
pub async fn approve_all(ctx: &AppState, user: &str) -> Reply {
    let pending = match ctx
        .sqlite
        .conn()
        .and_then(|c| store::list(&c, user, Some("pending")))
    {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Could not list pending orders: {}", e);
            Vec::new()
        }
    };
    if pending.is_empty() {
        return Reply::ok(json!({"status": "info", "message": "No pending orders to approve"}));
    }
    let (mut approved, mut executed) = (0i64, 0i64);
    let mut failed: Vec<Value> = Vec::new();
    let mut handled: Vec<i64> = Vec::new();
    for row in &pending {
        if !approve_row(ctx, row.id, user) {
            if already_handled(ctx, row.id, user) {
                handled.push(row.id);
            }
            continue;
        }
        approved += 1;
        let ex = execute_approved(ctx, row.id).await;
        if ex.success {
            executed += 1;
        } else if ex.status == ALREADY_SUBMITTING_STATUS {
            handled.push(row.id);
        } else {
            let msg = ex
                .body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error")
                .to_string();
            failed.push(json!({"order_id": row.id, "error": msg}));
        }
    }
    updated(
        ctx,
        json!({"action": "batch_approved", "user_id": user, "count": approved}),
    );
    let (mut message, status) = if approved == 0 && !handled.is_empty() {
        (
            "These orders were already approved or rejected from another screen, so none were sent again. Check the Action Center.".to_string(),
            "warning",
        )
    } else if approved == executed {
        (
            format!("Successfully approved and executed all {} orders", approved),
            "success",
        )
    } else if failed.is_empty() {
        (
            format!(
                "Approved {} orders. {} executed successfully",
                approved, executed
            ),
            "success",
        )
    } else if executed > 0 {
        (
            format!(
                "Approved {} orders. {} executed successfully, {} failed",
                approved,
                executed,
                failed.len()
            ),
            "warning",
        )
    } else {
        (
            format!("Approved {} orders but all executions failed", approved),
            "error",
        )
    };
    if !handled.is_empty() && approved > 0 {
        let n = handled.len();
        message.push_str(&format!(
            ". {} {} already handled from another screen and not sent again",
            n,
            if n == 1 { "order was" } else { "orders were" }
        ));
    }
    Reply::ok(json!({
        "status": status,
        "message": message,
        "approved_count": approved,
        "executed_count": executed,
        "failed_executions": failed,
        "already_handled": handled,
    }))
}

/// Pending orders left in `submitting` (a crash mid-send): never resent.
pub fn is_unconfirmed(row: &PendingOrderRow) -> bool {
    row.status == "approved" && row.broker_status.as_deref() == Some(SUBMITTING)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(api_type: &str, data: Value) -> PendingOrderRow {
        PendingOrderRow {
            id: 1,
            user_id: "u".into(),
            api_type: api_type.into(),
            order_data: data.to_string(),
            created_at: "2026-10-05 04:30:00.000000".into(),
            created_at_ist: Some("2026-10-05 10:00:00 IST".into()),
            status: "pending".into(),
            approved_at: None,
            approved_at_ist: None,
            approved_by: None,
            rejected_at_ist: None,
            rejected_by: None,
            rejected_reason: None,
            broker_order_id: None,
            broker_status: None,
        }
    }

    #[test]
    fn parses_each_order_type_like_the_web() {
        let now = Utc::now();
        let p = parse_pending_order(
            &row(
                "placeorder",
                json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY",
                "quantity": 5, "pricetype": "MARKET", "product": "MIS", "apikey": "k"}),
            ),
            now,
        );
        assert_eq!(p["symbol"], "SBIN");
        assert_eq!(p["price"], "0");
        assert_eq!(p["price_type"], "MARKET");
        assert_eq!(p["strategy"], "N/A");
        assert!(p["raw_order_data"].get("apikey").is_none());
        assert_eq!(p["approved_age_seconds"], Value::Null);
        let b = parse_pending_order(
            &row(
                "basketorder",
                json!({"orders": [{"quantity": 2, "exchange": "NSE"}, {"quantity": "3"}]}),
            ),
            now,
        );
        assert_eq!(b["symbol"], "Basket (2 orders)");
        assert_eq!(b["quantity"], "5");
        assert_eq!(b["exchange"], "Multiple");
        let m = parse_pending_order(
            &row(
                "optionsmultiorder",
                json!({"underlying": "NIFTY", "legs": [
                {"action": "buy", "quantity": 75}, {"action": "SELL", "quantity": 75}]}),
            ),
            now,
        );
        assert_eq!(m["symbol"], "NIFTY (2 legs)");
        assert_eq!(m["action"], "MULTI");
        assert_eq!(m["quantity"], 150);
        assert_eq!(m["product_type"], "MIS");
        let g = parse_pending_order(
            &row(
                "placegttorder",
                json!({"trigger_type": "OCO", "triggerprice_sl": 90, "triggerprice_tg": 110}),
            ),
            now,
        );
        assert_eq!(g["trigger_price"], "90 / 110");
        let s = parse_pending_order(
            &row("splitorder", json!({"quantity": 100, "splitsize": 25})),
            now,
        );
        assert_eq!(s["quantity"], "100 (split: 25)");
        let bad = parse_pending_order(
            &PendingOrderRow {
                order_data: "{".into(),
                ..row("x", json!({}))
            },
            now,
        );
        assert_eq!(bad["symbol"], "Error parsing order");
        let st = statistics(&[p, b, m]);
        assert_eq!(st["total_pending"], 3);
        assert_eq!(st["total_buy_orders"], 1);
        assert_eq!(st["total_basketorder"], 1);
        assert_eq!(st.as_object().unwrap().len(), 12);
    }

    #[test]
    fn order_id_summary_fits_the_column() {
        let ids: Vec<String> = (0..100).map(|i| format!("ORDER-{:010}", i)).collect();
        let s = summarize_ids(&ids);
        assert!(s.len() <= 255);
        assert!(s.ends_with("(+99 more)"));
        assert_eq!(summarize_ids(&["a".into(), "b".into()]), "a,b");
    }
}
