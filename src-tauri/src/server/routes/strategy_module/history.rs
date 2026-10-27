//! Read-only history of a strategy (web `blueprints/strategy_module.py`
//! read routes): its runs, orders, events, webhook calls, books, positions
//! and checkpoints.

use super::{
    data_off_worker, err, int_arg, read_failed, resolve, success_text, text_or_failed, Ctx,
    EVENTS_DEFAULT_LIMIT, EVENTS_MAX_LIMIT, Q,
};
use crate::server::envelope::json_response;
use crate::server::middleware::User;
use crate::state::AppState;
use crate::strategy::store::{EVENT_KINDS, EVENT_SEVERITIES};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// GET .../runs
pub async fn runs(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let (id, what) = (row.id, format!("the runs of strategy {}", row.id));
    data_off_worker(&ctx, what, move |c| c.strategy.store.list_runs(id, 100)).await
}

/// GET .../orders?run_id=
pub async fn orders(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let run_id = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return *r,
    };
    // The books' shared read of the history; shaped and serialized off the
    // async workers.
    let text = match ctx.strategy.order_history(row.id, run_id).await {
        Ok(history) => {
            ctx.blocking(move |_| {
                let rows: Vec<Value> = history.iter().map(|o| o.to_dict()).collect();
                success_text(json!({ "data": rows }))
            })
            .await
        }
        Err(e) => Err(e),
    };
    text_or_failed(&format!("the orders of strategy {}", row.id), text)
}

/// GET .../events?run_id=&kind=&severity=&limit=
pub async fn events(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    Query(q): Q,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let run_id = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return *r,
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
        Err(r) => return *r,
    };
    // Clamped: SQLite reads a negative LIMIT as no limit.
    let limit = requested
        .filter(|v| *v != 0)
        .unwrap_or(EVENTS_DEFAULT_LIMIT)
        .clamp(1, EVENTS_MAX_LIMIT);
    let (id, kind, severity) = (row.id, kind.cloned(), severity.cloned());
    let read = move |c: &AppState| {
        c.strategy
            .store
            .list_events(id, run_id, kind.as_deref(), severity.as_deref(), limit)
    };
    data_off_worker(&ctx, format!("the events of strategy {}", id), read).await
}

/// GET .../webhook_events
pub async fn webhook_events(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let (id, what) = (row.id, format!("the webhook events of strategy {}", row.id));
    data_off_worker(&ctx, what, move |c| {
        c.strategy.store.list_webhook_events(id, 200)
    })
    .await
}

async fn book(
    ctx: &AppState,
    user: &str,
    sid: &str,
    q: &HashMap<String, String>,
    which: u8,
) -> Response {
    let row = match resolve(ctx, user, sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let run_id = match int_arg(q, "run_id") {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let payload = match which {
        0 => ctx.strategy.strategy_orderbook(row.id, run_id).await,
        1 => ctx.strategy.strategy_tradebook(row.id, run_id).await,
        _ => ctx.strategy.strategy_positions(row.id, run_id).await,
    };
    let payload = match payload {
        Ok(p) => p,
        Err(e) => return read_failed(format_args!("the books of strategy {}", row.id), e),
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let requested = match int_arg(&q, "run_id") {
        Ok(v) => v,
        Err(r) => return *r,
    };
    // Polled every 5 s per running strategy: read and serialized off the
    // async workers.
    let id = row.id;
    let read = ctx.blocking(
        move |c| match c.strategy.checkpoint_page(&row, requested)? {
            Some(page) => success_text(page).map(Some),
            None => Ok(None),
        },
    );
    match read.await {
        Ok(Some(body)) => crate::server::routes::webui::ok_text(body),
        Ok(None) => err("Run not found", StatusCode::NOT_FOUND),
        Err(e) => read_failed(format_args!("the checkpoints of strategy {}", id), e),
    }
}
