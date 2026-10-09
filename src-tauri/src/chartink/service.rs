//! What the `/chartink` routes do (web `blueprints/chartink.py`), with the
//! web's bodies: the JSON API uses `message`, the older form routes
//! (delete, configure, symbol delete) use `error`.

use super::store::{NewMapping, NewStrategy, Strategy};
use super::webhook;
use crate::services::core::Reply;
use crate::state::AppState;
use chrono::NaiveTime;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Web `VALID_EXCHANGES`.
pub const VALID_EXCHANGES: &[&str] = &["NSE", "BSE"];

fn msg_err(status: u16, msg: impl Into<String>) -> Reply {
    Reply::error(status, msg)
}

fn form_err(status: u16, msg: impl Into<String>) -> Reply {
    Reply::new(status, json!({"status": "error", "error": msg.into()}))
}

/// Web `validate_strategy_name`: the name with its `chartink_` prefix.
pub fn validate_name(name: &str) -> Result<String, String> {
    if name.is_empty() {
        return Err("Strategy name is required".into());
    }
    let name = if name.starts_with("chartink_") {
        name.to_string()
    } else {
        format!("chartink_{}", name)
    };
    let ok = name
        .replace("chartink_", "")
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ');
    if !ok {
        return Err(
            "Strategy name can only contain letters, numbers, spaces, hyphens and underscores"
                .into(),
        );
    }
    Ok(name)
}

/// Web `validate_strategy_times`.
pub fn validate_times(start: &str, end: &str, squareoff: &str) -> Result<(), String> {
    let p = |s: &str| NaiveTime::parse_from_str(s.trim(), "%H:%M");
    let (Ok(s), Ok(e), Ok(q)) = (p(start), p(end), p(squareoff)) else {
        return Err("Invalid time format".into());
    };
    if s >= e {
        return Err("Start time must be before end time".into());
    }
    if e >= q {
        return Err("End time must be before square off time".into());
    }
    Ok(())
}

/// The strategy when it exists and is this user's, else the web's reply.
fn owned(ctx: &AppState, id: i64, user: &str, form: bool) -> Result<Strategy, Reply> {
    let mk = |status: u16, m: &str| {
        if form {
            form_err(status, m)
        } else {
            msg_err(status, m)
        }
    };
    match ctx.chartink.store.get(id) {
        Ok(Some(s)) if s.user_id == user => Ok(s),
        Ok(Some(_)) => Err(mk(403, "Unauthorized")),
        Ok(None) => Err(mk(404, "Strategy not found")),
        Err(e) => {
            tracing::error!("Chartink strategy {} could not be read: {}", id, e);
            Err(mk(404, "Strategy not found"))
        }
    }
}

pub fn list(ctx: &AppState, user: &str) -> Reply {
    let rows = ctx.chartink.store.for_user(user).unwrap_or_else(|e| {
        tracing::error!("Chartink strategies could not be read: {}", e);
        Vec::new()
    });
    Reply::ok(json!({"strategies": rows.iter().map(Strategy::to_dict).collect::<Vec<_>>()}))
}

pub fn get(ctx: &AppState, user: &str, id: i64) -> Reply {
    let s = match owned(ctx, id, user, false) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let mappings = ctx.chartink.store.mappings(id).unwrap_or_default();
    Reply::ok(json!({
        "strategy": s.to_dict(),
        "mappings": mappings.iter().map(|m| m.to_dict()).collect::<Vec<_>>(),
    }))
}

fn opt_text(body: &Map<String, Value>, k: &str) -> Option<String> {
    match body.get(k) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

pub fn create(ctx: &AppState, user: &str, body: Option<&Map<String, Value>>) -> Reply {
    let Some(body) = body.filter(|b| !b.is_empty()) else {
        return msg_err(400, "No data provided");
    };
    let name = match body.get("name") {
        Some(Value::String(s)) => s.trim().to_string(),
        None => String::new(),
        Some(_) => return msg_err(400, "Strategy name is required"),
    };
    let name = match validate_name(&name) {
        Ok(n) => n,
        Err(m) => return msg_err(400, m),
    };
    let strategy_type = opt_text(body, "strategy_type").unwrap_or_else(|| "intraday".into());
    let is_intraday = strategy_type == "intraday";
    let (mut start, mut end, mut squareoff) = (
        opt_text(body, "start_time"),
        opt_text(body, "end_time"),
        opt_text(body, "squareoff_time"),
    );
    if is_intraday {
        let (Some(s), Some(e), Some(q)) = (&start, &end, &squareoff) else {
            return msg_err(400, "All time fields are required for intraday strategy");
        };
        if let Err(m) = validate_times(s, e, q) {
            return msg_err(400, m);
        }
    } else {
        start = None;
        end = None;
        squareoff = None;
    }
    let new = NewStrategy {
        name,
        webhook_id: uuid::Uuid::new_v4().to_string(),
        user_id: user.to_string(),
        is_intraday,
        start_time: start,
        end_time: end,
        squareoff_time: squareoff,
    };
    match ctx.chartink.store.create(&new, ctx.now()) {
        Ok(s) => Reply::ok(json!({"status": "success", "data": {"strategy_id": s.id}})),
        Err(e) => {
            tracing::error!("Chartink strategy could not be created: {}", e);
            msg_err(500, "Failed to create strategy")
        }
    }
}

pub fn toggle(ctx: &AppState, user: &str, id: i64) -> Reply {
    if let Err(r) = owned(ctx, id, user, false) {
        return r;
    }
    match ctx.chartink.store.toggle(id, ctx.now()) {
        Ok(Some(s)) => {
            // Turning the strategy off or on is how the trader unlocks a
            // webhook locked after wrong-address alerts.
            ctx.chartink.guard.unlock(id);
            Reply::ok(json!({"status": "success", "data": {"is_active": s.is_active}}))
        }
        Ok(None) => msg_err(500, "Failed to toggle strategy"),
        Err(e) => {
            tracing::error!("Chartink strategy {} could not be toggled: {}", id, e);
            msg_err(500, "Failed to toggle strategy")
        }
    }
}

pub fn delete(ctx: &AppState, user: &str, id: i64) -> Reply {
    if let Err(r) = owned(ctx, id, user, true) {
        return r;
    }
    match ctx.chartink.store.delete(id) {
        Ok(true) => {
            ctx.chartink.guard.unlock(id);
            Reply::ok(json!({"status": "success"}))
        }
        Ok(false) => form_err(500, "Failed to delete strategy"),
        Err(e) => {
            tracing::error!("Chartink strategy {} could not be deleted: {}", id, e);
            form_err(500, "Failed to delete strategy")
        }
    }
}

fn field_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_bulk(text: &str) -> Result<Vec<NewMapping>, String> {
    let mut out = Vec::new();
    for line in text.trim().split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.trim().split(',').collect();
        if parts.len() != 4 {
            return Err(format!("Invalid format in line: {}", line));
        }
        let (symbol, exchange, quantity, product) = (parts[0], parts[1], parts[2], parts[3]);
        if !VALID_EXCHANGES.contains(&exchange) {
            return Err(format!("Invalid exchange: {}", exchange));
        }
        let quantity = quantity
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: '{}'", quantity))?;
        out.push(NewMapping {
            chartink_symbol: symbol.trim().to_string(),
            exchange: exchange.trim().to_string(),
            quantity,
            product_type: product.trim().to_string(),
        });
    }
    Ok(out)
}

/// `POST /chartink/<id>/configure`: one symbol, or `symbols` as CSV lines
/// `SYMBOL,EXCHANGE,QUANTITY,PRODUCT`.
pub fn configure(ctx: &AppState, user: &str, id: i64, data: &Map<String, Value>) -> Reply {
    if let Err(r) = owned(ctx, id, user, true) {
        return r;
    }
    let now = ctx.now();
    if let Some(v) = data.get("symbols") {
        let text = match v {
            Value::String(s) => s.clone(),
            _ => {
                return form_err(
                    400,
                    "Symbols must be text, one SYMBOL,EXCHANGE,QUANTITY,PRODUCT per line",
                )
            }
        };
        let rows = match parse_bulk(&text) {
            Ok(r) => r,
            Err(m) => return form_err(400, m),
        };
        if !rows.is_empty() {
            if let Err(e) = ctx.chartink.store.add_mappings(id, &rows, now) {
                tracing::error!("Chartink symbols for strategy {} not saved: {}", id, e);
                return form_err(400, "The symbols could not be saved");
            }
        }
        return Reply::ok(json!({"status": "success"}));
    }
    let symbol = field_text(data.get("symbol"));
    let exchange = field_text(data.get("exchange"));
    let quantity = field_text(data.get("quantity"));
    let product = field_text(data.get("product_type"));
    let missing: Vec<&str> = ["symbol", "exchange", "quantity", "product_type"]
        .into_iter()
        .filter(|k| !data.get(*k).is_some_and(crate::scalping::service::truthy))
        .collect();
    if !missing.is_empty() {
        return form_err(
            400,
            format!("Missing required fields: {}", missing.join(", ")),
        );
    }
    let (symbol, exchange, quantity, product) = (
        symbol.unwrap_or_default(),
        exchange.unwrap_or_default(),
        quantity.unwrap_or_default(),
        product.unwrap_or_default(),
    );
    if !VALID_EXCHANGES.contains(&exchange.as_str()) {
        return form_err(400, format!("Invalid exchange: {}", exchange));
    }
    let Ok(quantity) = quantity.trim().parse::<i64>() else {
        return form_err(400, "Quantity must be a valid number");
    };
    if quantity <= 0 {
        return form_err(400, "Quantity must be greater than 0");
    }
    let row = NewMapping {
        chartink_symbol: symbol,
        exchange,
        quantity,
        product_type: product,
    };
    match ctx.chartink.store.add_mappings(id, &[row], now) {
        Ok(()) => Reply::ok(json!({"status": "success"})),
        Err(e) => {
            tracing::error!("Chartink symbol for strategy {} not saved: {}", id, e);
            form_err(400, "Failed to add symbol mapping")
        }
    }
}

pub fn delete_symbol(ctx: &AppState, user: &str, id: i64, mapping_id: i64) -> Reply {
    match ctx.chartink.store.get(id) {
        Ok(Some(s)) if s.user_id == user => {}
        _ => return form_err(404, "Strategy not found"),
    }
    match ctx.chartink.store.delete_mapping(id, mapping_id) {
        Ok(_) => Reply::ok(json!({"status": "success"})),
        Err(e) => {
            tracing::error!("Chartink symbol {} could not be deleted: {}", mapping_id, e);
            form_err(400, "The symbol could not be deleted")
        }
    }
}

/// `GET /chartink/search?q=&exchange=`.
pub fn search(ctx: &AppState, q: &str, exchange: Option<&str>) -> Reply {
    let q = q.trim();
    if q.is_empty() {
        return Reply::ok(json!({"results": []}));
    }
    let snap = ctx.symbols.snapshot();
    let rows = crate::services::search_ui_service::enhanced_search(
        snap.rows(),
        Some(q),
        exchange.filter(|e| !e.is_empty()),
    );
    let results: Vec<Value> = rows
        .iter()
        .map(|r| json!({"symbol": r.symbol, "name": r.name, "exchange": r.exchange}))
        .collect();
    Reply::ok(json!({"results": results}))
}

/// Whether `webhook_id` is a live Chartink webhook address: its locator,
/// then a constant-time match. Cheap, and records nothing.
pub fn webhook_known(ctx: &AppState, webhook_id: &str) -> bool {
    let Some(locator) = webhook::locator(webhook_id) else {
        return false;
    };
    ctx.chartink
        .store
        .by_locator(locator)
        .map(|c| {
            c.iter()
                .any(|s| webhook::id_matches(webhook_id, &s.webhook_id))
        })
        .unwrap_or(false)
}

/// The webhook after `admit` and the size cap: resolve the id in constant
/// time, apply the per-webhook lock, then run the alert. The `bool` is
/// whether this request failed authentication (counted per address by the
/// route).
pub async fn webhook_alert(
    ctx: &Arc<AppState>,
    webhook_id: &str,
    body: &[u8],
) -> (u16, Value, bool) {
    let invalid = || {
        (
            404,
            json!({"status": "error", "error": webhook::INVALID_WEBHOOK}),
            true,
        )
    };
    let Some(locator) = webhook::locator(webhook_id) else {
        return invalid();
    };
    let candidates = match ctx.chartink.store.by_locator(locator) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Chartink webhook lookup failed: {}", e);
            return invalid();
        }
    };
    let found = candidates
        .iter()
        .find(|s| webhook::id_matches(webhook_id, &s.webhook_id))
        .cloned();
    let Some(strategy) = found else {
        // A wrong id that shares a webhook's locator is somebody probing
        // that webhook: count it there, whatever address it came from.
        for s in &candidates {
            if ctx.chartink.guard.record_failure(s.id) {
                tracing::warn!(
                    "Chartink strategy {} webhook locked after {} alerts with a wrong address",
                    s.id,
                    webhook::LOCKOUT_FAILURES
                );
            }
        }
        tracing::warn!("Chartink alert with an unknown webhook address refused");
        return invalid();
    };
    if ctx.chartink.guard.is_locked(strategy.id) {
        return (
            403,
            json!({"status": "error", "error": webhook::LOCKED_MESSAGE}),
            false,
        );
    }
    let (status, body) = super::handle_alert(&ctx.chartink, &strategy, body).await;
    (status, body, false)
}
