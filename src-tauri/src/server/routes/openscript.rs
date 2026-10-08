//! Web `blueprints/openscript.py`: the trader's OpenScript sources and the
//! compiled program stored beside each, in the `openscript/` folder of the app
//! data directory, plus `/openscript/instrument`, the instrument facts the
//! engine reads.
//!
//! A source is served as `text/plain` and a program as `application/json`:
//! neither is ever a type the page could import as code. Writes carry the
//! session's CSRF token like every page route.

use super::{Access, RouteSpec};
use crate::server::envelope::json_response;
use crate::server::middleware::User;
use crate::state::AppState;
use crate::trading::names::{is_exchange_code, is_script_name, script_name_refusal};
use crate::trading::scripts::{Refused, StoredScript};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, MethodRouter},
};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// A save carries a source and a program, each escaped as a JSON string:
/// room for both at their limits.
pub const SAVE_BODY_LIMIT: usize = 4 * 1024 * 1024;
const MAX_SYMBOL_LENGTH: usize = 64;

fn save_route() -> MethodRouter<Arc<AppState>> {
    post(save).layer(DefaultBodyLimit::max(SAVE_BODY_LIMIT))
}

pub fn table() -> Vec<RouteSpec> {
    let spec =
        |path: &'static str, method: Method, make: fn() -> MethodRouter<Arc<AppState>>| RouteSpec {
            path,
            method,
            access: Access::User,
            make,
        };
    vec![
        spec("/openscript/index.json", Method::GET, || get(index)),
        spec("/openscript/instrument", Method::GET, || get(instrument)),
        spec("/openscript/program/{file}", Method::GET, || get(program)),
        spec("/openscript/{file}", Method::GET, || get(source)),
        spec("/openscript/{file}", Method::POST, save_route),
        spec("/openscript/{file}", Method::DELETE, || delete(remove)),
    ]
}

fn err(status: StatusCode, message: impl Into<String>) -> Response {
    json_response(
        status,
        json!({"status": "error", "message": message.into()}),
    )
}

fn rejected(file: &str) -> Response {
    err(StatusCode::BAD_REQUEST, script_name_refusal(file))
}

fn refused(r: Refused) -> Response {
    err(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::BAD_REQUEST),
        r.message,
    )
}

fn bytes_as(content_type: &'static str, bytes: Vec<u8>) -> Response {
    let mut r = Body::from(bytes).into_response();
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    r
}

/// Every stored source with its size, time and whether a program is beside it.
pub async fn index(State(ctx): Ctx) -> Response {
    let store = ctx.trading.scripts.clone();
    let list: Vec<StoredScript> = tokio::task::spawn_blocking(move || store.index())
        .await
        .unwrap_or_default();
    json_response(StatusCode::OK, json!(list))
}

/// The instrument record the engine reads, for `?symbol=&exchange=`.
pub async fn instrument(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let symbol = q.get("symbol").map(|s| s.trim()).unwrap_or_default();
    let exchange = q
        .get("exchange")
        .map(|s| s.trim().to_ascii_uppercase())
        .unwrap_or_default();
    if symbol.is_empty()
        || symbol.chars().count() > MAX_SYMBOL_LENGTH
        || symbol.chars().any(char::is_control)
    {
        return err(
            StatusCode::BAD_REQUEST,
            "Pick a symbol from the search to read its details.",
        );
    }
    if !is_exchange_code(&exchange) {
        return err(
            StatusCode::BAD_REQUEST,
            "Pick the symbol again from the search, so its exchange comes with it.",
        );
    }
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let mut facts = ctx.trading.facts.get(&ctx, symbol, &exchange, today);
    if let Some(m) = facts.as_object_mut() {
        m.insert("status".into(), json!("success"));
    }
    json_response(StatusCode::OK, facts)
}

/// The compiled program stored beside one source.
pub async fn program(State(ctx): Ctx, Path(file): Path<String>) -> Response {
    if !is_script_name(&file) {
        return rejected(&file);
    }
    match ctx.trading.scripts.program(&file) {
        Some(bytes) => bytes_as("application/json; charset=utf-8", bytes),
        None => err(
            StatusCode::NOT_FOUND,
            format!(
                "{} has no compiled program yet. Open it in the chart and save it once the console shows no errors.",
                file
            ),
        ),
    }
}

/// One source as plain text.
pub async fn source(State(ctx): Ctx, Path(file): Path<String>) -> Response {
    if !is_script_name(&file) {
        return rejected(&file);
    }
    if !ctx.trading.scripts.dir().is_dir() {
        return err(StatusCode::NOT_FOUND, "No scripts directory");
    }
    match ctx.trading.scripts.source(&file) {
        Some(bytes) => bytes_as("text/plain; charset=utf-8", bytes),
        None => err(StatusCode::NOT_FOUND, format!("{} was not found.", file)),
    }
}

/// Create or replace one source, and the compiled program beside it.
pub async fn save(
    State(ctx): Ctx,
    User(_user): User,
    Path(file): Path<String>,
    body: Bytes,
) -> Response {
    if !is_script_name(&file) {
        return rejected(&file);
    }
    let data: Option<Value> = serde_json::from_slice(&body).ok();
    let Some(source) = data
        .as_ref()
        .and_then(|d| d.get("source"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "Send a JSON body with a 'source' string",
        );
    };
    let program =
        match data.as_ref().and_then(|d| d.get("program")) {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(p.clone()),
            Some(_) => return err(
                StatusCode::BAD_REQUEST,
                "Send the compiled program as text, or leave it out to save the script on its own.",
            ),
        };
    let store = ctx.trading.scripts.clone();
    let name = file.clone();
    let saved =
        tokio::task::spawn_blocking(move || store.save(&name, &source, program.as_deref())).await;
    match saved {
        Ok(Ok(s)) => json_response(
            StatusCode::OK,
            json!({
                "status": "success",
                "file": file,
                "bytes": s.bytes,
                "mtime": s.mtime,
                "program": s.program,
            }),
        ),
        Ok(Err(r)) => refused(r),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save this script. Try again.",
        ),
    }
}

/// Delete one source, its backup and its program.
pub async fn remove(State(ctx): Ctx, Path(file): Path<String>) -> Response {
    if !is_script_name(&file) {
        return rejected(&file);
    }
    let store = ctx.trading.scripts.clone();
    let name = file.clone();
    match tokio::task::spawn_blocking(move || store.delete(&name)).await {
        Ok(Ok(())) => json_response(StatusCode::OK, json!({"status": "success", "file": file})),
        Ok(Err(r)) => refused(r),
        Err(_) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not delete this script. Try again.",
        ),
    }
}
