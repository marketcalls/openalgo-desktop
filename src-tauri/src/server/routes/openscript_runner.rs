//! Web `blueprints/openscript_runner.py`: `/openscript/runner`. Starts one
//! saved OpenScript deployment, pauses it, closes it, reports what is running,
//! holds what each deployment runs on and when it is scheduled, and serves the
//! per-deployment books. Every one of these needs the signed-in user.
//!
//! **A start carries nothing.** What a deployment runs on is saved against it
//! and read at start, so a run started from the page and one started by a
//! schedule are the same run; a body carrying options is refused, not
//! ignored. **Starting answers with the run, never a result**: the run
//! outlives the request.
//!
//! The `host/` routes below are not for the trader's pages. They are the
//! runner page's own channel (program, bars, inbox, intents, log), public in
//! the route table and authenticated by the per-run secret the runner handed
//! that page in its URL fragment, as a webhook is by its token.

use super::{Access, RouteSpec};
use crate::server::envelope::json_response;
use crate::server::middleware::User;
use crate::state::AppState;
use crate::strategy::dispatch::RunMode;
use crate::trading::names::{
    is_deployment_id, is_script_name, names_something, script_name_refusal,
};
use crate::trading::runner::{books, schedule, store, Refusal};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::{delete, get, post, MethodRouter},
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

/// What a settings body may carry, and nothing else.
pub const SETTINGS_FIELDS: &[&str] = &[
    "symbol",
    "exchange",
    "interval",
    "product",
    "inputs",
    "deployment",
];

/// The header the runner page sends its run's secret in.
pub const TOKEN_HEADER: &str = "x-runner-token";

pub fn table() -> Vec<RouteSpec> {
    let user =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    let page =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::Public,
            make,
        };
    vec![
        user("/openscript/runner/start/{name}", Method::POST, || {
            post(start)
        }),
        user("/openscript/runner/pause/{name}", Method::POST, || {
            post(pause)
        }),
        user("/openscript/runner/stop/{name}", Method::POST, || {
            post(pause)
        }),
        user("/openscript/runner/close/{name}", Method::POST, || {
            post(close)
        }),
        user("/openscript/runner/status", Method::GET, || get(status_all)),
        user("/openscript/runner/status/{name}", Method::GET, || {
            get(status_one)
        }),
        user("/openscript/runner/config", Method::GET, || {
            get(list_settings)
        }),
        user("/openscript/runner/config/{name}", Method::GET, || {
            get(get_settings)
        }),
        user("/openscript/runner/config/{name}", Method::POST, || {
            post(set_settings)
        }),
        user("/openscript/runner/config/{name}", Method::DELETE, || {
            delete(clear_settings)
        }),
        user("/openscript/runner/schedule/{name}", Method::POST, || {
            post(set_schedule)
        }),
        user("/openscript/runner/schedule/{name}", Method::DELETE, || {
            delete(clear_schedule)
        }),
        user("/openscript/runner/instruments", Method::GET, || {
            get(instruments)
        }),
        user("/openscript/runner/intervals", Method::GET, || {
            get(intervals)
        }),
        user("/openscript/runner/orderbook/{name}", Method::GET, || {
            get(orderbook)
        }),
        user("/openscript/runner/tradebook/{name}", Method::GET, || {
            get(tradebook)
        }),
        user("/openscript/runner/positions/{name}", Method::GET, || {
            get(positions)
        }),
        // The runner page's channel (per-run secret, see the module note).
        page("/openscript/runner/host/{run}/spec", Method::GET, || {
            get(host_spec)
        }),
        page("/openscript/runner/host/{run}/bars", Method::GET, || {
            get(host_bars)
        }),
        page("/openscript/runner/host/{run}/inbox", Method::GET, || {
            get(host_inbox)
        }),
        page(
            "/openscript/runner/host/{run}/intents",
            Method::POST,
            || post(host_intents),
        ),
        page("/openscript/runner/host/{run}/log", Method::POST, || {
            post(host_log)
        }),
        page("/openscript/runner/host/{run}/ended", Method::POST, || {
            post(host_ended)
        }),
    ]
}

fn err(status: u16, message: impl Into<String>) -> Response {
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        json!({"status": "error", "message": message.into()}),
    )
}

fn ok(body: Value) -> Response {
    json_response(StatusCode::OK, body)
}

fn refusal(name: &str) -> Response {
    err(400, script_name_refusal(name))
}

fn refused(r: Refusal) -> Response {
    err(r.status, r.message)
}

fn object(body: &Bytes) -> Option<Map<String, Value>> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.as_object().cloned())
}

// ------------------------------------------------------------ start / stop

/// Start one deployment, and answer with its run (202).
pub async fn start(State(ctx): Ctx, Path(name): Path<String>, body: Bytes) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    if let Ok(options) = serde_json::from_slice::<Value>(&body) {
        if options.as_object().is_none_or(|m| !m.is_empty()) {
            return err(
                400,
                "Starting a script takes no options. What it runs on is saved in its run settings, and where its orders go is the platform's own setting.",
            );
        }
    }
    let runner = &ctx.trading.runner;
    if let Some(r) = runner.why_not_runnable(&name) {
        return refused(r);
    }
    match runner.start(&name) {
        Ok(info) => json_response(
            StatusCode::ACCEPTED,
            json!({
                "status": "success",
                "run": info.answer(),
                "message": format!("{} is starting. Its log shows what it does next.", name),
            }),
        ),
        Err(r) => err(409, r.message),
    }
}

fn not_running(ctx: &AppState, name: &str) -> Response {
    if ctx.trading.runner.is_starting(name) {
        return err(
            409,
            format!("{} is still starting. Try again in a moment.", name),
        );
    }
    err(404, format!("{} is not running.", name))
}

/// End one run and leave its position where it is (`/stop` is the same).
pub async fn pause(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    if !ctx.trading.runner.is_running(&name) {
        return not_running(&ctx, &name);
    }
    match ctx.trading.runner.pause(&name) {
        Ok(message) => ok(json!({"status": "success", "file": name, "message": message})),
        Err(r) => err(409, r.message),
    }
}

/// Close what one run holds, then end it. This one spends money.
pub async fn close(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    if !ctx.trading.runner.is_running(&name) {
        return not_running(&ctx, &name);
    }
    match ctx.trading.runner.close(&name).await {
        Ok(message) => ok(json!({"status": "success", "file": name, "message": message})),
        Err(r) => err(409, r.message),
    }
}

// ------------------------------------------------------------ status

fn log_dir(ctx: &AppState) -> Value {
    json!(ctx.trading.runner.logs_dir().to_string_lossy())
}

pub async fn status_all(State(ctx): Ctx) -> Response {
    let runner = &ctx.trading.runner;
    let running: Vec<Value> = runner.running().iter().map(|r| r.answer()).collect();
    let (scheduled, settings) = match ctx.sqlite.conn() {
        Ok(conn) => (
            store::all_schedules(&conn).unwrap_or_default(),
            store::all_deployments(&conn).unwrap_or_default(),
        ),
        Err(_) => Default::default(),
    };
    ok(json!({
        "status": "success",
        "running": running,
        "scheduled": scheduled.iter().map(|(n, s)| s.answer(n)).collect::<Vec<_>>(),
        "settings": settings.iter().map(|(n, d)| d.answer(n)).collect::<Vec<_>>(),
        "log_dir": log_dir(&ctx),
    }))
}

pub async fn status_one(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    let runner = &ctx.trading.runner;
    let run_id = runner.as_run_id(&name);
    let entry = runner
        .running()
        .into_iter()
        .find(|r| r.script == name || r.run_id == run_id)
        .map(|r| r.answer());
    let (schedule, saved) = match ctx.sqlite.conn() {
        Ok(conn) => (
            store::all_schedules(&conn)
                .ok()
                .and_then(|mut s| s.remove(&name)),
            store::read(&conn, &name).ok().flatten(),
        ),
        Err(_) => (None, None),
    };
    ok(json!({
        "status": "success",
        "file": name,
        "running": entry.is_some(),
        "run": entry,
        "schedule": schedule.map(|s| s.answer(&name)),
        "settings": saved.map(|d| d.answer(&name)),
        "logs": runner.logs_for(&run_id),
        "log_dir": log_dir(&ctx),
    }))
}

// ------------------------------------------------------------ settings

pub async fn list_settings(State(ctx): Ctx) -> Response {
    let all = ctx
        .sqlite
        .conn()
        .ok()
        .and_then(|c| store::all_deployments(&c).ok())
        .unwrap_or_default();
    let mut settings: Vec<Value> = all.iter().map(|(n, d)| d.answer(n)).collect();
    settings.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                v["file"].as_str().unwrap_or_default().to_string(),
                v["symbol"].as_str().unwrap_or_default().to_string(),
                v["interval"].as_str().unwrap_or_default().to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    ok(json!({"status": "success", "settings": settings, "products": store::PRODUCTS}))
}

pub async fn get_settings(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    let Ok(conn) = ctx.sqlite.conn() else {
        return err(500, "The run settings could not be read just now.");
    };
    match store::read(&conn, &name) {
        Ok(Some(d)) => ok(json!({
            "status": "success",
            "settings": d.answer(&name),
            "products": store::PRODUCTS,
        })),
        _ => {
            let why = match store::require(&conn, &name) {
                Ok(Err(w)) => w,
                _ => format!("{} has no run settings saved.", name),
            };
            err(404, why)
        }
    }
}

pub async fn set_settings(
    State(ctx): Ctx,
    User(user): User,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    if !is_script_name(&name) {
        return refusal(&name);
    }
    let Some(body) = object(&body) else {
        return err(
            400,
            "Send the instrument, the exchange and the interval to run this on.",
        );
    };
    let mut unknown: Vec<&str> = body
        .keys()
        .map(String::as_str)
        .filter(|k| !SETTINGS_FIELDS.contains(k))
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return err(
            400,
            format!(
                "Run settings are the instrument, the exchange, the interval, the product and the script's own parameters. This one also carried {}. Where a strategy's orders go is the platform's own setting and is not chosen here.",
                unknown.join(", ")
            ),
        );
    }
    for field in SETTINGS_FIELDS.iter().filter(|f| **f != "inputs") {
        if let Some(v) = body.get(*field) {
            if !v.is_null() && !v.is_string() {
                return err(400, format!("Give the {} as text, or leave it out.", field));
            }
        }
    }
    let inputs = body.get("inputs").cloned().unwrap_or(Value::Null);
    if !inputs.is_null() && !inputs.is_object() {
        return err(
            400,
            "Give the strategy parameters as a set of named values, or leave them out.",
        );
    }
    let text = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let input = store::SettingsInput {
        symbol: text("symbol"),
        exchange: text("exchange"),
        interval: text("interval"),
        product: text("product"),
        user_id: Some(user.username),
        inputs,
        deployment: text("deployment"),
    };
    let Ok(mut conn) = ctx.sqlite.conn() else {
        return err(500, "These run settings could not be saved on this server");
    };
    match store::write(&mut conn, &name, input, ctx.now()) {
        Ok(Ok((key, message))) => {
            let saved = store::read(&conn, &key).ok().flatten();
            ok(json!({
                "status": "success",
                "settings": saved.map(|d| d.answer(&key)),
                "message": message,
            }))
        }
        Ok(Err(why)) => err(400, why),
        Err(e) => {
            tracing::error!("Could not save OpenScript run settings: {}", e);
            err(400, "These run settings could not be saved on this server")
        }
    }
}

pub async fn clear_settings(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    match ctx.trading.runner.remove_deployment(&name) {
        Ok(message) => ok(json!({"status": "success", "file": name, "message": message})),
        Err(r) => refused(r),
    }
}

// ------------------------------------------------------------ schedule

pub async fn set_schedule(State(ctx): Ctx, Path(name): Path<String>, body: Bytes) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    let Some(body) = object(&body) else {
        return err(400, "Send a start time, as 24 hour HH:MM in IST.");
    };
    let entry = match schedule::parse(&body) {
        Ok(s) => s,
        Err(why) => return err(400, why),
    };
    let stored =
        ctx.sqlite.conn().map_err(|e| e.to_string()).and_then(|c| {
            store::set_schedule(&c, &name, &entry, ctx.now()).map_err(|e| e.to_string())
        });
    if let Err(e) = stored {
        tracing::error!("Could not store the schedule for {}: {}", name, e);
        return err(
            500,
            "The schedule could not be saved, so it was not set. Check that the app's data folder can be written to.",
        );
    }
    let window = match &entry.stop_time {
        Some(stop) => format!("{} to {} IST", entry.start_time, stop),
        None => format!("{} IST", entry.start_time),
    };
    ok(json!({
        "status": "success",
        "file": name,
        "schedule": entry.answer(&name),
        "message": format!("{} runs {} on {}.", name, window, entry.days.join(", ")),
    }))
}

pub async fn clear_schedule(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    if !names_something(&name) {
        return refusal(&name);
    }
    let removed = ctx
        .sqlite
        .conn()
        .map_err(|e| e.to_string())
        .and_then(|c| store::delete_schedule(&c, &name).map_err(|e| e.to_string()));
    if let Err(e) = removed {
        tracing::error!("Could not remove the schedule for {}: {}", name, e);
        return err(500, "The schedule could not be removed. Try again.");
    }
    ok(json!({
        "status": "success",
        "file": name,
        "message": format!("{} is no longer scheduled.", name),
    }))
}

// ------------------------------------------------------------ pickers

pub async fn instruments(State(ctx): Ctx, Query(q): Q) -> Response {
    let exchange = q
        .get("exchange")
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_default();
    let query = q.get("q").map(|s| s.trim()).unwrap_or_default();
    if query.chars().count() < 2 {
        return ok(json!({"status": "success", "data": []}));
    }
    let reply = crate::services::symbol_service::search(
        &ctx,
        query,
        (!exchange.is_empty()).then_some(exchange.as_str()),
    );
    crate::server::api_v1::send(reply)
}

pub async fn intervals(State(ctx): Ctx) -> Response {
    let reply = crate::services::market_data_service::intervals(&ctx);
    if reply.is_success() {
        return ok(reply.body);
    }
    err(400, "Connect your broker to see the intervals it serves.")
}

// ------------------------------------------------------------ books

async fn book(ctx: &AppState, name: &str, which: &str) -> Response {
    if !names_something(name) {
        return refusal(name);
    }
    let runner = &ctx.trading.runner;
    let run_id = runner.as_run_id(name);
    let tag = if is_deployment_id(&run_id) {
        run_id.clone()
    } else {
        String::new()
    };
    let mode: RunMode = runner.mode_for(name);
    let own = runner.orders_of(name);
    let services = runner_services(ctx);
    let answer = match which {
        "orderbook" => books::orderbook(services.as_ref(), &tag, mode, &own).await,
        "tradebook" => books::tradebook(services.as_ref(), &tag, mode, &own).await,
        _ => books::positions(services.as_ref(), mode, &own).await,
    };
    let status = if answer["status"] == "success" {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    json_response(status, answer)
}

fn runner_services(ctx: &AppState) -> Arc<dyn crate::trading::runner::services::RunnerServices> {
    ctx.trading.runner.services_handle()
}

pub async fn orderbook(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    book(&ctx, &name, "orderbook").await
}

pub async fn tradebook(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    book(&ctx, &name, "tradebook").await
}

pub async fn positions(State(ctx): Ctx, Path(name): Path<String>) -> Response {
    book(&ctx, &name, "positions").await
}

// ------------------------------------------------------------ the page

fn token(headers: &HeaderMap) -> String {
    headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn gone() -> Response {
    err(403, "This strategy page is not running any more.")
}

pub async fn host_spec(State(ctx): Ctx, Path(run): Path<String>, headers: HeaderMap) -> Response {
    match ctx.trading.runner.page_spec(&run, &token(&headers)) {
        None => gone(),
        Some(Ok(v)) => ok(v),
        Some(Err(r)) => refused(r),
    }
}

pub async fn host_bars(State(ctx): Ctx, Path(run): Path<String>, headers: HeaderMap) -> Response {
    match ctx.trading.runner.page_bars(&run, &token(&headers)).await {
        None => gone(),
        Some(Ok(v)) => ok(v),
        Some(Err(r)) => refused(r),
    }
}

pub async fn host_inbox(
    State(ctx): Ctx,
    Path(run): Path<String>,
    headers: HeaderMap,
    Query(q): Q,
) -> Response {
    let after = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    match ctx
        .trading
        .runner
        .page_inbox(&run, &token(&headers), after)
        .await
    {
        None => gone(),
        Some(v) => ok(v),
    }
}

pub async fn host_intents(
    State(ctx): Ctx,
    Path(run): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(body) = object(&body) else {
        return err(400, "Send the intents of one confirmed bar.");
    };
    if body.get("confirmed") != Some(&Value::Bool(true)) {
        return err(400, "Only a confirmed bar sends orders.");
    }
    let intents = body
        .get("intents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    match ctx
        .trading
        .runner
        .page_intents(&run, &token(&headers), &intents)
        .await
    {
        None => gone(),
        Some(v) => ok(v),
    }
}

pub async fn host_log(
    State(ctx): Ctx,
    Path(run): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let lines: Vec<String> = object(&body)
        .and_then(|b| b.get("lines").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|l| l.as_str().map(str::to_string))
        .collect();
    if ctx.trading.runner.page_log(&run, &token(&headers), &lines) {
        ok(json!({"status": "success"}))
    } else {
        gone()
    }
}

pub async fn host_ended(
    State(ctx): Ctx,
    Path(run): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let message = object(&body)
        .and_then(|b| b.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    if ctx
        .trading
        .runner
        .page_ended(&run, &token(&headers), &message)
    {
        ok(json!({"status": "success"}))
    } else {
        gone()
    }
}
