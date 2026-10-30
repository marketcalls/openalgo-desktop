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
    StrategyRow, ALREADY_RUNNING, CHANGED_WHILE_EDITING, CHANGED_WHILE_STARTING, RUN_MODES,
    STRATEGY_STATUSES,
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

mod history;

pub use history::{
    checkpoints, events, orderbook, orders, positions, runs, tradebook, webhook_events,
};

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

pub const NOT_FOUND: &str = "Strategy not found";
pub const CLOSE_ALL_REQUESTED_MESSAGE: &str = "Operator requested closure of all held legs";
const EVENTS_DEFAULT_LIMIT: i64 = 500;
const EVENTS_MAX_LIMIT: i64 = 1000;
const READ_FAILED: &str = "The strategy records could not be read just now, so nothing is shown rather than an incomplete picture. Try again; if it keeps happening, restart the app.";

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

/// The bytes [`ok200`] answers for `payload`, serialized where a long
/// history was read (the blocking pool), so the async worker only sends
/// them.
fn success_text(payload: Value) -> crate::error::Result<Vec<u8>> {
    let mut body = Map::new();
    body.insert("status".into(), json!("success"));
    if let Value::Object(m) = payload {
        body.extend(m);
    }
    Ok(serde_json::to_vec(&Value::Object(body))?)
}

/// The answer [`success_text`] made, or [`read_failed`].
fn text_or_failed(what: &str, text: crate::error::Result<Vec<u8>>) -> Response {
    match text {
        Ok(body) => crate::server::routes::webui::ok_text(body),
        Err(e) => read_failed(format_args!("{}", what), e),
    }
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
/// The helpers' error replies are boxed: a reply is large.
async fn resolve(
    ctx: &AppState,
    user: &str,
    sid: &str,
) -> Result<crate::strategy::store::StrategyRow, Box<Response>> {
    let Ok(id) = sid.parse::<i64>() else {
        return Err(Box::new(err(NOT_FOUND, StatusCode::NOT_FOUND)));
    };
    match ctx.strategy.owned_strategy(id, user).await {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(Box::new(err(NOT_FOUND, StatusCode::NOT_FOUND))),
        // Not "not found" (SM-04): a stop or a kill switch must not be told
        // that a strategy it cannot read does not exist.
        Err(e) => Err(Box::new(read_failed(format_args!("strategy {}", id), e))),
    }
}

/// A local read that failed (SM-04): logged, and answered as an error, never
/// as a successful empty list that reads as "never ran" or "no positions".
fn read_failed(what: std::fmt::Arguments<'_>, e: AppError) -> Response {
    tracing::error!("Could not read {}: {}", what, e);
    err(READ_FAILED, StatusCode::INTERNAL_SERVER_ERROR)
}

/// `{"data": rows}` from a store read, or [`read_failed`]: read and
/// serialized on the blocking pool, since a strategy's history grows with
/// every run and the async workers carry the feed and the risk owners.
async fn data_off_worker<T, F>(ctx: &Arc<AppState>, what: String, read: F) -> Response
where
    T: serde::Serialize,
    F: FnOnce(&AppState) -> crate::error::Result<T> + Send + 'static,
{
    let text = ctx
        .blocking(move |c| success_text(json!({ "data": read(c)? })))
        .await;
    text_or_failed(&what, text)
}

fn json_body(bytes: &Bytes) -> Result<Map<String, Value>, Box<Response>> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(Box::new(err(
            "The request body must be a JSON object",
            StatusCode::BAD_REQUEST,
        ))),
        Err(_) => Err(Box::new(err(
            "A JSON body is required",
            StatusCode::BAD_REQUEST,
        ))),
    }
}

fn int_arg(q: &HashMap<String, String>, name: &str) -> Result<Option<i64>, Box<Response>> {
    match q.get(name).map(String::as_str) {
        None | Some("") => Ok(None),
        Some(v) => v.parse::<i64>().map(Some).map_err(|_| {
            Box::new(err(
                format!("{} must be a whole number", name),
                StatusCode::BAD_REQUEST,
            ))
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
    let status = status.cloned();
    let read = move |c: &AppState| {
        c.strategy.store.list_strategies(
            &u.username,
            status.as_deref(),
            (!query.is_empty()).then_some(query.as_str()),
        )
    };
    data_off_worker(&ctx, "the strategy list".into(), read).await
}

/// POST /strategy/api/strategies
pub async fn create(State(ctx): Ctx, User(u): User, body: Bytes) -> Response {
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let what = format!("the realized P&L of strategy {}", row.id);
    data_off_worker(&ctx, what, move |c| c.strategy.strategy_detail(&row)).await
}

/// The edit's fields the strategy does not accept, as the refusal.
fn unknown_fields(payload: &Map<String, Value>, fields: &[&str]) -> Option<Response> {
    let mut unknown: Vec<&String> = payload
        .keys()
        .filter(|k| !fields.contains(&k.as_str()))
        .collect();
    if unknown.is_empty() {
        return None;
    }
    unknown.sort();
    Some(err(
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
    ))
}

/// An edit refused for the strategy as it stands: while it runs, or one
/// that changes its kind.
fn edit_refused(row: &StrategyRow, payload: &Map<String, Value>) -> Option<Response> {
    if row.status == "running" {
        return Some(err(
            "Stop the strategy before editing it",
            StatusCode::CONFLICT,
        ));
    }
    if let Some(kind) = payload.get("strategy_kind").filter(|v| !v.is_null()) {
        if kind.as_str() != Some(row.strategy_kind.as_str()) {
            return Some(err(
                "A strategy cannot change between batch and signal. The two kinds do not share a leg shape, so every leg would describe the wrong kind of contract. Create a new strategy instead.",
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    None
}

/// The stored configuration with the edit's fields over it.
fn merged_with(
    row: &StrategyRow,
    payload: &Map<String, Value>,
    fields: &[&str],
) -> Map<String, Value> {
    let stored = row.to_dict(true);
    let mut merged = Map::new();
    for f in fields {
        if let Some(v) = stored.get(*f) {
            merged.insert((*f).to_string(), v.clone());
        }
    }
    for (k, v) in payload {
        merged.insert(k.clone(), v.clone());
    }
    merged
}

/// The `strategy_updated` event naming the changed fields.
async fn record_update(ctx: &AppState, sid: i64, user: &str, changes: &Map<String, Value>) {
    let mut names: Vec<&String> = changes.keys().collect();
    names.sort();
    record(
        ctx,
        sid,
        user,
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
}

/// PATCH /strategy/api/strategies/{sid}
pub async fn update(
    State(ctx): Ctx,
    User(u): User,
    Path(sid): Path<String>,
    body: Bytes,
) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let mut payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    if payload.is_empty() {
        return err("Nothing to update", StatusCode::BAD_REQUEST);
    }
    let fields = config_fields();
    if let Some(refusal) =
        unknown_fields(&payload, &fields).or_else(|| edit_refused(&row, &payload))
    {
        return refusal;
    }
    payload.remove("strategy_kind");
    if payload.is_empty() {
        return err("Nothing to update", StatusCode::BAD_REQUEST);
    }
    let merged = merged_with(&row, &payload, &fields);
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
    let updated = match ctx.strategy.store.update_strategy(&row, &changes) {
        Ok(r) => r,
        Err(e) => return store_error(e),
    };
    record_update(&ctx, row.id, &u.username, &changes).await;
    sync_schedule(&ctx, row.id, false);
    ok200(json!({"data": updated.to_dict(true)}))
}

/// DELETE /strategy/api/strategies/{sid}
pub async fn delete(State(ctx): Ctx, User(u): User, Path(sid): Path<String>) -> Response {
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let payload = match json_body(&body) {
        Ok(p) => p,
        Err(r) => return *r,
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
    let row = match resolve(ctx, user, sid).await {
        Ok(r) => r,
        Err(r) => return *r,
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
        let code = StatusCode::from_u16(r.error_status()).unwrap_or(StatusCode::CONFLICT);
        return err_with(
            r.error.unwrap_or_else(|| "Could not stop the run".into()),
            code,
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
    let row = match resolve(&ctx, &u.username, &sid).await {
        Ok(r) => r,
        Err(r) => return *r,
    };
    let Some(run_id) = row.current_run_id else {
        return err("This strategy is not running", StatusCode::CONFLICT);
    };
    let Ok(leg) = leg_id.parse::<i64>() else {
        return err("That leg is not open", StatusCode::CONFLICT);
    };
    let r = ctx.strategy.close_leg(run_id, leg, &u.username).await;
    if !r.ok {
        let code = StatusCode::from_u16(r.error_status()).unwrap_or(StatusCode::CONFLICT);
        return err(
            r.error.unwrap_or_else(|| "Could not close that leg".into()),
            code,
        );
    }
    ok200(json!({
        "run_id": run_id,
        "leg_id": leg_id,
        "run_stopped": r.run_stopped.unwrap_or(false),
        "exits": r.exits,
    }))
}

// ------------------------------------------------------------------ public webhook

/// The webhook's 429, as the web words it.
fn webhook_rate_limited() -> Response {
    json_response(
        StatusCode::TOO_MANY_REQUESTS,
        json!({
            "status": "error",
            "result": "rate_limited",
            "message": "Rate limit exceeded. Please slow down your requests.",
            "retry_after": 60,
        }),
    )
}

/// The 413 for a payload over the cap, declared or received.
fn webhook_too_large(headers: &HeaderMap, body: &Bytes) -> Option<Response> {
    use crate::strategy::webhook::MAX_PAYLOAD_BYTES;
    let declared = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|d| d > MAX_PAYLOAD_BYTES) || body.len() > MAX_PAYLOAD_BYTES {
        return Some(json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"status": "error", "message": format!("Payload larger than {} bytes", MAX_PAYLOAD_BYTES)}),
        ));
    }
    None
}

/// The token first: a digest lookup. A valid token is limited only by its
/// own windows. An unknown one counts against the caller's failure budget;
/// once that is spent, the caller's unknown tokens are refused at once,
/// with no lookup audit row, while valid tokens still pass. Answers whether
/// the token is known, or `None` when the call is rate limited.
fn admit_webhook_token(
    ctx: &AppState,
    ip: std::net::IpAddr,
    (tunnel, deferred): (bool, bool),
    token: &str,
) -> Option<bool> {
    use crate::server::middleware::{count_failure, failures_exhausted, limiter_key};
    use crate::server::ratelimit::Bucket;
    use crate::strategy::webhook::admit_token;
    let known = ctx.strategy.token_known(token);
    if known {
        let tunnel_key = tunnel.then(|| limiter_key(ip, "strategy-webhook", token));
        if !admit_token(&ctx.strategy.webhook, &ctx.limiter, tunnel_key, token) {
            return None;
        }
    } else {
        if deferred || failures_exhausted(ctx, ip, Bucket::WebhookFail) {
            ctx.strategy
                .webhook
                .note_throttled("too many failed webhook authentications from one caller");
            return None;
        }
        count_failure(ctx, ip, Bucket::WebhookFail);
    }
    Some(known)
}

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
    use crate::server::middleware::{count_failure, Source};
    use crate::server::ratelimit::Bucket;
    use crate::strategy::webhook::admit_address;
    // This computer and devices on the network: the web's per-address
    // limit, before any lookup. Tunnel callers share one identity, so they
    // are limited per token once it checked out (S-03).
    let tunnel = source == Source::Tunnel;
    let mut deferred = deferred;
    if !tunnel {
        match admit_address(&ctx.strategy.webhook, &ctx.limiter, ip) {
            None => return webhook_rate_limited(),
            Some(d) => deferred |= d,
        }
    }
    if let Some(r) = webhook_too_large(&headers, &body) {
        return r;
    }
    let Some(known) = admit_webhook_token(&ctx, ip, (tunnel, deferred), &token) else {
        return webhook_rate_limited();
    };
    if known && !ctx.security.is_unlocked() {
        return crate::server::envelope::webhook_while_locked("message");
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
         State(app): State<std::sync::Weak<AppState>>| async move {
            let Some(ctx) = app.upgrade() else {
                return;
            };
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
    .and_then(|id| crate::session::web_session(ctx, &id))
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
