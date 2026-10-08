//! Web `blueprints/scalping.py`: the scalping terminal's routes under
//! `/scalping/api/`. Every route needs the signed-in user; writes carry the
//! session's CSRF token like every other page route. The handlers parse the
//! request and hand it to `crate::scalping::service`.

use super::{Access, RouteSpec};
use crate::scalping::service as svc;
use crate::server::envelope::json_response;
use crate::server::routes::webui::JsonBody;
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{delete, get, post},
};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

macro_rules! s {
    ($m:ident, $path:expr, $f:path) => {
        RouteSpec {
            path: $path,
            method: Method::$m,
            access: Access::User,
            make: || s!(@route $m, $f),
        }
    };
    (@route GET, $f:path) => { get($f) };
    (@route POST, $f:path) => { post($f) };
    (@route DELETE, $f:path) => { delete($f) };
}

/// The scalping routes, each with its access rule.
pub fn table() -> Vec<RouteSpec> {
    vec![
        s!(GET, "/scalping/api/underlyings", underlyings),
        s!(GET, "/scalping/api/history", history),
        s!(GET, "/scalping/api/all_underlyings", all_underlyings),
        s!(GET, "/scalping/api/expiry", expiry),
        s!(GET, "/scalping/api/strikes", strikes),
        s!(GET, "/scalping/api/search", search),
        s!(GET, "/scalping/api/futures", futures),
        s!(POST, "/scalping/api/order", order),
        s!(POST, "/scalping/api/close_leg", close_leg),
        s!(POST, "/scalping/api/close_all", close_all),
        s!(POST, "/scalping/api/cancel_all", cancel_all),
        s!(GET, "/scalping/api/tracked", tracked),
        s!(DELETE, "/scalping/api/tracked", reset_tracked),
        s!(GET, "/scalping/api/sl", get_sl),
        s!(POST, "/scalping/api/sl", upsert_sl),
        s!(DELETE, "/scalping/api/sl", delete_sl),
    ]
}

fn send(r: Reply) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

fn arg<'a>(q: &'a HashMap<String, String>, k: &str) -> &'a str {
    q.get(k).map(String::as_str).unwrap_or("")
}

pub async fn underlyings() -> Response {
    send(svc::underlyings())
}

pub async fn history(State(ctx): Ctx, Query(q): Q) -> Response {
    let interval = q.get("interval").map(String::as_str).unwrap_or("1m");
    send(
        svc::history(
            &ctx,
            arg(&q, "symbol"),
            arg(&q, "exchange"),
            interval,
            arg(&q, "date"),
        )
        .await,
    )
}

pub async fn all_underlyings(State(ctx): Ctx, Query(q): Q) -> Response {
    send(svc::all_underlyings(
        &ctx,
        arg(&q, "exchange"),
        arg(&q, "instrumenttype"),
    ))
}

pub async fn expiry(State(ctx): Ctx, Query(q): Q) -> Response {
    send(svc::expiry(
        &ctx,
        arg(&q, "underlying"),
        arg(&q, "exchange"),
        arg(&q, "instrumenttype"),
    ))
}

pub async fn strikes(State(ctx): Ctx, Query(q): Q) -> Response {
    send(
        svc::strikes(
            &ctx,
            arg(&q, "underlying"),
            arg(&q, "exchange"),
            arg(&q, "expiry"),
            q.get("strike_count").map(String::as_str),
        )
        .await,
    )
}

pub async fn search(State(ctx): Ctx, Query(q): Q) -> Response {
    send(svc::search(&ctx, arg(&q, "exchange"), arg(&q, "query")))
}

pub async fn futures(State(ctx): Ctx, Query(q): Q) -> Response {
    send(svc::futures(
        &ctx,
        arg(&q, "underlying"),
        arg(&q, "exchange"),
    ))
}

pub async fn order(State(ctx): Ctx, JsonBody(b): JsonBody) -> Response {
    send(svc::place(&ctx, &b).await)
}

pub async fn close_leg(State(ctx): Ctx, JsonBody(b): JsonBody) -> Response {
    send(svc::close_leg(&ctx, &b).await)
}

pub async fn close_all(State(ctx): Ctx) -> Response {
    send(svc::close_all(&ctx).await)
}

pub async fn cancel_all(State(ctx): Ctx) -> Response {
    send(svc::cancel_all(&ctx).await)
}

pub async fn tracked(State(ctx): Ctx) -> Response {
    send(svc::tracked(&ctx))
}

pub async fn reset_tracked(State(ctx): Ctx) -> Response {
    send(svc::reset_tracked(&ctx))
}

pub async fn get_sl(State(ctx): Ctx) -> Response {
    send(svc::get_sl(&ctx))
}

pub async fn upsert_sl(State(ctx): Ctx, JsonBody(b): JsonBody) -> Response {
    send(svc::upsert_sl(&ctx, &b))
}

pub async fn delete_sl(State(ctx): Ctx, JsonBody(b): JsonBody) -> Response {
    send(svc::delete_sl(&ctx, &b))
}
