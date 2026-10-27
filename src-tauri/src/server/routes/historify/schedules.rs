//! The schedule routes (web `historify.py` `/schedules`): list, create,
//! read, update, delete, enable, pause and trigger a schedule, and its
//! executions.

use super::{arg, ok_or_400, Ctx, Q};
use crate::historify::db::ScheduleUpdate;
use crate::historify::scheduler::{parse_hhmm, AddSchedule};
use crate::historify::service as svc;
use crate::server::api_v1::send;
use crate::server::envelope::{error, json_response, EarlyReply};
use crate::server::routes::webui::JsonBody;
use crate::services::core::Reply;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};

fn store_error(e: crate::error::AppError) -> Response {
    tracing::error!("Reading Historify schedules failed: {}", e);
    error(StatusCode::INTERNAL_SERVER_ERROR, svc::STORE_UNAVAILABLE)
}

/// GET /historify/api/schedules
pub async fn schedules_list(State(ctx): Ctx) -> Response {
    match ctx.historify.scheduler.all().await {
        Ok(s) => send(Reply::ok(
            json!({"status": "success", "count": s.len(), "data": s}),
        )),
        Err(e) => store_error(e),
    }
}

/// A JSON integer (not a bool, not a float).
fn int_field(body: &JsonBody, k: &str) -> Result<Option<i64>, ()> {
    match body.0.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_i64().map(Some).ok_or(()),
        Some(_) => Err(()),
    }
}

fn lookback(body: &JsonBody, default: Option<i64>) -> Result<Option<i64>, EarlyReply> {
    let bad = || {
        error(
            StatusCode::BAD_REQUEST,
            "lookback_days must be between 1 and 365",
        )
    };
    match int_field(body, "lookback_days") {
        Ok(Some(v)) if (1..=365).contains(&v) => Ok(Some(v)),
        Ok(None) => Ok(default),
        _ => Err(bad().into()),
    }
}

/// POST /historify/api/schedules
pub async fn schedule_create(State(ctx): Ctx, body: JsonBody) -> Response {
    let add = match schedule_to_add(&body) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    match ctx.historify.scheduler.add(&id, add).await {
        Ok(m) => json_response(
            StatusCode::CREATED,
            json!({"status": "success", "message": m, "schedule_id": id}),
        ),
        Err(m) => error(StatusCode::BAD_REQUEST, m),
    }
}

/// The schedule a create request describes, checked field by field in the
/// web's order, or the answer for the first field that is wrong.
fn schedule_to_add(body: &JsonBody) -> Result<AddSchedule, EarlyReply> {
    let bad = |m: &str| -> EarlyReply { Box::new(error(StatusCode::BAD_REQUEST, m)) };
    let name = body.str("name").unwrap_or_default();
    if name.is_empty() {
        return Err(bad("Schedule name is required"));
    }
    let schedule_type = body.str("schedule_type").unwrap_or_default();
    if schedule_type != "interval" && schedule_type != "daily" {
        return Err(bad(
            "Invalid schedule type. Must be \"interval\" or \"daily\"",
        ));
    }
    let data_interval = body.str("data_interval").unwrap_or_else(|| "D".into());
    if data_interval != "1m" && data_interval != "D" {
        return Err(bad("Invalid data interval. Must be \"1m\" or \"D\""));
    }
    let interval_value = int_field(body, "interval_value");
    let interval_unit = body
        .str("interval_unit")
        .unwrap_or_else(|| "minutes".into());
    let time_of_day = body.str("time_of_day").unwrap_or_else(|| "09:15".into());
    if schedule_type == "interval" {
        if !matches!(interval_value, Ok(Some(v)) if v >= 1) {
            return Err(bad("Invalid interval value"));
        }
        if interval_unit != "minutes" && interval_unit != "hours" {
            return Err(bad(
                "Invalid interval unit. Must be \"minutes\" or \"hours\"",
            ));
        }
    } else if parse_hhmm(&time_of_day).is_none() {
        return Err(bad("Invalid time format. Use HH:MM (e.g., 09:15)"));
    }
    let lookback_days = lookback(body, Some(1))?.unwrap_or(1);
    Ok(AddSchedule {
        name,
        schedule_type,
        data_interval,
        interval_value: interval_value.ok().flatten(),
        interval_unit: Some(interval_unit),
        time_of_day: Some(time_of_day),
        lookback_days,
        description: body.str("description"),
    })
}

/// GET /historify/api/schedules/{id}
pub async fn schedule_get(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    match ctx.historify.scheduler.schedule(&id).await {
        Ok(Some(s)) => send(Reply::ok(json!({"status": "success", "data": s}))),
        Ok(None) => error(StatusCode::NOT_FOUND, "Schedule not found"),
        Err(e) => store_error(e),
    }
}

/// PUT /historify/api/schedules/{id}
pub async fn schedule_update(State(ctx): Ctx, Path(id): Path<String>, body: JsonBody) -> Response {
    let existing = match ctx.historify.scheduler.schedule(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Schedule not found"),
        Err(e) => return store_error(e),
    };
    let schedule_type = body
        .str("schedule_type")
        .or_else(|| existing["schedule_type"].as_str().map(str::to_string))
        .unwrap_or_default();
    let interval_value = match int_field(&body, "interval_value") {
        Ok(v) => v,
        Err(()) => return error(StatusCode::BAD_REQUEST, "Invalid interval value"),
    };
    if schedule_type == "interval" {
        if interval_value.is_some_and(|v| v < 1) {
            return error(StatusCode::BAD_REQUEST, "Invalid interval value");
        }
        let unit = body
            .str("interval_unit")
            .or_else(|| existing["interval_unit"].as_str().map(str::to_string))
            .unwrap_or_else(|| "minutes".into());
        if unit != "minutes" && unit != "hours" {
            return error(StatusCode::BAD_REQUEST, "Invalid interval unit");
        }
    } else if schedule_type == "daily" {
        let t = body
            .str("time_of_day")
            .or_else(|| existing["time_of_day"].as_str().map(str::to_string))
            .unwrap_or_else(|| "09:15".into());
        if parse_hhmm(&t).is_none() {
            return error(StatusCode::BAD_REQUEST, "Invalid time format. Use HH:MM");
        }
    }
    if let Some(di) = body.str("data_interval") {
        if di != "1m" && di != "D" {
            return error(
                StatusCode::BAD_REQUEST,
                "Invalid data interval. Must be \"1m\" or \"D\"",
            );
        }
    }
    let lookback_days = match lookback(&body, None) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let u = ScheduleUpdate {
        name: body.str("name").filter(|s| !s.is_empty()),
        description: body.str("description"),
        schedule_type: body.str("schedule_type"),
        interval_value,
        interval_unit: body.str("interval_unit"),
        time_of_day: body.str("time_of_day"),
        data_interval: body.str("data_interval"),
        lookback_days,
        ..Default::default()
    };
    ok_or_400(ctx.historify.scheduler.update(&id, u).await)
}

/// DELETE /historify/api/schedules/{id}
pub async fn schedule_delete(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.delete(&id).await)
}

/// POST /historify/api/schedules/{id}/enable
pub async fn schedule_enable(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.enable(&id).await)
}

/// POST /historify/api/schedules/{id}/disable
pub async fn schedule_disable(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.disable(&id).await)
}

/// POST /historify/api/schedules/{id}/pause
pub async fn schedule_pause(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.pause(&id).await)
}

/// POST /historify/api/schedules/{id}/resume
pub async fn schedule_resume(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.resume(&id).await)
}

/// POST /historify/api/schedules/{id}/trigger
pub async fn schedule_trigger(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    ok_or_400(ctx.historify.scheduler.trigger(&id).await)
}

/// GET /historify/api/schedules/{id}/executions (limit?, at most 100)
pub async fn schedule_executions(State(ctx): Ctx, Path(id): Path<String>, Query(q): Q) -> Response {
    let limit = arg(&q, "limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(20)
        .min(100);
    match ctx
        .historify
        .db
        .run(move |c| crate::historify::db::executions(c, &id, limit))
        .await
    {
        Ok(rows) => send(Reply::ok(
            json!({"status": "success", "count": rows.len(), "data": rows}),
        )),
        Err(e) => store_error(e),
    }
}
