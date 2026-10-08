//! `/api/v1/strategy/*` (web `restx_api/strategy.py` and
//! `strategy_schema.py`): lifecycle and history for API-key callers.
//!
//! Nine POST routes, identifier in the body: `list`, `status`, `start`,
//! `stop`, `close_all`, `close_leg`, `runs`, `orders`, `events`. Nothing here
//! can create, edit, enable live trading, rotate a token or delete.
//!
//! * `mode` is required on start and never defaulted.
//! * `live` is refused (409) unless the strategy opted in.
//! * A strategy that is not yours answers 404, never 403.
//! * No route returns a webhook token.

use super::{authorize, Auth};
use crate::server::envelope::json_response;
use crate::server::middleware::ClientIp;
use crate::services::core::INVALID_API_KEY;
use crate::services::schema::{Field, Schema, Validator};
use crate::state::AppState;
use crate::strategy::store::{EVENT_KINDS, EVENT_SEVERITIES, RUN_MODES, STRATEGY_STATUSES};
use axum::{body::Bytes, extract::State, http::StatusCode, response::Response};
use serde_json::{json, Map, Value};
use std::net::IpAddr;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

const NOT_FOUND: &str = "Strategy not found";
const NOT_RUNNING: &str = "This strategy is not running";
const UNEXPECTED: &str = "An unexpected error occurred";
const CLOSE_ALL_REQUESTED_MESSAGE: &str = "Operator requested closure of all held legs";

pub const EVENTS_DEFAULT_LIMIT: i64 = 500;
pub const EVENTS_MAX_LIMIT: f64 = 1000.0;
pub const RUNS_DEFAULT_LIMIT: i64 = 100;
pub const RUNS_MAX_LIMIT: f64 = 500.0;

fn apikey() -> Field {
    Field::str("apikey")
        .required()
        .validate(Validator::length(1, 256))
}

fn strategy_id() -> Field {
    Field::int("strategy_id")
        .required()
        .validate(Validator::min(1.0, None))
}

fn opt_run_id() -> Field {
    Field::int("run_id")
        .default(|| Value::Null)
        .validate(Validator::min(1.0, None))
}

/// `StrategyListSchema`.
pub fn list_schema() -> Schema {
    Schema::new(vec![
        apikey(),
        Field::str("status")
            .default(|| Value::Null)
            .validate(Validator::one_of(STRATEGY_STATUSES)),
        Field::str("q")
            .default(|| Value::Null)
            .validate(Validator::Length {
                min: None,
                max: Some(100),
                error: None,
            }),
    ])
}

/// `StrategyRefSchema` and its extensions.
pub fn ref_schema(extra: Vec<Field>) -> Schema {
    let mut fields = vec![apikey(), strategy_id()];
    fields.extend(extra);
    Schema::new(fields)
}

pub fn start_schema() -> Schema {
    ref_schema(vec![Field::str("mode")
        .required()
        .validate(Validator::one_of(RUN_MODES))])
}

pub fn close_leg_schema() -> Schema {
    ref_schema(vec![Field::int("leg_id")
        .required()
        .validate(Validator::min(1.0, None))])
}

pub fn runs_schema() -> Schema {
    ref_schema(vec![Field::int("limit")
        .default(|| json!(RUNS_DEFAULT_LIMIT))
        .validate(Validator::between(1.0, RUNS_MAX_LIMIT))])
}

pub fn orders_schema() -> Schema {
    ref_schema(vec![opt_run_id()])
}

pub fn events_schema() -> Schema {
    ref_schema(vec![
        opt_run_id(),
        Field::str("kind")
            .default(|| Value::Null)
            .validate(Validator::one_of(EVENT_KINDS)),
        Field::str("severity")
            .default(|| Value::Null)
            .validate(Validator::one_of(EVENT_SEVERITIES)),
        Field::int("limit")
            .default(|| json!(EVENTS_DEFAULT_LIMIT))
            .validate(Validator::between(1.0, EVENTS_MAX_LIMIT)),
    ])
}

fn success(payload: Value) -> Response {
    let mut body = Map::new();
    body.insert("status".into(), json!("success"));
    if let Value::Object(m) = payload {
        body.extend(m);
    }
    json_response(StatusCode::OK, Value::Object(body))
}

fn failure(message: impl Into<String>, code: StatusCode, extra: Value) -> Response {
    let mut body = Map::new();
    body.insert("status".into(), json!("error"));
    body.insert("message".into(), json!(message.into()));
    if let Value::Object(m) = extra {
        body.extend(m);
    }
    json_response(code, Value::Object(body))
}

/// `request.get_json(silent=True) or {}`: anything unreadable is an empty
/// body, which the schema then refuses field by field.
fn lenient_body(bytes: &Bytes) -> Map<String, Value> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Validate, authenticate the key, and (when asked) load the strategy.
#[allow(clippy::result_large_err)]
fn resolve(
    ctx: &AppState,
    ip: IpAddr,
    bytes: &Bytes,
    schema: &Schema,
    needs_strategy: bool,
) -> Result<(Value, String, Option<crate::strategy::store::StrategyRow>), Response> {
    let body = lenient_body(bytes);
    let data = match schema.load(&body) {
        Ok(d) => Value::Object(d),
        Err(e) => {
            return Err(json_response(
                StatusCode::BAD_REQUEST,
                json!({"status": "error", "message": e.to_json()}),
            ))
        }
    };
    let key = data["apikey"].as_str().unwrap_or_default();
    if !authorize(ctx, key, ip, Auth::KeyOnly) {
        return Err(failure(INVALID_API_KEY, StatusCode::FORBIDDEN, Value::Null));
    }
    let user = match ctx.strategy.store.api_key_owner() {
        Ok(Some(u)) => u,
        _ => return Err(failure(INVALID_API_KEY, StatusCode::FORBIDDEN, Value::Null)),
    };
    if !needs_strategy {
        return Ok((data, user, None));
    }
    let sid = data["strategy_id"].as_i64().unwrap_or(0);
    match ctx.strategy.store.get_strategy(sid, &user) {
        Ok(Some(row)) => Ok((data, user, Some(row))),
        Ok(None) => Err(failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null)),
        Err(e) => {
            tracing::error!("Could not read strategy {}: {}", sid, e);
            Err(failure(
                UNEXPECTED,
                StatusCode::INTERNAL_SERVER_ERROR,
                Value::Null,
            ))
        }
    }
}

async fn stop_current(
    ctx: &AppState,
    row: &crate::strategy::store::StrategyRow,
    user: &str,
    event: bool,
) -> Response {
    let Some(run_id) = row.current_run_id else {
        return failure(NOT_RUNNING, StatusCode::CONFLICT, Value::Null);
    };
    if event {
        ctx.strategy
            .emit(
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
        return failure(
            r.error.unwrap_or_else(|| "Could not stop the run".into()),
            StatusCode::CONFLICT,
            json!({"stop_pending": r.stop_pending, "exits": r.exits}),
        );
    }
    success(json!({"run_id": run_id, "stop_pending": r.stop_pending, "exits": r.exits}))
}

/// POST /api/v1/strategy/list
pub async fn list(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, user, _) = match resolve(&ctx, ip, &body, &list_schema(), false) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let q = data["q"].as_str().map(str::trim).filter(|s| !s.is_empty());
    match ctx
        .strategy
        .store
        .list_strategies(&user, data["status"].as_str(), q)
    {
        Ok(rows) => success(json!({"data": rows})),
        Err(e) => {
            tracing::error!("Strategy list failed: {}", e);
            failure(UNEXPECTED, StatusCode::INTERNAL_SERVER_ERROR, Value::Null)
        }
    }
}

/// POST /api/v1/strategy/status
pub async fn status(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (_, _, row) = match resolve(&ctx, ip, &body, &ref_schema(vec![]), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let run = row
        .current_run_id
        .and_then(|id| ctx.strategy.store.get_run(id).ok().flatten())
        .map(|r| r.to_dict())
        .unwrap_or(Value::Null);
    success(json!({"data": row.to_dict(true), "run": run}))
}

/// POST /api/v1/strategy/start
pub async fn start(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, user, row) = match resolve(&ctx, ip, &body, &start_schema(), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let mode = data["mode"].as_str().unwrap_or_default().to_string();
    if mode == "live" && !row.live_enabled {
        return failure(
            "This strategy is not enabled for live trading. Enable it on the strategy page, or start it with mode 'sandbox'.",
            StatusCode::CONFLICT,
            Value::Null,
        );
    }
    let r = ctx
        .strategy
        .start_run(row.id, &user, &mode, "manual", None)
        .await;
    if !r.ok {
        let e = r
            .error
            .unwrap_or_else(|| "Could not start the strategy".into());
        let code = if e.contains("already running") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        return failure(e, code, Value::Null);
    }
    success(json!({"run_id": r.run_id, "mode": mode, "legs": r.legs}))
}

/// POST /api/v1/strategy/stop
pub async fn stop(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    match resolve(&ctx, ip, &body, &ref_schema(vec![]), true) {
        Ok((_, user, Some(row))) => stop_current(&ctx, &row, &user, false).await,
        Ok(_) => failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null),
        Err(r) => r,
    }
}

/// POST /api/v1/strategy/close_all
pub async fn close_all(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    match resolve(&ctx, ip, &body, &ref_schema(vec![]), true) {
        Ok((_, user, Some(row))) => stop_current(&ctx, &row, &user, true).await,
        Ok(_) => failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null),
        Err(r) => r,
    }
}

/// POST /api/v1/strategy/close_leg
pub async fn close_leg(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, user, row) = match resolve(&ctx, ip, &body, &close_leg_schema(), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let Some(run_id) = row.current_run_id else {
        return failure(NOT_RUNNING, StatusCode::CONFLICT, Value::Null);
    };
    let leg_id = data["leg_id"].as_i64().unwrap_or(0);
    let r = ctx.strategy.close_leg(run_id, leg_id, &user).await;
    if !r.ok {
        return failure(
            r.error.unwrap_or_else(|| "Could not close that leg".into()),
            StatusCode::CONFLICT,
            Value::Null,
        );
    }
    success(json!({
        "run_id": run_id,
        "leg_id": leg_id,
        "run_stopped": r.run_stopped.unwrap_or(false),
        "exits": r.exits,
    }))
}

/// POST /api/v1/strategy/runs
pub async fn runs(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, _, row) = match resolve(&ctx, ip, &body, &runs_schema(), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let limit = data["limit"].as_i64().unwrap_or(RUNS_DEFAULT_LIMIT);
    success(json!({"data": ctx.strategy.store.list_runs(row.id, limit).unwrap_or_default()}))
}

/// POST /api/v1/strategy/orders
pub async fn orders(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, _, row) = match resolve(&ctx, ip, &body, &orders_schema(), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let rows: Vec<Value> = ctx
        .strategy
        .store
        .list_orders_for_strategy(row.id, data["run_id"].as_i64())
        .unwrap_or_default()
        .iter()
        .map(|o| o.to_dict())
        .collect();
    success(json!({"data": rows}))
}

/// POST /api/v1/strategy/events
pub async fn events(State(ctx): Ctx, ClientIp(ip): ClientIp, body: Bytes) -> Response {
    let (data, _, row) = match resolve(&ctx, ip, &body, &events_schema(), true) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(row) = row else {
        return failure(NOT_FOUND, StatusCode::NOT_FOUND, Value::Null);
    };
    let events = ctx
        .strategy
        .store
        .list_events(
            row.id,
            data["run_id"].as_i64(),
            data["kind"].as_str(),
            data["severity"].as_str(),
            data["limit"].as_i64().unwrap_or(EVENTS_DEFAULT_LIMIT),
        )
        .unwrap_or_default();
    success(json!({"data": events}))
}
