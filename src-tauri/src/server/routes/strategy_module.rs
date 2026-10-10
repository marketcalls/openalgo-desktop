//! Web `blueprints/strategy_module.py`: the `/strategy/api` session routes,
//! the public `/strategy/webhook/<token>` endpoint, and the
//! `strategy_subscribe` / `strategy_unsubscribe` Socket.IO handlers.
//!
//! Rules every route holds to:
//!
//! * **404, never 403, for a strategy that is not yours**: every `{sid}`
//!   route resolves through the owner-scoped read first.
//! * **409 while a strategy is running** for edits and deletes.
//! * **A PATCH is validated as the strategy it will become**: the change set
//!   is merged onto the stored config and the whole is re-validated.
//! * `mode` is required on start and never defaulted; live is opt-in.
//! * No response carries a webhook token except create and rotate (once).

use crate::error::AppError;
use crate::server::envelope::json_response;
use crate::server::middleware::{ClientIp, User};
use crate::state::AppState;
use crate::strategy::store::{
    ALREADY_RUNNING, CHANGED_WHILE_EDITING, CHANGED_WHILE_STARTING, EVENT_KINDS, EVENT_SEVERITIES,
    RUN_MODES, STRATEGY_STATUSES,
};
use crate::strategy::validate::{config_fields, validate_strategy_config};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

pub const NOT_FOUND: &str = "Strategy not found";
pub const CLOSE_ALL_REQUESTED_MESSAGE: &str = "Operator requested closure of all held legs";
const EVENTS_DEFAULT_LIMIT: i64 = 500;
const EVENTS_MAX_LIMIT: i64 = 1000;

fn ok(payload: Value, code: StatusCode) -> Response {
    let mut body = Map::new();
    body.insert("status".into(), json!("success"));
    if let Value::Object(m) = payload {
        body.extend(m);
    }
    json_response(code, Value::Object(body))
}

fn ok200(payload: Value) -> Response {
    ok(payload, StatusCode::OK)
}

fn err(message: impl Into<String>, code: StatusCode) -> Response {
    json_response(code, json!({"status": "error", "message": message.into()}))
}

fn err_with(message: impl Into<String>, code: StatusCode, extra: Value) -> Response {
    let mut body = Map::new();
    body.insert("status".into(), json!("error"));
    body.insert("message".into(), json!(message.into()));
    if let Value::Object(m) = extra {
        body.extend(m);
    }
    json_response(code, Value::Object(body))
}

fn store_error(e: AppError) -> Response {
    match e {
        AppError::NotFound(_) => err(NOT_FOUND, StatusCode::NOT_FOUND),
        AppError::Validation(m)
            if m.starts_with("Stop the strategy")
                || m.contains("already exists")
                || m == CHANGED_WHILE_EDITING =>
        {
            err(m, StatusCode::CONFLICT)
        }
        AppError::Validation(m) => err(m, StatusCode::BAD_REQUEST),
        other => {
            tracing::error!("Strategy store call failed: {}", other);
            err(
                "The request could not be completed. Try again.",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

/// `(row)` for an owner-scoped route, or the 404 that hides another user's id.
#[allow(clippy::result_large_err)]
fn resolve(
    ctx: &AppState,
    user: &str,
    sid: &str,
) -> Result<crate::strategy::store::StrategyRow, Response> {
    let Ok(id) = sid.parse::<i64>() else {
        return Err(err(NOT_FOUND, StatusCode::NOT_FOUND));
    };
    match ctx.strategy.store.get_strategy(id, user) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(err(NOT_FOUND, StatusCode::NOT_FOUND)),
        // A failed read is not an absent strategy (SM-04): a stop or a kill
        // switch must not be told "not found" for a strategy that exists.
        Err(e) => {
            tracing::error!("Could not read strategy {}: {}", id, e);
            Err(read_failed())
        }
    }
}

/// The answer to a local read that failed (SM-04): an error, never a
/// successful empty list that reads as "no history" or "no positions".
fn read_failed() -> Response {
    err(
        "This strategy's records could not be read just now, so nothing is shown rather than an incomplete picture. Try again; if it keeps happening, restart the app.",
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// `{"data": rows}` from a store read, or the logged read failure.
fn data_or_failed<T: serde::Serialize>(
    what: &str,
    sid: i64,
    read: crate::error::Result<T>,
) -> Response {
    match read {
        Ok(rows) => ok200(json!({ "data": rows })),
        Err(e) => {
            tracing::error!("Could not read the {} of strategy {}: {}", what, sid, e);
            read_failed()
        }
    }
}

#[allow(clippy::result_large_err)]
fn json_body(bytes: &Bytes) -> Result<Map<String, Value>, Response> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(err(
            "The request body must be a JSON object",
            StatusCode::BAD_REQUEST,
        )),
        Err(_) => Err(err("A JSON body is required", StatusCode::BAD_REQUEST)),
    }
}

#[allow(clippy::result_large_err)]
fn int_arg(q: &HashMap<String, String>, name: &str) -> Result<Option<i64>, Response> {
    match q.get(name).map(String::as_str) {
        None | Some("") => Ok(None),
        Some(v) => v.parse::<i64>().map(Some).map_err(|_| {
            err(
                format!("{} must be a whole number", name),
                StatusCode::BAD_REQUEST,
            )
        }),
    }
}

/// Bring this strategy's jobs in line with what was just saved.
fn sync_schedule(ctx: &AppState, sid: i64, removed: bool) {
    if removed {
        ctx.strategy.remove_strategy_jobs(sid);
    } else {
        ctx.strategy.sync_strategy_jobs(sid);
    }
}

async fn record(
    ctx: &AppState,
    sid: i64,
    user: &str,
    kind: &str,
    message: &str,
    f: crate::strategy::store::EventFields,
) {
    ctx.strategy.emit(sid, user, kind, message, f).await;
}

// ------------------------------------------------------------------ CRUD

/// GET /strategy/api/strategies
pub async fn list(State(ctx): Ctx, User(u): User, Query(q): Q) -> Response {
    let status = q.get("status").filter(|s| !s.is_empty());
    if let Some(s) = status {
        if !STRATEGY_STATUSES.contains(&s.as_str()) {
            return err(
                format!("status must be one of: {}", STRATEGY_STATUSES.join(", ")),
                StatusCode::BAD_REQUEST,
            );
        }
    }
    let query: String = q
        .get("q")
        .map(|s| s.trim().chars().take(100).collect())
        .unwrap_or_default();
    match ctx.strategy.store.list_strategies(
        &u.username,
        status.map(String::as_str),
        (!query.is_empty()).then_some(query.as_str()),
    ) {
        Ok(rows) => ok200(json!({"data": rows})),
        Err(e) => {
            tracing::error!("Could not list strategies: {}", e);
            err(
                "Your strategies could not be read just now. Try again; if it keeps happening, restart the app.",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

/// POST /strategy/api/strategies
pub async fn create(State(ctx): Ctx, User(u): User, body: Bytes) -> Response {
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let config = match validate_strategy_config(&Value::Object(payload), &ctx.symbols.snapshot()) {
        Ok(c) => c,
        Err(m) => return err(m, StatusCode::BAD_REQUEST),
    };
    let (row, token) = match ctx.strategy.store.create_strategy(&u.username, &config) {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    record(
        &ctx,
        row.id,
        &u.username,
        "strategy_created",
        &format!("Strategy '{}' created", row.name),
        Default::default(),
    )
    .await;
    sync_schedule(&ctx, row.id, false);
    ok(
        json!({
            "data": row.to_dict(true),
            "webhook_token": token,
            "message": "Copy the webhook token now. It is stored as a hash and cannot be shown again; rotate it if you lose it.",
        }),
        StatusCode::CREATED,
    )
}

/// GET /strategy/api/strategies/{sid}
pub async fn detail(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    match resolve(&ctx, &u.username, &sid) {
        Ok(row) => ok200(json!({"data": row.to_dict(true)})),
        Err(r) => r,
    }
}

/// PATCH /strategy/api/strategies/{sid}
pub async fn update(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    body: Bytes,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let mut payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if payload.is_empty() {
        return err("Nothing to update", StatusCode::BAD_REQUEST);
    }
    let fields = config_fields();
    let mut unknown: Vec<&String> = payload
        .keys()
        .filter(|k| !fields.contains(&k.as_str()))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return err(
            format!(
                "The request does not accept {}. Updatable fields: {}",
                unknown
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                fields.join(", ")
            ),
            StatusCode::BAD_REQUEST,
        );
    }
    if row.status == "running" {
        return err("Stop the strategy before editing it", StatusCode::CONFLICT);
    }
    if let Some(kind) = payload.get("strategy_kind").filter(|v| !v.is_null()) {
        if kind.as_str() != Some(row.strategy_kind.as_str()) {
            return err(
                "A strategy cannot change between batch and signal. The two kinds do not share a leg shape, so every leg would describe the wrong kind of contract. Create a new strategy instead.",
                StatusCode::BAD_REQUEST,
            );
        }
    }
    payload.remove("strategy_kind");
    if payload.is_empty() {
        return err("Nothing to update", StatusCode::BAD_REQUEST);
    }
    let stored = row.to_dict(true);
    let mut merged = Map::new();
    for f in &fields {
        if let Some(v) = stored.get(*f) {
            merged.insert((*f).to_string(), v.clone());
        }
    }
    for (k, v) in &payload {
        merged.insert(k.clone(), v.clone());
    }
    let config = match validate_strategy_config(&Value::Object(merged), &ctx.symbols.snapshot()) {
        Ok(c) => c,
        Err(m) => return err(m, StatusCode::BAD_REQUEST),
    };
    let mut changes = Map::new();
    for k in payload.keys() {
        if let Some(v) = config.get(k) {
            changes.insert(k.clone(), v.clone());
        }
    }
    // Saved only if the strategy is still the revision validated above.
    let updated =
        match ctx
            .strategy
            .store
            .update_strategy(row.id, &u.username, row.revision, &changes)
        {
            Ok(r) => r,
            Err(e) => return store_error(e),
        };
    let mut names: Vec<&String> = changes.keys().collect();
    names.sort();
    record(
        &ctx,
        row.id,
        &u.username,
        "strategy_updated",
        &format!(
            "Updated {}",
            names
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        crate::strategy::store::EventFields {
            payload: Some(json!({"fields": names})),
            ..Default::default()
        },
    )
    .await;
    sync_schedule(&ctx, row.id, false);
    ok200(json!({"data": updated.to_dict(true)}))
}

/// DELETE /strategy/api/strategies/{sid}
pub async fn delete(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if row.status == "running" {
        return err("Stop the strategy before deleting it", StatusCode::CONFLICT);
    }
    if let Err(e) = ctx.strategy.store.delete_strategy(row.id, &u.username) {
        return store_error(e);
    }
    sync_schedule(&ctx, row.id, true);
    ok200(json!({"message": "Strategy deleted"}))
}

// ------------------------------------------------------------------ token, live, kill switch

/// POST /strategy/api/strategies/{sid}/webhook/rotate
pub async fn rotate_webhook(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let token = match ctx.strategy.store.rotate_webhook_token(row.id, &u.username) {
        Ok(t) => t,
        Err(e) => return store_error(e),
    };
    record(
        &ctx,
        row.id,
        &u.username,
        "webhook_token_rotated",
        "Webhook token rotated",
        Default::default(),
    )
    .await;
    ok200(json!({
        "webhook_token": token,
        "message": "Copy the new webhook token now. The previous one no longer works and this one cannot be shown again.",
    }))
}

/// POST /strategy/api/strategies/{sid}/live  `{"enabled": bool}`
pub async fn set_live(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    body: Bytes,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Some(enabled) = payload.get("enabled") else {
        return err("enabled is required", StatusCode::BAD_REQUEST);
    };
    let Some(enabled) = enabled.as_bool() else {
        return err("enabled must be true or false", StatusCode::BAD_REQUEST);
    };
    if row.status == "running" {
        return err(
            "Stop the strategy before changing its mode",
            StatusCode::CONFLICT,
        );
    }
    if let Err(e) = ctx
        .strategy
        .store
        .set_live_enabled(row.id, &u.username, enabled)
    {
        return store_error(e);
    }
    record(
        &ctx,
        row.id,
        &u.username,
        if enabled {
            "live_enabled"
        } else {
            "live_disabled"
        },
        if enabled {
            "Live trading enabled"
        } else {
            "Live trading disabled"
        },
        crate::strategy::store::EventFields {
            severity: Some(if enabled { "warn" } else { "info" }),
            ..Default::default()
        },
    )
    .await;
    ok200(json!({"live_enabled": enabled}))
}

/// POST /strategy/api/strategies/{sid}/kill_switch: lock the webhook, then
/// flatten. The lock goes on first so a signal mid-flatten cannot re-enter.
pub async fn kill_switch(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = ctx
        .strategy
        .store
        .set_webhook_locked(row.id, &u.username, true)
    {
        return store_error(e);
    }
    let run_id = row.current_run_id;
    let (mut accepted, mut stop_pending, mut exits) = (false, false, vec![]);
    if let Some(run_id) = run_id {
        let r = ctx.strategy.stop_run(run_id, &u.username, "manual").await;
        accepted = r.ok;
        stop_pending = r.stop_pending;
        exits = r.exits;
        if !accepted {
            tracing::error!(
                "Kill switch on strategy {} locked the webhook but could not flatten run {}",
                row.id,
                run_id
            );
        }
    }
    let stopped = accepted && !stop_pending;
    let flatten = if stopped {
        " and open legs closed"
    } else if accepted && stop_pending {
        "; exit fills pending"
    } else if stop_pending {
        "; flatten refused, stop remains pending and can be retried"
    } else {
        ""
    };
    record(
        &ctx,
        row.id,
        &u.username,
        "webhook_locked",
        &format!("Kill switch engaged{}", flatten),
        crate::strategy::store::EventFields {
            run_id,
            severity: Some("critical"),
            ..Default::default()
        },
    )
    .await;
    ok200(json!({
        "webhook_locked": true,
        "run_stopped": stopped,
        "stop_pending": stop_pending,
        "exits": exits,
        "message": format!("Webhook locked{}", flatten),
    }))
}

/// POST /strategy/api/strategies/{sid}/unlock_webhook
pub async fn unlock_webhook(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(e) = ctx
        .strategy
        .store
        .set_webhook_locked(row.id, &u.username, false)
    {
        return store_error(e);
    }
    ctx.strategy.webhook.clear_webhook_failures(row.id);
    ok200(json!({"webhook_locked": false, "message": "Webhook unlocked"}))
}

// ------------------------------------------------------------------ lifecycle

/// POST /strategy/api/strategies/{sid}/start  `{"mode": "live"|"sandbox"}`
pub async fn start(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    body: Bytes,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let mode = payload.get("mode").and_then(Value::as_str).unwrap_or("");
    if !RUN_MODES.contains(&mode) {
        let mut modes = RUN_MODES.to_vec();
        modes.sort();
        return err(
            format!("mode must be one of: {}", modes.join(", ")),
            StatusCode::BAD_REQUEST,
        );
    }
    let r = ctx
        .strategy
        .start_run(row.id, &u.username, mode, "manual", None)
        .await;
    if !r.ok {
        let e = r
            .error
            .unwrap_or_else(|| "Could not start the strategy".into());
        let code = if e == ALREADY_RUNNING || e == CHANGED_WHILE_STARTING {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        return err(e, code);
    }
    ok200(json!({"run_id": r.run_id, "mode": mode, "legs": r.legs}))
}

async fn stop_for(ctx: &AppState, user: &str, sid: &str, event: bool) -> Response {
    let row = match resolve(ctx, user, sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let Some(run_id) = row.current_run_id else {
        return err("This strategy is not running", StatusCode::CONFLICT);
    };
    if event {
        record(
            ctx,
            row.id,
            user,
            "close_all_manual",
            CLOSE_ALL_REQUESTED_MESSAGE,
            crate::strategy::store::EventFields {
                run_id: Some(run_id),
                ..Default::default()
            },
        )
        .await;
    }
    let r = ctx.strategy.stop_run(run_id, user, "manual").await;
    if !r.ok {
        return err_with(
            r.error.unwrap_or_else(|| "Could not stop the run".into()),
            StatusCode::CONFLICT,
            json!({"stop_pending": r.stop_pending, "exits": r.exits}),
        );
    }
    ok200(json!({"run_id": run_id, "stop_pending": r.stop_pending, "exits": r.exits}))
}

/// POST /strategy/api/strategies/{sid}/stop
pub async fn stop(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    stop_for(&ctx, &u.username, &sid, false).await
}

/// POST /strategy/api/strategies/{sid}/close_all
pub async fn close_all(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    stop_for(&ctx, &u.username, &sid, true).await
}

/// POST /strategy/api/strategies/{sid}/legs/{leg_id}/close
pub async fn close_leg(
    State(ctx): Ctx,
    User(u): User,
    Path((sid, leg_id)): Path<(String, String)>,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let Some(run_id) = row.current_run_id else {
        return err("This strategy is not running", StatusCode::CONFLICT);
    };
    let Ok(leg) = leg_id.parse::<i64>() else {
        return err("That leg is not open", StatusCode::CONFLICT);
    };
    let r = ctx.strategy.close_leg(run_id, leg, &u.username).await;
    if !r.ok {
        return err(
            r.error.unwrap_or_else(|| "Could not close that leg".into()),
            StatusCode::CONFLICT,
        );
    }
    ok200(json!({
        "run_id": run_id,
        "leg_id": leg_id,
        "run_stopped": r.run_stopped.unwrap_or(false),
        "exits": r.exits,
    }))
}

// ------------------------------------------------------------------ read-only history

/// GET .../runs
pub async fn runs(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    data_or_failed("runs", row.id, ctx.strategy.store.list_runs(row.id, 100))
}

/// GET .../orders?run_id=
pub async fn orders(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let run_id = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let rows = ctx
        .strategy
        .store
        .list_orders_for_strategy(row.id, run_id)
        .map(|rows| rows.iter().map(|o| o.to_dict()).collect::<Vec<Value>>());
    data_or_failed("orders", row.id, rows)
}

/// GET .../events?run_id=&kind=&severity=&limit=
pub async fn events(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let run_id = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let kind = q.get("kind").filter(|s| !s.is_empty());
    if let Some(k) = kind {
        if !EVENT_KINDS.contains(&k.as_str()) {
            return err(
                format!("kind must be one of: {}", EVENT_KINDS.join(", ")),
                StatusCode::BAD_REQUEST,
            );
        }
    }
    let severity = q.get("severity").filter(|s| !s.is_empty());
    if let Some(s) = severity {
        if !EVENT_SEVERITIES.contains(&s.as_str()) {
            return err(
                format!("severity must be one of: {}", EVENT_SEVERITIES.join(", ")),
                StatusCode::BAD_REQUEST,
            );
        }
    }
    let requested = match int_arg(&q, "limit") {
        Ok(v) => v,
        Err(r) => return r,
    };
    // Clamped: SQLite reads a negative LIMIT as no limit.
    let limit = requested
        .filter(|v| *v != 0)
        .unwrap_or(EVENTS_DEFAULT_LIMIT)
        .clamp(1, EVENTS_MAX_LIMIT);
    let rows = ctx.strategy.store.list_events(
        row.id,
        run_id,
        kind.map(String::as_str),
        severity.map(String::as_str),
        limit,
    );
    data_or_failed("events", row.id, rows)
}

/// GET .../webhook_events
pub async fn webhook_events(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    data_or_failed(
        "webhook events",
        row.id,
        ctx.strategy.store.list_webhook_events(row.id, 200),
    )
}

async fn book(
    ctx: &AppState,
    user: &str,
    sid: &str,
    q: &HashMap<String, String>,
    which: u8,
) -> Response {
    let row = match resolve(ctx, user, sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let run_id = match int_arg(q, "run_id") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let payload = match which {
        0 => ctx.strategy.strategy_orderbook(row.id, run_id).await,
        1 => ctx.strategy.strategy_tradebook(row.id, run_id).await,
        _ => ctx.strategy.strategy_positions(row.id, run_id).await,
    };
    let payload = match payload {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Could not read the records of strategy {}: {}", row.id, e);
            return read_failed();
        }
    };
    let code = if payload["status"] == "success" {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    json_response(code, payload)
}

/// GET .../orderbook
pub async fn orderbook(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    book(&ctx, &u.username, &sid, &q, 0).await
}

/// GET .../tradebook
pub async fn tradebook(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    book(&ctx, &u.username, &sid, &q, 1).await
}

/// GET .../positions
pub async fn positions(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    book(&ctx, &u.username, &sid, &q, 2).await
}

/// GET .../checkpoints?run_id=: defaults to the current, then the latest run.
pub async fn checkpoints(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let requested = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return r,
    };
    let run_id = match requested {
        Some(id) => match ctx.strategy.store.get_run(id) {
            Ok(Some(run)) if run.strategy_id == row.id => id,
            Ok(_) => return err("Run not found", StatusCode::NOT_FOUND),
            Err(e) => {
                tracing::error!("Could not read run {} of strategy {}: {}", id, row.id, e);
                return read_failed();
            }
        },
        None => match row.current_run_id {
            Some(id) => id,
            // A failed read of the latest run is not "never run".
            None => match ctx.strategy.store.list_runs(row.id, 1) {
                Ok(runs) => match runs.first().and_then(|x| x["id"].as_i64()) {
                    Some(id) => id,
                    None => return ok200(json!({"data": [], "run_id": null})),
                },
                Err(e) => {
                    tracing::error!("Could not read the runs of strategy {}: {}", row.id, e);
                    return read_failed();
                }
            },
        },
    };
    match ctx
        .strategy
        .store
        .list_checkpoints(run_id, 1000, Some(row.id))
    {
        Ok(rows) => ok200(json!({"data": rows, "run_id": run_id})),
        Err(e) => {
            tracing::error!("Could not read the checkpoints of run {}: {}", run_id, e);
            read_failed()
        }
    }
}

// ------------------------------------------------------------------ public webhook

/// POST /strategy/webhook/{token}. Public: the URL token is the credential.
/// Rate limits and the declared size cap come before any lookup, and an
/// unknown token answers 404 from here.
pub async fn webhook(
    State(ctx): Ctx,
    ClientIp(ip): ClientIp,
    crate::server::middleware::Src(source): crate::server::middleware::Src,
    crate::server::middleware::Deferred(deferred): crate::server::middleware::Deferred,
    Path(token): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    use crate::server::middleware::{count_failure, failures_exhausted, limiter_key, Source};
    use crate::server::ratelimit::Bucket;
    use crate::strategy::webhook::{admit_address, admit_token, MAX_PAYLOAD_BYTES};
    let limited = || {
        json_response(
            StatusCode::TOO_MANY_REQUESTS,
            json!({
                "status": "error",
                "result": "rate_limited",
                "message": "Rate limit exceeded. Please slow down your requests.",
                "retry_after": 60,
            }),
        )
    };
    // This computer and devices on the network: the web's per-address
    // limit, before any lookup. Tunnel callers share one identity, so they
    // are limited per token once it checked out (S-03).
    let tunnel = source == Source::Tunnel;
    let mut deferred = deferred;
    if !tunnel {
        match admit_address(&ctx.strategy.webhook, &ctx.limiter, ip) {
            None => return limited(),
            Some(d) => deferred |= d,
        }
    }
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|d| d > MAX_PAYLOAD_BYTES) || body.len() > MAX_PAYLOAD_BYTES {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"status": "error", "message": format!("Payload larger than {} bytes", MAX_PAYLOAD_BYTES)}),
        );
    }
    // The token first: a digest lookup. A valid token is limited only by
    // its own windows. An unknown one counts against the caller's failure
    // budget; once that is spent, the caller's unknown tokens are refused at
    // once, with no lookup audit row, while valid tokens still pass.
    let known = ctx.strategy.token_known(&token);
    if known {
        let tunnel_key = tunnel.then(|| limiter_key(ip, "strategy-webhook", &token));
        if !admit_token(&ctx.strategy.webhook, &ctx.limiter, tunnel_key, &token) {
            return limited();
        }
    } else {
        if deferred || failures_exhausted(&ctx, ip, Bucket::WebhookFail) {
            ctx.strategy
                .webhook
                .note_throttled("too many failed webhook authentications from one caller");
            return limited();
        }
        count_failure(&ctx, ip, Bucket::WebhookFail);
    }
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    // An IP allowlist applies to devices on the network only: a caller
    // behind a tunnel has no address the app can see, so it never matches
    // an entry (security review S-03).
    let ip_text = source.network_address().map(|ip| ip.to_string());
    let outcome = ctx
        .strategy
        .handle_webhook(&token, &body, ip_text.as_deref(), ua)
        .await;
    if known && matches!(outcome.result.as_str(), "rejected_token" | "rejected_ip") {
        // A valid token from outside its allowlist: a failed
        // authentication, counted against the caller.
        count_failure(&ctx, ip, Bucket::WebhookFail);
    }
    json_response(
        StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::BAD_REQUEST),
        outcome.body(),
    )
}

// ------------------------------------------------------------------ Socket.IO

/// `strategy_subscribe` / `strategy_unsubscribe` on the default namespace.
/// Ownership is checked on the join; a strategy that is not yours and one
/// that does not exist are refused identically.
pub fn register_socket_handlers(socket: &socketioxide::extract::SocketRef) {
    use socketioxide::extract::{AckSender, Data, SocketRef, State};
    socket.on(
        "strategy_subscribe",
        |s: SocketRef,
         Data(data): Data<Value>,
         ack: AckSender,
         State(ctx): State<Arc<AppState>>| async move {
            let reply = strategy_subscribe(&ctx, &s, &data).await;
            let _ = ack.send(&reply);
        },
    );
    socket.on(
        "strategy_unsubscribe",
        |s: SocketRef, Data(data): Data<Value>, ack: AckSender| async move {
            let reply = match strategy_id_of(&data) {
                Some(sid) => {
                    s.leave(crate::strategy::broadcast::room_for(sid));
                    json!({"status": "success", "strategy_id": sid})
                }
                None => json!({"status": "error", "message": "strategy_id is required"}),
            };
            let _ = ack.send(&reply);
        },
    );
}

fn strategy_id_of(data: &Value) -> Option<i64> {
    match data.get("strategy_id")? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The signed-in user of a socket, from its session cookie.
pub fn socket_user(ctx: &AppState, socket: &socketioxide::extract::SocketRef) -> Option<String> {
    crate::server::middleware::cookie_value(
        &socket.req_parts().headers,
        crate::session::web::COOKIE_NAME,
    )
    .and_then(|id| ctx.sessions.get(&id, ctx.now()))
    .and_then(|s| s.user)
}

pub async fn strategy_subscribe(
    ctx: &AppState,
    socket: &socketioxide::extract::SocketRef,
    data: &Value,
) -> Value {
    let Some(user) = socket_user(ctx, socket) else {
        return json!({"status": "error", "message": "Not authenticated"});
    };
    let Some(sid) = strategy_id_of(data) else {
        return json!({"status": "error", "message": "strategy_id is required"});
    };
    let row = match ctx.strategy.store.get_strategy(sid, &user) {
        Ok(Some(r)) => r,
        _ => return json!({"status": "error", "message": NOT_FOUND}),
    };
    socket.join(crate::strategy::broadcast::room_for(sid));
    // The current picture at once, rather than a blank page until a tick.
    if let Some(run_id) = row.current_run_id {
        ctx.strategy
            .broadcast
            .push_snapshot(ctx.strategy.state.snapshot(run_id))
            .await;
    }
    json!({"status": "success", "strategy_id": sid})
}
