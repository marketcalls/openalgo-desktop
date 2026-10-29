//! The Flow editor's lookups (the web's `blueprints/flow.py`
//! `get_index_symbols_lot_sizes`, `get_symbol_lot_sizes`,
//! `get_option_strikes`): the signed-in user's, answered by
//! `crate::flow::lookups`.

use super::super::{Access, RouteSpec};
use crate::flow::lookups::{self as svc, StrikeQuery};
use crate::server::envelope::json_response;
use crate::services::core::Reply;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{Method, StatusCode},
    response::Response,
    routing::{get, post, MethodRouter},
};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn spec(
    path: &'static str,
    method: Method,
    make: fn() -> MethodRouter<Arc<AppState>>,
) -> RouteSpec {
    RouteSpec {
        path,
        method,
        access: Access::User,
        make,
    }
}

/// The lookup routes.
pub fn table() -> Vec<RouteSpec> {
    vec![
        spec("/flow/api/index-symbols", Method::GET, || {
            get(index_symbols)
        }),
        spec("/flow/api/symbol-lotsizes", Method::POST, || {
            post(symbol_lot_sizes)
        }),
        spec("/flow/api/option-strikes", Method::GET, || {
            get(option_strikes)
        }),
    ]
}

fn send(r: Reply) -> Response {
    json_response(
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        r.body,
    )
}

pub async fn index_symbols(State(ctx): Ctx) -> Response {
    send(svc::index_symbols(&ctx))
}

/// The body read as the web's `get_json(silent=True)`: anything that is not
/// JSON is no body.
pub async fn symbol_lot_sizes(State(ctx): Ctx, body: Bytes) -> Response {
    let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
    send(svc::symbol_lot_sizes(&ctx, parsed.as_ref()))
}

pub async fn option_strikes(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    send(svc::option_strikes(&ctx, &StrikeQuery::from_params(&q)).await)
}
